# Project Two

An open-source, non-validator Hyperliquid node in Rust that replays HyperCore
blocks and serves L2/L4 order books, built to be faster than the official
`hl-node`. Before any speed work there has to be something to be correct
against; this repository starts there.

## Phase 1: ground truth and the checker

| crate          | what it is                                                                                     |
|----------------|------------------------------------------------------------------------------------------------|
| `hl-book-ref`  | The simplest correct reference order book: a `BTreeMap` per side keyed by integer price ticks, each level a FIFO queue, plus an order-id index. Insert, insert-before, cancel, resize, L2 top-N aggregation, and replay matching (GTC/IOC/ALO, price-time priority). Integer ticks and lots only; floats are banned by `clippy.toml`. Every faster design is diffed against this. |
| `hl-dataset`   | A versioned JSON Lines format (v2) for a recorded dataset: blocks with raw transactions and per-order effects, plus `l2_book` / `l4_book` states per asset, integer ticks/lots with audit decimal strings. Validating reader and writer, and `hl-convert` from the public-data capture. Spec: [docs/dataset-format.md](docs/dataset-format.md). |
| `book-check`   | Replays a dataset through one candidate book per asset and reports the first divergence: block, coin, side, price level and the field that differs (size, order count, queue position). Exits non-zero on divergence. `--replay` replays raw signed actions per [docs/ACTION_ORDERING.md](docs/ACTION_ORDERING.md) (the spec of record). |

`golden/` holds hand-built datasets (L4 and L2) with known expected books and
deliberately corrupted copies that the checker must catch
([golden/README.md](golden/README.md)).

Out of scope for this phase, on purpose: latency work, pre-allocation,
array-vs-tree book layouts, networking, replaying raw actions (needs the
unverified oid-assignment rule).

## Build and check

Rust toolchain is pinned in `rust-toolchain.toml`. The whole local gate:

```bash
scripts/check.sh
```

which runs `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
`cargo test`, `cargo doc` and the golden vectors. There is no CI on GitHub for
now (no Actions minutes); every PR description carries the output of this
script, with the failing test shown before the code that made it pass.

```bash
cargo run -p book-check -- path/to/dataset.jsonl [--candidate ref]
cargo run -p book-check -- path/to/dataset.jsonl --replay [--no-stp] [--no-compare]
```

Exit codes: 0 clean, 1 divergence or an op the candidate rejected, 2 usage or
unreadable dataset.

## Working rules

- Test-driven: the failing test is written and run before the code.
- `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test` clean.
- `#![forbid(unsafe_code)]` workspace-wide for now.
- Read-only against Hyperliquid: no keys, no signing, no orders.
- Facts about the protocol are cited or marked unverified.

## Licence

Dual-licensed under MIT or Apache-2.0, at your option. See
[LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE).
