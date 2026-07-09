//! The pmr daemon: singleton guard, unix-socket server, signal handling,
//! graceful shutdown.

pub mod cron;
pub mod dump;
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

    use std::io::Write;
    let mut lock = lock;
    lock.set_len(0)?;
    write!(lock, "{}", std::process::id())?;
    lock.flush()?;

    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(serve(lock))
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
    });

    tokio::spawn(crate::daemon::worker::run(ctx.clone()));

    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let mut sigusr2 =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::user_defined2())?;

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
                    Err(e) => dlog!("accept failed: {e}"),
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

    let ids: Vec<u32> = {
        let table = ctx.table.lock().unwrap();
        table.procs.keys().copied().collect()
    };
    for id in ids {
        let _ = ops::stop_one(ctx, id).await;
    }

    let _ = std::fs::remove_file(paths::rpc_sock());
    let _ = std::fs::remove_file(paths::pid_file());
    dlog!("daemon stopped");
    std::process::exit(0);
}
