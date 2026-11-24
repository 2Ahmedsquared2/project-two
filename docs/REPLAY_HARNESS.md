# Replay harness (`hl-replay`)

One process, one clock, one line at a time:

```
feeder --> reader --> parser --> book --> decision --> signer --> send --> loopback sink
 feed        t0         t1                                          t2
```

| Stamp | Taken when |
|---|---|
| `sched` | when the line was due: start + i x interval. The feeder is open loop: it never waits for an answer, and a late line is sent at once without moving the schedule |
| `feed` | the feeder, just before it actually writes the line |
| `t0` | the hot thread's `read` returned the line's bytes (line seen) |
| `t1` | the parser returned |
| `t2` | the write of the signed order to the sink socket returned (bytes handed to the socket) |

Five HDR histograms per arm, nanoseconds, 3 significant digits, 1 ns to 60 s:

| Metric | Meaning |
|---|---|
| `sched-t2` | **the headline for comparing arms.** Due time to bytes handed to the socket. Includes the wake-up delay, which is what the arms differ on, and keeps a late feeder in the number (no coordinated omission) |
| `feed-t2` | the same from the actual write. `sched-t2` minus this is the feeder's own lateness |
| `feed-t0` | detection: actual write to line seen |
| `t2-t0` | in-process: line seen to bytes handed to the socket |
| `t1-t0` | line seen to parsed |

The report prints the sample count, p50, p90, p99, p99.9, p99.99 and max, and
no mean. Rounds are merged histograms, never averaged percentiles.

## Rerun on the Tokyo host

```bash
git rev-parse HEAD                       # must match the report's `commit:` line, with no -dirty
cargo build --release -p hl-replay
out=results/replay-$(date -u +%Y%m%dT%H%M%SZ)
facts=$out.host-facts.txt
mkdir -p results
./scripts/capture-host.sh "$facts"       # read-only; second argument names the NIC if it is not the default route's
./target/release/hl-replay \
  --arms all --rounds 5 --lines 11000 --warmup 1000 --interval-us 1000 \
  --feeder-core 2 --hot-core 3 \
  --host-facts "$facts" --out "$out"
```

### Host facts

`--out` is refused (exit 2, nothing run, nothing written) unless
`--host-facts FILE` names a file written by `scripts/capture-host.sh` on the
same host. The harness checks, before anything runs, that the file has the
header and all 18 sections, each terminated and not empty, and that it was
captured on the running kernel. The report then carries
`host facts: FILE, captured <UTC>, 18 sections checked, unavailable in: ...`
and the file is copied to `DIR/host-facts.txt`.

| Section | What the script reads |
|---|---|
| `kernel-cmdline` | `/proc/cmdline`, then `isolcpus`, `nohz_full`, `rcu_nocbs`, `intel_idle.max_cstate` and `processor.max_cstate` parsed out of it (`not set` when absent) |
| `lscpu` | `lscpu` |
| `numa` | `/sys/devices/system/node/node*/cpulist` |
| `cstates` | every online CPU's cpuidle states (name, disable, latency) and `pm_qos_resume_latency_us`, the cpuidle driver, the `intel_idle` and `processor` `max_cstate` module values, and whether `/dev/cpu_dma_latency` exists (it is not opened: opening it files a latency request, and its holder cannot be read) |
| `governor` | every CPU's `scaling_governor` |
| `clocksource` | current and available clocksource |
| `tsc` | the TSC flags in `/proc/cpuinfo` |
| `ena-irq-affinity` | `smp_affinity_list` of every interrupt named for ENA or the NIC |
| `irqbalance` | whether a process named `irqbalance` runs |
| `busy-poll-sysctls` | `net.core.busy_poll`, `busy_read`, `netdev_budget`, `netdev_budget_usecs`; `kernel.sched_rt_runtime_us`, `numa_balancing`, `timer_migration`; `vm.stat_interval` |
| `ethtool-c`, `ethtool-k`, `ethtool-l` | `ethtool -c/-k/-l` on the NIC (queries only) |
| `thp` | transparent hugepage `enabled` and `defrag` |
| `swappiness` | `vm.swappiness` |
| `chrony` | `chronyc tracking` |
| `hugepages-mlock` | the hugepage lines of `/proc/meminfo` and `ulimit -l` |
| `smt` | `/sys/devices/system/cpu/smt/active` and every online CPU's thread siblings |

