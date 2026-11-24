//! Warm-up steps on this host, and the measured-window property: after
//! warm-up, a hot loop over preallocated, pre-touched memory takes 0 minor and
//! 0 major page faults and 0 allocations on its thread.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use hl_warm::{lock_memory, pin_current_thread, Arena, Backing, ThreadUsage};

/// Off Linux every warm step refuses instead of passing as a no-op: a gate
/// that reported "warm" where nothing was pinned or locked would be wrong.
#[cfg(not(target_os = "linux"))]
mod unsupported_os {
    use hl_warm::{
        lock_memory, pin_current_thread, Arena, Backing, LockOutcome, PinError, ThreadUsage,
    };

    #[test]
    fn pin_refuses_as_unsupported() {
        let e = pin_current_thread(0).unwrap_err();
        assert!(matches!(e, PinError::Unsupported(_)), "{e:?}");
        assert!(e.to_string().contains("unsupported on this OS"), "{e}");
    }

    #[test]
    fn mlockall_refuses_as_unsupported() {
        let l = lock_memory();
        assert!(
            matches!(&l, LockOutcome::Refused(m) if m.contains("unsupported on this OS")),
            "{l:?}"
        );
        assert!(hl_warm::unlock_memory().is_err());
    }

    #[test]
    fn thread_usage_is_an_error_not_zeros() {
        let e = ThreadUsage::now().unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::Unsupported, "{e}");
    }

    #[test]
    fn arena_never_claims_huge_pages() {
        // memmap2 defines MAP_HUGETLB as 0 off Linux: a "huge" map would be normal.
        let a = Arena::new(4 << 20).unwrap();
        assert_eq!(a.backing(), Backing::Normal);
        assert!(a.touched_pages() >= (4 << 20) / 4096);
    }
}

#[test]
fn arena_is_pretouched_and_reports_backing() {
    let a = Arena::new(8 << 20).unwrap();
    assert!(a.len() >= 8 << 20);
    // Whatever was obtained is recorded, never silently assumed.
    assert!(matches!(
        a.backing(),
        Backing::HugeTlb | Backing::ThpAdvised | Backing::Normal
    ));
    assert!(a.touched_pages() >= (8 << 20) / 4096 || a.backing() == Backing::HugeTlb);
}

#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "Linux only: getrusage(RUSAGE_THREAD)"
)]
fn touching_a_pretouched_arena_takes_no_faults() {
    std::thread::spawn(|| {
        let mut a = Arena::new(16 << 20).unwrap();
        let before = ThreadUsage::now().unwrap();
        let buf = a.as_mut_slice();
        for i in (0..buf.len()).step_by(4096) {
            buf[i] = buf[i].wrapping_add(1);
        }
        let d = ThreadUsage::now().unwrap().since(&before);
        assert_eq!(d.minor_faults, 0, "{d:?}");
        assert_eq!(d.major_faults, 0, "{d:?}");
    })
    .join()
    .unwrap();
}

#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "Linux only: getrusage(RUSAGE_THREAD)"
)]
fn a_fresh_mapping_does_fault_so_the_measurement_is_live() {
    // Mutation check for the test above: without pre-touch, writes fault.
    std::thread::spawn(|| {
        let mut m = memmap2::MmapOptions::new()
            .len(16 << 20)
            .map_anon()
            .unwrap();
        let before = ThreadUsage::now().unwrap();
        for i in (0..m.len()).step_by(4096) {
            m[i] = 1;
        }
        let d = ThreadUsage::now().unwrap().since(&before);
        assert!(d.minor_faults > 0, "{d:?}");
    })
    .join()
    .unwrap();
}

#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "Linux only: getrusage(RUSAGE_THREAD)"
)]
fn hot_loop_after_warmup_zero_faults_zero_allocs() {
    std::thread::spawn(|| {
        let mut arena = Arena::new(4 << 20).unwrap();
        let mut ring: Vec<u64> = Vec::with_capacity(1 << 16);
        let hot = |i: u64, arena: &mut Arena, ring: &mut Vec<u64>| {
            let b = arena.as_mut_slice();
            let idx =
                usize::try_from(i.wrapping_mul(2_654_435_761) % u64::try_from(b.len()).unwrap())
                    .unwrap();
            b[idx] ^= 0x5a;
            if ring.len() == ring.capacity() {
                ring.clear();
            }
            ring.push(i);
        };
        for i in 0..100_000 {
            hot(i, &mut arena, &mut ring); // warm-up
        }
        let before = ThreadUsage::now().unwrap();
        let rep = hl_alloc_gate::count_ticks(0, 200_000, |i| hot(i, &mut arena, &mut ring));
        let d = ThreadUsage::now().unwrap().since(&before);
        assert_eq!(rep.total.count, 0, "{rep:?}");
        assert_eq!(d.minor_faults, 0, "{d:?}");
        assert_eq!(d.major_faults, 0, "{d:?}");
        eprintln!("measured window: {d:?}");
    })
    .join()
    .unwrap();
}

#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "Linux only: sched_setaffinity, mlockall"
)]
fn pin_and_lock_report_outcome() {
    let pinned = pin_current_thread(0);
    assert!(pinned.is_ok(), "{pinned:?}");
    // mlockall may be refused by RLIMIT_MEMLOCK; the outcome must say which.
    let l = lock_memory();
    eprintln!("mlockall: {l:?}");
    let _ = format!("{l}");
}

#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "Linux only: RLIMIT_MEMLOCK, mlockall"
)]
fn finite_memlock_limit_is_refused_and_allocation_still_works() {
    #[cfg(target_os = "linux")]
    {
        let (soft, _) =
            nix::sys::resource::getrlimit(nix::sys::resource::Resource::RLIMIT_MEMLOCK).unwrap();
        let l = lock_memory();
        if soft != nix::sys::resource::RLIM_INFINITY {
            assert!(
                matches!(&l, hl_warm::LockOutcome::Refused(m) if m.contains("RLIMIT_MEMLOCK")),
                "{l:?}"
            );
        }
        let v: Vec<u8> = vec![1u8; 64 << 20];
        assert_eq!(
            v.iter().map(|&b| u64::from(b)).sum::<u64>(),
            64 << 20,
            "{l:?}"
        );
        if matches!(l, hl_warm::LockOutcome::Locked) {
            let _ = hl_warm::unlock_memory();
        }
    }
}
