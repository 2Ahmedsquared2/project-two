#!/usr/bin/env bash
# Rebuild Kani 0.68.0 with exactly one upstream fix, model-checking/kani#5066
# (commit 1bf25ce33c: ICE reading the discriminant of an enum whose niche
# starts above u64::MAX), and install its kani-compiler into the 0.68.0
# bundle. Drop this once a Kani release contains #5066.
set -euo pipefail
BASE_TAG=kani-0.68.0
FIX=1bf25ce33c
SRC=${TMPDIR:-/tmp}/kani-src
cargo install --locked kani-verifier --version 0.68.0
cargo kani setup
rm -rf "$SRC"
git clone -q --depth 50 --branch "$BASE_TAG" https://github.com/model-checking/kani.git "$SRC"
cd "$SRC"
git submodule update --init --depth 1 charon
curl -fsSL "https://github.com/model-checking/kani/commit/$FIX.patch" | git apply
CARGO_TARGET_DIR="$SRC/target" cargo build --release -p kani-compiler
BIN="$HOME/.kani/kani-0.68.0/bin"
[ -f "$BIN/kani-compiler.stock" ] || cp "$BIN/kani-compiler" "$BIN/kani-compiler.stock"
cp "$SRC/target/release/kani-compiler" "$BIN/kani-compiler"
echo "installed kani-compiler $BASE_TAG + $FIX"
