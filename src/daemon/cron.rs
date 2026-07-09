//! cron_restart: one task per proc with a cron expression, restarting on schedule.

use std::sync::Arc;

use crate::daemon::state::Ctx;
use crate::daemon::{dlog, ops};

/// Attach a cron-restart task to the proc if its config asks for one.
/// The JoinHandle is stored in the row and aborted on delete.
pub fn register(ctx: &Arc<Ctx>, pm_id: u32) {
    let expr = {
        let table = ctx.table.lock().unwrap();
        let Some(p) = table.procs.get(&pm_id) else {
            return;
        };
        match &p.config.cron_restart {
            Some(e) if !e.is_empty() => e.clone(),
            _ => return,
        }
    };
    let cron = match croner::Cron::new(&expr).with_seconds_optional().parse() {
        Ok(c) => c,
        Err(e) => {
            dlog!("[{pm_id}] invalid cron_restart '{expr}': {e}");
            return;
        }
    };
    let ctx2 = ctx.clone();
    let task = tokio::spawn(async move {
        loop {
            let now = chrono::Local::now();
            let Ok(next) = cron.find_next_occurrence(&now, false) else {
                return;
            };
            let wait = (next - now).to_std().unwrap_or_default();
            tokio::time::sleep(wait).await;
            // Proc gone → stop the schedule.
            if !ctx2.table.lock().unwrap().procs.contains_key(&pm_id) {
                return;
            }
            dlog!("[{pm_id}] cron restart fired ({expr})");
            let _ = ops::restart_one(&ctx2, pm_id).await;
        }
    });
    let mut table = ctx.table.lock().unwrap();
    if let Some(p) = table.procs.get_mut(&pm_id)
        && let Some(old) = p.cron_task.replace(task)
    {
        old.abort();
    }
}
