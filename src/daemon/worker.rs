//! Periodic worker: CPU/mem sampling, max_memory_restart, backoff reset.
//! Interval defaults to 30s like pm2; override with PMR_WORKER_INTERVAL (ms)
//! — tests rely on that.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use sysinfo::{ProcessRefreshKind, ProcessesToUpdate};

use crate::daemon::state::{Ctx, now_ms};
use crate::daemon::{dlog, ops};
use crate::ipc::{Monit, Status};

/// pm2's EXP_BACKOFF_RESET_TIMER.
const BACKOFF_RESET_AFTER_MS: i64 = 30_000;

pub fn interval() -> Duration {
    let ms = std::env::var("PMR_WORKER_INTERVAL")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30_000u64);
    Duration::from_millis(ms.max(50))
}

pub async fn run(ctx: Arc<Ctx>) {
    let mut tick = tokio::time::interval(interval());
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        // force=true: the tick is the only enforcer of max_memory_restart and
        // backoff resets — it must never be starved by RPC polling hitting
        // the sample rate gate.
        let over_limit = sample(&ctx, true);
        for pm_id in over_limit {
            dlog!("[{pm_id}] over max_memory_restart, restarting");
            // Spawned: 20 slow-dying procs must not stall enforcement for
            // the rest of the fleet by 20 × kill_timeout.
            let ctx2 = ctx.clone();
            tokio::spawn(async move {
                let _ = ops::restart_one(&ctx2, pm_id).await;
            });
        }
        rotate_logs(&ctx);
    }
}

/// The daemon's own pmr.log is rotated at a fixed cap — nothing else bounds it
/// on a 30-day run.
const DAEMON_LOG_MAX: u64 = 10 * 1024 * 1024;

/// Native log rotation: rename any log file over its app's `max_log_size` to
/// `<file>.old` (one backup slot) and make the pumps reopen. Also recovers
/// log files an operator deleted (`rm logs/app-out.log` leaves the pump
/// writing to an unlinked inode — invisible disk growth) by bumping the
/// generation so pumps recreate them.
// ponytail: single .old slot; point users at OS logrotate for N generations
fn rotate_logs(ctx: &Ctx) {
    // Snapshot paths under the lock, stat AFTER releasing it — a hung
    // NFS/FUSE mount must not freeze the whole daemon behind the table lock.
    let mut files: Vec<(std::path::PathBuf, Option<u64>)> = {
        let table = ctx.table.lock().unwrap();
        table
            .procs
            .values()
            .filter(|p| p.pid != 0 && !p.config.disable_logs && !p.config.disable_log_files)
            .flat_map(|p| {
                [&p.out_file, &p.error_file]
                    .into_iter()
                    .map(|f| (f.clone(), p.config.max_log_size))
                    .collect::<Vec<_>>()
            })
            .collect()
    };
    files.sort();
    files.dedup();

    let mut bump = false;
    for (file, limit) in &files {
        match std::fs::metadata(file) {
            // Deleted out from under the pump: reopen recreates it.
            Err(_) => bump = true,
            Ok(m) if limit.is_some_and(|l| m.len() > l) => {
                let old = file.with_extension(format!(
                    "{}old",
                    file.extension()
                        .map(|e| format!("{}.", e.to_string_lossy()))
                        .unwrap_or_default()
                ));
                match std::fs::rename(file, &old) {
                    Ok(()) => {
                        dlog!("rotated {} → {}", file.display(), old.display());
                        bump = true;
                    }
                    Err(e) => dlog!("cannot rotate {}: {e}", file.display()),
                }
            }
            Ok(_) => {}
        }
    }
    if bump {
        // One bump reopens every pump; the ones whose file didn't move just
        // reopen the same path — harmless.
        ctx.log_generation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    rotate_daemon_log();
}

/// Rotate pmr.log (dlog's stderr) once it exceeds the cap: rename to .old and
/// dup2 a fresh file over fd 1/2. Skipped on a tty (foreground `pmr daemon`).
fn rotate_daemon_log() {
    let path = crate::paths::daemon_log();
    if !std::fs::metadata(&path).is_ok_and(|m| m.len() > DAEMON_LOG_MAX) {
        return;
    }
    if nix::unistd::isatty(2).unwrap_or(false) {
        return;
    }
    let old = path.with_extension("log.old");
    if let Err(e) = std::fs::rename(&path, &old) {
        dlog!("cannot rotate daemon log: {e}");
        return;
    }
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(f) => {
            use std::os::fd::AsRawFd;
            let fd = f.as_raw_fd();
            let _ = nix::unistd::dup2(fd, 1);
            let _ = nix::unistd::dup2(fd, 2);
            dlog!("daemon log rotated to {}", old.display());
        }
        Err(e) => dlog!("cannot reopen daemon log: {e}"),
    }
}

