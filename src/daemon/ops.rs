//! Process operations shared by the RPC layer, worker loop, cron and watcher.

use std::sync::Arc;

use anyhow::{Result, anyhow, bail};
use tokio::sync::oneshot;

use crate::config::AppConfig;
use crate::daemon::state::{Ctx, Proc, SupervisorCmd};
use crate::daemon::{cron, dlog, health, supervisor, watcher};
use crate::ipc::{ProcessSnapshot, Status, Target};

/// Insert table rows for `app` (one per instance) and launch supervisors.
/// `fixed` pins pm_id/instance (resurrect); otherwise ids are allocated.
pub async fn start_app(
    ctx: &Arc<Ctx>,
    app: AppConfig,
    fixed: Option<(u32, u32)>,
) -> Result<Vec<u32>> {
    app.validate()?;
    let name = app.effective_name();

    // pm2 rejects a start when the name is already in the table (use restart).
    {
        let table = ctx.table.lock().unwrap();
        let clash = table
            .procs
            .values()
            .any(|p| p.name() == name && fixed.is_none_or(|(id, _)| p.pm_id != id));
        if clash && fixed.is_none() {
            bail!(
                "process '{name}' already exists — use `pmr restart {name}` or `pmr delete {name}` first"
            );
        }
    }

    let mut ids = Vec::new();
    let instances = if fixed.is_some() { 1 } else { app.instances };
    for i in 0..instances {
        let (pm_id, instance) = {
            let mut table = ctx.table.lock().unwrap();
            let (pm_id, instance) = match fixed {
                Some((id, inst)) => {
                    table.bump_next_id(id);
                    (id, inst)
                }
                None => (table.alloc_id(), i),
            };
            let proc = Proc::new(pm_id, instance, app.clone());
            table.procs.insert(pm_id, proc);
            (pm_id, instance)
        };
        let _ = instance;
        ids.push(pm_id);
        cron::register(ctx, pm_id);
        watcher::register(ctx, pm_id);
        health::register(ctx, pm_id);

        if app.autostart {
            launch(ctx, pm_id).await?;
        }
        ctx.publish_process_event(pm_id, &name, "start");
    }
    Ok(ids)
}

/// Spawn a supervisor for an existing (not running) table row and wait for the
/// first spawn attempt.
pub async fn launch(ctx: &Arc<Ctx>, pm_id: u32) -> Result<()> {
    {
        let table = ctx.table.lock().unwrap();
        let p = table
            .procs
            .get(&pm_id)
            .ok_or_else(|| anyhow!("no process with id {pm_id}"))?;
        if p.cmd_tx.is_some() {
            return Ok(()); // already supervised
        }
    }
    let (ack_tx, ack_rx) = oneshot::channel();
    supervisor::spawn(ctx.clone(), pm_id, ack_tx);
    match ack_rx.await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(anyhow!(e)),
        Err(_) => Err(anyhow!("supervisor for {pm_id} died before starting")),
    }
}

/// Resolve a target or fail when nothing matches.
pub fn resolve(ctx: &Ctx, target: &Target) -> Result<Vec<u32>> {
    let table = ctx.table.lock().unwrap();
    let ids = table.resolve(target);
    if ids.is_empty() {
        match target {
            Target::All => Ok(vec![]), // "all" over an empty table is not an error
            Target::Ids(v) => bail!("no process with id {:?}", v),
            Target::Names(v) => bail!("no process found: {}", v.join(", ")),
        }
    } else {
        Ok(ids)
    }
}

pub async fn stop_one(ctx: &Arc<Ctx>, pm_id: u32) -> Result<()> {
    let tx = {
        let mut table = ctx.table.lock().unwrap();
        let Some(p) = table.procs.get_mut(&pm_id) else {
            return Ok(());
        };
        match p.cmd_tx.clone() {
            Some(tx) => tx,
            None => {
                p.status = Status::Stopped; // already down (e.g. errored) — normalize
                return Ok(());
            }
        }
    };
    let (ack_tx, ack_rx) = oneshot::channel();
    if tx.send(SupervisorCmd::Stop(ack_tx)).await.is_ok() {
        let _ = ack_rx.await;
    }
    Ok(())
}

