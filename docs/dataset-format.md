# Dataset format, version 2

A dataset is the ground truth `book-check` replays against: a contiguous run
of HyperCore blocks, each with the transactions it carried and (when the
capture knows them) the per-order effects on each asset's resting book, plus
recorded book states after some or all blocks. Version 2 follows the Hyperliquid
Reason's capture brief (Brief #1): two record families keyed by height
(blocks and book states), explicit `l2` vs `l4` book kinds so the same checker
handles public L2 snapshots now and node L4 data later, an asset table in the
header, and integer ticks/lots with the source decimal strings kept for audit.

The reader and writer live in the `hl-dataset` crate, which enforces every rule
below. This document is the contract; the crate's tests
(`crates/hl-dataset/tests/format.rs`) pin the exact bytes. Version 1 (single
asset, L4 only) is not read; no v1 data was ever captured.

## Encoding

JSON Lines: one JSON object per line, `\n`-terminated, UTF-8. Every line has a
`kind`. Prices are integer **ticks** and sizes integer **lots**, scaled per
asset (below). Floats never appear. Decimal strings may appear beside integers
(`px`, `sz`) for audit; the reader checks that they convert exactly to the
integer beside them.

## Ticks and lots

For an asset with `sz_decimals` (Hyperliquid `szDecimals`):

- `size = sz_lots / 10^sz_decimals`
- `price = px_ticks / 10^max_price_decimals`, where `max_price_decimals` is
  `6 - sz_decimals` for perps and `8 - sz_decimals` for spot.

Source: Hyperliquid docs, "Tick and lot size"
(<https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/tick-and-lot-size>).
Every valid price is a whole number of ticks on this grid; the additional
5-significant-figure limit on prices only removes grid points, so it does not
change the scaling. Conversion is exact string arithmetic
(`hl_dataset::decimal`): trailing zeros are accepted, any other excess digit,
sign, exponent or whitespace is rejected.

## Records

### `header` (first line, exactly once)

```json
{"kind":"header","format_version":2,"chain":"mainnet","first_height":100,"source":"explorer blockDetails + info l2Book, 2026-11-09","assets":[{"asset_index":0,"name":"BTC","sz_decimals":5,"is_spot":false,"max_price_decimals":1}]}
```

| field            | type   | meaning                                                                 |
|------------------|--------|-------------------------------------------------------------------------|
| `format_version` | u32    | Must be `2`. Checked before anything else.                              |
| `chain`          | string | `mainnet`, `testnet`, or a label for synthetic data.                    |
| `first_height`   | u64    | Height of the first `block` record.                                     |
| `source`         | string | Free-text provenance. Not parsed.                                       |
| `assets`         | array  | Every asset whose ops or books appear. `asset_index` unique.            |

`AssetMeta`: `asset_index` (u32; perp = position in `meta.universe`, spot =
10000 + spot index), `name`, `sz_decimals`, `is_spot`, `max_price_decimals`
(must match the rule above; the reader rejects a mismatch).

Any other header key is preserved verbatim (`Header::extra`).

### `block`

```json
{"kind":"block","height":100,"time_ms":1700000000000,"hash":"0x…","proposer":"0x…","txs":[{"time_ms":1700000000000,"user":"0x…","hash":"0x…","error":null,"action":{"type":"order","orders":[…],"grouping":"na"}}],"ops":[{"asset":0,"op":"insert","oid":1,"side":"bid","px":1000,"sz":5}]}
```

| field      | type           | meaning                                                         |
|------------|----------------|-----------------------------------------------------------------|
| `height`   | u64            | `first_height` for the first block, then `+1` each.             |
| `time_ms`  | u64, optional  | Block time (explorer `blockTime`).                              |
| `hash`     | string, opt.   | Block hash.                                                     |
| `proposer` | string, opt.   | Proposer address.                                               |
| `txs`      | array, default `[]` | Transactions in file order (explorer order = execution order). |
| `ops`      | array, default `[]` | Per-order book effects in application order, across assets.   |

A tx is `{time_ms?, user, hash, error, action}`: `error` is `null` or the
error string; `action` is the signed action **byte for byte** as the source
returned it (key order, spacing and number strings preserved: it is what was
signed and later phases hash it).

An op is `{"asset": u32, ...BookOp}`:

| `op`            | fields                         | meaning                                                                 |
|-----------------|--------------------------------|-------------------------------------------------------------------------|
| `insert`        | `oid`, `side`, `px`, `sz`      | Order comes to rest at the back of its level. `sz > 0`. Must not cross. |
| `insert_before` | `before`, `order:{oid,side,px,sz}` | Order rests directly ahead of `before` (same side and price). hl-node raw diff `insertBefore`. |
| `cancel`        | `oid`                          | A resting order leaves (cancel, full fill, expiry).                     |
| `resize`        | `oid`, `sz`                    | Remaining size changes in place, queue position kept. `sz > 0`.         |

`px` is ticks and `sz` lots. `side` is `"bid"` or `"ask"`. Matching is not in
the ops: a crossing taker shows up as the `resize`/`cancel` it caused on makers
and an `insert` for any rested remainder.

### `l4_book`

```json
{"kind":"l4_book","height":100,"asset":0,"bids":[{"px_ticks":1000,"orders":[{"oid":1,"sz_lots":5,"user":"0x…"}]}],"asks":[]}
```

Full per-order book of one asset after block `height`. Levels best first
(bids strictly descending, asks strictly ascending), orders front of queue
first. Optional: `time_ms`, level `px` (audit string), order `user`.

### `l2_book`

```json
{"kind":"l2_book","height":100,"asset":0,"time_ms":1700000000500,"depth":20,"bids":[{"px_ticks":1000,"sz_lots":2,"n":1,"px":"100.0","sz":"0.00002"}],"asks":[]}
```

Aggregated book (public `l2Book`: `px`, `sz`, `n`) of one asset anchored to
block `height`. `depth` is how many levels per side the source returns at most
(20 for public `l2Book`); the checker then compares only the candidate's top
`depth` levels. Without `depth` the record is the whole book.

## Structural rules (enforced on read and write)

1. The first line is a `header` with `format_version` 2 and valid, unique assets.
2. Block heights are contiguous from `first_height`.
3. A book record carries the height of the block it directly follows (books
   come after their block and before the next one).
4. At most one book record per asset per block.
5. Every op and book names an asset declared in the header.
6. Levels are strictly best-first; audit strings match their integers.
7. Blank lines are ignored; a malformed line or unknown `kind` is reported
   with its 1-based line number.

## What the checker can and cannot do with this today

- `ops` are replayed; `txs` are counted, **not replayed**. Turning raw actions
  into book effects needs the oid-assignment rule (unverified; the Hyperliquid
  Reason is testing it) and the matching engine (`Book::place` exists for
  GTC/IOC/ALO, but self-trade prevention, triggers, margin and liquidations
  are not modelled).
- A replay starts from an empty book. A capture that starts mid-history needs
  a seed `l4_book` (with order ids) for the first height; public `l2Book`
  cannot seed a replay because it has no order ids.
- An `l2_book` anchored by `time_ms` to a height is only as good as that
  anchoring (unverified: whether `l2Book.time` equals a `blockTime`).
