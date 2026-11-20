#!/usr/bin/env bash
# The local PR gate. Ahmed is out of GitHub Actions minutes, so this runs on the
# developer's machine and its output goes in the PR description.
set -euo pipefail
cd "$(dirname "$0")/.."
echo "== cargo fmt --all --check";   cargo fmt --all --check
echo "== cargo clippy -D warnings";  cargo clippy --workspace --all-targets -- -D warnings
echo "== cargo test";                cargo test --workspace
echo "== cargo doc";                 RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
echo "== golden vectors"
cargo run -q -p book-check -- golden/basic.jsonl
cargo run -q -p book-check -- golden/l2.jsonl
for bad in golden/basic-corrupt-*.jsonl golden/l2-corrupt-*.jsonl; do
  if cargo run -q -p book-check -- "$bad"; then
    echo "golden corrupt vector $bad was NOT caught"; exit 1
  fi
done
cargo run -q -p book-check -- golden/replay.jsonl --replay > /dev/null
for bad in "golden/replay-corrupt-modify-queue.jsonl --replay" "golden/replay.jsonl --replay --no-stp"; do
  # shellcheck disable=SC2086
  if cargo run -q -p book-check -- $bad > /dev/null 2>&1; then
    echo "golden replay vector '$bad' was NOT caught"; exit 1
  fi
done
# Node-output converter: golden node files -> dataset -> book-check.
node_out="$(mktemp -d)"
trap 'rm -rf "$node_out"' EXIT
node_convert() {
  cargo run -q -p hl-node-convert -- --meta golden/node/meta.json \
    --diffs golden/node/node_raw_book_diffs --statuses golden/node/node_order_statuses \
    --seed golden/node/seed.json --coin BTC --coin PURR/USDC --check "$1" --out "$2" > /dev/null
}
node_convert golden/node/check.json "$node_out/ok.jsonl"
cargo run -q -p book-check -- "$node_out/ok.jsonl"
node_convert golden/node/check-corrupt-queue.json "$node_out/bad.jsonl"
if cargo run -q -p book-check -- "$node_out/bad.jsonl"; then
  echo "golden node vector check-corrupt-queue.json was NOT caught"; exit 1
fi
echo "all checks passed"
