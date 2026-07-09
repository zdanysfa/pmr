//! Terminal rendering for `ls` and `describe`.

use comfy_table::{Cell, Color, Table, presets::UTF8_BORDERS_ONLY};

use crate::ipc::{ProcessSnapshot, Status};

pub fn render_list(procs: &[ProcessSnapshot]) -> String {
    let mut table = Table::new();
    table.load_preset(UTF8_BORDERS_ONLY);
    table.set_header(vec![
        "id",
        "name",
        "namespace",
        "pid",
        "uptime",
        "↺",
        "status",
        "cpu",
        "mem",
    ]);
    for p in procs {
        table.add_row(vec![
            Cell::new(p.pm_id),
            Cell::new(&p.name),
            Cell::new(&p.namespace),
            Cell::new(if p.pid == 0 {
                "-".into()
            } else {
                p.pid.to_string()
            }),
            Cell::new(format_uptime(p.uptime_ms)),
            Cell::new(p.restarts),
            status_cell(p.status),
            Cell::new(format!("{:.1}%", p.monit.cpu)),
            Cell::new(format_bytes(p.monit.memory)),
        ]);
    }
    table.to_string()
}

pub fn render_describe(p: &ProcessSnapshot) -> String {
    let mut table = Table::new();
    table.load_preset(UTF8_BORDERS_ONLY);
    let cfg = &p.config;
    let rows: Vec<(&str, String)> = vec![
        ("id", p.pm_id.to_string()),
        ("name", p.name.clone()),
        ("namespace", p.namespace.clone()),
        ("status", p.status.to_string()),
        ("pid", p.pid.to_string()),
        ("instance", p.instance.to_string()),
        ("uptime", format_uptime(p.uptime_ms)),
        ("restarts", p.restarts.to_string()),
        ("unstable restarts", p.unstable_restarts.to_string()),
        (
            "exit code",
            p.exit_code.map_or("-".into(), |c| c.to_string()),
        ),
        ("script", cfg.script.clone()),
        ("args", cfg.args.join(" ")),
        (
            "interpreter",
            cfg.effective_interpreter()
                .unwrap_or_else(|| "direct".into()),
        ),
        (
            "cwd",
            cfg.cwd
                .as_ref()
                .map(|c| c.display().to_string())
                .unwrap_or_else(|| "-".into()),
        ),
        ("instances", cfg.instances.to_string()),
        ("autorestart", cfg.autorestart.to_string()),
        ("max restarts", cfg.max_restarts.to_string()),
        ("watch", cfg.watch.to_string()),
        (
            "cron restart",
            cfg.cron_restart.clone().unwrap_or_else(|| "-".into()),
        ),
        (
            "max memory restart",
            cfg.max_memory_restart
                .map(format_bytes)
                .unwrap_or_else(|| "-".into()),
        ),
        ("out log", p.out_file.display().to_string()),
        ("error log", p.error_file.display().to_string()),
        ("pid file", p.pid_file.display().to_string()),
        ("cpu", format!("{:.1}%", p.monit.cpu)),
        ("memory", format_bytes(p.monit.memory)),
    ];
    for (k, v) in rows {
        table.add_row(vec![Cell::new(k).fg(Color::Cyan), Cell::new(v)]);
    }
    table.to_string()
}

fn status_cell(status: Status) -> Cell {
    let color = match status {
        Status::Online => Color::Green,
        Status::Launching | Status::WaitingRestart => Color::Yellow,
        Status::Stopping | Status::Stopped => Color::Grey,
        Status::Errored => Color::Red,
    };
    Cell::new(status.to_string()).fg(color)
}

pub fn format_bytes(b: u64) -> String {
    const UNITS: [&str; 4] = ["b", "kb", "mb", "gb"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{b}b")
    } else {
        format!("{v:.1}{}", UNITS[i])
    }
}

pub fn format_uptime(since_epoch_ms: Option<i64>) -> String {
    let Some(start) = since_epoch_ms else {
        return "-".into();
    };
    let now = chrono::Utc::now().timestamp_millis();
    let mut secs = ((now - start).max(0) / 1000) as u64;
    let days = secs / 86400;
    secs %= 86400;
    let hours = secs / 3600;
    secs %= 3600;
    let mins = secs / 60;
    secs %= 60;
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {mins}m")
    } else if mins > 0 {
        format!("{mins}m {secs}s")
    } else {
        format!("{secs}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes() {
        assert_eq!(format_bytes(512), "512b");
        assert_eq!(format_bytes(2048), "2.0kb");
        assert_eq!(format_bytes(50 * 1024 * 1024), "50.0mb");
    }
}
