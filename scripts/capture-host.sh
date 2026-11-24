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
#
# The options are the script's own. A caller that runs `set -e` with SHELLOPTS
# exported hands errexit and pipefail to every bash it starts, and under
# errexit one fact that cannot be read ends the capture instead of being
# written as unavailable (#38).
set -u
set +e +o pipefail
shopt -s nullglob

out="${1:?usage: capture-host.sh FILE [IFACE]}"
iface="${2:-}"

# A capture that ends non-zero says so: its status, how far it had got and the
# command it ended on. The line goes to a copy of stderr taken here, because
# the script turns its own stderr away around reads that may fail. A capture
# that works prints nothing, and none of this changes the file.
exec {errfd}>&2
step="start"
on_exit() {
  local rc=$? last="$BASH_COMMAND"
  ((rc == 0)) && return 0
  printf 'capture-host.sh: exit %s; file: %s; last step: %s; last command: %s\n' \
    "$rc" "$out" "$step" "$last" >&"$errfd"
}
trap on_exit EXIT

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
  step="section $name"
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

# CPUs that are online (cpu0 has no "online" file and always is).
online_cpus() {
  local d state
  for d in /sys/devices/system/cpu/cpu[0-9]*; do
    state=1
    if [[ -r "$d/online" ]]; then state="$(<"$d/online")"; fi
    [[ "$state" == 1 ]] && printf '%s\n' "$d"
  done
}

s_cmdline() {
  local cmdline tok key found
  if [[ ! -r /proc/cmdline ]]; then
    printf 'unavailable (/proc/cmdline not readable)\n'
    return 0
  fi
  cmdline="$(</proc/cmdline)"
  printf '%s\n' "$cmdline"
  for key in isolcpus nohz_full rcu_nocbs intel_idle.max_cstate processor.max_cstate; do
    found="not set"
    for tok in $cmdline; do
      [[ "$tok" == "$key="* ]] && found="${tok#*=}"
    done
    printf '%s: %s\n' "$key" "$found"
  done
}

s_cstates() {
  local c d states found=0
  while read -r c; do
    found=1
    states=""
    for d in "$c"/cpuidle/state[0-9]*; do
      states+=" $(rd "$d/name")(disable=$(rd "$d/disable"),latency_us=$(rd "$d/latency"))"
    done
    [[ -n "$states" ]] || states=" unavailable (no cpuidle states exposed)"
    printf '%s states:%s; pm_qos_resume_latency_us=%s\n' "${c##*/}" "$states" \
      "$(rd "$c/power/pm_qos_resume_latency_us")"
  done < <(online_cpus)
  ((found)) || printf 'unavailable (no /sys/devices/system/cpu/cpu*)\n'
  printf 'cpuidle driver: '
  rd /sys/devices/system/cpu/cpuidle/current_driver
  printf 'intel_idle max_cstate (module): '
  rd /sys/module/intel_idle/parameters/max_cstate
  printf 'processor max_cstate (module): '
  rd /sys/module/processor/parameters/max_cstate
  # Opening the device registers a latency request of its own, so it is not opened.
  if [[ -e /dev/cpu_dma_latency ]]; then
    printf 'cpu_dma_latency: device present; holder and value not read (opening it files a request)\n'
  else
    printf 'cpu_dma_latency: unavailable (no /dev/cpu_dma_latency)\n'
  fi
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
    # A process that has ended since the glob has no comm left: skipped.
    { IFS= read -r name <"$f"; } 2>/dev/null || continue
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

s_sysctls() {
  local name
  for name in net.core.busy_poll net.core.busy_read net.core.netdev_budget \
    net.core.netdev_budget_usecs kernel.sched_rt_runtime_us kernel.numa_balancing \
    kernel.timer_migration vm.stat_interval; do
    printf '%s: ' "$name"
    rd "/proc/sys/${name//.//}"
  done
}

s_hugepages() {
  local key rest found=0
  if [[ -r /proc/meminfo ]]; then
    while IFS=: read -r key rest; do
      case "$key" in
        HugePages_* | Hugepagesize | Hugetlb | AnonHugePages)
          [[ "$key" == HugePages_Total ]] && found=1
          printf '%s: %s\n' "$key" "${rest#"${rest%%[![:space:]]*}"}"
          ;;
      esac
    done </proc/meminfo
  fi
  ((found)) || printf 'HugePages_Total: unavailable (not in /proc/meminfo)\n'
  printf 'max locked memory (ulimit -l): %s\n' "$(ulimit -l 2>/dev/null || printf 'unavailable')"
}

s_smt() {
  local c found=0
  printf 'active: '
  rd /sys/devices/system/cpu/smt/active
  while read -r c; do
    found=1
    printf '%s thread siblings: ' "${c##*/}"
    rd "$c/topology/thread_siblings_list"
  done < <(online_cpus)
  ((found)) || printf 'thread siblings: unavailable (no /sys/devices/system/cpu/cpu*)\n'
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
step="opening the file"
TZ=UTC printf -v now '%(%Y-%m-%dT%H:%M:%SZ)T' -1

{
  step="header"
  printf 'hl-host-facts v1\n'
  printf 'captured-utc: %s\n' "$now"
  printf 'kernel: '
  rd /proc/sys/kernel/osrelease
  printf 'iface: %s\n' "${iface:-unavailable (no default route in /proc/net/route)}"
  section kernel-cmdline s_cmdline
  section lscpu tool lscpu
  section numa s_numa
  section cstates s_cstates
  section governor s_governor
  section clocksource s_clocksource
  section tsc s_tsc
  section ena-irq-affinity s_irq_affinity
  section irqbalance s_irqbalance
  section busy-poll-sysctls s_sysctls
  section ethtool-c s_ethtool -c
  section ethtool-k s_ethtool -k
  section ethtool-l s_ethtool -l
  section thp s_thp
  section swappiness rd /proc/sys/vm/swappiness
  section chrony tool chronyc tracking
  section hugepages-mlock s_hugepages
  section smt s_smt
} >"$out"
