# Changelog

All notable changes to pmr are documented here.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versioning follows [SemVer](https://semver.org/).

## [Unreleased]

## [0.5.0] - 2026-07-13

### Changed
- **`pmr delete` now removes the process's log files** (out, error, and their
  rotated `.old` siblings) along with the pid file — a deleted app's logs are
  stale the moment the name is reused. Deliberate divergence from pm2, which
  keeps logs after delete. Paths still shared by live instances (`merge_logs`)
  are kept. `pmr flush` remains for clearing logs of running processes.
- All delete paths (RPC direct and via supervisor) route through one shared
  `remove_proc`, so cleanup behavior can't drift between them.

## [0.4.0] - 2026-07-10

Hardening release: four independent audits (concurrency, 24/7 durability,
crash-safety/reboot, large-scale performance) — every confirmed finding fixed
and verified empirically.

### Added
- **Live cpu/mem in `pmr ls`** — metrics sample on demand per `list`/`describe`
  (like pm2's pidusage), with a pidusage-style lifetime-average fallback for
  the first sample of a fresh pid; no more `0% / 0b` right after start.
- **Systemd unit hardening**: `Type=forking` + `PIDFile` + `Restart=on-failure`
  — an OOM-killed daemon is restarted by systemd; `network-online` ordering so
  apps don't burn their unstable-restart budget before DNS is up.
- Daemon raises `RLIMIT_NOFILE` to the hard limit at boot (default 1024 soft
  dies at ~170 procs); daemon log `pmr.log` self-rotates at 10 MB.
- Orphan detection at daemon start: stale pid files pointing at live processes
  are reported loudly before a `resurrect` could double-start them.
- `pmr kill` now waits for the daemon to actually exit (socket EOF), so
  `pmr kill && pmr resurrect` and systemd `ExecStop` cannot race the shutdown.

### Fixed
- **Log pump**: invalid UTF-8 no longer kills log capture permanently; lines
  cap at 1 MiB (an app printing without newlines can't balloon the daemon);
  bus copies cap at 8 KiB (a stalled `pmr logs` subscriber can't pin ~1 GiB);
  deleted log files are recreated automatically; failed log-file opens are
  reported instead of silently dropping output.
- **Watcher**: `pmr stop` disarms the file watcher (pm2 `stopWatch` parity) —
  a stopped app is no longer revived by a file change; re-armed on start.
- **Stop/restart/delete racing a crash**: status flips to `stopping`
  synchronously (pm2 parity), so a natural exit racing the command can no
  longer respawn a process the user just stopped.
- **Dump durability**: `save` fsyncs file + directory, refuses to overwrite a
  dump with an empty table, never replaces a good backup with a corrupt
  fragment; `resurrect` falls back to `dump.pmr.bak` on parse failure too —
  a power-loss-truncated dump no longer boots the VPS with zero apps.
- `stop/restart/delete all` run per-process kill sequences in parallel
  (6 stubborn apps: 9.6 s → 1.6 s; 300 would have taken ~8 minutes).
- `max_memory_restart` enforcement can no longer be starved by frequent
  `jlist` polling; over-limit restarts run in parallel off the worker tick.
- Dead `pmr logs` subscribers are detected via read-half EOF — previously each
  leaked an fd + task + bus receiver forever (a per-minute watchdog exhausted
  1024 fds in under a day).
- sysinfo process map no longer accumulates one stale entry per restart.
- `pmr delete` can no longer silently no-op when racing a stop; duplicate
  supervisors can no longer spawn from concurrent cold starts; `start` during
  daemon shutdown is rejected (previously leaked an unmanaged child).
- Daemon survives `SIGHUP` (SSH disconnect); accept-loop errors back off
  instead of spinning at 100 % CPU on fd exhaustion; RPC request lines are
  length-capped; oversized-request and binary log content handled lossily.
- `pmr logs` tail no longer goes blank when a log file contains binary bytes.
- Graceful shutdown drains log pumps before exiting (children's final lines
  were lost); blocking `stat()` calls moved off the table lock (a hung NFS
  mount froze every RPC).
- `sudo pmr startup` resolves the real user via `SUDO_USER` (previously
  installed a unit for root's `/root/.pmr`); warns when `PMR_HOME` is tmpfs.
- Dump files are `0600` and `~/.pmr` is `0700` (dumps carry app env secrets).

## [0.3.0] - 2026-07-10

### Added
- **`runtime` config field / `--runtime` flag**: declare the runtime by name
  (`runtime: bun`, `node`, `deno`, `python`) — pmr probes the usual install
  locations at spawn (`~/.bun/bin/bun` → `/usr/local/bin` → `/usr/bin` →
  PATH), so ecosystem files need no hardcoded binary paths. Mutually
  exclusive with `interpreter`.
- **pm2-style `pmr ls` table**: full grid with solid column lines, new
  `mode`, `user` and `watching` columns, bold header, colored id/status.
- **First-run welcome banner** (interactive terminals only, shown once when
  `~/.pmr` is created).
- **`pmr startup`/`unstartup` auto-sudo**: re-executes itself through sudo on
  a terminal instead of making you copy-paste a sudo command like pm2 does.
- `pmr version` command: prints client and daemon versions.
- Brand: rust-orange isometric cube logo (light/dark) in the README;
  documentation moved to the Mintlify site (pmr-docs repo).

### Changed
- `.ts`/`.tsx` auto-detection now picks bun (was node), matching pm2.

## [0.2.0] - 2026-07-10

### Added
- **Zero-coupling mode**: `--disable-logs` / `disable_logs: true` — the child
  is spawned with stdout/stderr on `/dev/null`. No pipe exists between app and
  daemon at all: nothing to block on, nothing to drain, literally zero
  log-path overhead. (`pmr logs` shows nothing for such apps.)
- docs: full pmr ↔ app touch-point matrix in `docs/production.md` — every
  interaction, its cost, and how to turn it off; why races cannot occur
  (no shared memory, single-writer logs, sequential per-process commands,
  ordered kill sequence).

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

[Unreleased]: https://github.com/zdanysfa/pmr/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/zdanysfa/pmr/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/zdanysfa/pmr/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/zdanysfa/pmr/releases/tag/v0.1.0
