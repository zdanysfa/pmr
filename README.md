<p align="center">
  <img src="assets/logo.svg" alt="pmr — efficient, fast, production-grade process manager" width="700"/>
</p>

<p align="center">
  <a href="https://github.com/zdanysfa/pmr/actions"><img src="https://github.com/zdanysfa/pmr/actions/workflows/ci.yml/badge.svg" alt="CI"/></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT"/></a>
  <a href="https://github.com/zdanysfa/pmr/releases"><img src="https://img.shields.io/github/v/release/zdanysfa/pmr" alt="Release"/></a>
</p>

**Efficient. Fast. Production-grade.** The pm2 workflow you know, rewritten in
Rust — one 3.4 MB binary, a fraction of the memory, no Node.js runtime required.

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
soak analysis: [benchmarks](https://pmr.mintlify.app/benchmarks)):

|                      | pmr                 | pm2 v7                     |
| -------------------- | ------------------- | -------------------------- |
| Daemon memory (RSS), idle → 25 procs | **5.6 → 7.0 MB** | 78.9 → 94.1 MB (**13–14× more**) |
| Cold start (`ping`)  | **104 ms**          | 444 ms                     |
| `ls` latency with 25 procs | **4 ms**      | 212 ms                     |
| Restart 25 processes | **36 ms**           | 1 908 ms                   |
| Log flood drain @ daemon CPU | **141 k lines/s @ 66 %** | 131 k lines/s @ 111 % |
| 24/7 stability       | soak-tested, no leak indicators ([analysis](https://pmr.mintlify.app/benchmarks)) | battle-tested |
| Install footprint    | **one 3.4 MB binary** | Node.js runtime + node_modules |
| Runtime dependency   | **none**            | Node.js                    |

Same muscle memory: `start`, `stop`, `restart`, `ls`, `logs`, `monit`, `save`,
`resurrect`, `startup` — and pm2 config field names are accepted in ecosystem
files.

## Install

Linux only (unix sockets + signals; the daemon is nix-native).

**Prebuilt static binary** (any distro, no dependencies at all):

```sh
curl -fsSL https://github.com/zdanysfa/pmr/releases/latest/download/pmr-$(curl -fsSL https://api.github.com/repos/zdanysfa/pmr/releases/latest | grep -oP '"tag_name": "\K[^"]+')-x86_64-unknown-linux-musl.tar.gz | sudo tar xz -C /usr/local/bin
```

**With cargo:**

```sh
cargo install pmr                     # from crates.io
cargo install --git https://github.com/zdanysfa/pmr   # from git
```

No Rust on the target machine? Build once (musl = fully static, runs on any
distro), copy the binary:

```sh
cargo build --release --target x86_64-unknown-linux-musl
scp target/x86_64-unknown-linux-musl/release/pmr user@vps:/usr/local/bin/
```

Shell completions: `pmr completions bash | sudo tee /etc/bash_completion.d/pmr`
(also zsh/fish/elvish/powershell).

**VPS / production setup** (Debian/Ubuntu/RHEL/Arch with systemd):

```sh
pmr start ecosystem.yaml --env production
pmr save                              # persist the process list
pmr startup                           # systemd unit (asks sudo itself)
```

Reboot-proof: the unit runs `pmr resurrect` at boot and your apps come back.
Full guide: [production](https://pmr.mintlify.app/production).

## Quick start

```sh
pmr start app.js                       # interpreter auto-detected (node)
pmr start main.ts --runtime bun        # runtime by name — binary auto-resolved
pmr start worker.py --name worker -i 4 # 4 instances, NODE_APP_INSTANCE=0..3
pmr start ecosystem.yaml --env production
pmr ls                                 # process table
pmr logs worker                        # tail + live stream
pmr monit                              # TUI dashboard
pmr stop worker && pmr restart all
pmr kill                               # stop daemon + everything
```

All commands: [cli](https://pmr.mintlify.app/cli) or `pmr --help`.

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
`spawn_blocking` from async code. Details: [library](https://pmr.mintlify.app/library).

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

Every option: [configuration](https://pmr.mintlify.app/configuration).

## Beyond pm2

Two production features pm2 doesn't have built in:

- **Native log rotation** — `max_log_size: 10M` per app; no pm2-logrotate
  module, no external config.
- **Health checks** — `health_check: {command, interval, max_fails}`; a
  process that is "online" but hung gets caught and restarted. pm2 has no
  equivalent.
- **Live-only logs** — `--no-log-file`: zero disk I/O on the log path while
  `pmr logs` keeps streaming live. Child pipes are enlarged to 1 MB so log
  bursts never block your app.

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

Full docs: **https://pmr.mintlify.app**

| Page | Contents |
| --- | --- |
| [quickstart](https://pmr.mintlify.app/quickstart) | first process, daily commands, surviving reboots |
| [production](https://pmr.mintlify.app/production) | VPS setup, systemd, 24/7 operation, logrotate, monitoring |
| [configuration](https://pmr.mintlify.app/configuration) | every config field with defaults |
| [cli](https://pmr.mintlify.app/cli) | full command reference |
| [library](https://pmr.mintlify.app/library) | Rust crate API |
| [comparison](https://pmr.mintlify.app/comparison) | pmr vs pm2 feature map |
| [benchmarks](https://pmr.mintlify.app/benchmarks) | full benchmark results, soak test, 24 h leak analysis |

## Development

```sh
cargo test                      # 22 unit + 11 e2e (real daemons in /tmp sandboxes)
cargo clippy --all-targets -- -D warnings
cargo run --example programmatic
```

Rust edition 2024, pinned nightly (`rust-toolchain.toml`).

## License

[MIT](LICENSE) © zdanysfa
