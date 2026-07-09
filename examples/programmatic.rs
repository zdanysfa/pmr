//! Programmatic usage — what a bot project does with `pmr = "0.1"` in Cargo.toml.
//! Run with: cargo run --example programmatic

use pmr::{AppConfig, Pmr};

fn main() -> anyhow::Result<()> {
    // Connects to the daemon, auto-spawning it when it's not running.
    let mut pmr = Pmr::connect()?;

    // Start a worker (a shell one-liner here; use your bot script in practice).
    let script = std::env::temp_dir().join("pmr-example-worker.sh");
    std::fs::write(
        &script,
        "#!/bin/bash\nwhile true; do echo working; sleep 1; done\n",
    )?;
    std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755))?;

    pmr.start(
        AppConfig::new(script.display().to_string())
            .name("example-worker")
            .instances(2)
            .env("MODE", "demo")
            .max_restarts(5),
    )?;

    // Inspect.
    for p in pmr.list()? {
        println!(
            "{:>3}  {:<16} {:<8} pid={}",
            p.pm_id,
            p.name,
            p.status.to_string(),
            p.pid
        );
    }

    // Stream a few log lines (a subscribed connection is stream-only,
    // so use a second connection for it).
    let stream = Pmr::connect()?.subscribe(&["log:out"], None)?;
    for event in stream.take(4) {
        if let Ok(pmr::Event::Log { name, line, .. }) = event {
            println!("log [{name}] {line}");
        }
    }

    // Clean up.
    pmr.delete("example-worker")?;
    println!("done");
    Ok(())
}
