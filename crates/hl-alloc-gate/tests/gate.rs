//! The gate must fail when a tick allocates after warmup, and only then.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::hint::black_box;

use hl_alloc_gate::{assert_no_alloc_after_warmup, count_ticks, measure, Allocs};

#[test]
fn measure_counts_a_box_and_its_bytes() {
    let (v, a) = measure(|| black_box(Box::new(7_u64)));
    assert_eq!(*v, 7);
    assert_eq!(a, Allocs { count: 1, bytes: 8 });
}

#[test]
fn measure_sees_nothing_for_stack_only_work() {
    let (sum, a) = measure(|| (0..1000_u64).fold(0, |acc, x| black_box(acc + x)));
    assert_eq!(sum, 499_500);
    assert_eq!(a, Allocs::default());
}

#[test]
fn a_growing_vec_counts_each_reallocation() {
    let mut v: Vec<u8> = Vec::with_capacity(1);
    let ((), a) = measure(|| {
        for i in 0..64 {
            v.push(i);
        }
    });
    assert!(a.count >= 1, "growth must be seen: {a:?}");
    assert!(a.bytes >= 64, "{a:?}");
}

#[test]
fn allocations_on_another_thread_are_not_charged_to_this_one() {
    let ((), a) = measure(|| {
        std::thread::scope(|s| {
            s.spawn(|| drop(black_box(vec![0_u8; 4096])));
        });
    });
    // Spawning itself allocates on this thread; the 4096-byte buffer does not.
    assert!(a.bytes < 4096, "{a:?}");
}

#[test]
fn a_reused_buffer_passes_after_warmup() {
    let mut buf: Vec<u8> = Vec::new();
    let r = count_ticks(4, 1000, |i| {
        buf.clear();
        // Same size every tick: the first warmup tick allocates, later ones do not.
        buf.extend_from_slice(&[u8::try_from(i % 251).unwrap(); 512]);
        black_box(&buf);
    });
    assert_eq!(r.ticks, 1000);
    assert_eq!(r.allocating_ticks, 0);
    assert_eq!(r.total, Allocs::default());
    assert_eq!(r.first, None);
    assert_eq!(r.worst, None);
}

#[test]
fn a_fresh_vec_per_tick_is_reported_with_first_and_worst_tick() {
    let r = count_ticks(10, 100, |i| {
        // Tick 10 is measured tick 0; measured tick 42 allocates three times.
        let n = if i == 52 { 3 } else { 1 };
        for _ in 0..n {
            drop(black_box(vec![0_u8; 32]));
        }
    });
    assert_eq!(r.ticks, 100);
    assert_eq!(r.allocating_ticks, 100);
    assert_eq!(
        r.total,
        Allocs {
            count: 102,
            bytes: 102 * 32
        }
    );
    assert_eq!(
        r.first,
        Some((
            0,
            Allocs {
                count: 1,
                bytes: 32
            }
        ))
    );
    assert_eq!(
        r.worst,
        Some((
            42,
            Allocs {
                count: 3,
                bytes: 96
            }
        ))
    );
}

#[test]
fn one_late_allocation_is_caught_and_located() {
    let mut buf: Vec<u8> = Vec::with_capacity(64);
    let r = count_ticks(8, 500, |i| {
        buf.clear();
        // Measured tick 300 is larger than anything seen in warmup: the Vec grows.
        let len = if i == 308 { 4096 } else { 64 };
        buf.resize(len, 1);
        black_box(&buf);
    });
    assert_eq!(r.allocating_ticks, 1);
    assert_eq!(r.first.map(|(i, _)| i), Some(300));
    assert_eq!(r.total.count, 1);
}

#[test]
fn warmup_allocations_are_not_counted() {
    let mut keep: Vec<Box<u64>> = Vec::with_capacity(16);
    let r = count_ticks(16, 50, |i| {
        if i < 16 {
            keep.push(Box::new(i));
        }
    });
    assert_eq!(r.total, Allocs::default());
    assert_eq!(keep.len(), 16);
}

#[test]
fn the_tick_index_runs_across_warmup_and_measured_ticks() {
    let mut seen = [0_u64; 7];
    let mut n = 0;
    count_ticks(3, 4, |i| {
        seen[n] = i;
        n += 1;
    });
    assert_eq!(seen, [0, 1, 2, 3, 4, 5, 6]);
}

#[test]
fn the_gate_passes_for_a_zero_allocation_tick() {
    let mut acc = 0_u64;
    assert_no_alloc_after_warmup("integer sum", 2, 1000, |i| acc = black_box(acc ^ i));
}

#[test]
#[should_panic(expected = "leaky tick: 100 of 100 ticks allocated after 5 warmup ticks")]
fn the_gate_fails_when_every_tick_allocates() {
    assert_no_alloc_after_warmup("leaky tick", 5, 100, |i| {
        drop(black_box(i.to_string()));
    });
}

#[test]
#[should_panic(expected = "first at tick 7 (1 allocations, 4096 bytes)")]
fn the_gate_failure_names_the_first_offending_tick() {
    let mut buf: Vec<u8> = Vec::with_capacity(8);
    assert_no_alloc_after_warmup("grows once", 1, 20, |i| {
        buf.clear();
        buf.resize(if i == 8 { 4096 } else { 8 }, 0);
    });
}

#[test]
#[should_panic(expected = "zero ticks measured")]
fn the_gate_refuses_to_pass_vacuously() {
    assert_no_alloc_after_warmup("nothing", 10, 0, |_| {});
}

/// A real hot-path function already in the tree: the timestamp parse on the
/// success path. This is the pattern later gates follow.
#[test]
fn timestamp_parse_does_not_allocate_on_success() {
    let stamps = [
        "2025-07-27T08:50:10.051000",
        "2025-07-27T08:50:10",
        "2024-02-29T23:59:59.999999999",
    ];
    assert_no_alloc_after_warmup("parse_naive_utc_ms", 3, 3000, |i| {
        let s = stamps[usize::try_from(i).unwrap() % stamps.len()];
        black_box(hl_latency::parse_naive_utc_ms(black_box(s)).unwrap());
    });
}
