//! save / resurrect: persist the process table as a JSON array in dump.pmr.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::AppConfig;
use crate::daemon::state::Ctx;
use crate::daemon::{dlog, ops};
use crate::paths;

/// One dumped instance. `config.instances` is irrelevant here — every entry
/// resurrects exactly one instance with its original pm_id (pm2 does the same
/// by stripping `instances` from the dump).
#[derive(Serialize, Deserialize)]
pub struct DumpEntry {
    pub pm_id: u32,
    pub instance: u32,
    pub config: AppConfig,
}

/// Write the current table to dump.pmr (backing up the previous dump first).
pub fn save(ctx: &Ctx) -> Result<PathBuf> {
    let entries: Vec<DumpEntry> = {
        let table = ctx.table.lock().unwrap();
        let mut rows: Vec<&crate::daemon::state::Proc> = table.procs.values().collect();
        rows.sort_by_key(|p| p.pm_id);
        rows.iter()
            .map(|p| DumpEntry {
                pm_id: p.pm_id,
                instance: p.instance,
                config: p.config.clone(),
            })
            .collect()
    };
    let path = paths::dump_file();
    if path.exists() {
        let _ = std::fs::copy(&path, paths::dump_backup_file());
    }
    let json = serde_json::to_string_pretty(&entries)?;
    std::fs::write(&path, json).with_context(|| format!("cannot write {}", path.display()))?;
    dlog!("saved {} process(es) to {}", entries.len(), path.display());
    Ok(path)
}

/// Read the dump (falling back to the backup) and start every app that isn't
/// already in the table. Returns the pm_ids now covered by the dump.
pub async fn resurrect(ctx: &Arc<Ctx>) -> Result<Vec<u32>> {
    let raw = std::fs::read_to_string(paths::dump_file())
        .or_else(|_| std::fs::read_to_string(paths::dump_backup_file()))
        .context("no dump file found — run `pmr save` first")?;
    let entries: Vec<DumpEntry> = serde_json::from_str(&raw).context("dump file is corrupted")?;

    let mut ids = Vec::new();
    for entry in entries {
        let occupied = {
            let table = ctx.table.lock().unwrap();
            table.procs.contains_key(&entry.pm_id)
        };
        if occupied {
            ids.push(entry.pm_id); // already running — pm2 also skips these
            continue;
        }
        match ops::start_app(ctx, entry.config, Some((entry.pm_id, entry.instance))).await {
            Ok(started) => ids.extend(started),
            Err(e) => dlog!("resurrect: failed to start pm_id {}: {e:#}", entry.pm_id),
        }
    }
    Ok(ids)
}
