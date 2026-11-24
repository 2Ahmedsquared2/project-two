#!/usr/bin/env bash
# Writes the host-facts file a replay-harness write-up needs:
#
#   scripts/capture-host.sh FILE [IFACE]
#   hl-replay ... --out DIR --host-facts FILE
#
# Read-only: it reads /proc and /sys and asks lscpu, ethtool and chronyc to
# print what they already know. It sets nothing and contacts no remote host.
# A fact that cannot be read is written as "unavailable (why)", never left
# out; hl-replay refuses a file with a section missing.
#
# Uses bash builtins only, apart from those three optional tools, so it gives
# the same file layout on a stripped host.
set -u
shopt -s nullglob

out="${1:?usage: capture-host.sh FILE [IFACE]}"
iface="${2:-}"

# The value of a file, or why not.
rd() {
  local v=""
  if [[ -r "$1" ]] && v="$(<"$1")" 2>/dev/null && [[ -n "$v" ]]; then
    printf '%s\n' "$v"
  else
    printf 'unavailable (%s not readable)\n' "$1"
  fi
}

# The output of a query tool, or why not.
tool() {
  local v=""
  if ! command -v "$1" >/dev/null 2>&1; then
    printf 'unavailable (%s: not installed)\n' "$1"
  elif v="$("$@" 2>&1)" && [[ -n "$v" ]]; then
    printf '%s\n' "$v"
  else
    printf 'unavailable (%s failed: %s)\n' "$*" "${v%%$'\n'*}"
  fi
}

section() { # NAME, body on stdin from the function named by the rest
  local name="$1"
  shift
  printf '== begin %s\n' "$name"
  "$@"
  printf '== end %s\n' "$name"
}

default_iface() {
  local ifc dest _rest
  [[ -r /proc/net/route ]] || return 0
  while read -r ifc dest _rest; do
    if [[ "$dest" == 00000000 ]]; then
      printf '%s' "$ifc"
      return 0
    fi
  done </proc/net/route
}

s_numa() {
  local n found=0
  for n in /sys/devices/system/node/node[0-9]*; do
    found=1
    printf '%s cpus: ' "${n##*/}"
    rd "$n/cpulist"
  done
  ((found)) || printf 'unavailable (no /sys/devices/system/node/node*)\n'
}

s_cstates() {
  local d found=0
  for d in /sys/devices/system/cpu/cpu0/cpuidle/state[0-9]*; do
    found=1
    printf 'cpu0 %s: name=%s disable=%s latency_us=%s\n' "${d##*/}" \
      "$(rd "$d/name")" "$(rd "$d/disable")" "$(rd "$d/latency")"
  done
  ((found)) || printf 'cpuidle states: unavailable (no /sys/devices/system/cpu/cpu0/cpuidle/state*)\n'
  printf 'cpuidle driver: '
  rd /sys/devices/system/cpu/cpuidle/current_driver
  printf 'intel_idle max_cstate: '
  rd /sys/module/intel_idle/parameters/max_cstate
}

s_governor() {
  local f cpu found=0
  for f in /sys/devices/system/cpu/cpu[0-9]*/cpufreq/scaling_governor; do
    found=1
    cpu="${f%/cpufreq/scaling_governor}"
    printf '%s: ' "${cpu##*/}"
    rd "$f"
  done
  ((found)) || printf 'unavailable (no cpufreq scaling_governor exposed)\n'
}

s_clocksource() {
  printf 'current: '
  rd /sys/devices/system/clocksource/clocksource0/current_clocksource
  printf 'available: '
  rd /sys/devices/system/clocksource/clocksource0/available_clocksource
}

s_tsc() {
  local key rest flags="" f
  if [[ -r /proc/cpuinfo ]]; then
    while IFS=: read -r key rest; do
      if [[ "$key" == flags* ]]; then
        flags=" $rest "
        break
      fi
    done </proc/cpuinfo
  fi
  if [[ -z "$flags" ]]; then
    printf 'unavailable (no flags line in /proc/cpuinfo)\n'
    return 0
  fi
  for f in tsc constant_tsc nonstop_tsc tsc_reliable rdtscp tsc_deadline_timer tsc_known_freq; do
    if [[ "$flags" == *" $f "* ]]; then
      printf '%s: yes\n' "$f"
    else
      printf '%s: no\n' "$f"
    fi
  done
}

s_irq_affinity() {
  local line irq found=0
  if [[ ! -r /proc/interrupts ]]; then
    printf 'unavailable (/proc/interrupts not readable)\n'
    return 0
  fi
  while read -r line; do
    irq="${line%%:*}"
    [[ "$irq" =~ ^[0-9]+$ ]] || continue
    if [[ "$line" =~ (^|[^a-z])ena[-@] ]] || { [[ -n "$iface" ]] && [[ "$line" == *"$iface"* ]]; }; then
      found=1
      printf 'irq %s (%s): smp_affinity_list=%s\n' "$irq" "${line##* }" \
        "$(rd "/proc/irq/$irq/smp_affinity_list")"
    fi
  done </proc/interrupts
  ((found)) || printf 'unavailable (no interrupt named for ena or interface "%s" in /proc/interrupts)\n' "$iface"
}

s_irqbalance() {
  local f pid name seen=0
  for f in /proc/[0-9]*/comm; do
    seen=1
    name=""
    { name="$(<"$f")"; } 2>/dev/null || continue
    if [[ "$name" == irqbalance ]]; then
      pid="${f%/comm}"
      printf 'running (pid %s)\n' "${pid##*/}"
      return 0
    fi
  done
  if ((seen)); then
    printf 'not running\n'
  else
    printf 'unavailable (/proc/*/comm not readable)\n'
  fi
}

s_busy_poll() {
  printf 'net.core.busy_poll: '
  rd /proc/sys/net/core/busy_poll
  printf 'net.core.busy_read: '
  rd /proc/sys/net/core/busy_read
}

s_ethtool() { # FLAG: one of the read-only queries -c -k -l
  if [[ -z "$iface" ]]; then
    printf 'unavailable (no default-route interface; pass one as the second argument)\n'
  else
    tool ethtool "$1" "$iface"
  fi
}

s_thp() {
  printf 'enabled: '
  rd /sys/kernel/mm/transparent_hugepage/enabled
  printf 'defrag: '
  rd /sys/kernel/mm/transparent_hugepage/defrag
}

[[ -n "$iface" ]] || iface="$(default_iface)"
TZ=UTC printf -v now '%(%Y-%m-%dT%H:%M:%SZ)T' -1

{
  printf 'hl-host-facts v1\n'
  printf 'captured-utc: %s\n' "$now"
  printf 'kernel: '
  rd /proc/sys/kernel/osrelease
  printf 'iface: %s\n' "${iface:-unavailable (no default route in /proc/net/route)}"
  section kernel-cmdline rd /proc/cmdline
  section lscpu tool lscpu
  section numa s_numa
  section cstates s_cstates
  section governor s_governor
  section clocksource s_clocksource
  section tsc s_tsc
  section ena-irq-affinity s_irq_affinity
  section irqbalance s_irqbalance
  section busy-poll-sysctls s_busy_poll
  section ethtool-c s_ethtool -c
  section ethtool-k s_ethtool -k
  section ethtool-l s_ethtool -l
  section thp s_thp
  section swappiness rd /proc/sys/vm/swappiness
  section chrony tool chronyc tracking
} >"$out"
