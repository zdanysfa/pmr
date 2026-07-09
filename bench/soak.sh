#!/usr/bin/env bash
# pmr soak test: run a realistic churning workload for hours/days and sample
# the daemon's RSS, CPU, fds and threads over time. Detects memory/fd leaks.
#
# Usage:
#   bench/soak.sh                      # 24 hours, sample every 60s
#   DURATION=3600 INTERVAL=10 bench/soak.sh   # 1 hour, sample every 10s
#
# Output: bench/soak-<timestamp>.csv + verdict on stdout.
#
# Workload while soaking:
#   - 3 tickers writing a log line every 100ms   (log pipeline pressure)
#   - 1 crasher exiting every ~2s with backoff   (restart machinery pressure)
#   - 1 cron app restarted every minute          (cron + kill sequence pressure)
#   - 1 memory cycler allocating then exiting    (worker/monitor pressure)
set -euo pipefail

PMR_BIN="${PMR_BIN:-$(dirname "$0")/../target/release/pmr}"
PMR_BIN="$(realpath "$PMR_BIN")"
DURATION="${DURATION:-86400}"
INTERVAL="${INTERVAL:-60}"

export PMR_HOME="${PMR_HOME:-/tmp/pmr-soak-$$}"
CSV="$(dirname "$0")/soak-$(date +%Y%m%d-%H%M%S).csv"
CLK_TCK=$(getconf CLK_TCK)

cleanup() { "$PMR_BIN" kill >/dev/null 2>&1 || true; rm -rf "$PMR_HOME"; }
trap cleanup EXIT
mkdir -p "$PMR_HOME"

cat > "$PMR_HOME/ticker.sh" <<'EOF'
#!/bin/bash
while true; do echo "tick $(date +%s%3N) payload padding text"; sleep 0.1; done
EOF
cat > "$PMR_HOME/crasher.sh" <<'EOF'
#!/bin/bash
echo "up"; sleep 2; echo "dying"; exit 1
EOF
cat > "$PMR_HOME/memcycle.sh" <<'EOF'
#!/bin/bash
data=$(head -c 20000000 /dev/zero | tr '\0' 'x')  # hold ~20MB
echo "allocated ${#data} bytes"; sleep 20; exit 0
EOF
chmod +x "$PMR_HOME"/*.sh

echo "soak: $DURATION s, sampling every $INTERVAL s → $CSV"
"$PMR_BIN" ping >/dev/null
DPID=$(cat "$PMR_HOME/pmr.pid")

"$PMR_BIN" start "$PMR_HOME/ticker.sh"  --name ticker -i 3 >/dev/null
"$PMR_BIN" start "$PMR_HOME/crasher.sh" --name crasher --exp-backoff-restart-delay 100 --max-restarts 999999 >/dev/null
"$PMR_BIN" start "$PMR_HOME/ticker.sh"  --name cronny --cron-restart "* * * * *" >/dev/null
# memcycle exits 0 repeatedly; treat as restartable worker
"$PMR_BIN" start "$PMR_HOME/memcycle.sh" --name memcycle --max-restarts 999999 >/dev/null

# comm in /proc/pid/stat may contain spaces — strip to the last ')'.
cpu_ticks() { sed 's/.*) //' "/proc/$1/stat" | awk '{print $12+$13}'; }

echo "elapsed_s,rss_kb,cpu_pct,fds,threads,managed_procs,total_restarts" > "$CSV"

START=$(date +%s)
prev_ticks=$(cpu_ticks "$DPID")
prev_t=$START

while :; do
    sleep "$INTERVAL"
    now=$(date +%s)
    elapsed=$((now - START))

    if ! kill -0 "$DPID" 2>/dev/null; then
        echo "FATAL: daemon died at ${elapsed}s" | tee -a "$CSV"
        exit 1
    fi

    rss=$(ps -o rss= -p "$DPID" | tr -d ' ')
    fds=$(ls "/proc/$DPID/fd" | wc -l)
    thr=$(ps -o nlwp= -p "$DPID" | tr -d ' ')
    ticks=$(cpu_ticks "$DPID")
    cpu=$(awk -v dt="$((ticks - prev_ticks))" -v ck="$CLK_TCK" -v s="$((now - prev_t))" \
        'BEGIN { printf "%.2f", (dt / ck) / s * 100 }')
    prev_ticks=$ticks; prev_t=$now

    stats=$("$PMR_BIN" jlist 2>/dev/null | python3 -c '
import json,sys
l = json.load(sys.stdin)
print(len(l), sum(p["restarts"] for p in l))' 2>/dev/null || echo "0 0")
    procs=${stats% *}; restarts=${stats#* }

    echo "$elapsed,$rss,$cpu,$fds,$thr,$procs,$restarts" | tee -a "$CSV"

    [ "$elapsed" -ge "$DURATION" ] && break
done

# ---------- verdict ----------
python3 - "$CSV" <<'EOF'
import csv, sys
rows = [r for r in csv.DictReader(open(sys.argv[1])) if r["rss_kb"].isdigit()]
if len(rows) < 8:
    print("not enough samples for a verdict"); sys.exit(0)
q = max(1, len(rows) // 4)
first = rows[:q]; last = rows[-q:]
avg = lambda rs, k: sum(float(r[k]) for r in rs) / len(rs)
rss0, rss1 = avg(first, "rss_kb"), avg(last, "rss_kb")
fd0,  fd1  = avg(first, "fds"),   avg(last, "fds")
growth = (rss1 - rss0) / rss0 * 100
hours = int(rows[-1]["elapsed_s"]) / 3600
print(f"\n=== soak verdict ({hours:.2f}h, {len(rows)} samples) ===")
print(f"RSS  first-quartile avg {rss0:.0f} KB → last-quartile avg {rss1:.0f} KB ({growth:+.1f}%)")
print(f"FDs  {fd0:.0f} → {fd1:.0f}")
print(f"restarts survived: {rows[-1]['total_restarts']}")
print(f"avg daemon CPU: {avg(rows[1:], 'cpu_pct'):.2f}%")
leak = growth > 10 or (fd1 - fd0) > 20
print("VERDICT:", "POSSIBLE LEAK — investigate" if leak else "stable, no leak indicators")
EOF
