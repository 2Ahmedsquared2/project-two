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
- **Join:** block time in ms (`hl_latency::pair`); the node line also gives the
  height. Blocks seen by one side only and key collisions are counted, never
  dropped silently.
- **Report:** `hl-latency observations.jsonl` prints the node lead
  (`ws_ns - node_ns`, positive = node earlier): matched, node-only, ws-only,
  node-first count, min/p50/p90/p99/p99.9/max in ns.

Observation line format (one per arrival):

```json
{"source":"node","block_time_ms":1753606210051,"block_number":676607012,"recv_ns":1753606210402113000}
{"source":"public_ws","block_time_ms":1753606210051,"recv_ns":1753606210467000000}
```

## Run plan

1. 1 h smoke run, then 24 h, same host, both arms simultaneously.
2. Repeat with the WS arm on a second region-local box only if the first host's
   CPU load could skew the WS arm (check probe CPU < 5 %).
3. Capture `ethtool -S` allowance counters before/after; non-zero invalidates.
4. Second experiment (later): our own node's output vs `hl-node`'s, same method.

## Caveats

- This measures "time to a usable byte in our process", which is what a
  strategy sees; it includes the node's file write and our read.
- `l2Book` frames may be sent only when the book changed; quiet coins are not
  evidence. Using several active coins and `trades` covers most blocks.
- Public WS behaviour (batching, throttling) is not documented in detail;
  results are a measurement of today's service, not a constant.

## Status

Done (this PR): parser, join, nearest-rank statistics, report CLI, tests
including real captured WS frames. Next: the capture binary (inotify tailer +
WS client) behind the ingest trait once the runtime A/B lands.