The script sets nothing and contacts no remote host. A fact the host cannot
give is written as `unavailable (why)`, never left out; the report lists the
sections that hold one, so read that list before trusting a run.

The report also prints `warning:` lines, which never stop a run: a pinned
feeder or hot core outside `isolcpus` (or `isolcpus` not set at all), and a
hot core whose physical core has a second thread.

For a local run that is not a result, `--allow-missing-host-facts` lets
`--out` go ahead; the report then says
`host facts: NOT CAPTURED (--allow-missing-host-facts): local run, not a result for a write-up`.

### RTT to the exchange: a manual step, not part of the harness

Nothing in this repository connects to a Hyperliquid host: not the harness,
not the capture script, not a test, not a default. Latency's write-up still
wants the round-trip time from the Tokyo host (n=100), so the operator runs it
by hand, once, after the capture and before the replay, and appends the output
to the host-facts file under its own marked section:

```bash
{ echo "== begin rtt-manual"
  ping -c 100 -q api.hyperliquid.xyz
  echo "== end rtt-manual"; } >> "$facts"
```

If ICMP gets no answers, record TCP connect times in the same section instead
(`curl -o /dev/null -s -w '%{time_connect}\n' https://api.hyperliquid.xyz`,
100 times) and say which was used. Neither command has been run from this
repository's development machines. The harness never requires or reads this
section; the report only says `rtt-manual: present` or `absent`.

Set `--feeder-core` and `--hot-core` to two different physical cores, the hot
one isolated if the host has isolated cores. To replay recorded node output
instead of generated lines, add `--input <file>` (one block-info JSON line per
line; the file is cycled). Send Latency the whole `--out` directory, which
includes `host-facts.txt`.

`--out` holds `report.txt`, `host-facts.txt` and, per arm and metric, `<arm>.<metric>.hdr` (HDR
V2 binary: mergeable, readable by any HdrHistogram library and by
`LatencyHist::read_v2`) and `<arm>.<metric>.buckets.txt` (`value_ns count` per
line, ending with a comment naming arm, metric, parser mode and commit).

## Arms

Runtime arms (`--arms runtime`, the default): lines arrive on a loopback TCP
socket.

| Arm | What it is |
|---|---|
| `busy-poll` | pinned thread spinning on a non-blocking socket |
| `tokio-current-thread` | tokio `current_thread`, pinned |
| `tokio-multi-thread` | tokio multi-thread, 2 workers, task spawned, never pinned: the stock setup |
| `compio` | compio 0.19.2 on `io_uring`, pinned |
| `compio-sqpoll` | the same with a kernel submission-queue polling thread |

Detect arms (`--arms detect`): lines are appended to a file in `--tail-dir`
(default `/dev/shm`) and the reader tails it, as the node's output is read.

| Arm | When the file has nothing new |
|---|---|
| `file-busy-poll` | spin |
| `file-poll-10us` | sleep 10 µs |
| `file-poll-100us` | sleep 100 µs |
| `file-inotify` | sleep on inotify `IN_MODIFY` |

A round runs every arm once, in a random order of its own drawn from
`--seed` (printed, so a run can be repeated in the same order). The report
ends with each round's order and the kernel's load figures before and after
it (1, 5 and 15 minute load, runnable/total), so a noisy round can be told
from a quiet one. Before trusting busy-poll tails on the Tokyo host, check
that the feeder and hot cores have nothing else on them. Every arm reads
straight into the reader's buffer, the `io_uring` arms included (the reader
lends its storage for the read). If
`io_uring` cannot be set up (seccomp, container policy, old kernel) the two
compio arms print `unavailable: <reason>` and the run carries on.

compio is behind the cargo feature `compio`, on by default. If it ever fails
to build on a host: `cargo build --release -p hl-replay --no-default-features`
builds everything else and those two arms report `unavailable`.

## What is real

