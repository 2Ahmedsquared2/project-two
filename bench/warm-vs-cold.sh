#!/usr/bin/env bash
# Cold vs warm first-N-blocks latency: TRIALS fresh processes per
# configuration, configuration order shuffled per round (seeded), one CSV of
# every sample plus a summary. Release build.
# Usage: bench/warm-vs-cold.sh OUT_DIR [TRIALS=30] [N=200] [CORE=0] [SEED=1]
set -euo pipefail
out="$1"; trials="${2:-30}"; n="${3:-200}"; core="${4:-0}"; seed="${5:-1}"
cd "$(dirname "$0")/.."
cargo build -q --release -p p2-node
bin="${CARGO_TARGET_DIR:-target}/release/p2-node"
mkdir -p "$out"
"$bin" gen --blocks "$n" --actions 8 --seed 11 --out-blocks "$out/blocks.jsonl" --out-diffs "$out/diffs.jsonl"
"$bin" gen --blocks 200 --actions 8 --seed 99 --out-blocks "$out/pw-blocks.jsonl" --out-diffs "$out/pw-diffs.jsonl"
configs=(cold warm warm-no-pin warm-no-arena warm-no-prewarm)
echo "config,event,ns,pin,arena,minor_faults,major_faults,invol_switches,sink" > "$out/samples.csv"
: > "$out/order.txt"
RANDOM="$seed"
for ((r = 0; r < trials; r++)); do
  order=($(for c in "${configs[@]}"; do echo "$RANDOM $c"; done | sort -n | cut -d' ' -f2))
  echo "round $r: ${order[*]}" >> "$out/order.txt"
  for c in "${order[@]}"; do
    "$bin" warm-trial --config "$c" --blocks "$out/blocks.jsonl" --diffs "$out/diffs.jsonl" \
      --prewarm-blocks "$out/pw-blocks.jsonl" --prewarm-diffs "$out/pw-diffs.jsonl" \
      --n "$n" --prewarm 20 --core "$core" >> "$out/samples.csv"
  done
done
{
  echo "host: $(uname -srm); cpu: $(lscpu | sed -n 's/^Model name: *//p'); nproc: $(nproc); load: $(cat /proc/loadavg)"
  echo "git: $(git rev-parse HEAD)$(git diff --quiet || echo '-dirty'); trials: $trials; n: $n; core: $core; seed: $seed"
  echo "memlock: $(ulimit -l) KiB (mlockall not in matrix unless unlimited)"
  python3 - "$out/samples.csv" "$n" <<'PY'
import csv, sys, collections
rows = list(csv.DictReader(open(sys.argv[1])))
n = int(sys.argv[2])
by = collections.defaultdict(list)
meta = {}
for r in rows:
    by[(r["config"], int(r["event"]))].append(int(r["ns"]))
    meta[r["config"]] = (r["pin"], r["arena"])
def pct(v, m):
    v = sorted(v); k = max(1, -(-m * len(v) // 1000)); return v[k - 1]
print(f"{'config':<16}{'events':<12}{'samples':>8}{'p50 ns':>10}{'p90 ns':>10}{'p99 ns':>10}{'max ns':>10}  pin/arena")
for c in ["cold", "warm", "warm-no-pin", "warm-no-arena", "warm-no-prewarm"]:
    for label, rng in [("0", range(0, 1)), ("1-9", range(1, 10)), (f"0-{n-1}", range(0, n))]:
        v = [x for i in rng for x in by[(c, i)]]
        if v:
            print(f"{c:<16}{label:<12}{len(v):>8}{pct(v,500):>10}{pct(v,900):>10}{pct(v,990):>10}{max(v):>10}  {'/'.join(meta[c])}")
fl = collections.defaultdict(set)
for r in rows:
    fl[r["config"]].add((r["minor_faults"], r["major_faults"], r["invol_switches"]))
for c, s in fl.items():
    mins = sorted(int(a) for a, _, _ in s); maj = sorted(int(b) for _, b, _ in s); inv = sorted(int(x) for _, _, x in s)
    print(f"{c:<16} per-trial measured-window minor faults {mins[0]}..{mins[-1]}, major {maj[0]}..{maj[-1]}, involuntary switches {inv[0]}..{inv[-1]}")
PY
} | tee "$out/summary.txt"
