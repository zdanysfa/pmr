#!/usr/bin/env bash
# pmr comprehensive benchmark: startup latency, memory, command latency,
# start/restart throughput, log throughput, daemon CPU, quick leak check.
# Compares against pm2 when it's installed.
#
# Usage:   bench/bench.sh [path-to-pmr-binary]
# Default: target/release/pmr (build with `cargo build --release` first)
set -euo pipefail

PMR_BIN="${1:-$(dirname "$0")/../target/release/pmr}"
PMR_BIN="$(realpath "$PMR_BIN")"
N_PROCS="${N_PROCS:-25}"
LAT_ITER="${LAT_ITER:-20}"
LOG_SECONDS="${LOG_SECONDS:-10}"

export PMR_HOME=/tmp/pmr-bench-$$
PM2_OK=0
if command -v pm2 >/dev/null 2>&1; then
    PM2_OK=1
    export PM2_HOME=/tmp/pm2-bench-$$
fi

cleanup() {
    "$PMR_BIN" kill >/dev/null 2>&1 || true
    [ "$PM2_OK" = 1 ] && pm2 kill >/dev/null 2>&1 || true
    rm -rf "$PMR_HOME" "${PM2_HOME:-/nonexistent}"
}
trap cleanup EXIT
mkdir -p "$PMR_HOME"

# ---------- helpers ----------
now_ms() { date +%s%3N; }

rss_kb() { ps -o rss= -p "$1" 2>/dev/null | tr -d ' ' || echo 0; }
fd_count() { ls "/proc/$1/fd" 2>/dev/null | wc -l; }
threads() { ps -o nlwp= -p "$1" 2>/dev/null | tr -d ' ' || echo 0; }

# cumulative CPU ticks (utime+stime) of a pid.
# comm (field 2) may contain spaces/parens (pm2 does!) — strip to the last ')'.
cpu_ticks() { sed 's/.*) //' "/proc/$1/stat" 2>/dev/null | awk '{print $12+$13}' || echo 0; }
CLK_TCK=$(getconf CLK_TCK)

daemon_pid() { cat "$PMR_HOME/pmr.pid"; }

avg_ms() { # run a command $LAT_ITER times, print avg ms
    local total=0 t0 t1
    for _ in $(seq "$LAT_ITER"); do
        t0=$(now_ms); "$@" >/dev/null; t1=$(now_ms)
        total=$((total + t1 - t0))
    done
    echo "$((total / LAT_ITER))"
}

section() { printf '\n== %s ==\n' "$1"; }

# workload scripts
cat > "$PMR_HOME/sleeper.sh" <<'EOF'
#!/bin/bash
sleep 600
EOF
cat > "$PMR_HOME/spammer.sh" <<'EOF'
#!/bin/bash
i=0
while true; do
    echo "log line $i with some typical payload text 1234567890"
    i=$((i+1))
