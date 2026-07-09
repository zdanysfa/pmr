# Configuration reference

Apps are declared in an ecosystem file (JSON, YAML or TOML — chosen by
extension) or via CLI flags on `pmr start`. `pmr init` writes a commented
sample. pm2 field spellings are accepted where noted (aliases).

Top-level shape:

```yaml
apps:
  - script: ./app.js
    # ...options
  - script: worker.py
```

(JSON may also be a bare array of apps.)

## Fields

| Field | Type / default | Aliases | Meaning |
| --- | --- | --- | --- |
| `script` | string, **required** | `exec` | Path to script or binary |
| `name` | string, script stem | | Process name (targets, log file names) |
| `namespace` | string, `default` | | Grouping label shown in `ls` |
| `cwd` | path, script's directory | | Working directory |
| `args` | list, `[]` | | Arguments passed to the script |
| `interpreter` | string, auto-detect | `exec_interpreter` | `node`, `python3`, ... `"none"` = execute directly. Auto-detect by extension: `.js/.cjs/.mjs/.ts`→node, `.py`→python3, `.sh`→bash, `.rb`→ruby, `.pl`→perl, `.php`→php |
| `interpreter_args` | list, `[]` | `node_args`, `interpreterArgs` | Flags for the interpreter itself |
| `instances` | int, `1` | | Fork N processes; each gets the instance index env var |
| `instance_var` | string, `NODE_APP_INSTANCE` | | Name of the instance index env var |
| `env` | map, `{}` | | Environment variables |
| `env_<profile>` | map | | Overlay merged onto `env` when started with `--env <profile>` |

### Logs

| Field | Type / default | Aliases | Meaning |
| --- | --- | --- | --- |
| `out_file` | path, `~/.pmr/logs/<name>-<id>-out.log` | `out`, `output`, `out_log` | stdout log; `/dev/null` disables |
| `error_file` | path, `~/.pmr/logs/<name>-<id>-error.log` | `error`, `err`, `err_file`, `err_log` | stderr log |
| `log_date_format` | string, off | | chrono format prefix per line, e.g. `%Y-%m-%dT%H:%M:%S` (CLI `--time` sets this) |
| `merge_logs` | bool, `false` | `combine_logs` | All instances share one log file (no `-<id>` suffix) |
| `pid_file` | path, `~/.pmr/pids/<name>-<id>.pid` | `pid` | Where the child pid is written |

### Lifecycle & restart policy

| Field | Type / default | Aliases | Meaning |
| --- | --- | --- | --- |
| `autostart` | bool, `true` | | Start immediately when added |
| `autorestart` | bool, `true` | | Restart on unexpected exit |
| `max_restarts` | int, `16` | | Unstable restarts before giving up (`errored`) |
| `min_uptime` | ms, `1000` | | Uptime below this counts as an unstable restart |
| `restart_delay` | ms, `0` | | Fixed delay before each auto-restart |
| `exp_backoff_restart_delay` | ms, `0` = off | | Backoff base; grows ×1.5 to 15 s cap, resets after 30 s stable |
| `cron_restart` | cron expr, off | `cron` | Scheduled restart; 5-field, optional seconds field |
| `max_memory_restart` | bytes, off | | Restart when RSS exceeds this (checked every 30 s; CLI accepts `200M`, `1G`) |
| `stop_exit_codes` | list, `[]` | | Exit codes that mean "stop, don't restart" |
| `kill_timeout` | ms, `1600` | | Grace period between `kill_signal` and SIGKILL |
| `kill_signal` | string, `SIGINT` | | First signal sent on stop/restart |
| `treekill` | bool, `true` | | Signal the whole process group, not just the main pid |

### Watch & user

| Field | Type / default | Aliases | Meaning |
| --- | --- | --- | --- |
| `watch` | bool, `false` | | Restart when files under `cwd` change |
| `ignore_watch` | list, `[]` (+ `node_modules`, `.git` always) | | Substring filters for watch events |
| `watch_delay` | ms, `200` | | Debounce before the watch-triggered restart |
| `uid` | username, off | `user` | Run the child as this user (daemon must be root) |
| `gid` | group, uid's primary group | | Child's group |

## Restart decision, exactly

On unexpected exit:

1. Status was stopping/stopped/errored, **or** `autorestart: false`, **or**
   exit code ∈ `stop_exit_codes` → **stay stopped**.
2. If `now - counters_reset < min_uptime × max_restarts` **and**
   `uptime < min_uptime` → increment `unstable_restarts`.
3. `unstable_restarts ≥ max_restarts` → **errored**, give up (visible in `pmr ls`;
   recover with `pmr restart <name>` after fixing the cause).
4. Otherwise restart after `exp_backoff_restart_delay` chain (if set) or
   `restart_delay`. Manual `pmr restart` always resets the instability history.

## Environment variables read by pmr

| Variable | Effect |
| --- | --- |
| `PMR_HOME` | Home directory (default `~/.pmr`). Keep it short — unix socket path limit |
| `PMR_BIN` | Path to the pmr binary the library client spawns as daemon |
| `PMR_WORKER_INTERVAL` | Housekeeping tick in ms (default 30000); mainly for tests |

## Environment variables set for your app

| Variable | Value |
| --- | --- |
| `NODE_APP_INSTANCE` (or `instance_var`) | Instance index 0..N-1 |
| `PMR_ID` | pm_id of the process |
| `PMR_NAME` | Process name |