The report prints this table from the stage types themselves:

| Stage | Label | Today |
|---|---|---|
| reader | real | `LineBuf`: fixed buffer, `read` straight into it |
| parser | real, mode `header scan` | `hl_latency::scan_node_header` (block time and height from the fixed prefix), falling back to the strict `parse_node_line` for a line it cannot scan |
| book | stub | fixed 16-level arrays; prices derived from the block number, not from the line |
| decision | stub | one ALO order per update |
| signer | prototype | msgpack + 3x keccak-256 + libsecp256k1 + JSON body; placeholder domain, **not checked against SDK vectors** |
| send | prototype | plain TCP write to loopback: no TLS, no WebSocket framing, no NIC |

So `t2-t0` today is a header scan plus a plausible signing cost. It is not
tick-to-trade, and it is not comparable with runs made before the header
scan (the report's `parser:` line says which parser a run used). Every row is tagged `parser=strict` or
`parser=header scan` so a header scan's cost is never read as a full parse.
A header-scan parser is for stamping and routing only: a decision stage that
reads prices or sizes must take them from a strict parse.

### Complete lines only, and the strict validator

A header scan can return a result for a line cut right after a nested `}`,
where the strict parse fails. So parsers take `hl_latency::CompleteLine`, and
the harness makes one in exactly one place: `LineBuf::next_complete_line`,
after it has found the line's newline. `tests/complete_lines.rs` writes half a
line to a tailed file, checks that nothing is parsed, finishes the line and
checks that exactly one parse happens.

Every 16th line is re-parsed by the strict parser after its `t2` (off the
timed path). The report prints, per arm,
`scan-vs-strict: validated=N mismatches=M fallbacks=F`. A mismatch (scan
accepted, strict refused) fails the run. A fallback is a line the scan could
not read and the strict parser timed instead: if `fallbacks` is not 0 on real
node lines, that arm's numbers are partly the strict parser's.

To time the strict parse on every line, change `ActiveParser` to
`NodeLineParser`.

### Not measured yet

- Last-byte cache miss on real ~100 KB lines: generated lines are about 3 KB.
  Needs the Tokyo run with `--input` on real node output.
- Anything past the socket: TLS, WebSocket framing, the NIC, the network.

### Swapping a stage

Stages are type parameters of `Pipeline` (static dispatch). The active ones
are four aliases in `crates/hl-replay/src/stage.rs`:

```rust
pub type ActiveParser = HeaderScanParser;
pub type ActiveBook = StubBook;
pub type ActiveDecision = StubDecision;
pub type ActiveSigner = PrototypeSigner; // <- the one line to change
```

For the real signer: implement `Stage` (with
`LABEL = StageLabel::Real` once it passes the SDK vectors) and `Signer` for
it, give it a `throwaway()` constructor, and change the alias. The report's
`signer=` label follows from the type.

## Allocation gates

`cargo test -p hl-replay --test alloc_gate -- --nocapture`

Each stage alone and the whole busy-poll tick (line written to the source
socket, read, parsed, booked, decided, signed, written to the sink socket,
both latencies recorded) run under `hl_alloc_gate::assert_no_alloc_after_warmup`.

| Gate | Allocations per tick after warmup |
|---|---|
| reader, book, decision, signer, send, clock + histogram record | 0 (gated) |
| parser: header scan, and strict `parse_node_line`, 20-event 3,174-byte line | 0 (gated) |
| whole tick, the pipeline the harness runs | 0 (gated) |
| whole tick with the strict validator on every line | 0 (gated) |

Before the zero-allocation parser (PR #7) the strict parser made 331
allocations (42,462 bytes) per line and the parser and whole-tick gates were
`#[should_panic]` tests; both are live gates now.

## Safety

The send stage can only reach loopback. The sink address is a `LoopbackAddr`,
which is built only from an IP literal that `is_loopback()`; a host name, a
wildcard, a private or public address, or an IPv4-mapped address is refused
(`--sink-bind 8.8.8.8:443` exits 2). Every engine also checks the connected
peer address before its first write. The signing key is made up per process
from the pid and the time, never stored or printed, and controls nothing.
