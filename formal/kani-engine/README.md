# kani-engine

Checks on `hl-exec` (lane 1's execution core), driven only through its public
API. Owner: Proof. Nothing here edits `crates/hl-exec`.

A passing check holds for every input inside its stated bounds, on this
engine's code. It says nothing about speed and nothing about equivalence to
the native node.

## Status (run on lane 1 `4154560`)

**Coverage:** Kani proves only `Book::resize` on a book of at most 1 resting
order. Kani does NOT prove matching, quantity conservation, price-time
priority, rollback, L2, resync or no-panic; those rest on exhaustive tests
(evidence, not proof) or on nothing, as listed below.

The `hl-exec` library (`src/` and its non-dev dependencies) is identical
in `01f058e`, `1674f86` and `4154560`; those commits differ only in the
replay example, tests, tools and dev-dependencies.

`git diff --stat feae97a d4c3981 -- crates/hl-exec` is empty (there is no
`crates/hl-book`). `01f058e` changed `hl-exec` (F3(b): a hint on a leg that
takes no oid now counts), so both exhaustive tests were rerun on `4154560`.
Lane 1's `411f1bb` changes `crates/hl-exec/src` only in a doc comment
(`git diff --stat 4154560 411f1bb -- crates/hl-exec/src crates/hl-book-ref`:
`types.rs`, 3 lines), and `scripts/check.sh` reruns both exhaustive tests on
this branch's head, which contains `411f1bb`.

Every commit named here is in this branch's history. Commits made on the
Session's machine are not: publishing rewrites them, so none is cited.

### Kani proofs (verified)

| Harness | Bounds | Result |
|---|---|---|
| `book_resize_keeps_book_sound_{b,a}` | <= 1 resting order (price 1..=2, size 1..=2, 2 users), ONE `resize` at slot 0 or 1 (live or never used: `book_with` never removes) to size 0..=2; side fixed per instance, the two instances cover both sides | **VERIFIED** (Kani 0.68.0 built by `scripts/build-kani.sh`, run on `4154560`). Real content at this bound: no zero size after any resize (a size-0 resize is refused), plus `check_invariants` (links, counts, totals, ladder). With at most 1 order, "not crossed" and "level total = sum" are close to trivial. 114k-126k steps, peak 4.2 GiB, ~55 s each; cover 1/1 ("a live order resized"). Mutant M4 makes it FAIL (`assertion failed: book_sound(&b)`). Correctness only, nothing about speed |

### Descoped Kani harnesses (tool limit, no counterexample)

Rule: <= 2 orders, one op, one kind of journal entry, 8 GiB cap
(`RLIMIT_AS`), one harness at a time, at most three attempts. Each of these
hit the cap ("CBMC ran out of memory") or a 15-min timeout. CBMC never
reported a failed property for any of them. Their minimal-bound source is kept
in `src/descoped.rs`, compiled only with `cfg(kani)` and the `descoped`
feature, so the gate and a plain `cargo kani` never run it.
`cargo kani -p kani-engine --features descoped --only-codegen` builds all 62
harnesses (60 descoped + the 2 verified). It is the source of those runs
apart from comments and one cover label.

| Harness (property) | Tries | Smallest result | Stands in: exhaustive test, not proof |
|---|---|---|---|
| `book_ops_keep_book_sound` (1) | 3 + side split | OOM, 242k-1.65M steps, 7.3-7.7 GiB | `exhaustive_resync`, `check_invariants` (not crossed, no zero size, level totals) after every block: snapshot 0-2 orders (side x px {1,2,3} x sz {1,2} x 2 users x cloid none/1/2), ONE single-leg action, 2 users, STP on/off: 19,096,440 cases (`cargo test --release -p kani-engine --test exhaustive_resync`) |
| `book_remove_keeps_book_sound` (1, remove only, <= 1 order) | 3 | OOM, 111k-268k steps, 7.5-7.6 GiB | `exhaustive_resync`, `check_invariants` after every block (a cancel is a remove): snapshot 0-2 orders (side x px {1,2,3} x sz {1,2} x 2 users x cloid none/1/2), ONE single-leg action, 2 users, STP on/off: 19,096,440 cases (`cargo test --release -p kani-engine --test exhaustive_resync`) |
| `book_l2_incremental_equals_recomputed` (6) | 1 | OOM, 456k steps, 7.7 GiB | `exhaustive_resync`, `check_invariants` (each level's total and count against its queue) after every block: snapshot 0-2 orders (side x px {1,2,3} x sz {1,2} x 2 users x cloid none/1/2), ONE single-leg action, 2 users, STP on/off: 19,096,440 cases (`cargo test --release -p kani-engine --test exhaustive_resync`) |
| `match_conserves_quantity` (2) | 1 | OOM, 1.09M steps, 7.5 GiB | no exhaustive test; lane 1's `crates/hl-exec/tests/differential.rs` (proptest vs `hl-book-ref`, randomized, not exhaustive) |
| `match_price_time_priority` (3) | 1 | OOM, 883k steps, 7.7 GiB | no exhaustive test; lane 1's `crates/hl-exec/tests/differential.rs` (proptest vs `hl-book-ref`, randomized, no fixed case count) |
| `book_no_panic_any_input` (5) | 1 | OOM, 799k steps, 7.5 GiB | no exhaustive test (both exhaustive tests run every case without a panic, small domains only) |
| `book_error_leaves_book_unchanged` | 3 | OOM, 281k-427k steps, 7.5-7.8 GiB | none |
| `book_rollback_exact_{resized,removed,rested}` | 3 each | OOM at <= 1 order, 287k-455k steps, 6.9-7.8 GiB | `exhaustive_resync` (rollback through `resync(&[])`): snapshot 0-2 orders (side x px {1,2,3} x sz {1,2} x 2 users x cloid none/1/2), ONE single-leg action, 2 users, STP on/off: 19,096,440 cases (`cargo test --release -p kani-engine --test exhaustive_resync`) |
| `engine_resync_empty_one_action`, `engine_resync_own_diffs_reproduces_block` (4) | 1 | TIMEOUT 15 min in symbolic execution | `exhaustive_resync`: snapshot 0-2 orders (side x px {1,2,3} x sz {1,2} x 2 users x cloid none/1/2), ONE single-leg action, 2 users, STP on/off: 19,096,440 cases (`cargo test --release -p kani-engine --test exhaustive_resync`). Resync with native's own diffs: only lane 1's proptest |
| `engine_index_consistent` (7) | 1 | TIMEOUT 15 min | `exhaustive_resync` (index bijection + cloid resolution after every block): snapshot 0-2 orders (side x px {1,2,3} x sz {1,2} x 2 users x cloid none/1/2), ONE single-leg action, 2 users, STP on/off: 19,096,440 cases (`cargo test --release -p kani-engine --test exhaustive_resync`) |
| `engine_oid_hint_does_not_influence`, `engine_oid_hint_mismatch_counted` (F3) | 1 | TIMEOUT 15 min | `exhaustive_oid_hint`: snapshot 0-1 order (side x px {1,3} x sz {1,2} x 2 users), TWO single-leg actions (order px {1,3} x sz {0,2} x 4 TIFs x pre-reject on/off, modify, cancel), hints {None,10,11,12}: 10,053,137 cases (`cargo test --release -p kani-engine --test exhaustive_oid_hint`) |
| `engine_no_panic_any_action` (5) | 1 | TIMEOUT 15 min | none |

Peak memory is not recorded for the timeouts: the runner kills the
process group, and killed processes are not counted in the runner's
usage figures.

### Exhaustive tests (evidence, not proof)

Both are release-only: `cfg_attr(debug_assertions, ignore)` skips them in the
debug `cargo test --workspace`, and `scripts/check.sh` runs them in release
on every gate run. Each failure count below is of every failing case
(examples printed are capped; counts are not).

Results below were measured on `4154560` and again, with the same counts,
on this branch after merging `411f1bb` (with the full-count reporting).

| Test | Bounds | Result |
|---|---|---|
| `tests/exhaustive_resync.rs` | snapshot 0-2 orders (side x px 1..3 x sz 1..2 x 2 users x cloid none/1/2), ONE single-leg action (order GTC/ALO/IOC x cloid x prio 0/1, cancel or modify by oid 1/2/10/11 or cloid 1/2), 2 users, STP on/off: 19,096,440 cases | **0 failures**: `resync(&[])` restores the pre-block engine (orders + queue order, L2, next oid, oid index, cloid resolution); after the block `check_invariants` holds, the oid index is a bijection, each (user, cloid) resolves to its newest resting order. M1 makes it fail (see Mutants) |
| `tests/repro_b1_cloid_reuse.rs` | one case | passes (B1 fixed in `ff7821e`) |
| `tests/exhaustive_oid_hint.rs` (F3) | snapshot 0-1 order, block of TWO single-leg actions (order side x px {1,3} x sz {0,2} x TIF {GTC,ALO,IOC,Other} x pre-reject {none,TickSize}; modify oid 1/10; cancel), hint {None,10,11,12} per leg: 10,053,137 cases | **(a)** 0 failures: outcome (except `oid_mismatches`) and state equal the no-hint run for ANY hints, and `oid_mismatches == 0` when every hint is None or equal to the counter on a leg that takes an oid. **(b)** 0 failures, now on every leg: `oid_mismatches` = legs that take an oid with a differing hint + legs that take no oid with any hint (8,718,264 cases have such a leg). Cover: 9,218,264 cases with a counted mismatch, 112,620 with a correct hint. M2 fails (a) and (b); M3 fails (b) |

## Kani limits found

1. Stock Kani 0.68.0 crashes compiling `Book::rollback` (ICE in
   `codegen_get_discriminant`); fixed upstream in model-checking/kani#5066
   (upstream repository, no release yet). `scripts/build-kani.sh` rebuilds 0.68.0
   with that commit only. No further toolchain work is planned.
2. Measured: one `rest` on an empty book with a symbolic `Side` needs
   >8 GiB (50k steps); with the side fixed it verifies in 5 s (333 MiB).
   Harnesses therefore fix sides per instance (`per_sides!`). The cause is
   consistent with, NOT confirmed as, `Book::ladder_mut(side)` returning a
   pointer to either ladder `Vec`. Check that would confirm it: the same
   harness with a symbolic choice between two constant-side calls
   (`if kani::any() { rest(Bid) } else { rest(Ask) }`); if that stays near
   the fixed-side cost, the pointer merge is the cause.
3. Measured: one `match_taker` against one maker takes 566k steps and runs
   out of memory; everything except a 1-order `resize` hits the cap. This is
   consistent with, NOT confirmed as, Kani modelling `Vec` reallocation and
   copying for every push or insert at a symbolic length or index, even when
   capacity suffices (in `hl-exec`: `match_taker`'s diff pushes, the ladder
   insert of a second level, the rollback journal). Check that would
   confirm it (follow-up issue #75): rerun `descoped::match_*` with the
   growth path unreachable (a `#[kani::stub]` of the `Vec` growth function,
   or a fixed-capacity buffer); if steps and peak memory fall well under the
   cap, reallocation is the cause.

## Mutants (patch files under `mutants/`, applied only in a scratch worktree)

Each mutant is applied ALONE to the unmodified engine; they are independent
and not meant to stack. Each applies cleanly on its own
(`git apply --check mutants/<m>.patch` at `4154560` and `411f1bb`). M3 does
not apply on top of M2, because both edit the same lines of
`crates/hl-exec/src/engine.rs`. Counts are of every failing case, from the
command in the Check column with `--release ... -- --nocapture`; they do not
depend on the thread count (M1 gives the same counts on 2 and 4 cores).

| Mutant | Check | Result on `4154560` |
|---|---|---|
| `m1-rollback-skips-resize.patch` (rollback ignores `Resized` entries) | `cargo test --release -p kani-engine --test exhaustive_resync` | killed: 40,768 failing cases with the B1 input shape (live cloid reused) + 311,552 other failing cases, of 19,096,440 |
| `m2-hint-is-oid.patch` (the hint becomes the oid, as at `506239f`) | `cargo test --release -p kani-engine --test exhaustive_oid_hint` | killed: (a) 1,398,320 and (b) 44,960 failing cases, of 10,053,137 |
| `m3-mismatch-never-counted.patch` | `cargo test --release -p kani-engine --test exhaustive_oid_hint` | killed: (a) 0, (b) 1,414,640 failing cases, of 10,053,137 |
| `m4-resize-skips-level-total.patch` (resize leaves the level total stale) | Kani `book_resize_keeps_book_sound_b` | killed: `assertion failed: book_sound(&b)` |

## Run

```bash
cargo test --release -p kani-engine --test exhaustive_resync -- --nocapture   # ~17 s on 4 vCPU; ignored in debug
cargo test -p kani-engine --test repro_b1_cloid_reuse                          # B1 regression
cargo test --release -p kani-engine --test exhaustive_oid_hint -- --nocapture  # F3, ~11 s; ignored in debug
formal/kani-engine/scripts/build-kani.sh                                        # patched Kani 0.68.0
formal/kani-engine/scripts/run-harness.py book_resize_keeps_book_sound_b 1200 8  # 8 GiB cap, prints verdict + peak
cargo kani -p kani-engine --harness proofs::book_resize_keeps_book_sound_a --exact
cargo kani -p kani-engine --features descoped --only-codegen                   # build the descoped harnesses (do not run them uncapped)
```
