#!/usr/bin/env bash
# Latency tuning for the benchmark host. Needs root. Every change is printed
# with its before value, and host/tune.sh --undo restores them from the state
# file. Run host/capture-host.sh before AND after; attach both.
#
# Usage: sudo host/tune.sh [--hugepages N] [--undo]
#   --hugepages N   2 MiB pages to reserve (default 512 = 1 GiB)
# Not done here (needs a reboot, so it is a kernel cmdline change for the
# operator): isolcpus / nohz_full / rcu_nocbs for the cores given to each
# system under test.
set -euo pipefail
state=/var/tmp/p2-tune.state
hp=512
undo=0
while [ $# -gt 0 ]; do
  case "$1" in
    --hugepages) hp="$2"; shift 2;;
    --undo) undo=1; shift;;
    *) echo "unknown arg $1" >&2; exit 2;;
  esac
done
[ "$(id -u)" = 0 ] || { echo "needs root" >&2; exit 2; }
if [ "$undo" = 1 ]; then
  [ -f "$state" ] || { echo "no state file $state" >&2; exit 1; }
  while IFS='|' read -r path val; do
    printf '%s' "$val" > "$path" 2>/dev/null && echo "restored $path = $val" || echo "could not restore $path"
  done < "$state"
  rm -f "$state"; exit 0
fi
: > "$state"
set_file() { # set_file PATH VALUE
  local p="$1" v="$2" before
  [ -w "$p" ] || { echo "skip (absent/ro): $p"; return 0; }
  before="$(cat "$p")"
  # THP files show "[x]" around the active choice; store the bare word.
  case "$before" in *\[*\]*) before="$(sed 's/.*\[\(.*\)\].*/\1/' <<<"$before")";; esac
  echo "$p|$before" >> "$state"
  if printf '%s' "$v" > "$p" 2>/dev/null; then echo "set $p: '$before' -> '$v'"; else echo "FAILED $p -> $v"; fi
}
for g in /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; do set_file "$g" performance; done
for s in /sys/devices/system/cpu/cpu*/cpuidle/state[2-9]/disable; do set_file "$s" 1; done
set_file /sys/kernel/mm/transparent_hugepage/enabled madvise
set_file /sys/kernel/mm/transparent_hugepage/defrag defer+madvise
set_file /proc/sys/vm/nr_hugepages "$hp"
set_file /proc/sys/net/core/busy_poll 50
set_file /proc/sys/net/core/busy_read 50
set_file /proc/sys/kernel/numa_balancing 0
set_file /proc/sys/kernel/sched_rt_runtime_us -1
set_file /proc/sys/vm/stat_interval 120
set_file /proc/sys/kernel/watchdog 0
echo "reserved hugepages now: $(cat /proc/sys/vm/nr_hugepages)"
echo "state saved to $state (undo: sudo host/tune.sh --undo)"
