# pmr — pm2 rewritten in Rust (fork mode)

Process manager daemon + CLI + Rust library in one crate. `src/lib.rs` is the
public API; the CLI (`src/main.rs`) is its first consumer.

## Commands

```bash
cargo test                                  # 22 unit + 11 e2e + doctest
cargo test --test e2e                       # e2e only (spawns real daemons in /tmp)
cargo clippy --all-targets -- -D warnings   # CI gate
cargo fmt --check                           # CI gate
cargo run --example programmatic            # library API smoke test
```

Manual smoke: `PMR_HOME=/tmp/pmr-dev ./target/debug/pmr start script.sh && pmr ls`.
Always `pmr kill` afterwards and use a throwaway `PMR_HOME`.

## Architecture

- `src/ipc.rs` — wire protocol (ndjson over unix socket) + `ProcessSnapshot`; shared by everything.
- `src/client.rs` — `Pmr` client (sync, std-only). Auto-spawns the daemon binary.
- `src/daemon/supervisor.rs` — one tokio task per process instance; ALL restart
  semantics live in the pure `decide_restart()` (unit-tested; mirrors pm2 God.handleExit).
- `src/daemon/state.rs` — `Arc<Mutex<HashMap<u32, Proc>>>`; lock held microseconds
  only; long operations (kill sequences, delays) happen in supervisor tasks, never under the lock.
- `src/daemon/ops.rs` — shared operations (start/stop/restart); RPC, worker, cron,
  watcher all route through here so behavior stays in one place.
- Behavior contract comes from pm2 v7.0.3 (defaults: kill_timeout 1600ms,
  max_restarts 16, min_uptime 1000ms, backoff ×1.5 cap 15s). Don't change
  defaults without checking pm2 parity.

## Gotchas

- **Unix socket path limit (~108 bytes)**: `PMR_HOME` must be short. Tests use
  `/tmp/pmr-t-*`, never deep tempdirs.
- **Pinned nightly + edition 2024** (`rust-toolchain.toml`): clippy enforces
  let-chains (`if let ... && ...`). The bare `nightly` toolchain on some machines
  is a partial install — keep the dated pin.
- **`PMR_WORKER_INTERVAL` (ms)** overrides the 30s housekeeping tick — e2e tests
  set 300ms so max_memory_restart triggers fast.
- **Library spawns the daemon binary**, not `current_exe()` (host app would
  recurse): lookup order is `$PMR_BIN` → exe named `pmr` → sibling in cargo
  target dirs → `$PATH` (`client.rs::find_pmr_bin`).
- **`AppConfig` unknown fields** land in the flattened `env_profiles` map and are
  rejected by `validate()` unless prefixed `env_` — that's how `exec_mode: cluster`
  gets its clear fork-only error. `deny_unknown_fields` can't be used with `flatten`.
- **pm_id counter resets to 0 only when the table empties** (pm2 behavior);
  alloc + insert must happen under one lock.
- **e2e harness** (`tests/e2e.rs::Home`): unique `PMR_HOME` per test → fully
  parallel; `Drop` runs `pmr kill`. Assert on `jlist` JSON, not table output.
- Parent directory may be a pm2 source checkout (reference for audits); pmr is
  its own git repo — run git commands inside `pmr/`.
