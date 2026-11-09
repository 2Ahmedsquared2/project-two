# Dataset format, version 1

A dataset is the ground truth `book-check` replays against: one asset's
contiguous run of blocks, each with the per-order effects it had on the resting
book, plus the full resting book after each block (or after every k-th block).

The reader and writer live in the `hl-dataset` crate, which enforces every rule
below. This document is the contract; the crate's tests
(`crates/hl-dataset/tests/format.rs`) pin the exact bytes.

## Encoding

JSON Lines: one JSON object per line, `\n`-terminated, UTF-8. Every number is
an integer. Prices are **ticks** and sizes are **lots**; the header says how to
turn them back into decimals. Floats never appear.

This is deliberately not a hot-path format. It is written by a capture tool
(Rust or Python), read by a checker, and read by people with `head` and `jq`.

## Records

Every line has a `kind`.

### `header` (first line, exactly once)

```json
{"kind":"header","format_version":1,"chain":"mainnet","asset":"BTC","px_decimals":1,"sz_decimals":5,"first_height":100,"source":"hl-node 0.x, replica_cmds + l4 snapshots, 2026-11-09"}
```

| field            | type   | meaning                                                                 |
|------------------|--------|-------------------------------------------------------------------------|
| `format_version` | u32    | Must be `1`. The reader rejects anything else.                          |
| `chain`          | string | `mainnet`, `testnet`, or a label for synthetic data.                    |
| `asset`          | string | Coin whose book this is. One dataset = one asset.                       |
| `px_decimals`    | u32    | `price = px / 10^px_decimals`.                                          |
| `sz_decimals`    | u32    | `size = sz / 10^sz_decimals`.                                           |
| `first_height`   | u64    | Height of the first `block` record.                                     |
| `source`         | string | Free-text provenance: tool, hl-node binary and hash, dates. Not parsed. |

Any other key is preserved verbatim by the reader and writer (`Header::extra`),
so a capture tool can record provenance (`hl_node_binary_sha256`, capture
timestamps, snapshot heights) before this crate knows the field.

### `block`

```json
{"kind":"block","height":100,"time_ms":1700000000000,"ops":[{"op":"insert","oid":1,"side":"bid","px":1000,"sz":5},{"op":"resize","oid":7,"sz":2},{"op":"cancel","oid":3}]}
```

| field     | type         | meaning                                                      |
|-----------|--------------|--------------------------------------------------------------|
| `height`  | u64          | Must be `first_height` for the first block, then `+1` each.  |
| `time_ms` | u64, optional| Block time, milliseconds since the Unix epoch.               |
| `ops`     | array        | Per-order effects in application order. May be empty.        |

Ops (`hl_book_ref::BookOp`):

| `op`      | fields                        | meaning                                                                 |
|-----------|-------------------------------|-------------------------------------------------------------------------|
| `insert`  | `oid`, `side`, `px`, `sz`     | An order comes to rest at the back of its level. `sz > 0`. Must not cross. |
| `cancel`  | `oid`                         | A resting order leaves (cancel, full fill, expiry).                     |
| `resize`  | `oid`, `sz`                   | Remaining size changes in place, queue position kept. `sz > 0`.         |

`side` is `"bid"` or `"ask"`.

Matching is not in the format: a crossing taker shows up as the `resize` and
`cancel` ops it caused on resting makers, and as an `insert` if a remainder
rests. This lets the checker run before the matching rules are pinned down.

### `book`

```json
{"kind":"book","height":100,"bids":[{"px":1000,"orders":[[1,5],[3,2]]}],"asks":[{"px":1010,"orders":[[2,3]]}]}
```

The full resting book after the block with the same `height`. It must directly
follow that block; at most one per block; a block without one is allowed (the
checker then does not compare after it).

| field   | type  | meaning                                                      |
|---------|-------|--------------------------------------------------------------|
| `bids`  | array | Levels, highest price first.                                 |
| `asks`  | array | Levels, lowest price first.                                  |

A level is `{"px": ticks, "orders": [[oid, sz], ...]}` with orders in queue
order, front first. Levels are never empty and sizes never zero.

## Structural rules (enforced on read and write)

1. The first line is a `header` with `format_version` 1.
2. Block heights are contiguous from `first_height`.
3. A `book` carries the height of the block it directly follows.
4. At most one `book` per block.
5. Blank lines are ignored on read; a malformed line is reported with its
   1-based line number.

## Open questions for the capture side

- Which of these can hl-node emit directly? If only L2 (price, total size,
  order count) is available for some heights, a future version will allow a
  `book` without `orders`; the checker would then compare size and count only.
- Whether a size reduction keeps queue position on Hyperliquid (this format
  assumes it does; `resize` keeps position).
- Order ids for orders that were already resting when capture started.
