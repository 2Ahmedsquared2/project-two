# PR #4 safety corrections

## Independent design (before implementation inspection)

Treat conversion as a transaction: read inputs without mutation, write a unique sibling temporary file, flush it and atomically replace only the destination after success. Reject destination aliases to every input using filesystem identity, not path spelling. Keep unrelated output replacement.

Consume both input streams to EOF; merge their heights into the same block timeline and enforce the existing gap policy. Preserve zero-op status-only blocks and fail on malformed tails. Initially prefer indexing checks by (height, market), rejecting duplicate keys; preserve the existing whole-snapshot contract when choosing the final representation. Maintain historical order identities independently of active state, so only genuinely unknown pre-seed identities receive soft handling. Reject duplicate snapshot markets and enforce inner/outer market agreement.

## Acceptance rubric

Each item must pass its focused RED-to-GREEN regression: (1) all input alias forms and error preservation; (2) full statuses, gaps and malformed tails; (3) duplicate whole-check heights rejected, distinct markets within one snapshot retained; (4) repeated historical lifecycle operations fail while pre-seed soft behavior remains; (5) duplicate markets rejected; (6) inner market mismatches rejected. Each item scores 1 only when exercised. Compatibility requires all original converter tests, full local check.sh, and unchanged golden 4 blocks / 15 ops / 2 L4. No real-node or performance claim.

## Design comparison

The existing dataset permits one L4 record per (height, asset), and each check
snapshot expands missing selected markets to empty books. Therefore rejecting
duplicate whole-check heights is the smaller compatible correction; accepting
partial-market files at the same height would change the contract. Distinct
markets within one snapshot remain supported. This resolves the original
height/market-index proposal without silently dropping evidence.

Historical order identities are retained, but invalid repeated operations are
still emitted for book-check to reject rather than duplicating its reference
book. This preserves the established division of responsibility. Snapshot
validation also runs at the public converter boundary, so hand-built library
values cannot bypass parsing checks.

## TDD results

All checks ran locally on Reason cloud, without GitHub Actions.

- `cargo build -p hl-node-convert -p book-check`: exit 0.
- `python3 .local-evidence/review_regressions.py --repo . --bin-dir target/debug --out .local-evidence/red-probes`: RED exit 1, 0/7 safety assertions passed at original head `8ceaffaabefceb02367ea8fc2b2692f93e18ee70`.
- `cargo test -p hl-node-convert --test cli safety_`: RED exit 101, 4 failed / 1 positive control passed; GREEN exit 0, 5 passed. The three alias tests each exercise metadata, diffs, statuses, seed and check (15 combinations: exact path, hardlink, symlink). Error preservation includes strict findings and malformed input, with temporary-file cleanup; the positive control preserves unrelated output overwrite.
- `cargo test -p hl-node-convert --test convert safety_ --no-fail-fast`: RED exit 101, 12 failed / 1 compatibility control passed; GREEN exit 0, 13 passed. Coverage includes status suffix/checkpoint retention, gap/no-gap, seed plus only statuses, malformed/reversed status tails, duplicate whole checks, repeated removals, update-after-remove and anchor-after-remove. The 18 original converter-target tests remain passing.
- `cargo test -p hl-node-convert --test snapshot`: RED exit 101, 4 failed / 7 passed; integrated GREEN exit 0, 11 passed. Includes duplicate empty/identical/conflicting market records, optional inner market compatibility, supplied mismatches on either side and triggers, and manual library-value validation.
- `cargo test -p hl-node-convert --test cli`: exit 0, 9 passed.
- `cargo clippy -p hl-node-convert --bin hl-node-convert -- -D warnings`: exit 0.
- `python3 .local-evidence/review_regressions.py --repo . --bin-dir target/debug --out .local-evidence/green-probes`: GREEN exit 0, 7/7 passed.

The first integrated `./scripts/check.sh` stopped at 20 test-only clippy lints
(exit 101). These were corrected without suppressions or weakened assertions.
The complete rerun passed (exit 0): fmt, workspace/all-target clippy with
`-D warnings`, **212 tests passed / 0 failed** (188 original + 24 new), and
rustdoc with `-D warnings`. All golden positives and negatives passed;
node golden remains **4 blocks / 15 ops / 2 L4 comparisons**, and the corrupted
queue is rejected at block 1003 (BTC bid 1000000 ticks, recorded oid 32 versus
candidate oid 11).

Final rubric: **6/6 corrections exercised and passed**, with checkpoint identity
assessed under the clarified whole-snapshot contract. All 39 original converter
crate tests remain; the crate now has 63 tests. The workspace baseline of 188
is preserved and expanded to 212. Raw RED/GREEN logs and the portable seven-probe
script are retained in the Session's evidence attachment.

## Limits

Synthetic fixtures only: real-node differential checks, fuzz campaigns and
benchmarks were not run. No performance or live-schema compatibility claim.
No UI or deployment applies to this offline converter. Rust specialist
consultation was unavailable; local checks are not specialist sign-off.
