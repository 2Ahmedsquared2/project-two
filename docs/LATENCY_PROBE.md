# Same-block latency: our node vs the public WebSocket

Asked by the Latency Reason: the node only matters for HFT if it shows us a
block earlier than `wss://api.hyperliquid.xyz/ws`. This measures that, per block.

## Method

One probe process on the node host, one clock (`CLOCK_REALTIME`, disciplined by
chrony on the PHC; the difference of two stamps on one host does not depend on
sync with anyone else).

- **Node arm:** tail the newest hourly file under `~/hl/data/node_raw_book_diffs`
  (or `node_order_statuses`), written with `--stream-with-block-info
  --disable-output-file-buffering`. Stamp each line when `read` returns it
  (inotify `IN_MODIFY` wake-up, then read). First line seen for a block = the
  node arrival. Parsed by `hl_latency::parse_node_line`.
- **WS arm:** one connection, subscribed to `l2Book` and `trades` for the top
  coins (BTC, ETH, SOL, HYPE). Stamp each frame when the TLS read completes, before
  JSON parsing. Earliest frame carrying a block's time = the WS arrival. Parsed by
  `hl_latency::parse_ws_message`. **Drop the first frame after every
  (re)subscribe:** it is a snapshot (the first `trades` frame on 2026-11-19
  carried trades ~15 s old).
- **Stamp point (both arms):** `recv_ns` is taken immediately after the
  socket/file `read` returns, before any parsing or JSON decode. `hl-latency`
  never stamps; it only receives `recv_ns` from the capture side, so the
  capture binary owns this rule and must test it.
- **Join:** block time in ms (`hl_latency::pair`); the node line also gives the
  height. Blocks seen by one side only are counted, never dropped silently. A
  block time the node reports with two heights is *ambiguous*: counted and
  excluded from the distribution (the WS has no height to tell them apart).
- **Report:** `hl-latency observations.jsonl` prints the node lead
  (`ws_ns - node_ns`, positive = node earlier): `matched` (the sample count
  behind every percentile), `node_only`, `ws_only`, `ambiguous`,
  `key_collisions`, node-first count, min/p50/p90/p99/p99.9/max in ns, and
  `per_hour` buckets (sample count, p50, p99, max per UTC hour of block time)
  to catch drift. One-side-only counts are results, not noise. Treat p99.9 as
  weak under about 10,000 matched blocks (about 12 minutes at ~70 ms blocks).

Observation line format (one per arrival):

```json
{"source":"node","block_time_ms":1753606210051,"block_number":676607012,"recv_ns":1753606210402113000}
{"source":"public_ws","block_time_ms":1753606210051,"recv_ns":1753606210467000000}
```

## Host prep (before every run; done with/through the AWS Reason)

- ENA Express off on the interface.
- chrony on the Amazon Time Sync source (PHC where present); record
  `chronyc tracking` offset before and after each run, and at the end of
  the 24 h run (the clock can drift over a day).
- Verify PHC on the instance: `ls /dev/ptp*` and `ethtool -T <iface>`.
- Probe pinned to its own core (`taskset`), away from the node's cores.
- CPU governor `performance` if the instance allows it; record either way.
- `ethtool -S` allowance counters (`*_allowance_exceeded`) before/after each run;
  any increase invalidates the run.

## Run plan

1. 1 h smoke run, then 24 h, same host, both arms simultaneously; read the
   `per_hour` buckets for drift.
2. Repeat with the WS arm on a second region-local box only if the first host's
   CPU load could skew the WS arm (check probe CPU < 5 %).
4. Second experiment (later): our own node's output vs `hl-node`'s, same method.

## Caveats

- This measures "time to a usable byte in our process", which is what a
  strategy sees; it includes the node's file write and our read.
- `l2Book` frames may be sent only when the book changed; quiet coins are not
  evidence. Using several active coins and `trades` covers most blocks.
- Public WS behaviour (batching, throttling) is not documented in detail;
  results are a measurement of today's service, not a constant.

## Status

Done: parser, join, nearest-rank statistics, ambiguous-key exclusion,
per-hour buckets, report CLI with contract tests, proptests and a cargo-fuzz
target (`fuzz/`, `cargo +nightly fuzz run hl_latency_parsers`), tests
including real captured WS frames.

Done (capture, `crates/hl-capture`): node arm. `hl-capture node <dir> --out
obs.jsonl [--poll-us N] [--duration-s N]` follows the newest hourly file
(numeric `{date}/{hour}` order, so hour 10 is newer than 9), reads a file that
exists at start from its end and every later file from byte 0, drains the old
file before switching at rotation, and stamps `CLOCK_REALTIME` once per `read`
that returns data, before parsing; a line split across reads carries the stamp
of the read that delivered its newline. Bad lines are counted, not fatal.
Polling (default 100 us, on a pinned core) rather than inotify for now.
`WsFilter` drops the first `l2Book`/`trades` frame per coin after each
(re)connect and counts malformed frames. Tested against fake node files
(append, partial line, rotation, late-appearing file) and synthetic and
captured WS frames.

Next: the WS socket client feeding `WsFilter` (stamp at TLS read), then
the 1 h run on the host.
