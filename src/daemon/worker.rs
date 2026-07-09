//! Periodic worker: CPU/mem sampling, max_memory_restart, backoff reset.
//! Interval defaults to 30s like pm2; override with PMR_WORKER_INTERVAL (ms)
//! — tests rely on that.

use std::sync::Arc;
use std::time::Duration;

use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

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
    let mut sys = System::new();
    let mut tick = tokio::time::interval(interval());
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let over_limit = sample(&ctx, &mut sys);
        for pm_id in over_limit {
            dlog!("[{pm_id}] over max_memory_restart, restarting");
            let _ = ops::restart_one(&ctx, pm_id).await;
        }
    }
}

/// Refresh cpu/mem into the table; return pm_ids over their memory limit.
fn sample(ctx: &Ctx, sys: &mut System) -> Vec<u32> {
    let pids: Vec<sysinfo::Pid> = {
        let table = ctx.table.lock().unwrap();
        table
            .procs
            .values()
            .filter(|p| p.pid != 0)
            .map(|p| sysinfo::Pid::from_u32(p.pid))
            .collect()
    };
    if pids.is_empty() {
        return vec![];
    }
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&pids),
        true,
        ProcessRefreshKind::nothing().with_cpu().with_memory(),
    );

    let now = now_ms();
    let mut over = Vec::new();
    let mut table = ctx.table.lock().unwrap();
    for p in table.procs.values_mut() {
        if p.pid == 0 {
            p.monit = Monit::default();
            continue;
        }
        if let Some(proc_info) = sys.process(sysinfo::Pid::from_u32(p.pid)) {
            p.monit = Monit {
                cpu: proc_info.cpu_usage(),
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
