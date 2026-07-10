//! The pmr daemon: singleton guard, unix-socket server, signal handling,
//! graceful shutdown.

pub mod cron;
pub mod dump;
pub mod health;
pub mod logs;
pub mod ops;
pub mod rpc;
pub mod state;
pub mod supervisor;
pub mod watcher;
pub mod worker;

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result};
use nix::fcntl::{Flock, FlockArg};
use tokio::net::UnixListener;
use tokio::sync::{broadcast, mpsc};

use crate::daemon::state::Ctx;
use crate::ipc::Event;
use crate::paths;

/// Timestamped daemon log line (stdout/stderr are redirected to pmr.log).
macro_rules! dlog {
    ($($arg:tt)*) => {
        eprintln!("{}: [pmr] {}", chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f"), format_args!($($arg)*))
    };
}
pub(crate) use dlog;

pub fn run() -> Result<()> {
    paths::ensure_dirs()?;

    // ~6 fds per managed process: the default soft NOFILE of 1024 dies at
    // ~170 procs. Raise to the hard limit up front.
    if let Ok((soft, hard)) =
        nix::sys::resource::getrlimit(nix::sys::resource::Resource::RLIMIT_NOFILE)
        && soft < hard
    {
        let _ =
            nix::sys::resource::setrlimit(nix::sys::resource::Resource::RLIMIT_NOFILE, hard, hard);
    }

    // Singleton: exclusive flock on the pid file. Losing the race is fine —
    // another daemon is up and the client will connect to it.
    let pid_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(paths::pid_file())
        .with_context(|| format!("cannot open {}", paths::pid_file().display()))?;
    let lock = match Flock::lock(pid_file, FlockArg::LockExclusiveNonblock) {
        Ok(l) => l,
        Err(_) => {
            eprintln!("[pmr] daemon already running, exiting");
            return Ok(());
        }
    };
    // Holding the lock: any existing socket is stale.
    let _ = std::fs::remove_file(paths::rpc_sock());
    warn_orphans();

    use std::io::Write;
    let mut lock = lock;
    lock.set_len(0)?;
    write!(lock, "{}", std::process::id())?;
    lock.flush()?;

    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(serve(lock))
}

/// After an unclean daemon death (SIGKILL/OOM) children survive in their own
/// process groups. Killing them here risks recycled pids, so warn loudly
/// instead — a blind `resurrect` would double-run every app.
// ponytail: warn-only; pid-file adoption if users hit this often
fn warn_orphans() {
    let Ok(entries) = std::fs::read_dir(paths::pid_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if let Ok(s) = std::fs::read_to_string(&path)
            && let Ok(pid) = s.trim().parse::<i32>()
            && nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok()
        {
            dlog!(
                "WARNING: stale pid file {} points at live pid {pid} — a previous daemon died uncleanly; \
                 that process is unmanaged and `pmr resurrect` would start a duplicate. \
                 Inspect with `ps -p {pid}` and kill it before resurrecting.",
                path.display()
            );
        }
    }
}

async fn serve(_lock: Flock<std::fs::File>) -> Result<()> {
    let listener = UnixListener::bind(paths::rpc_sock())
        .with_context(|| format!("cannot bind {}", paths::rpc_sock().display()))?;
    let _ = std::fs::set_permissions(paths::rpc_sock(), std::fs::Permissions::from_mode(0o700));

    let (bus, _) = broadcast::channel::<Event>(1024);
    let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);
    let ctx = Arc::new(Ctx {
        table: Mutex::new(Default::default()),
        bus,
        log_generation: AtomicU64::new(0),
        shutting_down: AtomicBool::new(false),
        shutdown_tx,
        watchers: Mutex::new(HashMap::new()),
        sys: Mutex::new(sysinfo::System::new()),
        last_sample_ms: std::sync::atomic::AtomicI64::new(0),
        prev_sample_pids: Mutex::new(Vec::new()),
    });

    tokio::spawn(crate::daemon::worker::run(ctx.clone()));

    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let mut sigusr2 =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::user_defined2())?;
    // Default SIGHUP action is instant death (no dump, stale socket, orphaned
    // children) — and a daemon spawned from an SSH session gets HUP on
    // disconnect. Installing a handler is enough to survive it.
    let mut sighup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;

    dlog!(
        "daemon v{} online (pid {}, home {})",
        crate::VERSION,
        std::process::id(),
        paths::home().display()
    );

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _)) => {
                        tokio::spawn(rpc::handle(ctx.clone(), stream));
                    }
                    Err(e) => {
                        dlog!("accept failed: {e}");
                        // EMFILE fails instantly and forever — without a pause
                        // this loop spins a core and floods pmr.log.
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    }
                }
            }
            _ = sigterm.recv() => {
                dlog!("SIGTERM received");
                break;
            }
            _ = sigint.recv() => {
                dlog!("SIGINT received");
                break;
            }
            _ = sigusr2.recv() => {
                ctx.log_generation.fetch_add(1, Ordering::Relaxed);
                dlog!("SIGUSR2: log files will reopen");
            }
            _ = sighup.recv() => {
                dlog!("SIGHUP ignored (use SIGTERM or `pmr kill` to stop the daemon)");
            }
            _ = shutdown_rx.recv() => {
                dlog!("kill requested over RPC");
                break;
            }
        }
    }

    graceful_shutdown(&ctx).await;
    Ok(())
}

/// Dump the table, stop every child with the full kill sequence, clean up.
async fn graceful_shutdown(ctx: &Arc<Ctx>) {
    ctx.shutting_down.store(true, Ordering::Relaxed);
    ctx.publish(Event::DaemonKill);

    if let Err(e) = dump::save(ctx) {
        dlog!("dump on shutdown failed: {e:#}");
    }

    // Stop everything in parallel — 25 stubborn apps must not take
    // 25 × kill_timeout and blow past systemd's stop timeout.
    let ids: Vec<u32> = {
        let table = ctx.table.lock().unwrap();
        table.procs.keys().copied().collect()
    };
    let mut handles = Vec::with_capacity(ids.len());
    for id in ids {
        let ctx = ctx.clone();
        handles.push(tokio::spawn(async move {
            let _ = ops::stop_one(&ctx, id).await;
        }));
    }
    for h in handles {
        let _ = h.await;
    }

    // Let the log pumps drain pipe EOFs and flush — exit(0) right after the
    // stop acks loses the children's final lines (their shutdown output).
    // ponytail: fixed grace instead of tracking pump handles
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;

    let _ = std::fs::remove_file(paths::rpc_sock());
    let _ = std::fs::remove_file(paths::pid_file());
    dlog!("daemon stopped");
    std::process::exit(0);
}