done
EOF
chmod +x "$PMR_HOME"/*.sh

echo "pmr benchmark — binary: $PMR_BIN"
echo "host: $(uname -sr), $(nproc) cpus"
"$PMR_BIN" --version

# ---------- 1. cold start ----------
section "cold start (daemon spawn + first ping, 5 runs)"
total=0
for _ in 1 2 3 4 5; do
    "$PMR_BIN" kill >/dev/null 2>&1 || true
    sleep 0.3
    t0=$(now_ms); "$PMR_BIN" ping >/dev/null; t1=$(now_ms)
    total=$((total + t1 - t0))
done
PMR_COLD=$((total / 5))
echo "pmr:  ${PMR_COLD} ms"
if [ "$PM2_OK" = 1 ]; then
    total=0
    for _ in 1 2 3 4 5; do
        pm2 kill >/dev/null 2>&1 || true
        sleep 0.3
        t0=$(now_ms); pm2 ping >/dev/null 2>&1; t1=$(now_ms)
        total=$((total + t1 - t0))
    done
    echo "pm2:  $((total / 5)) ms"
fi

# ---------- 2. idle footprint ----------
section "idle daemon footprint"
sleep 1
DPID=$(daemon_pid)
echo "pmr:  rss=$(rss_kb "$DPID") KB  fds=$(fd_count "$DPID")  threads=$(threads "$DPID")"
if [ "$PM2_OK" = 1 ]; then
    P2PID=$(cat "$PM2_HOME/pm2.pid")
    echo "pm2:  rss=$(rss_kb "$P2PID") KB  fds=$(fd_count "$P2PID")  threads=$(threads "$P2PID")"
fi
PMR_RSS_IDLE=$(rss_kb "$DPID")

# ---------- 3. start throughput ----------
section "start $N_PROCS instances (one command)"
t0=$(now_ms)
"$PMR_BIN" start "$PMR_HOME/sleeper.sh" --name fleet -i "$N_PROCS" >/dev/null
t1=$(now_ms)
echo "pmr:  $((t1 - t0)) ms"
if [ "$PM2_OK" = 1 ]; then
    t0=$(now_ms)
    pm2 start "$PMR_HOME/sleeper.sh" --name fleet -i "$N_PROCS" >/dev/null 2>&1
    t1=$(now_ms)
    echo "pm2:  $((t1 - t0)) ms   (pm2 forces fork mode for .sh too)"
fi

# ---------- 4. command latency under load ----------
section "command latency with $N_PROCS processes (avg of $LAT_ITER)"
echo "pmr ls:    $(avg_ms "$PMR_BIN" ls) ms"
echo "pmr jlist: $(avg_ms "$PMR_BIN" jlist) ms"
if [ "$PM2_OK" = 1 ]; then
    echo "pm2 ls:    $(avg_ms pm2 ls) ms"
    echo "pm2 jlist: $(avg_ms pm2 jlist) ms"
fi

# ---------- 5. loaded footprint ----------
section "daemon footprint with $N_PROCS processes"
echo "pmr:  rss=$(rss_kb "$DPID") KB  fds=$(fd_count "$DPID")"
if [ "$PM2_OK" = 1 ]; then
    echo "pm2:  rss=$(rss_kb "$P2PID") KB  fds=$(fd_count "$P2PID")"
fi

# ---------- 6. log throughput + daemon CPU ----------
section "log pipeline: 1 process spamming stdout for ${LOG_SECONDS}s"
"$PMR_BIN" start "$PMR_HOME/spammer.sh" --name spam >/dev/null
sleep 1
LOG_FILE="$PMR_HOME/logs/spam-$("$PMR_BIN" id spam)-out.log"
"$PMR_BIN" flush spam >/dev/null
ticks0=$(cpu_ticks "$DPID"); t0=$(now_ms)
sleep "$LOG_SECONDS"
ticks1=$(cpu_ticks "$DPID"); t1=$(now_ms)
"$PMR_BIN" stop spam >/dev/null
lines=$(wc -l < "$LOG_FILE")
elapsed_ms=$((t1 - t0))
cpu_pct=$(awk -v dt="$((ticks1 - ticks0))" -v ck="$CLK_TCK" -v ms="$elapsed_ms" \
    'BEGIN { printf "%.1f", (dt / ck) / (ms / 1000) * 100 }')
echo "pmr:  $((lines * 1000 / elapsed_ms)) lines/s written, daemon cpu ${cpu_pct}% during"
if [ "$PM2_OK" = 1 ]; then
    pm2 start "$PMR_HOME/spammer.sh" --name spam >/dev/null 2>&1
    sleep 1
    pm2 flush spam >/dev/null 2>&1
    P2LOG="$PM2_HOME/logs/spam-out.log"
    ticks0=$(cpu_ticks "$P2PID"); t0=$(now_ms)
    sleep "$LOG_SECONDS"
    ticks1=$(cpu_ticks "$P2PID"); t1=$(now_ms)
    pm2 stop spam >/dev/null 2>&1
    lines=$(wc -l < "$P2LOG" 2>/dev/null || echo 0)
    elapsed_ms=$((t1 - t0))
    cpu_pct=$(awk -v dt="$((ticks1 - ticks0))" -v ck="$CLK_TCK" -v ms="$elapsed_ms" \
        'BEGIN { printf "%.1f", (dt / ck) / (ms / 1000) * 100 }')
    echo "pm2:  $((lines * 1000 / elapsed_ms)) lines/s written, daemon cpu ${cpu_pct}% during"
fi

# ---------- 7. restart throughput ----------
section "restart whole fleet ($N_PROCS processes, full kill sequence each)"
t0=$(now_ms)
"$PMR_BIN" restart fleet >/dev/null
t1=$(now_ms)
echo "pmr:  $((t1 - t0)) ms"
if [ "$PM2_OK" = 1 ]; then
    t0=$(now_ms)
    pm2 restart fleet >/dev/null 2>&1
    t1=$(now_ms)
    echo "pm2:  $((t1 - t0)) ms"
fi

# ---------- 8. quick leak check ----------
section "daemon RSS after all load vs idle (quick leak indicator)"
"$PMR_BIN" delete all >/dev/null
sleep 1
PMR_RSS_AFTER=$(rss_kb "$DPID")
echo "pmr:  idle ${PMR_RSS_IDLE} KB → after load ${PMR_RSS_AFTER} KB (Δ $((PMR_RSS_AFTER - PMR_RSS_IDLE)) KB)"
echo
echo "done. For long-run leak analysis use bench/soak.sh (24h capable)."
