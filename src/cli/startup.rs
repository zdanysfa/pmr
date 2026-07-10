//! `pmr startup` / `unstartup` — systemd unit so the daemon and saved apps
//! come back on boot.

use std::path::Path;

use anyhow::{Context, Result, bail};

fn unit_path() -> String {
    let user = whoami();
    format!("/etc/systemd/system/pmr-{user}.service")
}

fn whoami() -> String {
    // Bare `sudo pmr startup` gives USER=root; SUDO_USER carries the real
    // caller so the unit doesn't silently target /root/.pmr.
    match std::env::var("USER") {
        Ok(u) if u != "root" => u,
        _ => std::env::var("SUDO_USER").unwrap_or_else(|_| "root".into()),
    }
}

fn render_unit() -> Result<String> {
    let user = whoami();
    let home = effective_home(&user);
    let exe = std::env::current_exe().context("cannot locate the pmr binary")?;
    let exe = exe.display();
    let path_env = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into());
    if home.starts_with("/tmp") {
        eprintln!(
            "[pmr] WARNING: PMR_HOME={} is on tmpfs — the dump (and your saved apps) \
             will not survive a reboot",
            home.display()
        );
    }
    Ok(format!(
        r#"[Unit]
Description=pmr process manager ({user})
Wants=network-online.target
After=network-online.target

[Service]
# resurrect spawns the detached daemon (which writes pmr.pid) and exits —
# classic forking. PIDFile lets systemd track the real daemon, so an
# OOM-killed/crashed daemon fails the unit and Restart brings the fleet back.
Type=forking
PIDFile={home}/pmr.pid
Restart=on-failure
RestartSec=5
User={user}
Environment=PATH={path_env}
Environment=PMR_HOME={home}
ExecStart={exe} resurrect
ExecReload={exe} reloadLogs
ExecStop={exe} kill
TimeoutStopSec=90

[Install]
WantedBy=multi-user.target
"#,
        home = home.display(),
    ))
}

/// PMR_HOME for the unit: explicit env wins; otherwise the target user's
/// home — under bare `sudo` the process HOME is /root, not the caller's.
fn effective_home(user: &str) -> std::path::PathBuf {
    if let Ok(h) = std::env::var("PMR_HOME")
        && !h.is_empty()
    {
        return h.into();
    }
    if let Ok(Some(u)) = nix::unistd::User::from_name(user) {
        return u.dir.join(".pmr");
    }
    crate::paths::home()
}

pub fn startup(print_only: bool) -> Result<()> {
    let unit = render_unit()?;
    let dest = unit_path();

    if print_only {
        println!("{unit}");
        println!("# would be written to {dest}");
        return Ok(());
    }

    if !nix::unistd::Uid::effective().is_root() {
        return sudo_self("startup");
    }

    std::fs::write(&dest, unit).with_context(|| format!("cannot write {dest}"))?;
    run("systemctl", &["daemon-reload"])?;
    let service = Path::new(&dest)
        .file_stem()
        .unwrap()
        .to_string_lossy()
        .to_string();
    run("systemctl", &["enable", &service])?;
    println!("[pmr] installed and enabled {service}");
    println!("[pmr] save your process list with `pmr save` — it will be resurrected on boot");
    Ok(())
}

pub fn unstartup() -> Result<()> {
    let dest = unit_path();
    if !Path::new(&dest).exists() {
        println!("[pmr] no startup unit installed ({dest})");
        return Ok(());
    }
    if !nix::unistd::Uid::effective().is_root() {
        return sudo_self("unstartup");
    }
    let service = Path::new(&dest)
        .file_stem()
        .unwrap()
        .to_string_lossy()
        .to_string();
    let _ = run("systemctl", &["disable", &service]);
    std::fs::remove_file(&dest)?;
    run("systemctl", &["daemon-reload"])?;
    println!("[pmr] removed {dest}");
    Ok(())
}

/// Re-exec this command through sudo, preserving PATH/PMR_HOME/USER so the
/// generated unit records the caller's real environment. Interactive terminal
/// only (sudo needs to ask for a password); otherwise print the command,
/// which is all pm2 ever does.
fn sudo_self(subcmd: &str) -> Result<()> {
    use std::io::IsTerminal;
    let exe = std::env::current_exe()?;
    let path_env = std::env::var("PATH").unwrap_or_default();
    let home = crate::paths::home();

    if std::io::stdin().is_terminal() {
        println!("[pmr] {subcmd} needs root — asking sudo:");
        let status = std::process::Command::new("sudo")
            .arg("env")
            .arg(format!("PATH={path_env}"))
            .arg(format!("PMR_HOME={}", home.display()))
            .arg(format!("USER={}", whoami()))
            .arg(&exe)
            .arg(subcmd)
            .status()
            .context("failed to run sudo")?;
        if !status.success() {
            bail!("sudo {subcmd} failed");
        }
        return Ok(());
    }
    bail!(
        "root required. Run:\n\n  sudo env PATH=$PATH PMR_HOME={} USER={} {} {subcmd}\n",
        home.display(),
        whoami(),
        exe.display()
    );
}

fn run(cmd: &str, args: &[&str]) -> Result<()> {
    let status = std::process::Command::new(cmd)
        .args(args)
        .status()
        .with_context(|| format!("failed to run {cmd}"))?;
    if !status.success() {
        bail!("{cmd} {} failed with {status}", args.join(" "));
    }
    Ok(())
}
