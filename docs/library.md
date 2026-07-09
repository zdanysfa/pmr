# Rust library API

The pmr CLI is a thin layer over a public client API. Add the crate and manage
processes from your own application — a bot supervisor, a deploy tool, a
control panel.

```toml
[dependencies]
pmr = "0.1"
```

The client is **synchronous** (plain unix-socket I/O). Your app does not
inherit tokio. From async code, wrap calls in `tokio::task::spawn_blocking`.

> The daemon is the `pmr` binary. When your app calls `Pmr::connect()` and no
> daemon is running, the client launches one by locating the binary via
> `$PMR_BIN`, then a sibling of your executable (cargo target layouts), then
> `$PATH`. Ship the `pmr` binary alongside your app or `cargo install pmr` on
> the host.

## Connect

```rust
use pmr::{AppConfig, Pmr};

let mut pmr = Pmr::connect()?;       // spawn daemon if needed, then connect
let mut pmr = Pmr::try_connect()?;   // connect only if already running
```

Both verify the daemon version and print a warning on mismatch.

## Start processes

```rust
pmr.start(
    AppConfig::new("bot.js")          // interpreter auto-detected
        .name("bot")
        .instances(2)                 // NODE_APP_INSTANCE=0,1
        .cwd("/srv/bot")
        .args(["--mode", "prod"])
        .env("NODE_ENV", "production")
        .max_restarts(10)
        .max_memory_restart(200 * 1024 * 1024)
        .watch(false),
)?;
```

`AppConfig` is the same struct the ecosystem loader produces — every field in
[configuration.md](configuration.md) is available; the builder covers the
common ones and the rest are public fields:

```rust
let mut cfg = AppConfig::new("job.sh");
cfg.stop_exit_codes = vec![0];
cfg.cron_restart = Some("0 3 * * *".into());
pmr.start(cfg)?;
```

## Control

`stop`, `restart`, `delete`, `reset` accept a name, an id, or `Target::All`:

```rust
use pmr::Target;

pmr.stop("bot")?;
pmr.restart(3)?;
pmr.delete(Target::All)?;
pmr.scale("bot", 4)?;
pmr.send_signal("bot", "SIGUSR2")?;
```

All return `Vec<ProcessSnapshot>` for the affected processes.

## Inspect

```rust
let procs: Vec<pmr::ProcessSnapshot> = pmr.list()?;
for p in &procs {
    println!("{} {} {} cpu={:.1}% mem={}B restarts={}",
        p.pm_id, p.name, p.status, p.monit.cpu, p.monit.memory, p.restarts);
}

let detail = pmr.describe("bot")?;   // includes full config + env
```

`ProcessSnapshot` fields: `pm_id`, `name`, `namespace`, `status`, `pid`,
`instance`, `restarts`, `unstable_restarts`, `uptime_ms`, `monit {cpu, memory}`,
`out_file`, `error_file`, `pid_file`, `exit_code`, `config`, `env`.

## Events (logs, lifecycle)

A subscribed connection becomes stream-only, so use a dedicated connection:

```rust
use pmr::Event;

let events = Pmr::connect()?.subscribe(
    &["log:out", "log:err", "process:event"],
    None,                              // or Some(Target::Names(vec!["bot".into()]))
)?;

for event in events {
    match event? {
        Event::Log { name, stream, line, .. } => println!("[{name}/{stream}] {line}"),
        Event::Process { name, event, .. }    => println!("[{name}] {event}"),
        Event::DaemonKill                     => break,
    }
}
```

Topics: `log:out`, `log:err`, `process:event` (values: start, online, exit,
restart, stop, delete, errored, restart overlimit), `pmr:kill`. Slow consumers
skip events rather than blocking the daemon.

## Persistence & daemon control

```rust
let dump_path = pmr.save()?;
pmr.resurrect()?;
pmr.reload_logs()?;
pmr.flush(None)?;
let ping = pmr.ping()?;              // version + daemon pid
pmr.kill_daemon()?;
```

## Errors

Everything returns `anyhow::Result`. Daemon-side failures arrive as readable
messages, e.g. `process 'bot' already exists — use `pmr restart bot` ...` or
`no process found: web`.

## Full example

`examples/programmatic.rs` in the repo — run with
`cargo run --example programmatic`.
