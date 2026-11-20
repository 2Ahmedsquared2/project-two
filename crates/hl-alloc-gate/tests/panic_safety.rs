//! A panic inside a measured region must not corrupt the counter: totals stay
//! right, later measurements stay exact, and the process never aborts.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::hint::black_box;
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};

use hl_alloc_gate::{count_ticks, measure, Allocs, MAX_NESTING};

/// Unwinds without running the panic hook, so the test output stays quiet.
fn quiet_panic() -> ! {
    resume_unwind(Box::new("quiet"))
}

fn assert_counter_is_exact() {
    let (v, a) = measure(|| black_box(Box::new(7_u64)));
    assert_eq!(*v, 7);
    assert_eq!(a, Allocs { count: 1, bytes: 8 });
}

fn measure_nested(levels: u32) -> u64 {
    if levels == 0 {
        return 0;
    }
    let (inner, a) = measure(|| {
        drop(black_box(Box::new(1_u8)));
        measure_nested(levels - 1)
    });
    a.count + inner
}

#[test]
fn a_caught_nested_panic_keeps_the_outer_totals() {
    let ((), outer) = measure(|| {
        // Allocated by the outer region just before the inner one panics.
        drop(black_box(vec![0_u8; 4096]));
        let caught = catch_unwind(|| {
            measure(|| {
                drop(black_box(vec![0_u8; 100]));
                quiet_panic();
            })
        });
        assert!(caught.is_err());
        drop(black_box(vec![0_u8; 200]));
    });
    // The unwind machinery may allocate its own exception object, so this is a
    // floor, not an equality.
    assert!(outer.count >= 3, "{outer:?}");
    assert!(outer.bytes >= 4096 + 100 + 200, "{outer:?}");
    assert_counter_is_exact();
}

#[test]
fn many_caught_panics_do_not_crash_or_skew_later_measurements() {
    for _ in 0..1000 {
        let caught = catch_unwind(|| measure(|| quiet_panic()));
        assert!(caught.is_err());
    }
    assert_counter_is_exact();
}

#[test]
fn a_panicking_tick_does_not_poison_the_next_gate() {
    for _ in 0..200 {
        let caught = catch_unwind(|| {
            count_ticks(1, 3, |i| {
                if i == 2 {
                    quiet_panic();
                }
            })
        });
        assert!(caught.is_err());
    }
    let r = count_ticks(1, 10, |_| drop(black_box(Box::new(1_u32))));
    assert_eq!(r.allocating_ticks, 10);
    assert_eq!(
        r.total,
        Allocs {
            count: 10,
            bytes: 40
        }
    );
}

#[test]
#[should_panic(expected = "the original message")]
fn the_original_panic_payload_is_passed_on() {
    let _ = measure(|| panic!("the original message"));
}

#[test]
fn nesting_up_to_the_limit_is_counted() {
    // Each level makes one allocation and sees every level inside it.
    let total = measure_nested(MAX_NESTING);
    let n = u64::from(MAX_NESTING);
    assert_eq!(total, n * (n + 1) / 2);
    assert_counter_is_exact();
}

#[test]
fn nesting_past_the_limit_is_refused_without_crashing() {
    for levels in [MAX_NESTING + 1, 64, 65, 10_000] {
        let caught = catch_unwind(AssertUnwindSafe(|| measure_nested(levels)));
        let payload = caught.expect_err("nesting past the limit must be refused");
        let msg = payload
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| payload.downcast_ref::<&str>().copied())
            .unwrap_or("");
        assert!(msg.contains("nested more than"), "{levels}: {msg:?}");
        assert_counter_is_exact();
    }
}
