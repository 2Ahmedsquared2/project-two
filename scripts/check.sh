#!/usr/bin/env bash
# The local PR gate. Ahmed is out of GitHub Actions minutes, so this runs on the
# developer's machine and its output goes in the PR description.
set -euo pipefail
cd "$(dirname "$0")/.."
echo "== cargo fmt --all --check";   cargo fmt --all --check
echo "== cargo clippy -D warnings";  cargo clippy --workspace --all-targets -- -D warnings
echo "== cargo test";                cargo test --workspace
echo "== cargo doc";                 RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
echo "== golden vector";             cargo run -q -p book-check -- golden/basic.jsonl
if cargo run -q -p book-check -- golden/basic-corrupt-size.jsonl 2>/dev/null; then
  echo "golden corrupt vector was NOT caught"; exit 1
fi
echo "all checks passed"
