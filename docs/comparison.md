# pmr vs pm2

## Measured

Same Linux x86_64 machine (CachyOS, kernel 7.1), idle daemons, default
settings, pm2 v7 via Node.js, pmr release build:

| Metric | pmr 0.1 | pm2 v7 |
| --- | --- | --- |
| Daemon RSS, idle | **5.5 MB** | 78.8 MB |
| Cold start to first `ping` | **0.10 s** | 0.45 s |
| Install size | **3.4 MB** (one binary) | Node.js runtime + pm2 node_modules |

Reproduce:

```sh
# pmr
PMR_HOME=/tmp/pmr-bench pmr ping
ps -o rss= -p "$(cat /tmp/pmr-bench/pmr.pid)"
# pm2
PM2_HOME=/tmp/pm2-bench pm2 ping
ps -o rss= -p "$(cat /tmp/pm2-bench/pm2.pid)"
# cold start
pmr kill; time pmr ping
pm2 kill;  time pm2 ping
```

Numbers vary by machine; the ratio (roughly an order of magnitude on memory)
is the point. The pmr daemon is a tokio binary with one supervisor task per
process — no V8 heap, no GC pauses, no JIT warmup.

## Feature map

| | pmr | pm2 |
| --- | --- | --- |
| fork mode, instances, scale | ✅ | ✅ |
| restart policy (backoff, max_restarts, min_uptime, stop_exit_codes) | ✅ same semantics | ✅ |
| kill sequence (signal → timeout → SIGKILL, treekill) | ✅ same defaults | ✅ |
| cron_restart / max_memory_restart / watch | ✅ | ✅ |
| logs: files, merge, timestamps, flush, logrotate reopen | ✅ | ✅ |
| save / resurrect / startup (systemd) | ✅ | ✅ |
| monit TUI, jlist, describe, env | ✅ | ✅ |
| ecosystem files | JSON / YAML / TOML | JS / JSON / YAML |
| programmatic API | Rust crate | Node.js module |
| **cluster mode (shared port via Node)** | ❌ fork-only | ✅ |
| wait_ready IPC handshake | ❌ | ✅ |
| pm2.io / keymetrics SaaS | ❌ | ✅ |
| module system (pm2-logrotate etc.) | ❌ (use OS logrotate) | ✅ |
| deploy / serve / container helpers | ❌ | ✅ |
| runtime dependency | none | Node.js |

## When to keep pm2

- You depend on **cluster mode** for zero-config port sharing between Node
  workers and can't switch to `SO_REUSEPORT`/reverse proxy.
- You use **pm2.io** monitoring or pm2 modules.
- Your team's tooling evaluates **JS ecosystem files** with logic in them.

## When pmr wins

- Small VPS / containers where 70+ MB for a supervisor is real money.
- Polyglot fleets (Python, Ruby, binaries) — no Node.js runtime needed.
- Rust applications that want a **typed, in-process API** to spawn and
  supervise workers.
- Ops that want one auditable static binary and pm2's semantics without the
  platform attached.

Both managers can run side by side on the same machine — different home
directories, different sockets.
