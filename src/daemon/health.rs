//! Health checks: run a command periodically per app; N consecutive failures
//! restart the process. Catches "online but hung" — a state pm2 cannot see.

use std::sync::Arc;
use std::time::Duration;

use crate::daemon::state::Ctx;
use crate::daemon::{dlog, ops};
use crate::ipc::Status;

/// Attach a health-check task if the proc's config asks for one.
/// Handle stored in the row, aborted when the proc is deleted.
pub fn register(ctx: &Arc<Ctx>, pm_id: u32) {
    let hc = {
        let table = ctx.table.lock().unwrap();
        let Some(p) = table.procs.get(&pm_id) else {
            return;
        };
        match &p.config.health_check {
            Some(hc) => hc.clone(),
            None => return,
        }
    };

    let ctx2 = ctx.clone();
    let task = tokio::spawn(async move {
        let mut fails: u32 = 0;
        loop {
            tokio::time::sleep(Duration::from_millis(hc.interval)).await;

            // Proc gone → stop; not online → nothing to check.
            let (name, online, env) = {
                let table = ctx2.table.lock().unwrap();
                match table.procs.get(&pm_id) {
                    None => return,
                    Some(p) => (p.name(), p.status == Status::Online, p.child_env()),
                }
            };
            if !online {
                fails = 0;
                continue;
            }

            let mut cmd = tokio::process::Command::new("sh");
            cmd.arg("-c")
                .arg(&hc.command)
                .envs(&env)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true);

            let healthy =
                match tokio::time::timeout(Duration::from_millis(hc.timeout), cmd.status()).await {
                    Ok(Ok(status)) => status.success(),
                    Ok(Err(_)) | Err(_) => false, // spawn error or hang = unhealthy
                };

            if healthy {
                if fails > 0 {
                    dlog!("[{name}:{pm_id}] health check recovered");
                }
                fails = 0;
                continue;
            }
            fails += 1;
            dlog!(
                "[{name}:{pm_id}] health check failed ({fails}/{})",
                hc.max_fails
            );
            if fails >= hc.max_fails {
                ctx2.publish_process_event(pm_id, &name, "health check failed");
                dlog!("[{name}:{pm_id}] restarting after {fails} failed health checks");
                fails = 0;
                let _ = ops::restart_one(&ctx2, pm_id).await;
            }
        }
    });

    let mut table = ctx.table.lock().unwrap();
    if let Some(p) = table.procs.get_mut(&pm_id)
        && let Some(old) = p.health_task.replace(task)
    {
        old.abort();
    }
}
