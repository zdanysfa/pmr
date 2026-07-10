# Production / VPS guide

How to run pmr 24/7 on a server.

## Can it run 24 hours a day?

Yes — that is the whole point. The daemon is a long-lived background process
that keeps running after you log out (it is detached from your SSH session).
Your apps are supervised continuously: crash → restart (with backoff and
`max_restarts` protection), over memory → restart, cron schedule → restart.
The daemon itself idles at ~5 MB RSS with a 30-second housekeeping tick, so it
costs effectively nothing to leave running.

Two things you must set up for true 24/7 operation:

1. **`pmr save` + `pmr startup`** — so a server reboot brings everything back.
2. **Log rotation** — so log files don't fill the disk over months.

Both below.

## Full VPS setup

```sh
# 1. Put the binary on the server
scp target/release/pmr user@vps:/usr/local/bin/pmr
# (or on the server: cargo install pmr)

# 2. Describe your apps
pmr init                      # edit ecosystem.yaml
pmr start ecosystem.yaml --env production
pmr ls                        # verify everything is online

# 3. Persist across reboots
pmr save
pmr startup          # asks sudo itself (pm2 makes you copy-paste a sudo command)
```

`pmr startup` writes `/etc/systemd/system/pmr-<user>.service` and enables it.
The unit starts the daemon at boot and runs `pmr resurrect`, restoring the
exact process list from the last `pmr save`. Run `pmr save` again whenever the
list changes.

Preview without installing: `pmr startup --print-only`.
Remove: `sudo pmr unstartup`.

## Log rotation

**Built-in (simplest):** set `max_log_size` per app — pmr rotates the file to
`<file>.old` when it crosses the limit. One backup slot; enough for most VPS
setups, zero external config:

```sh
pmr start app.js --max-log-size 10M
```

**OS logrotate (compressed history, N generations):** pmr appends to
`~/.pmr/logs/*.log`; rotate with standard logrotate. `/etc/logrotate.d/pmr`:

```
/home/YOUR_USER/.pmr/logs/*.log {
    daily
    rotate 14
    compress
    delaycompress
    missingok
    notifempty
    postrotate
        kill -USR2 $(cat /home/YOUR_USER/.pmr/pmr.pid 2>/dev/null) 2>/dev/null || true
    endscript
}
```

SIGUSR2 (or `pmr reloadLogs`) makes the daemon reopen every log file, so
rotation is seamless. `copytruncate` also works (files are opened append-only)
but the postrotate signal is cleaner.

## Recommended app settings for production

```yaml
apps:
  - script: ./server.js
    name: api
    instances: 2
    exp_backoff_restart_delay: 100   # 100ms → ×1.5 → cap 15s between crash-restarts
    max_memory_restart: 524288000    # restart at 500MB
    kill_timeout: 5000               # give graceful shutdown 5s before SIGKILL
    max_log_size: 10485760           # rotate logs at 10MB (built-in)
    health_check:                    # restart when "online but hung"
      command: "curl -fsS http://localhost:3000/health"
      interval: 15000
      max_fails: 3
    env_production:
      NODE_ENV: production
```

- **`exp_backoff_restart_delay`** prevents a crash-looping app from burning CPU;
  the delay resets automatically after 30 s of stable uptime.
- **`max_restarts` / `min_uptime`** (defaults 16 / 1000 ms): an app that keeps
  dying within `min_uptime` is marked `errored` after `max_restarts` unstable
  restarts and left alone — check `pmr logs <name>` then `pmr restart <name>`.
- **`kill_timeout`**: your app gets `kill_signal` (default SIGINT) and this many
  ms to shut down cleanly before SIGKILL. Handle the signal, close connections.
- **`stop_exit_codes: [0]`**: treat clean exit as "done, don't restart" for
  one-shot jobs.

## Does pmr slow my app down?

For normal apps: no. Your process runs untouched; the only link is its
stdout/stderr pipe into the daemon. Writes to that pipe land in kernel memory
— usually cheaper for your app than writing files itself, since disk I/O is
offloaded to pmr.

The only physical coupling is pipe backpressure: if an app logs faster than
the daemon drains, its `write()` blocks. pmr minimizes this three ways:

