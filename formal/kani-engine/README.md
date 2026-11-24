# kani-engine

Checks on `hl-exec` (lane 1's execution core), driven only through its public
API. Owner: Proof. Nothing here edits `crates/hl-exec`.

A passing check holds for every input inside its stated bounds, on this
engine's code. It says nothing about speed and nothing about equivalence to
the native node.

## Status (base: lane 1 `feae97a`)

| Check | Kind | Bounds | Status |
|---|---|---|---|
| `tests/exhaustive_resync.rs` | exhaustive enumeration (evidence, not a Kani proof) | snapshot 0-2 orders (side x px 1..3 x sz 1..2 x 2 users x cloid none/1/2), ONE single-leg action (order GTC/ALO/IOC x cloid x prio 0/1, cancel or modify by oid 1/2/10/11 or cloid 1/2), 2 users, STP on/off: 19,096,440 cases | **0 failures**: `resync(&[])` restores the pre-block engine (orders + queue order, L2, next oid, oid index, cloid resolution) and after the block the oid index is a bijection and each (user, cloid) resolves to its newest resting order. On `56514ca` 379,904 cases failed, all bug B1, fixed by lane 1 in `ff7821e` |
| `tests/repro_b1_cloid_reuse.rs` | regression test | one case | failed on `56514ca` (B1), passes on `feae97a` |
| `src/proofs.rs` (12 Kani harnesses) | Kani | on each harness | written and compiled; **not yet verified**: see "Kani blocker" |

### B1: reused live cloid broke rollback (hl-exec bug, fixed in `ff7821e`)

A user's new order with a cloid that is already live for the same user
overwrites the `(user, cloid)` index entry (`engine.rs` `index_rest`). Then
`resync(&[])` un-rests the new order and deletes the entry (`engine.rs`
`resync`, `Restored::Unrested` arm), so the older order still rests but
`cancelByCloid` can no longer reach it. Lane 1's fix follows native: the
cloid index is per book, points at the newest order, and every change to it
is journalled, so `resync` restores an overwritten entry.

## Kani blocker

1. Stock Kani 0.68.0 crashes compiling `Book::rollback` (ICE in
   `codegen_get_discriminant`: `u64::try_from(niche_start).unwrap()`, an enum
   niche above `u64::MAX`). Fixed upstream in model-checking/kani#5066
   (commit `1bf25ce33c`), which is in no release yet. `scripts/build-kani.sh`
   rebuilds 0.68.0 with only that commit.
2. With that build, `rollback` compiles. But undoing a single `Rested`
   journal entry, all values concrete, grows the CBMC program from 21k to
   369k steps and CBMC runs out of memory: 11 GB, killed on a 16 GB host.
   An empty rollback verifies in 5 s. So every harness that reaches
   `rollback` (`resync`, `book_rollback_exact`) is blocked on this tool
   limit, not on a property failure.
3. A harness that never reaches `rollback` (`match_price_time_priority`:
   <=3 resting orders, one taker) also runs out of memory: 3.0M steps after
   31 min of symbolic execution on a 31 GB host. The Book's Vec-based arena
   is too large for CBMC at these bounds as encoded. The next step is
   smaller harnesses (one kind of journal entry, <=2 orders).

## Mutants (patch files under `mutants/`, applied only in a scratch worktree)

| Mutant | Check | Result |
|---|---|---|
| `m1-rollback-skips-resize.patch` (rollback ignores `Resized` entries) | `exhaustive_resync` | killed on `56514ca` and on `feae97a` (failures outside B1 appear, test fails) |

## Run

```bash
cargo test --release -p kani-engine --test exhaustive_resync -- --nocapture   # ~25 s on 4 vCPU
cargo test -p kani-engine --test repro_b1_cloid_reuse                          # B1 regression
formal/kani-engine/scripts/build-kani.sh                                        # patched Kani 0.68.0
cargo kani -p kani-engine --harness <name>
```
