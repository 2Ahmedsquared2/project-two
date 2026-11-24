#!/usr/bin/env python3
"""Run one Kani harness under a hard address-space cap and a wall-clock limit.

Usage: run-harness.py <harness> [timeout_s=1200] [cap_gib=8]

Prints one summary line: harness, verdict (VERIFIED / FAILED / OOM / TIMEOUT /
ERROR), wall time, peak RSS of the largest child process (CBMC), cover
results. The full log goes to $TMPDIR/kani-<harness>.log.
"""

import os
import resource
import subprocess
import sys
import time

harness = sys.argv[1]
timeout = int(sys.argv[2]) if len(sys.argv) > 2 else 1200
cap = int(float(sys.argv[3]) * (1 << 30)) if len(sys.argv) > 3 else 8 << 30
log = os.path.join(os.environ.get("TMPDIR", "/tmp"), f"kani-{harness}.log")


def limit():
    resource.setrlimit(resource.RLIMIT_AS, (cap, cap))


t0 = time.time()
with open(log, "w") as f:
    p = subprocess.Popen(
        ["cargo", "kani", "-p", "kani-engine", "--harness", "proofs::" + harness, "--exact"],
        stdout=f,
        stderr=subprocess.STDOUT,
        preexec_fn=limit,
        start_new_session=True,
    )
    try:
        rc = p.wait(timeout=timeout)
        timed_out = False
    except subprocess.TimeoutExpired:
        os.killpg(p.pid, 9)
        p.wait()
        rc, timed_out = -9, True
wall = time.time() - t0
peak_mib = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss // 1024
out = open(log).read()
if timed_out:
    verdict = "TIMEOUT"
elif any(s in out for s in ("Out of memory", "std::bad_alloc", "memory allocation", "Killed")) or rc in (-9, 137):
    verdict = "OOM"
elif "VERIFICATION:- SUCCESSFUL" in out:
    verdict = "VERIFIED"
elif "VERIFICATION:- FAILED" in out:
    verdict = "FAILED"
else:
    verdict = f"ERROR(rc={rc})"
covers = [l.strip("* ").strip() for l in out.splitlines() if "cover properties satisfied" in l]
steps = [l.split(":")[1].strip() for l in out.splitlines() if "size of program expression" in l]
print(f"{harness}\t{verdict}\t{wall:.0f}s\tpeak {peak_mib} MiB\t{covers[-1] if covers else 'covers: none reported'}\tsteps {steps[-1] if steps else '-'}\tlog {log}")