/// Refresh cpu/mem into the table; return pm_ids over their memory limit.
/// Called every worker tick (force=true) AND on-demand from `list`/`describe`
/// RPCs so metrics are fresh like pm2's pidusage-per-request (lock order:
/// sys → table).
pub fn sample(ctx: &Ctx, force: bool) -> Vec<u32> {
    // Refreshing faster than MINIMUM_CPU_UPDATE_INTERVAL makes sysinfo skip
    // the global cpu refresh and return garbage cpu%; the stored monit is
    // fresher than that anyway. compare_exchange so two concurrent `list`
    // calls can't both pass; a backward wall-clock step (negative delta)
    // counts as expired instead of freezing sampling for the step duration.
    let now = now_ms();
    let min_gap = sysinfo::MINIMUM_CPU_UPDATE_INTERVAL.as_millis() as i64 + 50;
    let last = ctx.last_sample_ms.load(Ordering::Relaxed);
    if !force {
        if (0..min_gap).contains(&(now - last)) {
            return vec![];
        }
        if ctx
            .last_sample_ms
            .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
        {
            return vec![]; // another sampler won the race
        }
    } else {
        ctx.last_sample_ms.store(now, Ordering::Relaxed);
    }

    let live: Vec<sysinfo::Pid> = {
        let table = ctx.table.lock().unwrap();
        table
            .procs
            .values()
            .filter(|p| p.pid != 0)
            .map(|p| sysinfo::Pid::from_u32(p.pid))
            .collect()
    };
    // Also refresh pids from the previous sample: dead ones fail the refresh
    // and get evicted from `sys`, keeping its map bounded to live processes.
    let mut pids = live.clone();
    {
        let mut prev = ctx.prev_sample_pids.lock().unwrap();
        pids.extend(prev.iter().filter(|p| !live.contains(p)));
        *prev = live;
    }
    if pids.is_empty() {
        return vec![];
    }
    let mut sys = ctx.sys.lock().unwrap();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&pids),
        true,
        ProcessRefreshKind::nothing().with_cpu().with_memory(),
    );

    let mut over = Vec::new();
    let mut table = ctx.table.lock().unwrap();
    for p in table.procs.values_mut() {
        if p.pid == 0 {
            p.monit = Monit::default();
            continue;
        }
        if let Some(proc_info) = sys.process(sysinfo::Pid::from_u32(p.pid)) {
            let mut cpu = proc_info.cpu_usage();
            // sysinfo needs two refreshes for a cpu delta, so a fresh pid reads
            // 0%. First sample (monit still empty) falls back to the lifetime
            // average — same as pm2's pidusage, which is why pm2 shows cpu
            // right after start.
            if cpu == 0.0 && p.monit.memory == 0 {
                cpu = proc_info.accumulated_cpu_time() as f32 * 100.0
                    / (proc_info.run_time().max(1) * 1000) as f32;
            }
            p.monit = Monit {
                cpu,
                memory: proc_info.memory(),
            };
            if let Some(limit) = p.config.max_memory_restart
                && p.monit.memory > limit
            {
                over.push(p.pm_id);
            }
        }
        // Reset exponential backoff after stable uptime, like pm2's Worker.
        if p.status == Status::Online
            && p.prev_restart_delay > 0
            && p.uptime_ms
                .is_some_and(|u| now - u > BACKOFF_RESET_AFTER_MS)
        {
            p.prev_restart_delay = 0;
        }
    }
    over
}
