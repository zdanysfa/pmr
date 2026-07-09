//! CLI command implementations — thin wrappers over the `Pmr` client API.

use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::cli::{Cmd, table};
use crate::client::Pmr;
use crate::config::{self, AppConfig};
use crate::ipc::Target;

pub fn dispatch(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Init => init(),
        Cmd::Startup { print_only } => crate::cli::startup::startup(print_only),
        Cmd::Unstartup => crate::cli::startup::unstartup(),
        Cmd::Ping => {
            let mut pmr = Pmr::connect()?;
            let reply = pmr.ping()?;
            println!("pong — daemon v{} (pid {})", reply.version, reply.pid);
            Ok(())
        }
        Cmd::Kill => {
            match Pmr::try_connect() {
                Ok(mut pmr) => {
                    pmr.kill_daemon()?;
                    println!("[pmr] daemon stopped");
                }
                Err(_) => println!("[pmr] daemon was not running"),
            }
            Ok(())
        }
        Cmd::Start {
            target,
            name,
            instances,
            interpreter,
            cwd,
            env,
            no_autorestart,
            max_restarts,
            max_memory_restart,
            restart_delay,
            exp_backoff_restart_delay,
            cron_restart,
            watch,
            time,
            kill_timeout,
            max_log_size,
            health_check,
            args,
        } => {
            let mut pmr = Pmr::connect()?;

            // Config file → start every app in it.
            let path = Path::new(&target);
            let ext = path.extension().and_then(|e| e.to_str());
            let is_config =
                matches!(ext, Some("yaml") | Some("yml") | Some("toml")) || is_ecosystem_json(path);

            if is_config && path.exists() {
                let mut apps = config::load_ecosystem(path)?;
                if let Some(profile) = &env {
                    for app in &mut apps {
                        app.apply_env_profile(profile)?;
                    }
                }
                let started = pmr.start_many(apps)?;
                println!("{}", table::render_list(&started));
                return Ok(());
            }

            // Existing stopped process by name/id → restart it.
            if !path.exists() {
                if let Ok(procs) = pmr.describe(target.as_str())
                    && !procs.is_empty()
                {
                    let restarted = pmr.restart(target.as_str())?;
                    println!("{}", table::render_list(&restarted));
                    return Ok(());
                }
                bail!("script or config file not found: {target}");
            }

            // Plain script.
            let script = path
                .canonicalize()
                .with_context(|| format!("cannot resolve {target}"))?
                .display()
                .to_string();
            let mut app = AppConfig::new(script);
            app.name = name;
            if let Some(n) = instances {
                app.instances = n;
            }
            app.interpreter = interpreter;
            if let Some(c) = cwd {
                app.cwd = Some(c.into());
            } else {
                app.cwd = Path::new(&app.script).parent().map(|p| p.to_path_buf());
            }
            app.args = args;
            app.autorestart = !no_autorestart;
            if let Some(n) = max_restarts {
                app.max_restarts = n;
            }
            if let Some(m) = max_memory_restart {
                app.max_memory_restart = Some(config::parse_memory(&m)?);
            }
            if let Some(d) = restart_delay {
                app.restart_delay = d;
            }
            if let Some(d) = exp_backoff_restart_delay {
                app.exp_backoff_restart_delay = d;
            }
            app.cron_restart = cron_restart;
            app.watch = watch;
            if time {
                app.log_date_format = Some("%Y-%m-%dT%H:%M:%S".into());
            }
            if let Some(k) = kill_timeout {
                app.kill_timeout = k;
            }
            if let Some(s) = max_log_size {
                app.max_log_size = Some(config::parse_memory(&s)?);
            }
            if let Some(cmd) = health_check {
                app = app.health_check(cmd);
            }
            let started = pmr.start(app)?;
            println!("{}", table::render_list(&started));
            Ok(())
        }
        Cmd::Stop { target } => {
            let mut pmr = Pmr::connect()?;
            let procs = pmr.stop(target.as_str())?;
            println!("{}", table::render_list(&procs));
            Ok(())
        }
        Cmd::Restart { target } | Cmd::Reload { target } => {
            let mut pmr = Pmr::connect()?;
            let procs = pmr.restart(target.as_str())?;
            println!("{}", table::render_list(&procs));
            Ok(())
        }
        Cmd::Delete { target } => {
            let mut pmr = Pmr::connect()?;
            let procs = pmr.delete(target.as_str())?;
            for p in &procs {
                println!("[pmr] deleted {} (id {})", p.name, p.pm_id);
            }
            Ok(())
        }
        Cmd::Reset { target } => {
            let mut pmr = Pmr::connect()?;
            let procs = pmr.reset(target.as_str())?;
            for p in &procs {
                println!("[pmr] reset counters for {} (id {})", p.name, p.pm_id);
            }
            Ok(())
        }
        Cmd::Ls => {
            let mut pmr = Pmr::connect()?;
            let procs = pmr.list()?;
            println!("{}", table::render_list(&procs));
            Ok(())
        }
        Cmd::Jlist => {
            let mut pmr = Pmr::connect()?;
            let procs = pmr.list()?;
            println!("{}", serde_json::to_string_pretty(&procs)?);
            Ok(())
        }
        Cmd::Describe { target } => {
            let mut pmr = Pmr::connect()?;
            let procs = pmr.describe(target.as_str())?;
            if procs.is_empty() {
                bail!("no process found: {target}");
            }
            for p in &procs {
                println!("{}", table::render_describe(p));
            }
            Ok(())
        }
        Cmd::Env { target } => {
            let mut pmr = Pmr::connect()?;
            let procs = pmr.describe(target.as_str())?;
            if procs.is_empty() {
                bail!("no process found: {target}");
            }
            for p in &procs {
                println!("── {} (id {}) ──", p.name, p.pm_id);
                for (k, v) in &p.env {
                    println!("{k}={v}");
                }
            }
            Ok(())
        }
        Cmd::Id { name } => {
            let mut pmr = Pmr::connect()?;
            let procs = pmr.describe(name.as_str())?;
            if procs.is_empty() {
                bail!("no process found: {name}");
            }
            for p in &procs {
                println!("{}", p.pm_id);
            }
            Ok(())
        }
        Cmd::Pid { target } => {
            let mut pmr = Pmr::connect()?;
            let procs = match target {
                Some(t) => pmr.describe(t.as_str())?,
                None => pmr.list()?,
            };
            for p in &procs {
                if p.pid != 0 {
                    println!("{}", p.pid);
                }
            }
            Ok(())
        }
        Cmd::Scale { name, instances } => {
            let mut pmr = Pmr::connect()?;
            let procs = pmr.scale(&name, instances)?;
            println!("{}", table::render_list(&procs));
            Ok(())
        }
        Cmd::SendSignal { signal, target } => {
            let mut pmr = Pmr::connect()?;
            let sent = pmr.send_signal(target.as_str(), &signal)?;
            println!("[pmr] {signal} sent to {sent} process(es)");
            Ok(())
        }
        Cmd::Flush { target } => {
            let mut pmr = Pmr::connect()?;
            pmr.flush(target.as_deref().map(Target::parse))?;
            println!("[pmr] logs flushed");
            Ok(())
        }
        Cmd::ReloadLogs => {
            let mut pmr = Pmr::connect()?;
            pmr.reload_logs()?;
            println!("[pmr] log files reopened");
            Ok(())
        }
        Cmd::Save => {
            let mut pmr = Pmr::connect()?;
            let path = pmr.save()?;
            println!("[pmr] process list saved to {path}");
            Ok(())
        }
        Cmd::Resurrect => {
            let mut pmr = Pmr::connect()?;
            let procs = pmr.resurrect()?;
            println!("{}", table::render_list(&procs));
            Ok(())
        }
        Cmd::Logs {
            target,
            lines,
            err,
            out,
            nostream,
            timestamp,
            raw,
        } => crate::cli::logs::run(target, lines, err, out, nostream, timestamp, raw),
        Cmd::Monit => crate::monit::run(),
        Cmd::Completions { shell } => {
            let mut cmd = <crate::cli::Cli as clap::CommandFactory>::command();
            clap_complete::generate(shell, &mut cmd, "pmr", &mut std::io::stdout());
            Ok(())
        }
        Cmd::Daemon => unreachable!("handled in main"),
    }
}

/// A .json file could be an ecosystem file or something else — peek for "apps"/array.
fn is_ecosystem_json(path: &Path) -> bool {
    if path.extension().and_then(|e| e.to_str()) != Some("json") {
        return false;
    }
    let Ok(raw) = std::fs::read_to_string(path) else {
        return false;
    };
    matches!(
        serde_json::from_str::<serde_json::Value>(&raw),
        Ok(serde_json::Value::Array(_))
    ) || raw.contains("\"apps\"")
}

fn init() -> Result<()> {
    let dest = Path::new("ecosystem.yaml");
    if dest.exists() {
        bail!("ecosystem.yaml already exists here");
    }
    std::fs::write(dest, config::SAMPLE_ECOSYSTEM)?;
    println!("[pmr] wrote {}", dest.display());
    Ok(())
}
