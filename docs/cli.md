# CLI reference

Targets are `<name>`, `<id>` or `all` everywhere a target is accepted.

## Starting

```sh
pmr start app.js                    # script (interpreter auto-detected)
pmr start ./binary                  # no extension → executed directly
pmr start ecosystem.yaml            # every app in the file (.json/.yaml/.toml)
pmr start ecosystem.yaml --env production   # apply env_production overlay
pmr start app.js -- --port 3000     # args after -- go to your app
pmr start web                       # existing stopped process → restart it
```

Flags on `pmr start <script>`:

| Flag | Meaning |
| --- | --- |
| `--name <n>` | process name |
| `-i, --instances <n>` | number of instances |
| `--interpreter <bin>` | force interpreter (`none` = direct) |
| `--cwd <dir>` | working directory |
| `--env <profile>` | env profile (config files) |
| `--no-autorestart` | don't restart on exit |
| `--max-restarts <n>` | unstable-restart limit |
| `--max-memory-restart <size>` | e.g. `200M`, `1G` |
| `--restart-delay <ms>` | fixed restart delay |
| `--exp-backoff-restart-delay <ms>` | backoff base |
| `--cron-restart <expr>` | scheduled restart |
| `--watch` | restart on file changes |
| `--time` | timestamp log lines |
| `--kill-timeout <ms>` | grace period before SIGKILL |

## Managing

```sh
pmr stop <target>        # graceful stop (kill_signal → kill_timeout → SIGKILL)
pmr restart <target>     # stop + start; resets instability history
pmr reload <target>      # alias of restart (pmr is fork-only)
pmr delete <target>      # stop + remove from the table       (alias: del)
pmr reset <target>       # zero the restart counters
pmr scale <name> <n>     # add/remove instances
pmr sendSignal <SIG> <target>   # e.g. pmr sendSignal SIGUSR2 api
```

## Inspecting

```sh
pmr ls                   # table (aliases: list, l, ps, status)
pmr jlist                # same data as JSON (for scripts / jq)
pmr describe <target>    # full details (aliases: show, info, desc)
pmr env <target>         # environment handed to the process
pmr id <name>            # pm_id(s) for a name
pmr pid [target]         # OS pid(s)
pmr monit                # TUI dashboard: q quit, ↑↓/jk select process
pmr ping                 # daemon liveness + version
```

## Logs

```sh
pmr logs                 # all processes: last 15 lines + live stream
pmr logs api             # one process
pmr logs --lines 100     # more history
pmr logs --err           # only stderr        (--out: only stdout)
pmr logs --nostream      # print tail and exit
pmr logs --timestamp     # prefix arrival time
pmr logs --raw           # no id|name gutter (pipe-friendly)
pmr flush [target]       # truncate log files
pmr reloadLogs           # reopen files (logrotate); same as kill -USR2 <daemon>
```

## Persistence & daemon

```sh
pmr save                 # write ~/.pmr/dump.pmr        (alias: dump)
pmr resurrect            # start everything from the dump
pmr kill                 # stop all processes + the daemon (dump written first)
pmr startup [--print-only]   # install systemd boot unit (needs sudo)
pmr unstartup            # remove it
pmr init                 # write a sample ecosystem.yaml
```

## Exit codes

`0` success; `1` any error (process not found, daemon unreachable, invalid
config). `pmr ping` doubles as a health probe.
