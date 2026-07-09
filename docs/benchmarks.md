# Benchmarks & 24/7 soak analysis

Everything here is reproducible with the scripts in [`bench/`](../bench):

- `bench/bench.sh` — the full performance suite below (~2 min; compares
  against pm2 automatically when it's installed).
- `bench/soak.sh` — long-run stability: realistic churning workload, samples
  the daemon's RSS/CPU/fds/threads over time, prints a leak verdict.
  Defaults to 24 hours: `bench/soak.sh`. Accelerated: `DURATION=420 INTERVAL=10 bench/soak.sh`.

## Test machine

Linux 7.1.3 (CachyOS), 4 CPUs. pmr 0.1.0 release build vs pm2 v7.0.3 on Node.js.
Absolute numbers vary by machine; the ratios are the point.

## Performance suite (`bench/bench.sh`)

| Metric | pmr | pm2 | ratio |
| --- | --- | --- | --- |
| Cold start → first `ping` (avg 5) | **104 ms** | 444 ms | 4× |
| Idle daemon RSS | **5.6 MB** | 78.9 MB | 14× |
| Idle daemon threads / fds | 5 / 11 | 11 / 23 | — |
| Start 25 instances (one command) | **32 ms** | 393 ms | 12× |
| `ls` latency, 25 procs (avg 20) | **4 ms** | 212 ms | 53× |
| `jlist` latency, 25 procs (avg 20) | **4 ms** | 199 ms | 50× |
| Daemon RSS with 25 procs | **7.0 MB** | 94.1 MB | 13× |
| Restart 25 procs (full kill sequence) | **36 ms** | 1 908 ms | 53× |
| Log pipeline throughput (1 proc spamming, 10 s) | 131 k lines/s | 135 k lines/s | ≈1× |
| Daemon CPU during that log flood | **72.8 %** | 109.8 % | 1.5× less CPU for the same work |

Log throughput is pipe-bound in both managers (the spammer saturates one CPU);
the difference is what the daemon pays to move the bytes: pmr does the same
volume with roughly a third less CPU. Command latency is where the
architectures diverge most — pmr answers `ls` from an in-memory table over a
unix socket in ~4 ms; pm2 pays Node startup for its CLI on every invocation.

## Soak test — stability over time

Accelerated run (7 min, 10 s samples, `bench/soak.sh`), workload designed to
stress every subsystem at once:

- 3 tickers logging every 100 ms (log pipeline),
- 1 crasher exiting every ~2 s with exponential backoff (restart machinery),
- 1 app cron-restarted every minute (cron + kill sequence),
- 1 memory cycler allocating ~20 MB then exiting cleanly (monitoring/worker).

Result:

```
=== soak verdict (0.12h, 42 samples) ===
RSS  first-quartile avg 6104 KB → last-quartile avg 6357 KB (+4.1%)
FDs  43 → 61
restarts survived: 61
avg daemon CPU: 0.17%
VERDICT: stable, no leak indicators
```

- **RSS +4.1 %** is allocator warm-up (arena growth under first real load),
  not a leak — see the code audit below for why nothing accumulates per
  restart. RSS flattens after the first minutes.
- **Average daemon CPU 0.17 %** while supervising 6 processes, streaming
  ~30 log lines/s and surviving a restart every ~7 s.
- **fd variance explained**: we chased the 43→61 growth. Two sources, both
  bounded: (a) live child count fluctuates (each child = 2 pipe fds + 2 log
  fds + 1 pidfd), and (b) `sysinfo` caches a `/proc/<pid>/stat` fd per
  sampled process between 30 s worker ticks; dead entries are released on the
  next tick. A dedicated diff test (10 restarts, 30 s) showed fd count
  17 → 18 with the only additions being the current child's pipes and one
  cached stat fd — no unbounded growth.

## What happens over 24 hours — code-level analysis

The soak script measures; this section explains why the measurement stays flat.
Everything the daemon holds per process is bounded and reclaimed:

| Resource | Growth behaviour |
| --- | --- |
| Process table (`HashMap<u32, Proc>`) | One entry per managed process; removed on `delete`. Counters are fixed-size integers — a billion restarts don't grow memory. |
| Event bus (`tokio::sync::broadcast`, capacity 1024) | Fixed ring buffer; old events are overwritten, slow subscribers skip. Log volume can never inflate the daemon. |
| Log pump tasks | 2 per **running** child; they exit at pipe EOF when the child dies. A restart spawns new pumps only after the old ones are gone. |
| Supervisor tasks | 1 per running instance; terminates on stopped/errored/deleted. The restart loop reuses the same task — churn does not accumulate tasks. |
| Log file handles | 2 per running child (append mode); closed with the pump. Log **files** grow on disk by design — rotate them (see [production.md](production.md)). |
| RPC connections | 1 task per client connection, ends on disconnect; subscribers drop when the client goes away. |
| Cron / watcher handles | 1 per configured process, aborted on delete. |
| `sysinfo` sampling | Single reused `System`; dead pids evicted every 30 s tick (`remove_dead`). |

There is no per-restart or per-log-line allocation that outlives its event.
The known unbounded growth on a 24/7 box is **log files on disk** — that is
intentional and handled by logrotate + SIGUSR2 reopen.

Failure containment for long runs:

- A crash-looping app is throttled by exponential backoff (cap 15 s) and
  parked as `errored` after `max_restarts` unstable restarts — it cannot spin
  the daemon.
- `max_memory_restart` bounds runaway children before the OOM killer gets
  involved.
- If the daemon is SIGKILLed anyway, children keep running (orphaned, same as
  pm2), and the next CLI command auto-starts a fresh daemon over the stale
  socket — verified by an automated e2e test.

**Run the real thing:** `bench/soak.sh` with no arguments soaks for 24 hours
and prints the same verdict from ~1 440 samples. If the last-quartile RSS ends
more than 10 % above the first quartile or fds climb monotonically, that's a
bug — please open an issue with the CSV attached.
