# Benchmark method (lane 3)

Written and committed before any counted run. A number is reported only with
its n, host, setup and the command that produced it; raw data goes under
`bench/results/<run>/`.

## B1. Block execution: our engine vs native hl-node (the headline)

Claim it can support, and only if every condition holds: "on the same machine,
over N real mainnet blocks, our engine executes a block in X (median / p90 /
p99 / worst range / mean) and its output is identical to native's on k/N
blocks".

- **Input**: a contiguous range of real mainnet blocks from `replica_cmds`
  written with `--replica-cmds-style actions-and-responses`, starting right
  after a `periodic_abci_states/{date}/{H}.rmp` snapshot, plus the matching
  `node_raw_book_diffs` (`--batch-by-block`). Block height = H + line index;
  alignment checked by block time on every block (`ReplayBlock::new`).
- **Oracle**: a block is *reproduced* only if our per-action responses equal
  native's `resps` exactly (JSON equality) AND our book diffs equal native's
  diffs for that height exactly. Missing native diffs = not reproduced. On a
  mismatch the engine is resynced from native's diffs and the block is
  counted under its first cause. Per interval: state at H+10,000 vs native's
  snapshot (when lane 1's state export exists).
- **Timing**: monotonic clock (`std::time::Instant`), two columns per block:
  `execute_block` only, and decode+input+`execute_block`. The comparison is
  after both stamps and untimed. Samples count only for blocks reproduced in
  that pass and only with the warm gate open.
- **Warm-up**: gate steps pin (hot thread on its own core), mlock
  (`--require-mlock` on the bench host: RLIMIT_MEMLOCK must be unlimited, or
  the gate fails), load, prewarm (`--prewarm-passes`, default 2 full untimed
  passes); then `--window-blocks` blocks at the start of every counted pass
  are warm-up.
- **Passes**: at least 5 counted passes (`--passes`, default 10), each with a
  fresh engine; worst case reported as the range of per-pass maxima. Pass
  order is fixed (single system); for B1-native the two systems alternate in
  a seeded random order.
- **Native column**: only from something native itself reports per block
  (e.g. a log line with a timestamp per applied block), measured on the same
  host in the same window. If native exposes nothing per block, the column
  reads "not measurable". It is never estimated, and wall-clock of native's
  file output is not execution time.
- **Host**: the Tokyo box, `host/capture-host.sh` before and after each run
  (stored with the run), `host/tune.sh` applied and its output stored.
- **Like-for-like rule**: if k < N, or the engine returns `Unsupported` for
  any action native executed, the speed rows are labelled "NOT a like-for-like
  claim" in the report and the tweet states only coverage/progress.
- **Stored per run**: `report.txt`, `blocks.csv` (pass, height, phase,
  reproduced, actions, exec_ns, parse_exec_ns), `warm.txt` (gate report, pin,
  mlock outcome), host captures, git SHA, hl-node binary sha256/version, input
  file names and sha256.

## B2. Warm-up effect (cold vs warm first-N blocks)

Shows what each warm-up step buys on our hot path; not a comparison with
anyone.

- `bench/warm-vs-cold.sh OUT TRIALS N CORE SEED`: per round, the 5
  configurations (`cold`, `warm`, `warm-no-pin`, `warm-no-arena`,
  `warm-no-prewarm`) run in a seeded shuffled order, one fresh process per
  trial; `TRIALS` rounds.
- Hot path per block: decode `replica_cmds` line, build engine input, apply
  the block's book diffs to the Phase 1 reference book (`hl-book-ref`),
  scratch writes into the arena. A stand-in for `execute_block` until
  `hl-exec` lands.
- Prewarm uses a different synthetic block set (seed 99) than the measured
  one (seed 11), so the measured blocks are never seen before; the book is
  reset after prewarm.
- Reported per configuration: event 0, events 1-9, events 0..N-1 (p50, p90,
  p99, max over all trials' samples) plus per-trial `getrusage(RUSAGE_THREAD)`
  minor/major faults and involuntary switches over the measured window.
- `mlockall` is in the matrix only where RLIMIT_MEMLOCK is unlimited.
- Input is synthetic (`p2-node gen`), so B2 numbers say nothing about mainnet.

## Dropped (lead planner re-plan, 2026-11-20)

The output-file pipeline comparison and the `order_book_server` head-to-head
are no longer goals: reading hl-node's output cannot show we execute blocks
faster than hl-node.
