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
echo "all checks passed"
