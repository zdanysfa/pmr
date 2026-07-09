//! Log pumps: read a child's stdout/stderr line by line, append to log files,
//! publish on the event bus. Files reopen when the global generation counter
//! moves (SIGUSR2 / `reload_logs`) — that's the logrotate contract.
//!
//! Performance notes (the app must never be slowed by its own logging):
//! - the child's pipes are enlarged to 1 MB so bursts are absorbed by the
//!   kernel instead of blocking the app's `write()`,
//! - reads use a 64 KB buffer, file writes go through a `BufWriter` that is
//!   flushed when the read buffer runs dry (batched during floods, prompt
//!   when quiet),
//! - `disable_log_files` skips disk entirely; the bus still streams live.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::Child;

use crate::daemon::state::{Ctx, now_ms};
use crate::ipc::Event;

const READ_BUF: usize = 64 * 1024;
const WRITE_BUF: usize = 64 * 1024;
/// Kernel pipe capacity to request for child stdio (default is 64 KB).
const PIPE_SIZE: i32 = 1024 * 1024;

type LogFile = std::io::BufWriter<std::fs::File>;

/// Attach pumps to the child's stdout/stderr. Tasks end at EOF (child death).
pub fn pump(
    ctx: &Arc<Ctx>,
    pm_id: u32,
    name: &str,
    child: &mut Child,
    out_file: &Path,
    error_file: &Path,
) {
    let (date_format, no_files) = {
        let table = ctx.table.lock().unwrap();
        match table.procs.get(&pm_id) {
            Some(p) => (p.config.log_date_format.clone(), p.config.disable_log_files),
            None => (None, false),
        }
    };
    if let Some(stdout) = child.stdout.take() {
        grow_pipe(&stdout);
        tokio::spawn(pump_stream(
            ctx.clone(),
            pm_id,
            name.to_string(),
            "out",
            stdout,
            out_file.to_path_buf(),
            date_format.clone(),
            no_files,
        ));
    }
    if let Some(stderr) = child.stderr.take() {
        grow_pipe(&stderr);
        tokio::spawn(pump_stream(
            ctx.clone(),
            pm_id,
            name.to_string(),
            "err",
            stderr,
            error_file.to_path_buf(),
            date_format,
            no_files,
        ));
    }
}

/// Enlarge the kernel pipe so log bursts never block the child's write().
/// Best-effort: capped by /proc/sys/fs/pipe-max-size (1 MB default).
fn grow_pipe<F: std::os::fd::AsRawFd>(pipe: &F) {
    let _ = nix::fcntl::fcntl(
        pipe.as_raw_fd(),
        nix::fcntl::FcntlArg::F_SETPIPE_SZ(PIPE_SIZE),
    );
}

#[allow(clippy::too_many_arguments)]
async fn pump_stream<R: AsyncRead + Unpin>(
    ctx: Arc<Ctx>,
    pm_id: u32,
    name: String,
    stream: &'static str,
    reader: R,
    path: PathBuf,
    date_format: Option<String>,
    no_files: bool,
) {
    let mut lines = BufReader::with_capacity(READ_BUF, reader).lines();
    let open = |skip: bool| -> Option<LogFile> { if skip { None } else { open_append(&path) } };
    let mut file = open(no_files);
    let mut generation = ctx.log_generation.load(Ordering::Relaxed);
    let mut write_failing = false;

    while let Ok(Some(line)) = lines.next_line().await {
        let current = ctx.log_generation.load(Ordering::Relaxed);
        if current != generation {
            generation = current;
            file = open(no_files);
        }
        if let Some(f) = file.as_mut() {
            let res = match &date_format {
                Some(fmt) => writeln!(f, "{}: {line}", chrono::Local::now().format(fmt)),
                None => writeln!(f, "{line}"),
            };
            // Flush when the read buffer is empty — we're about to await, so
            // batched lines hit the disk now and `pmr logs --nostream` stays
            // fresh. During a flood this flushes rarely.
            let res = res.and_then(|_| {
                if lines.get_ref().buffer().is_empty() {
                    f.flush()
                } else {
                    Ok(())
                }
            });
            // Warn once when writes start failing (full disk, removed dir) —
            // silent log loss is worse than a noisy daemon log.
            match res {
                Err(e) if !write_failing => {
                    write_failing = true;
                    crate::daemon::dlog!(
                        "[{name}:{pm_id}] cannot write to {}: {e} — log lines are being dropped",
                        path.display()
                    );
                }
                Ok(()) if write_failing => {
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
    if let Some(mut f) = file {
        let _ = f.flush();
    }
}

fn open_append(path: &Path) -> Option<LogFile> {
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
        .map(|f| std::io::BufWriter::with_capacity(WRITE_BUF, f))
        .ok()
}

fn is_null(path: &Path) -> bool {
    matches!(path.to_str(), Some("/dev/null") | Some("NULL") | Some(""))
}
