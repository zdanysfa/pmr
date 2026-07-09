//! `pmr startup` / `unstartup` — systemd unit so the daemon and saved apps
//! come back on boot.

use std::path::Path;

use anyhow::{Context, Result, bail};

fn unit_path() -> String {
    let user = whoami();
    format!("/etc/systemd/system/pmr-{user}.service")
}

fn whoami() -> String {
    std::env::var("USER").unwrap_or_else(|_| "root".into())
}

fn render_unit() -> Result<String> {
    let user = whoami();
    let home = crate::paths::home();
    let exe = std::env::current_exe().context("cannot locate the pmr binary")?;
    let exe = exe.display();
    let path_env = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into());
    Ok(format!(
        r#"[Unit]
Description=pmr process manager ({user})
After=network.target

[Service]
# resurrect spawns the detached daemon and exits; oneshot+RemainAfterExit
# keeps the unit "active" while the daemon runs.
Type=oneshot
User={user}
Environment=PATH={path_env}
Environment=PMR_HOME={home}
ExecStart={exe} resurrect
ExecReload={exe} reloadLogs
ExecStop={exe} kill
RemainAfterExit=yes

[Install]
WantedBy=multi-user.target
"#,
        home = home.display(),
    ))
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
        let exe = std::env::current_exe()?;
        bail!(
            "writing {dest} requires root. Run:\n\n  sudo env PATH=$PATH PMR_HOME={} USER={} {} startup\n",
            crate::paths::home().display(),
            whoami(),
            exe.display()
        );
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
        bail!("removing {dest} requires root; rerun with sudo");
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
