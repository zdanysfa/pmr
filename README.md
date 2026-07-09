# pmr

**Efficient. Fast. Production-grade.** The pm2 workflow you know, rewritten in
Rust — one 3.4 MB binary, a fraction of the memory, no Node.js runtime required.

[![CI](https://github.com/zdanysfa/pmr/actions/workflows/ci.yml/badge.svg)](https://github.com/zdanysfa/pmr/actions)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

```
┌─────────────────────────────────────────────────────────────────────┐
│ id   name     namespace   pid      uptime   ↺   status   cpu    mem │
╞═════════════════════════════════════════════════════════════════════╡
│ 0    api      default     130116   2d 4h    0   online   0.3%   54mb│
│ 1    worker   default     130117   2d 4h    2   online   1.1%   80mb│
└─────────────────────────────────────────────────────────────────────┘
```

## Why pmr over pm2

Measured on the same Linux machine with `bench/bench.sh` (methodology and 24 h
soak analysis: [docs/benchmarks.md](docs/benchmarks.md)):

|                      | pmr                 | pm2 v7                     |
| -------------------- | ------------------- | -------------------------- |
| Daemon memory (RSS), idle → 25 procs | **5.6 → 7.0 MB** | 78.9 → 94.1 MB (**13–14× more**) |
| Cold start (`ping`)  | **104 ms**          | 444 ms                     |
| `ls` latency with 25 procs | **4 ms**      | 212 ms                     |
| Restart 25 processes | **36 ms**           | 1 908 ms                   |
| Daemon CPU at ~132 k log lines/s | **73 %** | 110 %                      |
| 24/7 stability       | soak-tested, no leak indicators ([analysis](docs/benchmarks.md)) | battle-tested |
| Install footprint    | **one 3.4 MB binary** | Node.js runtime + node_modules |
| Runtime dependency   | **none**            | Node.js                    |

Same muscle memory: `start`, `stop`, `restart`, `ls`, `logs`, `monit`, `save`,
`resurrect`, `startup` — and pm2 config field names are accepted in ecosystem
files.

## Install

```sh
cargo install pmr                     # from crates.io
# or from git
cargo install --git https://github.com/zdanysfa/pmr
```

No Rust on the target machine? Build once, copy the binary:

```sh
cargo build --release && scp target/release/pmr user@vps:/usr/local/bin/
```

**VPS / production setup** (Debian/Ubuntu/RHEL/Arch with systemd):

```sh
pmr start ecosystem.yaml --env production
pmr save                              # persist the process list
sudo env PATH=$PATH USER=$USER $(which pmr) startup   # install systemd unit
```

Reboot-proof: the unit runs `pmr resurrect` at boot and your apps come back.
Full guide: [docs/production.md](docs/production.md).

## Quick start

```sh
pmr start app.js                       # interpreter auto-detected (node)
pmr start worker.py --name worker -i 4 # 4 instances, NODE_APP_INSTANCE=0..3
pmr start ecosystem.yaml --env production
pmr ls                                 # process table
pmr logs worker                        # tail + live stream
pmr monit                              # TUI dashboard
pmr stop worker && pmr restart all
pmr kill                               # stop daemon + everything
```

All commands: [docs/cli.md](docs/cli.md) or `pmr --help`.

## Use as a Rust library

The CLI is a thin layer over a public API — add pmr to your own project and
manage processes programmatically:

```toml
[dependencies]
pmr = "0.1"
```

```rust
use pmr::{AppConfig, Pmr};

let mut pmr = Pmr::connect()?; // auto-spawns the daemon when needed

pmr.start(
    AppConfig::new("bot.js")
        .name("bot")
        .instances(2)
        .env("NODE_ENV", "production")
        .max_memory_restart(200 * 1024 * 1024),
)?;

for p in pmr.list()? {
    println!("{} {} {}", p.pm_id, p.name, p.status);
}

// Live events (log lines, lifecycle) on a dedicated connection.
let events = Pmr::connect()?.subscribe(&["log:out", "process:event"], None)?;
for event in events { /* ... */ }
```

The client is synchronous — no tokio required in your app; wrap in
`spawn_blocking` from async code. Details: [docs/library.md](docs/library.md).

## Ecosystem files

JSON, YAML or TOML (`pmr init` writes a sample). pm2 spellings accepted
(`exec`, `combine_logs`, `cron`, `user`, ...):

```yaml
apps:
  - script: ./server.js
    name: api
    instances: 2
    env:
      NODE_ENV: development
    env_production:            # applied with --env production
      NODE_ENV: production
    max_memory_restart: 314572800   # bytes
    cron_restart: "0 3 * * *"
    watch: true
    ignore_watch: [node_modules, .git]
    stop_exit_codes: [0]
```

Every option: [docs/configuration.md](docs/configuration.md).

## pm2 semantics, faithfully

Built from a line-by-line audit of pm2 v7.0.3:

- State machine `launching → online → stopping → stopped | errored | waiting restart`.
- Kill sequence: `kill_signal` (SIGINT) → `kill_timeout` (1600 ms) → SIGKILL;
  `treekill` signals the whole process group.
- Restart policy: `autorestart`, `stop_exit_codes`, `max_restarts` (16) with
  `min_uptime` (1 s) unstable-restart detection, fixed `restart_delay` or
  `exp_backoff_restart_delay` (×1.5, cap 15 s, reset after 30 s stable).
- Automation: `cron_restart`, `max_memory_restart`, `watch`/`ignore_watch`.
- Logs: `~/.pmr/logs/<name>-<id>-{out,error}.log`, `merge_logs`,
  `log_date_format`, `pmr flush`, SIGUSR2/`reloadLogs` reopen (logrotate-ready).
- Persistence: `pmr save` → `~/.pmr/dump.pmr` → `pmr resurrect` (also written on
  daemon shutdown); pm_ids survive the cycle.

## Differences from pm2 (by design)

- **Fork mode only.** No Node `cluster` module. `instances: N` spawns N
  processes with `NODE_APP_INSTANCE` set; share ports with `SO_REUSEPORT` in
  your app if needed.
- **No JS config files** — the daemon doesn't evaluate JavaScript. JSON/YAML/TOML.
- **No pm2.io agent, module system, deploy, serve.** A process manager, not a platform.
- Own home (`~/.pmr`, override `PMR_HOME`) and own RPC (ndjson over
  `~/.pmr/rpc.sock`) — not wire-compatible with pm2. Both can run side by side.

## Documentation

| Doc | Contents |
| --- | --- |
| [docs/getting-started.md](docs/getting-started.md) | install, first process, daily commands |
| [docs/production.md](docs/production.md) | VPS setup, systemd, 24/7 operation, logrotate, monitoring |
| [docs/configuration.md](docs/configuration.md) | every config field with defaults |
| [docs/cli.md](docs/cli.md) | full command reference |
| [docs/library.md](docs/library.md) | Rust crate API |
| [docs/comparison.md](docs/comparison.md) | pmr vs pm2 feature map |
| [docs/benchmarks.md](docs/benchmarks.md) | full benchmark results, soak test, 24 h leak analysis |

## Development

```sh
cargo test                      # 22 unit + 11 e2e (real daemons in /tmp sandboxes)
cargo clippy --all-targets -- -D warnings
cargo run --example programmatic
```

Rust edition 2024, pinned nightly (`rust-toolchain.toml`).

## License

[MIT](LICENSE) © zdanysfa
