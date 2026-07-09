//! Per-connection RPC handler: ndjson request → operation → ndjson response.
//! A `subscribe` request flips the connection into event-forwarding mode.

use std::sync::Arc;

use anyhow::Result;
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::broadcast::error::RecvError;

use crate::daemon::state::Ctx;
use crate::daemon::{dlog, dump, ops};
use crate::ipc::{EventFrame, Method, PingReply, Request, Response, Target};

pub async fn handle(ctx: Arc<Ctx>, stream: UnixStream) {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();

    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        let req: Request = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                let resp = Response {
                    id: 0,
                    result: None,
                    error: Some(format!("invalid request: {e}")),
                };
                if send(&mut write, &resp).await.is_err() {
                    return;
                }
                continue;
            }
        };
        let id = req.id;

        // Subscribe hijacks the connection.
        if let Method::Subscribe { topics, target } = req.call {
            let ok = Response {
                id,
                result: Some(json!({"subscribed": true})),
                error: None,
            };
            if send(&mut write, &ok).await.is_err() {
                return;
            }
            forward_events(ctx, write, topics, target).await;
            return;
        }

        let result = dispatch(&ctx, req.call).await;
        let resp = match result {
            Ok(value) => Response {
                id,
                result: Some(value),
                error: None,
            },
            Err(e) => Response {
                id,
                result: None,
                error: Some(format!("{e:#}")),
            },
        };
        if send(&mut write, &resp).await.is_err() {
            return;
        }
    }
}

async fn send(
    write: &mut tokio::net::unix::OwnedWriteHalf,
    resp: &Response,
) -> std::io::Result<()> {
    let mut line = serde_json::to_string(resp).expect("response serializes");
    line.push('\n');
    write.write_all(line.as_bytes()).await
}

async fn dispatch(ctx: &Arc<Ctx>, method: Method) -> Result<serde_json::Value> {
    match method {
        Method::Ping => Ok(serde_json::to_value(PingReply {
            pong: true,
            version: crate::VERSION.into(),
            pid: std::process::id(),
        })?),
        Method::Version => Ok(json!(crate::VERSION)),
        Method::Start { apps } => {
            let mut all_ids = Vec::new();
            for app in apps {
                all_ids.extend(ops::start_app(ctx, app, None).await?);
            }
            Ok(serde_json::to_value(ops::snapshots(ctx, &all_ids, false))?)
        }
        Method::Stop { target } => {
            let ids = ops::resolve(ctx, &target)?;
            for id in &ids {
                ops::stop_one(ctx, *id).await?;
            }
            Ok(serde_json::to_value(ops::snapshots(ctx, &ids, false))?)
        }
        Method::Restart { target } => {
            let ids = ops::resolve(ctx, &target)?;
            for id in &ids {
                ops::restart_one(ctx, *id).await?;
            }
            Ok(serde_json::to_value(ops::snapshots(ctx, &ids, false))?)
        }
        Method::Delete { target } => {
            let ids = ops::resolve(ctx, &target)?;
            let before = ops::snapshots(ctx, &ids, false);
            for id in &ids {
                ops::delete_one(ctx, *id).await?;
            }
            Ok(serde_json::to_value(before)?)
        }
        Method::Reset { target } => {
            let ids = ops::resolve(ctx, &target)?;
            {
                let mut table = ctx.table.lock().unwrap();
                for id in &ids {
                    if let Some(p) = table.procs.get_mut(id) {
                        p.reset_counters();
                    }
                }
            }
            Ok(serde_json::to_value(ops::snapshots(ctx, &ids, false))?)
        }
        Method::List => Ok(serde_json::to_value(ops::all_snapshots(ctx))?),
        Method::Describe { target } => {
            let ids = ops::resolve(ctx, &target)?;
            Ok(serde_json::to_value(ops::snapshots(ctx, &ids, true))?)
        }
        Method::Scale { name, instances } => {
            let ids = ops::scale(ctx, &name, instances).await?;
            Ok(serde_json::to_value(ops::snapshots(ctx, &ids, false))?)
        }
        Method::SendSignal { target, signal } => {
            let sig: nix::sys::signal::Signal = signal
                .parse()
                .map_err(|_| anyhow::anyhow!("unknown signal '{signal}'"))?;
            let ids = ops::resolve(ctx, &target)?;
            let mut sent = 0u32;
            {
                let table = ctx.table.lock().unwrap();
                for id in &ids {
                    if let Some(p) = table.procs.get(id)
                        && p.pid != 0
                    {
                        let _ =
                            nix::sys::signal::kill(nix::unistd::Pid::from_raw(p.pid as i32), sig);
                        sent += 1;
                    }
                }
            }
            Ok(json!({"sent": sent}))
        }
        Method::Flush { target } => {
            let ids = match &target {
                Some(t) => ops::resolve(ctx, t)?,
                None => ops::resolve(ctx, &Target::All)?,
            };
            let files: Vec<std::path::PathBuf> = {
                let table = ctx.table.lock().unwrap();
                ids.iter()
                    .filter_map(|id| table.procs.get(id))
                    .flat_map(|p| [p.out_file.clone(), p.error_file.clone()])
                    .collect()
            };
            for f in files {
                // O_APPEND writers stay valid across truncation.
                let _ = std::fs::OpenOptions::new()
                    .write(true)
                    .truncate(true)
                    .open(&f);
            }
            Ok(json!({"ok": true}))
        }
        Method::ReloadLogs => {
            ctx.log_generation
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            dlog!("log files will reopen (reload_logs)");
            Ok(json!({"ok": true}))
        }
        Method::Save => {
            let path = dump::save(ctx)?;
            let count = ctx.table.lock().unwrap().procs.len();
            Ok(json!({"path": path.display().to_string(), "count": count}))
        }
        Method::Resurrect => {
            let ids = dump::resurrect(ctx).await?;
            Ok(serde_json::to_value(ops::snapshots(ctx, &ids, false))?)
        }
        Method::Kill => {
            let _ = ctx.shutdown_tx.send(()).await;
            Ok(json!({"ok": true}))
        }
        Method::Subscribe { .. } => unreachable!("handled in caller"),
    }
}

