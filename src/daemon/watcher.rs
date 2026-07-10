//! File watching (`watch: true`): restart on change, with ignore filters and
//! debounce. Watches the app's cwd (or the script's directory).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use notify::{RecursiveMode, Watcher as _};

use crate::daemon::state::Ctx;
use crate::daemon::{dlog, ops};

const DEFAULT_IGNORES: [&str; 2] = ["node_modules", ".git"];
const DEFAULT_DEBOUNCE_MS: u64 = 200;

/// Attach a watcher to the proc if its config asks for one.
pub fn register(ctx: &Arc<Ctx>, pm_id: u32) {
    let (dir, ignores, debounce) = {
        let table = ctx.table.lock().unwrap();
        let Some(p) = table.procs.get(&pm_id) else {
            return;
        };
        // cmd_tx gone = a stop won the race between the launch ack and this
        // call — arming now would revive a stopped proc on file change.
        if !p.config.watch || p.cmd_tx.is_none() {
            return;
        }
        let dir: PathBuf = p
            .config
            .cwd
            .clone()
            .or_else(|| {
                std::path::Path::new(&p.config.script)
                    .parent()
                    .map(|d| d.to_path_buf())
            })
            .unwrap_or_else(|| ".".into());
        let mut ignores = p.config.ignore_watch.clone();
        ignores.extend(DEFAULT_IGNORES.iter().map(|s| s.to_string()));
        let debounce = p.config.watch_delay.unwrap_or(DEFAULT_DEBOUNCE_MS);
        (dir, ignores, debounce)
    };

    let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(16);
    // notify runs its own thread; hop relevant events into tokio via the channel.
    let mut watcher =
        match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(event) = res {
                // ponytail: substring ignore match; globset if users ask for globs
                let ignored = event.paths.iter().all(|p| {
                    let s = p.to_string_lossy();
                    ignores.iter().any(|ig| s.contains(ig.as_str()))
                });
                if !ignored
                    && (event.kind.is_create() || event.kind.is_modify() || event.kind.is_remove())
                {
                    let _ = tx.try_send(());
                }
            }
        }) {
            Ok(w) => w,
            Err(e) => {
                dlog!("[{pm_id}] cannot create file watcher: {e}");
                return;
            }
        };
    if let Err(e) = watcher.watch(&dir, RecursiveMode::Recursive) {
        dlog!("[{pm_id}] cannot watch {}: {e}", dir.display());
        return;
    }
    dlog!("[{pm_id}] watching {} for changes", dir.display());
    ctx.watchers.lock().unwrap().insert(pm_id, watcher);

    let ctx2 = ctx.clone();
    tokio::spawn(async move {
        loop {
            if rx.recv().await.is_none() {
                return; // watcher dropped (proc deleted)
            }
            // Debounce: absorb the burst, restart once.
            loop {
                match tokio::time::timeout(Duration::from_millis(debounce), rx.recv()).await {
                    Ok(Some(())) => continue,
                    Ok(None) => return,
                    Err(_) => break,
                }
            }
            if !ctx2.table.lock().unwrap().procs.contains_key(&pm_id) {
                return;
            }
            dlog!("[{pm_id}] file change detected, restarting");
            let _ = ops::restart_one(&ctx2, pm_id).await;
        }
    });
}
