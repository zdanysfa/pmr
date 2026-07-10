# Getting started

## Install

**From crates.io** (needs Rust ≥ pinned nightly, see `rust-toolchain.toml`):

```sh
cargo install pmr
```

**From source:**

```sh
git clone https://github.com/zdanysfa/pmr.git
cd pmr
cargo build --release
sudo cp target/release/pmr /usr/local/bin/
```

**Prebuilt binary onto a server without Rust:** build on any Linux x86_64
machine (glibc versions must be compatible) and copy:

```sh
scp target/release/pmr user@server:/usr/local/bin/pmr
```

Verify: `pmr --version`.

## First process

```sh
pmr start app.js
```

That single command:
1. Spawns the pmr daemon in the background (first time only).
2. Detects the interpreter from the extension (`.js` → node, `.py` → python3,
   `.sh` → bash, `.rb` → ruby, `.pl` → perl, `.php` → php; no extension → run directly).
3. Starts the app, captures stdout/stderr into `~/.pmr/logs/`, and restarts it
   if it crashes.

Check on it:

```sh
pmr ls          # table: id, name, pid, uptime, restarts, status, cpu, mem
pmr logs app    # last 15 lines + live stream (Ctrl-C to detach; app keeps running)
pmr describe 0  # full detail for one process
```

## Daily commands

```sh
pmr stop app         # stop (stays in the list)
pmr restart app      # restart
pmr delete app       # stop + remove from the list
pmr restart all      # targets: name | id | all
pmr flush            # truncate log files
pmr monit            # live TUI dashboard (q to quit)
```

## Multiple instances

```sh
pmr start server.js --name web -i 4
```

Four processes; each gets `NODE_APP_INSTANCE` = 0..3 (rename the variable with
`instance_var`). Scale later with `pmr scale web 8`. Port sharing is your app's
job (`SO_REUSEPORT`), pmr does not proxy.

## Ecosystem file

```sh
pmr init             # writes a commented ecosystem.yaml
pmr start ecosystem.yaml
pmr start ecosystem.yaml --env production   # applies the env_production overlay
```

See [configuration.md](configuration.md) for every field.

## Surviving reboots

```sh
pmr save             # snapshot the process list
pmr startup          # systemd unit — asks sudo itself on a terminal
```

Details (VPS hardening, logrotate, 24/7 operation): [production.md](production.md).

## Where things live

```
~/.pmr/                 # override with PMR_HOME
├── rpc.sock            # daemon socket
├── pmr.pid             # daemon pid (flock singleton guard)
├── pmr.log             # daemon's own log
├── dump.pmr            # saved process list (pmr save / resurrect)
├── logs/<name>-<id>-out.log
├── logs/<name>-<id>-error.log
└── pids/<name>-<id>.pid
```
