//! Log pumps: read a child's stdout/stderr line by line, append to log files,
//! publish on the event bus. Files reopen when the global generation counter
//! moves (SIGUSR2 / `reload_logs`) — that's the logrotate contract.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::Child;

use crate::daemon::state::{Ctx, now_ms};
use crate::ipc::Event;

/// Attach pumps to the child's stdout/stderr. Tasks end at EOF (child death).
pub fn pump(
    ctx: &Arc<Ctx>,
    pm_id: u32,
    name: &str,
    child: &mut Child,
    out_file: &Path,
    error_file: &Path,
) {
    let date_format = {
        let table = ctx.table.lock().unwrap();
        table
            .procs
            .get(&pm_id)
            .and_then(|p| p.config.log_date_format.clone())
    };
    if let Some(stdout) = child.stdout.take() {
        tokio::spawn(pump_stream(
            ctx.clone(),
            pm_id,
            name.to_string(),
            "out",
            stdout,
            out_file.to_path_buf(),
            date_format.clone(),
        ));
    }
    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(pump_stream(
            ctx.clone(),
            pm_id,
            name.to_string(),
            "err",
            stderr,
            error_file.to_path_buf(),
            date_format,
        ));
    }
}

async fn pump_stream<R: AsyncRead + Unpin>(
    ctx: Arc<Ctx>,
    pm_id: u32,
    name: String,
    stream: &'static str,
    reader: R,
    path: PathBuf,
    date_format: Option<String>,
) {
    let mut lines = BufReader::new(reader).lines();
    // ponytail: sync std write in async task — log lines are tiny local appends
    let mut file = open_append(&path);
    let mut generation = ctx.log_generation.load(Ordering::Relaxed);
    let mut write_failing = false;

    while let Ok(Some(line)) = lines.next_line().await {
        let current = ctx.log_generation.load(Ordering::Relaxed);
        if current != generation {
            generation = current;
            file = open_append(&path);
        }
        let formatted = match &date_format {
            Some(fmt) => format!("{}: {line}", chrono::Local::now().format(fmt)),
            None => line.clone(),
        };
        if let Some(f) = file.as_mut() {
            // Warn once when writes start failing (full disk, removed dir) —
            // silent log loss is worse than a noisy daemon log.
            match writeln!(f, "{formatted}") {
                Err(e) if !write_failing => {
                    write_failing = true;
                    crate::daemon::dlog!(
                        "[{name}:{pm_id}] cannot write to {}: {e} — log lines are being dropped",
                        path.display()
                    );
                }
                Ok(_) if write_failing => {
                    write_failing = false;
                    crate::daemon::dlog!(
                        "[{name}:{pm_id}] log writes to {} recovered",
                        path.display()
                    );
                }
                _ => {}
            }
        }
        ctx.publish(Event::Log {
            pm_id,
            name: name.clone(),
            stream: stream.into(),
            line,
            at: now_ms(),
        });
    }
}

fn open_append(path: &Path) -> Option<std::fs::File> {
    if is_null(path) {
        return None;
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()
}

fn is_null(path: &Path) -> bool {
    matches!(path.to_str(), Some("/dev/null") | Some("NULL") | Some(""))
}
