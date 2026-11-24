# kani-engine

Checks on `hl-exec` (lane 1's execution core), driven only through its public
API. Owner: Proof. Nothing here edits `crates/hl-exec`.

A passing check holds for every input inside its stated bounds, on this
engine's code. It says nothing about speed and nothing about equivalence to
the native node.

## Status (base: lane 1 `4154560`, which contains `01f058e`)

`crates/hl-book` and `crates/hl-exec` are identical between `feae97a` and
`d4c3981`. `01f058e` changed `hl-exec` (F3(b): a hint on a leg that takes no
oid now counts), so both exhaustive tests were rerun on `4154560`.

### Kani proofs (verified)

| Harness | Bounds | Result |
|---|---|---|
| `book_resize_keeps_book_sound_{b,a}` | <= 1 resting order (price 1..=2, size 1..=2, 2 users), ONE `resize` at slot 0 or 1 (live, free or never used) to size 0..=2; side fixed per instance, the two instances cover both sides | **VERIFIED** (Kani 0.68.0 built by `scripts/build-kani.sh`): book sound (not crossed, no zero size, level total = sum, no empty level) and `check_invariants`. 114k-126k steps, peak 4.2 GiB, ~55 s each; cover 1/1 ("a live order resized"). Mutant M4 makes it FAIL (`assertion failed: book_sound(&b)`). Correctness only, nothing about speed |

### Descoped Kani harnesses (tool limit, no counterexample)

Rule: <= 2 orders, one op, one kind of journal entry, 8 GiB cap
(`RLIMIT_AS`), one harness at a time, at most three attempts. Each of these
hit the cap ("CBMC ran out of memory") or a 15-min timeout. CBMC never
reported a failed property for any of them. Their minimal-bound source is in
commit `eccdd82`.

| Harness (property) | Tries | Smallest result | Stands in: exhaustive test, not proof |
|---|---|---|---|
| `book_ops_keep_book_sound` (1) | 3 + side split | OOM, 242k-1.65M steps, 7.3-7.7 GiB | `exhaustive_resync`: `check_invariants` (not crossed, no zero size, level totals) after every block, 19,096,440 cases (bounds above) |
| `book_remove_keeps_book_sound` (1, remove only, <= 1 order) | 3 | OOM, 111k-268k steps, 7.5-7.6 GiB | same |
| `book_l2_incremental_equals_recomputed` (6) | 1 | OOM, 456k steps, 7.7 GiB | same (`check_invariants` compares each level's total/count with its queue) |
| `match_conserves_quantity` (2) | 1 | OOM, 1.09M steps, 7.5 GiB | no exhaustive test; lane 1's `crates/hl-exec/tests/differential.rs` (proptest vs `hl-book-ref`, randomized, not exhaustive) |
| `match_price_time_priority` (3) | 1 | OOM, 883k steps, 7.7 GiB | same |
| `book_no_panic_any_input` (5) | 1 | OOM, 799k steps, 7.5 GiB | no exhaustive test (both exhaustive tests run every case without a panic, small domains only) |
| `book_error_leaves_book_unchanged` | 3 | OOM, 281k-427k steps, 7.5-7.8 GiB | none |
| `book_rollback_exact_{resized,removed,rested}` | 3 each | OOM at <= 1 order, 287k-455k steps, 6.9-7.8 GiB | `exhaustive_resync` (rollback through `resync(&[])`) |
| `engine_resync_empty_one_action`, `engine_resync_own_diffs_reproduces_block` (4) | 1 | TIMEOUT 15 min in symbolic execution | `exhaustive_resync`, 19,096,440 cases, 0 failures |
| `engine_index_consistent` (7) | 1 | TIMEOUT 15 min | `exhaustive_resync` (index bijection + cloid resolution after every block) |
| `engine_oid_hint_does_not_influence`, `engine_oid_hint_mismatch_counted` (F3) | 1 | TIMEOUT 15 min | `exhaustive_oid_hint`, 10,053,137 cases, 0 failures |
| `engine_no_panic_any_action` (5) | 1 | TIMEOUT 15 min | none |

Peak memory is not recorded for the timeouts: the runner kills the
process group, and killed processes are not counted in the runner's
usage figures.

### Exhaustive tests (evidence, not proof)

| Test | Bounds | Result on `4154560` |
|---|---|---|
| `tests/exhaustive_resync.rs` | snapshot 0-2 orders (side x px 1..3 x sz 1..2 x 2 users x cloid none/1/2), ONE single-leg action (order GTC/ALO/IOC x cloid x prio 0/1, cancel or modify by oid 1/2/10/11 or cloid 1/2), 2 users, STP on/off: 19,096,440 cases | **0 failures**: `resync(&[])` restores the pre-block engine (orders + queue order, L2, next oid, oid index, cloid resolution); after the block `check_invariants` holds, the oid index is a bijection, each (user, cloid) resolves to its newest resting order. M1 makes it fail (40,768 + 40 failures) |
| `tests/repro_b1_cloid_reuse.rs` | one case | passes (B1 fixed in `ff7821e`) |
| `tests/exhaustive_oid_hint.rs` (F3) | snapshot 0-1 order, block of TWO single-leg actions (order side x px {1,3} x sz {0,2} x TIF {GTC,ALO,IOC,Other} x pre-reject {none,TickSize}; modify oid 1/10; cancel), hint {None,10,11,12} per leg: 10,053,137 cases | **(a)** 0 failures: outcome (except `oid_mismatches`) and state equal the no-hint run for ANY hints, and `oid_mismatches == 0` when every hint is None or equal to the counter on a leg that takes an oid. **(b)** 0 failures, now on every leg: `oid_mismatches` = legs that take an oid with a differing hint + legs that take no oid with any hint (8,718,264 cases have such a leg). Cover: 9,218,264 cases with a counted mismatch, 112,620 with a correct hint. M2 fails (a) and (b); M3 fails (b) |

## Kani limits found

1. Stock Kani 0.68.0 crashes compiling `Book::rollback` (ICE in
   `codegen_get_discriminant`); fixed upstream in model-checking/kani#5066
   (`1bf25ce33c`, no release yet). `scripts/build-kani.sh` rebuilds 0.68.0
   with that commit only. No further toolchain work is planned.
2. A symbolic `Side` makes `Book::ladder_mut(side)` a pointer to either
   ladder `Vec`; one symbolic `rest` on an empty book then needs >8 GiB
   (50k steps). With the side fixed it verifies in 5 s (333 MiB). Harnesses
   therefore fix sides per instance (`per_sides!`).
3. Any `Vec` push or insert at a symbolic length or index pulls in Kani's
   model of `Vec` reallocation and copying, even when capacity suffices. Inside `hl-exec`
   these are `match_taker`'s diff pushes (one `match_taker` against one
   maker: 566k steps, OOM), the ladder insert of a second level, and the
   rollback journal. That is why everything except a 1-order `resize` hits
   the cap.

## Mutants (patch files under `mutants/`, applied only in a scratch worktree)

| Mutant | Check | Result on `4154560` |
|---|---|---|
| `m1-rollback-skips-resize.patch` (rollback ignores `Resized` entries) | `exhaustive_resync` | killed: 40,768 B1-class + 40 other failures |
| `m2-hint-is-oid.patch` (the hint becomes the oid, as at `506239f`) | `exhaustive_oid_hint` | killed: (a) and (b) fail |
| `m3-mismatch-never-counted.patch` | `exhaustive_oid_hint` | killed: (b) fails |
| `m4-resize-skips-level-total.patch` (resize leaves the level total stale) | Kani `book_resize_keeps_book_sound_b` | killed: `assertion failed: book_sound(&b)` |

## Run

```bash
cargo test --release -p kani-engine --test exhaustive_resync -- --nocapture   # ~25 s on 4 vCPU
cargo test -p kani-engine --test repro_b1_cloid_reuse                          # B1 regression
cargo test --release -p kani-engine --test exhaustive_oid_hint -- --nocapture  # F3, ~20 s
formal/kani-engine/scripts/build-kani.sh                                        # patched Kani 0.68.0
formal/kani-engine/scripts/run-harness.py book_resize_keeps_book_sound_b 1200 8  # 8 GiB cap, prints verdict + peak
cargo kani -p kani-engine --harness proofs::book_resize_keeps_book_sound_a --exact
```
