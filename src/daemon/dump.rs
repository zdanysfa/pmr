//! save / resurrect: persist the process table as a JSON array in dump.pmr.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
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
    // pm2 parity (ActionMethods dumpProcessList): an empty table usually means
    // the daemon just crashed/restarted — refuse to destroy the last dump.
    if entries.is_empty() {
        if path.exists() {
            bail!(
                "process list is empty — refusing to overwrite {} (delete it manually if intended)",
                path.display()
            );
        }
        return Ok(path);
    }
    // Back up only a dump that parses: never replace a good .bak with a
    // corrupt fragment left by a power loss.
    if let Ok(old) = std::fs::read_to_string(&path)
        && serde_json::from_str::<Vec<DumpEntry>>(&old).is_ok()
    {
        let _ = std::fs::copy(&path, paths::dump_backup_file());
    }
    let json = serde_json::to_string_pretty(&entries)?;
    // Atomic AND durable: temp file, fsync data, rename, fsync dir — this is
    // the one file whose whole job is surviving power loss.
    let tmp = path.with_extension("pmr.tmp");
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600) // dumps carry app env vars (secrets)
            .open(&tmp)
            .with_context(|| format!("cannot write {}", tmp.display()))?;
        f.write_all(json.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &path)
        .with_context(|| format!("cannot move dump into place at {}", path.display()))?;
    if let Some(dir) = path.parent()
        && let Ok(d) = std::fs::File::open(dir)
    {
        let _ = d.sync_all();
    }
    dlog!("saved {} process(es) to {}", entries.len(), path.display());
    Ok(path)
}

/// Read the dump (falling back to the backup) and start every app that isn't
/// already in the table. Returns the pm_ids now covered by the dump.
pub async fn resurrect(ctx: &Arc<Ctx>) -> Result<Vec<u32>> {
    // Fall back to the backup on read failure AND on parse failure — a dump
    // truncated by power loss must not turn boot into "zero apps started"
    // while an intact .bak sits right next to it (pm2 does the same).
    let entries: Vec<DumpEntry> = match std::fs::read_to_string(paths::dump_file())
        .map_err(anyhow::Error::from)
        .and_then(|raw| serde_json::from_str(&raw).map_err(anyhow::Error::from))
    {
        Ok(e) => e,
        Err(e) => {
            let raw = std::fs::read_to_string(paths::dump_backup_file())
                .with_context(|| format!("no usable dump found ({e:#}) — run `pmr save` first"))?;
            dlog!("dump.pmr unusable ({e:#}); resurrecting from backup");
            serde_json::from_str(&raw).context("backup dump is corrupted too")?
        }
    };

    // Names present BEFORE resurrecting: a manual `pmr start` after an
    // unclean death must not get a same-name twin (pm2 parity). Snapshot
    // up front so a multi-instance app inside the dump still fully restores.
    let preexisting: std::collections::HashSet<String> = {
        let table = ctx.table.lock().unwrap();
        table.procs.values().map(|p| p.name()).collect()
    };

    let mut ids = Vec::new();
    for entry in entries {
        let occupied = {
            let table = ctx.table.lock().unwrap();
            table.procs.contains_key(&entry.pm_id)
                || preexisting.contains(&entry.config.effective_name())
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
