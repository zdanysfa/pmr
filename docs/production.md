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
sudo env PATH=$PATH USER=$USER $(which pmr) startup
```

`pmr startup` writes `/etc/systemd/system/pmr-<user>.service` and enables it.
The unit starts the daemon at boot and runs `pmr resurrect`, restoring the
exact process list from the last `pmr save`. Run `pmr save` again whenever the
list changes.

Preview without installing: `pmr startup --print-only`.
Remove: `sudo pmr unstartup`.

## Log rotation

pmr appends to `~/.pmr/logs/*.log` forever; rotate them with the standard OS
logrotate. `/etc/logrotate.d/pmr`:

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
