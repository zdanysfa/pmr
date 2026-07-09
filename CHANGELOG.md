# Changelog

All notable changes to pmr are documented here.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versioning follows [SemVer](https://semver.org/).

## [Unreleased]

## [0.1.0] - 2026-07-10

First release — a ground-up Rust rewrite of pm2 (fork mode), built from a full
audit of pm2 v7.0.3 internals.

### Beyond pm2
- **Native log rotation**: `max_log_size` per app (`--max-log-size 10M`) —
  file rotated to `<file>.old` when over the limit; no external module needed.
- **Health checks**: `health_check: {command, interval, timeout, max_fails}` —
  consecutive failures restart a hung-but-online process. No pm2 equivalent.
- **Live-only logs**: `--no-log-file` / `disable_log_files` — `pmr logs`
  streams live from the in-memory bus while nothing is written to disk.
- **Zero-coupling mode**: `--disable-logs` / `disable_logs` — child stdio
  goes straight to /dev/null; no pipe exists between app and daemon at all.
- **Low-overhead log pipeline**: child pipes enlarged to 1 MB (bursts never
  block the app), 64 KB buffered reads, batched file writes — measured
  141 k lines/s drained at 66 % daemon CPU (pm2: 131 k at 111 %).
- `pmr completions <shell>` (bash/zsh/fish/elvish/powershell).
- Benchmark suite (`bench/bench.sh`) and 24 h soak test (`bench/soak.sh`).
- Static musl release binaries (x86_64 + aarch64) built on version tags.

### Added

**Process management**
- Daemon with auto-spawn on first command, singleton via `flock`, stale-socket
  recovery after crashes.
- Full pm2 lifecycle state machine: `launching → online → stopping → stopped |
  errored | waiting restart`.
- Kill sequence: configurable `kill_signal` (default SIGINT) → `kill_timeout`
  (1600 ms) → SIGKILL; `treekill` (default on) signals the whole process group.
- Restart policy: `autorestart`, `stop_exit_codes`, `max_restarts` (16) with
  `min_uptime` unstable-restart detection, fixed `restart_delay`,
  `exp_backoff_restart_delay` (×1.5, cap 15 s, reset after 30 s stable uptime).
- `instances: N` fork scaling with per-instance `NODE_APP_INSTANCE`
  (customizable via `instance_var`); `pmr scale <name> <n>`.
- `cron_restart` (5/6-field cron), `max_memory_restart`, file `watch` with
  `ignore_watch` + `watch_delay` debounce.
- `uid`/`gid` privilege dropping (daemon must run as root).

**CLI**
- `start` (script or JSON/YAML/TOML ecosystem file, pm2 field aliases accepted),
  `stop`, `restart`, `reload`, `delete`, `reset`, `ls`, `jlist`, `describe`,
  `env`, `id`, `pid`, `logs` (tail + live stream, `--err/--out/--lines/
  --nostream/--timestamp/--raw`), `flush`, `reloadLogs`, `save`, `resurrect`,
  `kill`, `ping`, `sendSignal`, `scale`, `init`, `monit` (ratatui TUI),
  `startup`/`unstartup` (systemd).

**Library**
- `pmr` as a Rust crate: `Pmr` client (sync, no tokio required in host apps),
  `AppConfig` builder, `subscribe()` event streams. The CLI is built on the
  same API.

**Persistence & logs**
- `dump.pmr` written on `save` and on graceful daemon shutdown; `resurrect`
  restores processes with their original pm_ids.
- Per-process log files (`~/.pmr/logs/<name>-<id>-{out,error}.log`),
  `merge_logs`, `log_date_format`, SIGUSR2/`reloadLogs` file reopen
  (logrotate-compatible).

**Tooling**
- Rust edition 2024 on pinned nightly; CI (fmt, clippy `-D warnings`, tests);
  22 unit + 11 end-to-end tests spawning real daemons.

### Not included (by design)
- Node cluster mode (fork-only; clear error suggests `instances: N`).
- JavaScript config files (JSON/YAML/TOML only).
- pm2.io/keymetrics agent, module system, deploy, serve.

[Unreleased]: https://github.com/zdanysfa/pmr/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/zdanysfa/pmr/releases/tag/v0.1.0
