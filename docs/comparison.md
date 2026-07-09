# pmr vs pm2

## Measured

Same Linux x86_64 machine (CachyOS, kernel 7.1), pm2 v7 on Node.js, pmr
release build. Full methodology, more metrics and the 24 h soak analysis:
[benchmarks.md](benchmarks.md). Reproduce everything with `bench/bench.sh`.

| Metric | pmr 0.1 | pm2 v7 | ratio |
| --- | --- | --- | --- |
| Daemon RSS, idle | **5.6 MB** | 78.9 MB | 14× |
| Daemon RSS, 25 processes | **7.0 MB** | 94.1 MB | 13× |
| Cold start to first `ping` | **104 ms** | 444 ms | 4× |
| Start 25 instances | **32 ms** | 393 ms | 12× |
| `ls` latency (25 procs) | **4 ms** | 212 ms | 53× |
| Restart 25 processes | **36 ms** | 1 908 ms | 53× |
| Daemon CPU on the log flood test | **65.6 %** @ 141 k lines/s | 110.9 % @ 131 k lines/s | 1.7× |
| Install size | **3.4 MB** binary | Node.js + node_modules | — |

Numbers vary by machine; the ratios are the point. The pmr daemon is a tokio
binary with one supervisor task per process — no V8 heap, no GC pauses, no
JIT warmup, and the CLI doesn't pay Node startup on every command.

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
