//! Where a sequence number falls relative to the racer's window.
//!
//! The racer keeps `w` slots; sequence `seq` lives in slot `seq % w`. Keys
//! that have not been released yet are accepted only when they are fewer
//! than `w` ahead of the next key to release, so two unreleased keys never
//! share a slot. A key behind `next` can still be compared against the copy
//! that was released, as long as its slot has not been reused.
//!
//! All arithmetic is on `u64` and is overflow-free for every input: no
//! `next + w` is ever computed, only differences of ordered operands.

/// Position of `seq` relative to the window `[next, next + w)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Position {
    /// `next <= seq < next + w`: may be stored in `slot`.
    Ahead {
        /// `seq % w`.
        slot: usize,
    },
    /// `seq < next`: already released or skipped; compare with `slot` if it
    /// still holds `seq`.
    Behind {
        /// `seq % w`.
        slot: usize,
    },
    /// `seq >= next + w`: outside the window, dropped and counted.
    TooFar,
}

/// Classifies `seq` against the window `[next, next + w)` of `w` slots.
/// Returns `None` only for `w == 0`.
#[must_use]
pub fn classify(next: u64, seq: u64, w: u64) -> Option<Position> {
    let slot = usize::try_from(seq.checked_rem(w)?).ok()?;
    Some(if seq < next {
        Position::Behind { slot }
    } else if seq - next < w {
        Position::Ahead { slot }
    } else {
        Position::TooFar
    })
}

// Kani harnesses for `classify`, the only window arithmetic in the racer.
// Source: crates/hl-race/src/window.rs `classify` (this file), called by
// crates/hl-race/src/lib.rs `Racer::offer`, `Racer::pop`, `Racer::skip_missing`.
// Inputs: `next`, `seq`, `a`, `b` are any u64 (symbolic). The window `w` is
// concrete per harness: 1, 2, 3, 1024 (the size the alloc test uses) and
// 2^32. A symbolic 64-bit divisor did not finish within 10 minutes, so
// window sizes other than these five are NOT covered by Kani; the proptest
// in tests/race.rs covers windows 1..10 against a reference model.
// The specification is computed in u128, where nothing can overflow.
#[cfg(kani)]
mod proofs {
    use super::*;

    fn exact(w: u64) {
        let next: u64 = kani::any();
        let seq: u64 = kani::any();
        kani::cover!(next == u64::MAX && seq == u64::MAX, "top of u64");
        kani::cover!(
            u128::from(next) + u128::from(w) > u128::from(u64::MAX) && seq >= next,
            "window crosses u64::MAX"
        );
        let (n, s, ww) = (u128::from(next), u128::from(seq), u128::from(w));
        match classify(next, seq, w) {
            Some(Position::Ahead { slot }) => {
                assert!(n <= s && s < n + ww);
                assert!(slot as u128 == s % ww && (slot as u64) < w);
            }
            Some(Position::Behind { slot }) => {
                assert!(s < n);
                assert!(slot as u128 == s % ww && (slot as u64) < w);
            }
            Some(Position::TooFar) => assert!(s >= n + ww),
            None => assert!(false),
        }
    }

    fn distinct(w: u64) {
        let next: u64 = kani::any();
        let a: u64 = kani::any();
        let b: u64 = kani::any();
        kani::assume(a != b);
        kani::cover!(a.abs_diff(b) == w - 1, "keys at both ends of the window");
        if let (Some(Position::Ahead { slot: sa }), Some(Position::Ahead { slot: sb })) =
            (classify(next, a, w), classify(next, b, w))
        {
            assert!(sa != sb);
        }
    }

    macro_rules! harnesses {
        ($($e:ident $d:ident $w:expr;)*) => {$(
            #[kani::proof]
            #[kani::unwind(2)]
            fn $e() { exact($w); }
            #[kani::proof]
            #[kani::unwind(2)]
            fn $d() { distinct($w); }
        )*};
    }
    harnesses! {
        exact_w2 distinct_w2 2;
        exact_w3 distinct_w3 3;
        exact_w1024 distinct_w1024 1024;
        exact_w2p32 distinct_w2p32 1 << 32;
    }

    // w = 1 needs no distinctness harness: exact_w1 proves Ahead means
    // seq == next, so two distinct keys are never both Ahead.
    #[kani::proof]
    #[kani::unwind(2)]
    fn exact_w1() {
        exact(1);
    }

    #[kani::proof]
    #[kani::unwind(2)]
    fn zero_window_is_rejected() {
        let next: u64 = kani::any();
        let seq: u64 = kani::any();
        kani::cover!(seq == 0, "seq zero");
        assert!(classify(next, seq, 0).is_none());
    }
}
