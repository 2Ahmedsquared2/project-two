# Feeding NautilusTrader from our node: interface outline

Goal: a strategy in NautilusTrader gets Hyperliquid market data from our own
node instead of the public API, earlier and with per-order (L4) detail. Orders
still go through the official execution client to the public API. Product
choices here are recommendations for Ahmed via the CTO, not decisions.

Sources: team notes `hyperliquid-best-practices-nautilus` (Nautilus `develop`
@ d69f215: the existing Hyperliquid adapter has overridable `base_url_http` /
`base_url_ws` and supports L2_MBP books only, as full 20-level `l2Book`
snapshots with no sequence numbers).

## Two stages

### Stage 1: drop-in (no Nautilus code)

Our node serves a **Hyperliquid-compatible WebSocket and `/info`** on the
host. Point the existing adapter's `base_url_ws` / `base_url_http` at it.

- Channels, same JSON as the public API: `l2Book`, `trades`, `bbo`,
  `allMids`; `/info`: `meta`, `spotMeta`, `l2Book`, `clearinghouseState`
  (subset `hl-node --serve-info` already answers).
- Gain: whatever lead `docs/LATENCY_PROBE.md` measures, minus our serving
  cost. No L4.
- Risk: the adapter may call endpoints we do not serve (seen: `recentTrades`
  is not served by a self-hosted `/info`; the adapter treats the 422 as empty).

### Stage 2: our own data client (L3_MBO)

A separate Nautilus data client (out of tree first; name must not start with
`nautilus`) for venue `HYPERLIQUID`, paired with the official execution client.

Wire contract from the node, versioned (`v1`), one stream per subscribed coin:

| message | fields | notes |
|---|---|---|
| `snapshot` | `seq`, `height`, `coin`, full L4 book (oid, side, px_ticks, sz_lots, owner?) | sent on subscribe and on request; `seq` = sequence of the last event it includes |
| `delta` | `seq`, `height`, `tx_index`, `coin`, `action` (add/modify/cancel/fill), `oid`, `side`, `px_ticks`, `sz_lots` | one per order effect, in execution order |
| `end_of_block` | `seq`, `height`, `block_time_ms`, `node_recv_ns` | Nautilus `F_LAST` goes on the last delta before it |
| `trade` | `seq`, `height`, `coin`, px, sz, aggressor side, maker/taker oids | |
| `heartbeat` | `next_seq` | lets the client detect a silent gap |
| `gap` | `from_seq`, `to_seq` | node could not deliver; client must resnapshot |

Rules: `seq` strictly increasing per stream; prices and sizes integer ticks
and lots with per-asset scale in a `meta` message (Nautilus `Price`/`Quantity`
built from the integer plus precision, no float parsing); client asks for a
snapshot after any gap; `ts_event` = block time, `ts_init` = node receive time.

Transport: WebSocket on the host first (simple, debuggable); same-host shared
memory or binary TCP only if the probe shows the WS hop matters.

Mapping to Nautilus: `snapshot` -> `OrderBookDeltas` with `clear` + adds;
`delta` -> `OrderBookDelta` (`BookAction::Add/Update/Delete`, `order_id` = oid);
`end_of_block` -> `F_LAST` flag; `trade` -> `TradeTick`.

Testing: recorded fixtures from our node, Nautilus's mock-server pattern, no
network in tests, and a replay test that rebuilds the book from deltas and
compares with `snapshot` at every `end_of_block` (same idea as `book-check`).

## Open for Ahmed (via CTO)

- Stage 1 first (cheap, proves the lead) or straight to Stage 2.
- Licence of the adapter: NautilusTrader is LGPL-3.0; an upstream contribution
  ships under it (decided acceptable, `ahmed-decisions`).
