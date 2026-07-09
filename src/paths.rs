//! PMR home directory layout. Everything lives under `$PMR_HOME` (default `~/.pmr`).

use std::path::PathBuf;

pub fn home() -> PathBuf {
    if let Ok(h) = std::env::var("PMR_HOME")
        && !h.is_empty()
    {
        return PathBuf::from(h);
    }
    let base = std::env::var("HOME").unwrap_or_else(|_| "/etc".into());
    PathBuf::from(base).join(".pmr")
}

pub fn rpc_sock() -> PathBuf {
    home().join("rpc.sock")
}

pub fn pid_file() -> PathBuf {
    home().join("pmr.pid")
}

pub fn daemon_log() -> PathBuf {
    home().join("pmr.log")
}

pub fn dump_file() -> PathBuf {
    home().join("dump.pmr")
}

pub fn dump_backup_file() -> PathBuf {
    home().join("dump.pmr.bak")
}

pub fn log_dir() -> PathBuf {
    home().join("logs")
}

pub fn pid_dir() -> PathBuf {
    home().join("pids")
}

/// Create the home dir tree if missing. Idempotent.
pub fn ensure_dirs() -> std::io::Result<()> {
    std::fs::create_dir_all(log_dir())?;
    std::fs::create_dir_all(pid_dir())?;
    Ok(())
}

/// pm2-compatible name sanitization for file names: `[^a-zA-Z0-9.-]` → `-`.
pub fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// Default log file path for an app instance: `logs/<name>-<id>-out.log` / `-error.log`.
pub fn default_log_path(name: &str, pm_id: u32, kind: &str, merge_logs: bool) -> PathBuf {
    let base = sanitize_name(name);
    if merge_logs {
        log_dir().join(format!("{base}-{kind}.log"))
    } else {
        log_dir().join(format!("{base}-{pm_id}-{kind}.log"))
    }
}

/// Default pid file path: `pids/<name>-<id>.pid`.
pub fn default_pid_path(name: &str, pm_id: u32) -> PathBuf {
    pid_dir().join(format!("{}-{}.pid", sanitize_name(name), pm_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize() {
        assert_eq!(sanitize_name("my app/v2"), "my-app-v2");
        assert_eq!(sanitize_name("bot.js"), "bot.js");
    }
}
