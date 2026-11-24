//! Kani harnesses for `hl-exec`. Only harnesses that verify within an
//! 8 GiB cap stay here; the ones descoped as a Kani/CBMC memory or time
//! limit (no counterexample) are listed in `README.md` with the exhaustive
//! test that stands in for each. Their minimal-bound source is in commit
//! `eccdd82` of this branch. Run one: `scripts/run-harness.py <name>`
//! (8 GiB cap, peak memory) or
//! `cargo kani -p kani-engine --harness proofs::<name> --exact`.

use hl_exec::book::{Book, NewOrder};
use hl_exec::{Oid, Px, Side, Sz, User};

use crate::model::{book_sound, ASSET, USERS};

// ---------------------------------------------------------------- domains

/// Generates one harness per side combination: `body(Side::X, ...)`.
/// Sides are fixed per instance because a symbolic side makes
/// `Book::ladder_mut(side)` a pointer to either ladder `Vec`, and CBMC runs
/// out of memory on that even for one `rest` (see README, "Kani limits").
/// The instances of a property together cover every side combination.
macro_rules! per_sides {
    ($unwind:literal, $body:ident { $($name:ident: [$($s:ident),*]),* $(,)? }) => {
        $(
            #[kani::proof]
            #[kani::unwind($unwind)]
            fn $name() {
                $body($(Side::$s),*)
            }
        )*
    };
}

/// Price in 1..=2.
/// Price in 1..=2.
fn px() -> Px {
    if kani::any() {
        Px(1)
    } else {
        Px(2)
    }
}

/// Size in 1..=2.
fn sz() -> Sz {
    if kani::any() {
        Sz(1)
    } else {
        Sz(2)
    }
}

fn usr() -> User {
    if kani::any() {
        USERS[0]
    } else {
        USERS[1]
    }
}

/// Slot 0 or 1 (may name a free or never-used slot).
fn slot() -> u32 {
    u32::from(kani::any::<bool>())
}

fn new_order(oid: u64, side: Side) -> NewOrder {
    NewOrder {
        oid: Oid(oid),
        user: usr(),
        cloid: None,
        side,
        px: px(),
        sz: sz(),
        t_ms: 0,
        prio: 0,
    }
}

/// A book (preallocated for 4 orders/journal entries, 2 levels per side, so
/// no Vec grows inside a harness) holding up to `sides.len()` resting
/// orders: order i (oid i+1) on `sides[i]`, the first k of them for any
/// k, each a successful `rest` of an arbitrary order on that side.
fn book_with(sides: &[Side]) -> Book {
    let mut b = Book::with_capacity(ASSET, 4, 2);
    let k: usize = kani::any();
    kani::assume(k <= sides.len());
    for (i, &sd) in sides.iter().enumerate() {
        if i < k {
            let r = b.rest(new_order(i as u64 + 1, sd), None);
            kani::assume(r.is_ok());
        }
    }
    b
}

// ------------------------------------------------------ book-level proofs

/// (1), resize only. Bounds: <= 1 resting order on side `s0`, then ONE
/// `resize` at slot 0 or 1 (live, free or never used) to size 0..=2;
/// price 1..=2, size 1..=2, 2 users; the side is fixed per instance and the
/// two instances cover both. The book stays sound (not crossed, no zero
/// size, each level's total = sum of its orders, every listed level
/// non-empty) and `check_invariants` holds. Correctness only, nothing about
/// speed. Kills mutant M4 (`mutants/m4-resize-skips-level-total.patch`).
fn resize_sound_body(s0: Side) {
    let mut b = book_with(&[s0]);
    let before = b.node(0).map(|n| n.sz);
    let s: u64 = kani::any();
    kani::assume(s <= 2);
    let ok = b.resize(slot(), Sz(s)).is_ok();
    assert!(book_sound(&b));
    assert!(b.check_invariants());
    kani::cover!(
        ok && before.is_some() && b.node(0).map(|n| n.sz) != before,
        "a live order resized"
    );
}
per_sides!(
    3,
    resize_sound_body {
        book_resize_keeps_book_sound_b: [Bid],
        book_resize_keeps_book_sound_a: [Ask],
    }
);
