//! Converts `hl-node` output into an `hl-dataset` (format v2) that
//! `book-check` can verify.
//!
//! Inputs, all written by a non-validator `hl-visor run-non-validator` with
//! `--write-raw-book-diffs --write-order-statuses --stream-with-block-info`
//! (or `--batch-by-block`):
//!
//! - `node_raw_book_diffs`: per-order book changes (`new`, `update`,
//!   `remove`) in application order ([`node::BookDiff`]);
//! - `node_order_statuses`: order status events, used for the side of a
//!   `new` diff that does not carry one ([`node::OrderStatus`]);
//! - optional `l4Snapshots` files from the node's local info server
//!   (`fileSnapshot`, `includeUsers: true`, `includeHeightInOutput: true`):
//!   one seeds the book, others become recorded `l4_book` states that
//!   `book-check` compares against ([`snapshot::L4Snapshot`]).
//!
//! Every diff becomes one [`hl_dataset::AssetOp`] in its block; the
//! converter translates and does not judge: an op the reference book cannot
//! apply is left for `book-check` to report at its exact block. The schema,
//! its primary sources and the open questions are in
//! `docs/dataset-format.md`, section "Converting hl-node output".

pub mod convert;
pub mod node;
pub mod snapshot;
