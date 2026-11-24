#!/usr/bin/env bash
# Read-only snapshot of everything on a host that moves latency numbers.
# Usage: host/capture-host.sh [OUT_DIR]   (default ./host-capture-<utc>)
# Changes nothing. Each section goes to its own file; a missing tool is
# recorded as "unavailable", never fatal. Attach the whole directory to the
# benchmark run it describes (bench/results/<run>/host/).
set -uo pipefail
out="${1:-./host-capture-$(date -u +%Y%m%dT%H%M%SZ)}"
mkdir -p "$out"
run() { # run NAME CMD...
  local name="$1"; shift
  { echo "\$ $*"; if command -v "$1" >/dev/null 2>&1; then "$@" 2>&1; echo "exit=$?"; else echo "unavailable: $1"; fi; } > "$out/$name.txt"
}
cat_files() { # cat_files NAME PATH...
  local name="$1"; shift
  for f in "$@"; do echo "== $f"; cat "$f" 2>&1; done > "$out/$name.txt"
}
date -u +%Y-%m-%dT%H:%M:%S.%NZ > "$out/captured_at.txt"
run uname uname -a
run lscpu lscpu
run lscpu-extended lscpu --extended
run numactl numactl --hardware
cat_files cmdline /proc/cmdline
cat_files governor /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor
cat_files cur-freq /sys/devices/system/cpu/cpu*/cpufreq/scaling_cur_freq
cat_files cstates /sys/devices/system/cpu/cpu0/cpuidle/state*/name /sys/devices/system/cpu/cpu0/cpuidle/state*/disable /sys/devices/system/cpu/cpu0/cpuidle/state*/latency /sys/module/intel_idle/parameters/max_cstate
cat_files isolation /sys/devices/system/cpu/isolated /sys/devices/system/cpu/nohz_full
cat_files thp /sys/kernel/mm/transparent_hugepage/enabled /sys/kernel/mm/transparent_hugepage/defrag
cat_files hugepages /proc/sys/vm/nr_hugepages /proc/meminfo
cat_files sysctl-net /proc/sys/net/core/busy_poll /proc/sys/net/core/busy_read /proc/sys/net/core/netdev_budget /proc/sys/net/ipv4/tcp_low_latency
cat_files clocksource /sys/devices/system/clocksource/clocksource0/current_clocksource /sys/devices/system/clocksource/clocksource0/available_clocksource
cat_files irqs /proc/interrupts
for d in /proc/irq/[0-9]*; do echo "$(basename "$d") $(cat "$d/smp_affinity_list" 2>/dev/null)"; done > "$out/irq-affinity.txt"
run chrony chronyc tracking
run chrony-sources chronyc sources -v
run ip-link ip -s link
for i in $(ls /sys/class/net | grep -v '^lo$'); do
  run "ethtool-$i" ethtool "$i"
  run "ethtool-i-$i" ethtool -i "$i"
  run "ethtool-S-$i" ethtool -S "$i"
  run "ethtool-c-$i" ethtool -c "$i"
  run "ethtool-l-$i" ethtool -l "$i"
  run "ethtool-g-$i" ethtool -g "$i"
done
run loadavg cat /proc/loadavg
run vmstat vmstat 1 5
run top top -b -n 1 -w 200
run ulimit-l bash -c 'ulimit -l'
run df df -h
run lsblk lsblk -o NAME,MODEL,SIZE,ROTA,SCHED,MOUNTPOINT
cat_files imds-hint /sys/devices/virtual/dmi/id/product_name /sys/devices/virtual/dmi/id/board_asset_tag
echo "capture written to $out"
