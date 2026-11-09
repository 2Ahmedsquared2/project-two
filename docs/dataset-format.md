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

## Converting the public-data capture (`hl-convert`)

The Hyperliquid Reason's Phase 1 capture (explorer `blockDetails`, anchored
public `l2Book` snapshots and `meta.json`) converts to this format with:

```bash
zcat blocks_core.ndjson.gz | cargo run --release -p hl-dataset --bin hl-convert -- \
  --meta meta.json --blocks - --l2 l2_anchored.ndjson --out phase1-v2.jsonl
```

- Every block becomes a `block` with every tx; the action is copied byte for
  byte. `ops` stay empty (see limitations).
- Every snapshot becomes an `l2_book` with `depth: 20` after the block whose
  `blockTime` equals its `time` (all 253 matched).
- The header lists every perp (asset = position in `meta.universe`) and every
  spot pair (asset = `10000 + spotMeta.universe[].index`, `sz_decimals` of the
  base token, looked up by the token's `index` field, not array position).
  Builder perps (`100000 + dex * 10000 + index`) are **not** in `meta.json` and
  so not in the header; their legs are counted as unknown-asset.
- Every order and modify leg on a known asset is checked against the full tick
  rule (decimals and 5 significant figures, `AssetMeta::order_px_ticks`) and the
  lot rule; the counts and the first 20 violations are printed as JSON. With
  `--strict` any violation or parse failure exits 1.

## Replaying raw actions (`book-check --replay`)

`book-check DATASET --replay` replays the `txs` (ignoring `ops`) through the
reference book with `book_check::replay::Replayer`, following
[ACTION_ORDERING.md](ACTION_ORDERING.md) §7, the spec of record. Each rule's
status (VERIFIED / OBSERVED / INFERENCE / UNVERIFIED) is in that spec and in the
`replay` module docs. `--no-stp` turns self-trade prevention off;
`--no-compare` replays without comparing recorded books and prints the counts.

## Limitations of public data (Phase 1)

1. **No oids for new orders.** `blockDetails` shows orders as submitted, not
   the oid each one got, and the oid counter is not exactly one per leg
   (ACTION_ORDERING.md §5.2). So `ops` cannot be derived, the replayer's oids
   are synthetic (`OidRule::OnePerAcceptedLeg`), and plain `cancel {a, o}` and
   `modify` by oid are never resolved (counted, not guessed). `cancelByCloid`
   and modify-by-cloid resolve by (user, cloid) only when the order was placed
   inside the window.
2. **No seed book.** Public `l2Book` has no order ids and cannot seed a
   per-order replay, so a replay starts empty and its L2 diverges from the
   first anchored snapshot. A real-data equality check needs a node's
   `l4Snapshots` plus order statuses (or raw book diffs).
3. **L2 only**, 20 levels per side, about every 40 blocks per coin; no fills.
4. **Builder-perp metadata** is not in `meta.json`.
5. Margin, liquidations, funding, ADL, TWAP slices, CoreWriter orders from
   `evmRawTx`, trigger activation and reduce-only clipping are not modelled;
   the checker reports the divergences they cause.

The checker itself is proven on hand-built golden vectors (`golden/`), not on
real data.
