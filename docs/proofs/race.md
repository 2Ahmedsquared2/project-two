# hl-race: evidence

Everything here was run on a Reason cloud Linux box (4 cores), 2026-11-24, branch
`reason/p2-lane-2-hl-race-multi-1ebd83bc`. Proofs show correctness properties only,
never speed. `<skill-dir>` is the installed formal-verify skill (GPL-3.0 tooling; not
copied into this repo).

## 1. Dedupe key, from data

- **hl-node by-block files: key = `block_number`.** Read from 50,218 lines of Hyperliquid's
  public bucket `node_fills_by_block/hourly/20261123/23` (heights 1232362950..1232413167):
  0 repeated heights, 0 decreasing, 0 jumps > 1, 13,745 lines with no events (so empty blocks
  are written and the height is dense), 0 lines containing a backslash; line size p50 1,892 B,
  p99 31,025 B, max 336,185 B. The first 30 lines are `golden/race/node_fills_by_block_head.jsonl`
  (public data, not our node) and pinned by `tests/race.rs`.
- **Stream-style output (no `--batch-by-block`) must not be raced on height**: the repo's
  synthetic `golden/node` fixture has block 1001 on two lines (test
  `stream_mode_lines_can_repeat_a_height_so_only_by_block_streams_are_raced`).
- **Compare bytes after `local_time`**: `local_time` is the writing node's clock, so two
  nodes' copies of one block differ there; `node_line_body` drops it. Test
  `two_nodes_same_block_differing_only_in_local_time_do_not_alarm` shows whole-line
  comparison would raise a false alarm. Two real nodes' lines for one block: **UNVERIFIED**
  (only one node's output exists so far).
- **Public WebSocket** (2 parallel connections to api.hyperliquid.xyz, 90 s, 1,895 frames,
  this cloud box): for `l2Book` and `trades` every key seen on both connections had
  byte-identical frames (928 keys, 0 differing); `l2Book` `time` strictly increases per
  coin per connection. **`bbo` is not a shared stream**: 29 `bbo` keys were seen on one
  connection only (each connection sampled different block times). WS keys are block times
  (sparse), so WS sources cannot drive this racer's dense ordered release; they race at block
  granularity via `hl-latency`'s pairing. Not built in this PR (scope cut by the lead planner).

## 2. Tests (`cargo test -p hl-race`)

19 tests: 3 histogram unit tests, 15 racer tests (known answers for win/duplicate/lag,
divergence before and after release, gap held then filled by a slower source, skip counts a
gap, window/payload limits, u64 top: `u64::MAX` is released exactly once then the racer is
exhausted (no further pop/skip; later copies are duplicate, divergent or late; a skipped
`u64::MAX` is never held), JSON report, 3 simulated sources round-robin,
real-data key, the TLC trace below reproduced in Rust), a proptest (2,000 cases, windows
1..9, 3 sources, random loss/dup/reorder/5% divergent bytes) against an unbounded reference
model, and `tests/alloc.rs`: `offer`+`pop` allocate 0 times over 50,000 ticks after
16 warmup ticks (hl-alloc-gate).

## 3. TLA+ ordering model (`formal/race/Race.tla`)

Tool: TLC 2.19 (tla2tools v1.7.4), openjdk version "21.0.13.1" 2026-11-17 LTS.

Properties (invariant = always true; liveness = eventually happens; weak fairness = a step
that stays enabled is eventually taken):
- `InOrderOnce`: released keys are exactly 0,1,2,... each once, no key skipped.
- `ReleasedIsWinner`: the released value is the first copy the racer accepted.
- `DivergenceAlarmed`: every copy offered while its key is still held or kept, with bytes
  different from the first copy, raised the alarm.
- `NoFalseAlarm`: an alarm only for a real difference.
- `HeldNotLost`: an accepted, unreleased key is never evicted.
- `AllReleased` (liveness): if one source loses nothing, every key is released.

Model: sources walk keys in increasing order (as by-block files are, section 1) and interleave
arbitrarily, so copies arrive in any cross-source order and delay; configured loss,
duplicates and divergent bytes per source/key. Matrix: 2-3 sources, window 1/2/3, 3 or 5 keys.

```
bash <skill-dir>/scripts/tlc-matrix.sh $PWD/formal/race/Race.matrix
SUMMARY 6 of 6 runs passed, 2 of 2 reach checks passed, 2075 distinct states in total
python3 <skill-dir>/scripts/mutate.py $PWD/formal/race/Race.mutations
SUMMARY 10 of 10 mutations detected
```

**Found by TLC:** the first model let the driver offer again before draining `pop()`.
`AllReleased` failed (window 1: key 1 held, the complete source sends key 2, it is dropped as
`TooFar`, nobody resends it, the stream stalls forever). The code's driver already drains
after every offer; this is now a documented contract on `Racer::offer`, modelled in `Send`,
reproduced in Rust (`tlc_trace_offering_before_draining_drops_the_complete_sources_key`) and
kept as a mutation that must fail `AllReleased`.

Not covered: `skip_missing` (caller policy; the model has no give-up, so liveness assumes one
complete source), sources that reorder their own stream, payload limits, statistics, sizes
beyond the matrix, a copy arriving after its slot was reused (counted `Late`, not compared:
the alarm covers copies within the window only).

## 4. Kani (`crates/hl-race/src/window.rs`, Kani 0.68.0)

`classify` (the only window arithmetic) for **all u64** `next`, `seq`: result equals the
exact u128 window predicate and slot = seq mod w < w (`exact_w*`); two distinct keys inside
the window never share a slot (`distinct_w*`); w = 0 is rejected (`zero_window_is_rejected`).
Window sizes 1, 2, 3, 1024 and 2^32 only: a symbolic 64-bit divisor did not finish in 10 min.

```
bash <skill-dir>/scripts/kani-check.sh $PWD/crates/hl-race
SUMMARY 10 of 10 harnesses passed
python3 <skill-dir>/scripts/mutate.py $PWD/crates/hl-race/src/window.mutations
SUMMARY 11 of 11 mutations detected
```

Not covered: other window sizes (proptest covers 1..9), `Racer` beyond `classify`.

## 5. Not done / UNVERIFIED

- No live measurement: the 10-minute 4-connection WS race and the Tokyo runner were cut from
  scope by the lead planner (re-plan 2026-11-24); no live connectors are in this PR.
- Gossip source: not attempted; no handshake capture has been delivered.
- Properties were not independently reviewed.