pub async fn restart_one(ctx: &Arc<Ctx>, pm_id: u32) -> Result<()> {
    let tx = {
        let table = ctx.table.lock().unwrap();
        let Some(p) = table.procs.get(&pm_id) else {
            bail!("no process with id {pm_id}");
        };
        p.cmd_tx.clone()
    };
    match tx {
        Some(tx) => {
            let (ack_tx, ack_rx) = oneshot::channel();
            if tx.send(SupervisorCmd::Restart(ack_tx)).await.is_ok() {
                match ack_rx.await {
                    Ok(()) => Ok(()),
                    // Supervisor exited (e.g. a racing stop) before handling
                    // our command — the ack sender was dropped. Cold start so
                    // "restart" never silently does nothing.
                    Err(_) => launch(ctx, pm_id).await,
                }
            } else {
                // Supervisor died between the check and the send — cold start.
                launch(ctx, pm_id).await
            }
        }
        None => {
            {
                let mut table = ctx.table.lock().unwrap();
                if let Some(p) = table.procs.get_mut(&pm_id) {
                    p.reset_state();
                    p.restarts += 1;
                }
            }
            launch(ctx, pm_id).await
        }
    }
}

pub async fn delete_one(ctx: &Arc<Ctx>, pm_id: u32) -> Result<()> {
    let (tx, name) = {
        let table = ctx.table.lock().unwrap();
        let Some(p) = table.procs.get(&pm_id) else {
            return Ok(());
        };
        (p.cmd_tx.clone(), p.name())
    };
    match tx {
        Some(tx) => {
            let (ack_tx, ack_rx) = oneshot::channel();
            if tx.send(SupervisorCmd::Delete(ack_tx)).await.is_ok() {
                let _ = ack_rx.await;
                return Ok(());
            }
            remove_row(ctx, pm_id, &name);
            Ok(())
        }
        None => {
            remove_row(ctx, pm_id, &name);
            Ok(())
        }
    }
}

fn remove_row(ctx: &Ctx, pm_id: u32, name: &str) {
    {
        let mut table = ctx.table.lock().unwrap();
        if let Some(mut p) = table.procs.remove(&pm_id) {
            p.abort_tasks();
        }
    }
    ctx.watchers.lock().unwrap().remove(&pm_id);
    ctx.publish_process_event(pm_id, name, "delete");
}

pub fn snapshots(ctx: &Ctx, ids: &[u32], with_env: bool) -> Vec<ProcessSnapshot> {
    let table = ctx.table.lock().unwrap();
    let mut out: Vec<ProcessSnapshot> = ids
        .iter()
        .filter_map(|id| table.procs.get(id))
        .map(|p| p.snapshot(with_env))
        .collect();
    out.sort_by_key(|p| p.pm_id);
    out
}

pub fn all_snapshots(ctx: &Ctx) -> Vec<ProcessSnapshot> {
    let table = ctx.table.lock().unwrap();
    let mut out: Vec<ProcessSnapshot> = table.procs.values().map(|p| p.snapshot(false)).collect();
    out.sort_by_key(|p| p.pm_id);
    out
}

/// Scale an app to N instances: spawn clones or delete the highest instances.
pub async fn scale(ctx: &Arc<Ctx>, name: &str, want: u32) -> Result<Vec<u32>> {
    if want == 0 {
        bail!("instances must be >= 1; use `pmr delete {name}` to remove the app");
    }
    let (mut current, config): (Vec<(u32, u32)>, AppConfig) = {
        let table = ctx.table.lock().unwrap();
        let mut rows: Vec<&Proc> = table.procs.values().filter(|p| p.name() == name).collect();
        if rows.is_empty() {
            bail!("no process found: {name}");
        }
        rows.sort_by_key(|p| p.instance);
        (
            rows.iter().map(|p| (p.pm_id, p.instance)).collect(),
            rows[0].config.clone(),
        )
    };
    let have = current.len() as u32;

    if want > have {
        let next_instance = current.iter().map(|(_, i)| i + 1).max().unwrap_or(0);
        for i in 0..(want - have) {
            let mut cfg = config.clone();
            cfg.instances = 1;
            let ids = start_app(ctx, cfg, Some((alloc_id(ctx), next_instance + i))).await?;
            for id in ids {
                current.push((id, next_instance + i));
            }
        }
        dlog!("[{name}] scaled up to {want}");
    } else if want < have {
        for (pm_id, _) in current.split_off(want as usize) {
            delete_one(ctx, pm_id).await?;
        }
        dlog!("[{name}] scaled down to {want}");
    }
    Ok(current.into_iter().map(|(id, _)| id).collect())
}

fn alloc_id(ctx: &Ctx) -> u32 {
    ctx.table.lock().unwrap().alloc_id()
}