/// Forward bus events matching `topics` (and optional target filter) until the
/// client hangs up.
async fn forward_events(
    ctx: Arc<Ctx>,
    mut write: tokio::net::unix::OwnedWriteHalf,
    topics: Vec<String>,
    target: Option<Target>,
) {
    let mut rx = ctx.bus.subscribe();
    let wanted_ids: Option<Vec<u32>> = target.as_ref().map(|t| {
        let table = ctx.table.lock().unwrap();
        table.resolve(t)
    });

    loop {
        let event = match rx.recv().await {
            Ok(e) => e,
            Err(RecvError::Lagged(n)) => {
                let frame = EventFrame {
                    event: "pmr:lagged".into(),
                    data: json!({"skipped": n}),
                };
                if write_frame(&mut write, &frame).await.is_err() {
                    return;
                }
                continue;
            }
            Err(RecvError::Closed) => return,
        };

        if !topics.is_empty() && !topics.iter().any(|t| *t == event.topic()) {
            continue;
        }
        if let (Some(ids), Some(pm_id)) = (&wanted_ids, event.pm_id()) {
            // Name-target subscriptions follow restarts; id lists are fixed.
            let matches = ids.contains(&pm_id)
                || matches!(&target, Some(Target::Names(names))
                    if event.proc_name().is_some_and(|n| names.iter().any(|w| w == n)));
            if !matches {
                continue;
            }
        }

        let frame = EventFrame {
            event: event.topic(),
            data: serde_json::to_value(&event).expect("event serializes"),
        };
        if write_frame(&mut write, &frame).await.is_err() {
            return;
        }
    }
}

async fn write_frame(
    write: &mut tokio::net::unix::OwnedWriteHalf,
    frame: &EventFrame,
) -> std::io::Result<()> {
    let mut line = serde_json::to_string(frame).expect("frame serializes");
    line.push('\n');
    write.write_all(line.as_bytes()).await
}
