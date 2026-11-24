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
#
# State safety: the state lives in a directory owned by the running user with
# mode 0700 (default /var/lib/p2-tune), the state file must be a regular file
# (not a symlink), a second tune refuses instead of overwriting the saved
# originals, undo only writes to the exact sysfs/procfs paths this script
# tunes, and a failed restore keeps the state file and exits 1.
# Test seams (tests/host_tune.rs): P2_TUNE_ROOT prefixes every tuned path,
# P2_TUNE_STATE_DIR moves the state dir, P2_TUNE_ALLOW_NONROOT=1 skips the
# root check.
set -euo pipefail
root="${P2_TUNE_ROOT:-}"
sdir="${P2_TUNE_STATE_DIR:-/var/lib/p2-tune}"
state="$sdir/p2-tune.state"
hp=512
undo=0
while [ $# -gt 0 ]; do
  case "$1" in
    --hugepages) hp="$2"; shift 2;;
    --undo) undo=1; shift;;
    *) echo "unknown arg $1" >&2; exit 2;;
  esac
done
case "$hp" in ''|*[!0-9]*) echo "--hugepages must be a number" >&2; exit 2;; esac
[ "$(id -u)" = 0 ] || [ "${P2_TUNE_ALLOW_NONROOT:-0}" = 1 ] || { echo "needs root" >&2; exit 2; }

# The only paths this script ever writes (relative to $root).
allowed() { # allowed PATH -> 0 if PATH is one this script tunes
  local p="${1#"$root"}"
  [ "$root$p" = "$1" ] || return 1
  case "$p" in *..*) return 1;; esac
  case "$p" in
    /sys/devices/system/cpu/cpu[0-9]*/cpufreq/scaling_governor) return 0;;
    /sys/devices/system/cpu/cpu[0-9]*/cpuidle/state[2-9]/disable) return 0;;
    /sys/kernel/mm/transparent_hugepage/enabled|/sys/kernel/mm/transparent_hugepage/defrag) return 0;;
    /proc/sys/vm/nr_hugepages|/proc/sys/net/core/busy_poll|/proc/sys/net/core/busy_read) return 0;;
    /proc/sys/kernel/numa_balancing|/proc/sys/kernel/sched_rt_runtime_us) return 0;;
    /proc/sys/vm/stat_interval|/proc/sys/kernel/watchdog) return 0;;
  esac
  return 1
}

# State dir: exists (or is created) 0700, owned by us, not a symlink.
if [ ! -e "$sdir" ]; then mkdir -m 700 -p "$sdir"; fi
if [ -L "$sdir" ] || [ ! -d "$sdir" ] || [ "$(stat -c %u "$sdir")" != "$(id -u)" ] \
   || [ "$(stat -c %a "$sdir")" != 700 ]; then
  echo "refusing: state dir $sdir must be a real directory owned by uid $(id -u) with mode 700" >&2; exit 1
fi
if [ -L "$state" ] || { [ -e "$state" ] && [ ! -f "$state" ]; }; then
  echo "refusing: state file $state is not a regular file" >&2; exit 1
fi

if [ "$undo" = 1 ]; then
  [ -f "$state" ] || { echo "no state file $state" >&2; exit 1; }
  [ "$(stat -c %u "$state")" = "$(id -u)" ] || { echo "refusing: $state not owned by uid $(id -u)" >&2; exit 1; }
  fail=0
  while IFS='|' read -r path val; do
    if ! allowed "$path"; then echo "refusing to restore $path: not a path this script tunes" >&2; fail=1; continue; fi
    case "$val" in *[!A-Za-z0-9+_-]*) echo "refusing to restore $path: unexpected value" >&2; fail=1; continue;; esac
    if [ -f "$path" ] && printf '%s' "$val" > "$path" 2>/dev/null; then echo "restored $path = $val"
    else echo "could not restore $path" >&2; fail=1; fi
  done < "$state"
  if [ "$fail" = 1 ]; then echo "undo incomplete: state kept at $state" >&2; exit 1; fi
  rm -f "$state"; exit 0
fi
if [ -e "$state" ]; then
  echo "already tuned: $state holds the original values; run --undo first" >&2; exit 1
fi
# Journal: the state file is created first (exclusively, mode 600) and each
# original value is appended and synced BEFORE its path is written, so a tune
# that stops part-way (signal, set -e, a failed write) always leaves a state
# file that --undo can restore from. A re-tune refuses while it exists.
( umask 077; set -o noclobber; : > "$state" ) || { echo "refusing: could not create $state" >&2; exit 1; }
persist() { sync -- "$state" 2>/dev/null || sync; }
failed=0
stopped() { echo "tune stopped part-way: run sudo host/tune.sh --undo to restore from $state" >&2; }
trap stopped ERR
trap 'stopped; exit 130' INT
trap 'stopped; exit 143' TERM
set_file() { # set_file PATH VALUE
  local p="$1" v="$2" before
  allowed "$p" || { echo "internal: $p not in the allowed set" >&2; exit 1; }
  [ -w "$p" ] || { echo "skip (absent/ro): $p"; return 0; }
  before="$(cat "$p")"
  # THP files show "[x]" around the active choice; store the bare word.
  case "$before" in *\[*\]*) before="$(sed 's/.*\[\(.*\)\].*/\1/' <<<"$before")";; esac
  echo "$p|$before" >> "$state"
  persist
  if printf '%s' "$v" > "$p" 2>/dev/null; then echo "set $p: '$before' -> '$v'"; else echo "FAILED $p -> $v" >&2; failed=1; fi
}
for g in "$root"/sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; do [ -e "$g" ] && set_file "$g" performance; done
for s in "$root"/sys/devices/system/cpu/cpu*/cpuidle/state[2-9]/disable; do [ -e "$s" ] && set_file "$s" 1; done
set_file "$root"/sys/kernel/mm/transparent_hugepage/enabled madvise
set_file "$root"/sys/kernel/mm/transparent_hugepage/defrag defer+madvise
set_file "$root"/proc/sys/vm/nr_hugepages "$hp"
set_file "$root"/proc/sys/net/core/busy_poll 50
set_file "$root"/proc/sys/net/core/busy_read 50
set_file "$root"/proc/sys/kernel/numa_balancing 0
set_file "$root"/proc/sys/kernel/sched_rt_runtime_us -1
set_file "$root"/proc/sys/vm/stat_interval 120
set_file "$root"/proc/sys/kernel/watchdog 0
trap - ERR INT TERM
if [ "$failed" = 1 ]; then
  echo "tune incomplete: some writes failed; originals of everything attempted are in $state (undo: sudo host/tune.sh --undo)" >&2; exit 1
fi
echo "reserved hugepages now: $(cat "$root"/proc/sys/vm/nr_hugepages 2>/dev/null || echo '?')"
echo "state saved to $state (undo: sudo host/tune.sh --undo)"
