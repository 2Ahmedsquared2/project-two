# Lane 3 report: block execution benchmark status

Everything below was run; each line names its command, input and n. Anything
not run is listed under "Not measured". Method: [METHOD.md](METHOD.md).

## Headline table (B1): NOT AVAILABLE YET

"Block execution time, our engine vs native node, same machine, N mainnet
blocks" cannot be filled in yet:

| row | our engine | native |
|---|---|---|
| median / p90 / p99 / worst / mean | not measured: the `hl-exec` adapter is not wired (lane 1's engine is on an unmerged branch) and no seed book at height 1232430000 has been loaded | see "Native timing" below (catch-up, not at tip) |
| state identical k/N | not measured | n/a |

**No speed comparison with native is claimed.** Even when the adapter runs,
the census below shows a book-only engine cannot reproduce most blocks, so a
speed row would not be like-for-like until the engine covers what native
executes.

## What the real data shows (our node, heights 1232430001..1232432000)

Input: AWS upload `window/our-node/replica_first2000.jsonl.gz` (sha256
`280ea6f4…09e07`, matches the uploaded SHA256SUMS), our node
(i-03e94d5085a58895c, Tokyo), `--replica-cmds-style actions-and-responses`.

**Decoder.** Strict decoding first rejected 2000/2000 lines and so found three
real differences from the published spec: a top-level `height`, `isFrontend`
on 76 of 277,766 signed actions (first 300 blocks), `user: null` on 64
rejected responses. After adding exactly those three: 2000/2000 blocks
decoded, 0 errors, every other field still rejected if unknown.

**Census** (`zcat replica_first2000.jsonl.gz | hl-block census -`, full table
in `bench/results/census-our-node-1232430001-1232432000/`):

- 2000 blocks, 3974 bundles, 1,858,991 actions, 42 action types (13 not in the
  Dwellir list); 139 round gaps, 0 regressions.
- Top types: cancelByCloid 801,540; order 763,870; cancel 187,785;
  batchModify 55,867; modify 19,739; noop 17,506; scheduleCancel 6,424;
  evmRawTx 3,259.
- order/cancel/cancelByCloid/modify/batchModify/noop = 99.318% of actions,
  but only 221/2000 blocks contain only those types, and only 106/2000 also
  have no response that depends on state a book cannot know (margin 51,519
  rejections, spot balance 3,770, open-order limit 2,982, reduce-only, OI cap,
  minimum value, reference-price band, expiry, unknown user). So a book-only
  engine can reproduce at most 106/2000 blocks (5.3%) exactly.

## Native timing (what native itself reports)

`p2-node native-times node_fast_block_times_first2000.jsonl` (native's own
`apply_duration` field per block, parsed as integer ns):

| n | median | p90 | p99 | max | mean |
|---|---|---|---|---|---|
| 2000 blocks, 1232430001..1232432000, contiguous | 13.12 ms | 29.41 ms | 53.01 ms | 109.36 ms | 15.07 ms |

Caveat: `begin_block_wall_time` is about 20 minutes after `block_time` for
these heights: the node was catching up after start, so these are catch-up
applies, not at-tip. What `apply_duration` covers is not documented; it is
reported by name, not interpreted. The lead planner's clean window
(1232480001..1232490000) is the at-tip native baseline once delivered.

## Warm-up effect (B2, synthetic input, not mainnet)

`bench/warm-vs-cold.sh bench/results/b2-warm-vs-cold-cloud-869b282 30 200 1 1`
on Reason cloud (2 vCPU Xeon, kernel 6.1, load ~3, shared): 30 fresh
processes per configuration, configuration order shuffled per round,
200 blocks each; hot path = decode + engine input + reference-book apply.

| config | event 0 p50 | events 1-9 p50 | events 0-199 p99 | measured-window minor faults per trial |
|---|---|---|---|---|
| cold | 57.8 µs | 23.8 µs | 44.5 µs | 72-74 |
| warm (pin + arena + prewarm) | 28.6 µs | 22.2 µs | 32.6 µs | 0 |
| warm without pin | 28.7 µs | 22.1 µs | 29.1 µs | 0 |
| warm without arena | 28.7 µs | 22.3 µs | 31.0 µs | 0 |
| warm without prewarm | 66.4 µs | 25.0 µs | 48.7 µs | 71-74 |

Read: on this host, prewarm is the step that earns its place (first block
about 2x faster, page faults in the window go from ~73 to 0). Pinning and the
pre-touched arena show no measurable effect on this 2-vCPU shared box; they
stay in the sequence for the Tokyo host, where they are unmeasured. `mlockall`
was not testable here (RLIMIT_MEMLOCK 8 MiB); on the bench host the run
requires it (`--require-mlock`).

Alloc/fault gate (`cargo test -p hl-warm`): over 200,000 ticks after warm-up,
a hot loop on a pre-touched arena took 0 minor and 0 major faults and 0
allocations; mutation check: the same loop on a fresh mapping faults.

## Proofs

`docs/proofs/lane3.md`: Kani 0.68.0, 3/3 harnesses pass on the warm gate
(`GateCore`), 6/6 planted bugs detected, bounds 2-3 steps, 6 operations,
times < 16. One real bug found and fixed (a step failing after the window
armed did not close the gate). Proofs say nothing about speed.

## Not measured / not done

- B1 (our engine vs native, per block): adapter to `hl-exec`, seed book load,
  per-asset decimals and HIP-3 coin mapping, response re-rendering.
- End-state check against native's translated snapshot at 1232440000.
- Any measurement on the Tokyo host from this lane (host scripts sent to AWS,
  output not yet received).
- The output-file pipeline / `order_book_server` head-to-head: dropped by the
  lead planner's re-plan.
- A WebSocket serve layer: not built.