- child pipes are enlarged to **1 MB** (vs the 64 KB default), so bursts are
  absorbed by the kernel without blocking your app;
- the daemon drains with 64 KB buffered reads and batched file writes —
  measured ~140 k lines/s per process (pm2 saturates sooner and burns ~1.7×
  the CPU doing it);
- `--no-log-file` (`disable_log_files: true`) removes disk from the path
  entirely while `pmr logs` keeps streaming live from the in-memory bus.

An app logging even 10 000 lines/s feels nothing. If you're above ~140 k
lines/s sustained, reduce log volume — no process manager survives that
politely. And for literal zero: `--disable-logs` spawns the child with
stdout/stderr on `/dev/null` — **no pipe exists at all**, nothing to block on,
nothing to drain. (`pmr logs` shows nothing for that app.)

### Every pmr ↔ app touch point, and its cost

pmr shares **no memory and no locks** with your app — it's a plain parent
process watching plain child processes. The complete list of interactions:

| Interaction | When | Cost to your app | Turn it off |
| --- | --- | --- | --- |
| stdout/stderr pipe | only when the app writes logs | ~0 (kernel memcpy; blocks only past ~140 k lines/s sustained) | `--no-log-file` (no disk) or `--disable-logs` (no pipe) |
| Signals (SIGINT/SIGKILL) | only on stop/restart you asked for | none during normal run | — (that's supervision) |
| CPU/mem sampling | every 30 s | zero — reads `/proc` metadata, never touches the process | it's already free |
| Health check | opt-in, your command, your interval | whatever your check command costs | don't configure one |
| Watch → restart | opt-in, debounced (`watch_delay`, default 200 ms) | none until a file actually changes | don't use `--watch` |
| exit detection | when the app dies | zero (kernel notifies the daemon) | — |

Race conditions between pmr and the app cannot occur by construction: log
files have a **single writer** (the daemon — unlike pm2's cluster mode where
children write their own logs), commands for one process are queued and
executed sequentially by its supervisor, and the kill sequence is strictly
ordered (signal → grace period → SIGKILL), so your app always gets its clean
shutdown window.

One subtlety that works in your favor: with stdout on a pipe (not a TTY),
libc switches your app to block-buffering — the app makes *fewer* write
syscalls under pmr than it would printing to a terminal.

## Monitoring

```sh
pmr ls        # quick table (cpu/mem refreshed every 30s by the daemon)
pmr monit     # live TUI: process list, cpu/mem gauges, streaming logs
pmr jlist     # full JSON — pipe into jq / scrape from scripts
pmr logs --timestamp   # all processes, live, timestamped
```

For external monitoring, `pmr jlist` is stable machine-readable output; exit
code is non-zero when the daemon is unreachable, so
`pmr ping || alert` works as a liveness probe.

## Operational notes

- **Daemon upgrade**: install the new binary, then `pmr kill && pmr resurrect`.
  The CLI warns automatically when its version differs from the running daemon.
- **Multiple users**: each user gets an isolated `~/.pmr` (own daemon, own
  socket, mode 0700). Root-owned daemon + `uid:` per app also works for a
  single shared daemon that drops privileges per process.
- **Crashed daemon** (`kill -9`, OOM): apps keep running but become orphans
  (they are not re-adopted — same as pm2). The next pmr command auto-starts a
  fresh daemon over the stale socket. This is why `max_memory_restart` on your
  apps matters: it protects the box from the OOM killer ever reaching the daemon.
- **PMR_HOME** must be a short-ish path — unix socket paths are limited to
  ~108 bytes by the kernel.

## Is pmr production ready?

What's solid: the supervision core (spawn/restart/kill semantics) is a faithful
port of pm2's battle-tested behavior, backed by 33 automated tests including
end-to-end tests that spawn real daemons, crash real processes, SIGKILL the
daemon, and verify recovery. Static binary, no GC, no runtime dependencies.

What it hasn't had yet: years of diverse production mileage that pm2 has. Run
it on a staging box first, like you would any 0.x tool. Fork-only design also
means no built-in port sharing — if you relied on pm2 cluster mode, you need
`SO_REUSEPORT` or a reverse proxy in front.
