//! Detect modes: how the reader waits between polls, and the fallbacks.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{Duration, Instant};

use hl_capture::{Detect, Waiter};

const POLL: Duration = Duration::from_millis(20);

#[test]
fn detect_names_round_trip() {
    for (name, mode) in [
        ("busy-poll", Detect::BusyPoll),
        ("poll", Detect::Poll),
        ("inotify", Detect::Inotify),
    ] {
        assert_eq!(Detect::parse(name), Some(mode));
        assert_eq!(mode.name(), name);
    }
    assert_eq!(Detect::parse("busy"), None);
    assert_eq!(Detect::parse(""), None);
}

#[test]
fn busy_poll_needs_a_second_cpu_for_the_writer_thread() {
    let (w, note) = Waiter::new(Detect::BusyPoll, POLL, 2);
    assert_eq!((w.mode(), note), (Detect::BusyPoll, None));
    let (w, note) = Waiter::new(Detect::BusyPoll, POLL, 1);
    assert_eq!(w.mode(), Detect::Poll, "one CPU: fall back");
    assert!(note.unwrap().contains("1 CPU"));
}

#[test]
fn busy_poll_does_not_sleep_and_poll_sleeps_its_interval() {
    let (mut busy, _) = Waiter::new(Detect::BusyPoll, POLL, 2);
    let start = Instant::now();
    for _ in 0..1000 {
        assert!(!busy.wait(start + Duration::from_secs(5)).rescan);
    }
    assert!(
        start.elapsed() < POLL,
        "1000 busy waits took {:?}",
        start.elapsed()
    );

    let (mut poll, note) = Waiter::new(Detect::Poll, POLL, 2);
    assert_eq!((poll.mode(), note), (Detect::Poll, None));
    let start = Instant::now();
    poll.wait(start + Duration::from_secs(5));
    assert!(start.elapsed() >= POLL, "slept {:?}", start.elapsed());
    // Never past the deadline it is given.
    let start = Instant::now();
    let (mut long, _) = Waiter::new(Detect::Poll, Duration::from_secs(30), 2);
    long.wait(start + Duration::from_millis(30));
    assert!(start.elapsed() < Duration::from_secs(5));
}

#[cfg(not(target_os = "linux"))]
#[test]
fn inotify_falls_back_to_poll_off_linux() {
    let (w, note) = Waiter::new(Detect::Inotify, POLL, 2);
    assert_eq!(w.mode(), Detect::Poll);
    assert!(note.unwrap().contains("Linux"));
}

#[cfg(target_os = "linux")]
mod inotify {
    use std::fs;
    use std::io::Write;

    use super::*;

    fn waiter() -> Waiter {
        let (w, note) = Waiter::new(Detect::Inotify, POLL, 2);
        assert_eq!((w.mode(), note), (Detect::Inotify, None));
        w
    }

    #[test]
    fn wakes_on_a_write_to_the_followed_file() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().join("hourly").join("20261119");
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("8");
        fs::write(&file, b"").unwrap();
        let mut w = waiter();
        assert_eq!(w.follow(t.path(), Some(&file)), None);

        // Nothing written: the wait runs to its deadline.
        let start = Instant::now();
        let wake = w.wait(start + Duration::from_millis(50));
        assert!(start.elapsed() >= Duration::from_millis(50));
        assert!(!wake.rescan);

        let path = file.clone();
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            let mut f = fs::OpenOptions::new().append(true).open(path).unwrap();
            f.write_all(b"x\n").unwrap();
        });
        let start = Instant::now();
        let wake = w.wait(start + Duration::from_secs(20));
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "not woken by the write"
        );
        assert!(
            !wake.rescan,
            "a write to the file is not a directory change"
        );
        writer.join().unwrap();
    }

    #[test]
    fn a_new_hour_or_a_new_date_asks_for_a_rescan() {
        let t = tempfile::tempdir().unwrap();
        let hourly = t.path().join("hourly");
        let dir = hourly.join("20261119");
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("23");
        fs::write(&file, b"").unwrap();
        let mut w = waiter();
        assert_eq!(w.follow(t.path(), Some(&file)), None);
        let far = || Instant::now() + Duration::from_secs(20);

        fs::write(dir.join("24x"), b"").unwrap();
        let start = Instant::now();
        assert!(
            w.wait(far()).rescan,
            "a file appeared beside the current one"
        );
        assert!(start.elapsed() < Duration::from_secs(10));

        // A new date directory, then the file inside it: both are seen, the
        // second because the new directory is watched as soon as it appears.
        let next = hourly.join("20261120");
        fs::create_dir(&next).unwrap();
        assert!(w.wait(far()).rescan, "a date directory appeared");
        fs::write(next.join("0"), b"").unwrap();
        let start = Instant::now();
        assert!(w.wait(far()).rescan, "a file appeared in the new directory");
        assert!(start.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn before_any_file_exists_the_root_is_watched() {
        let t = tempfile::tempdir().unwrap();
        let mut w = waiter();
        assert_eq!(w.follow(t.path(), None), None);
        fs::create_dir(t.path().join("hourly")).unwrap();
        let start = Instant::now();
        assert!(w.wait(start + Duration::from_secs(20)).rescan);
        assert!(start.elapsed() < Duration::from_secs(10));
    }

    /// A root that does not exist yet cannot be watched: that is not an
    /// error, the caller's rescan timer covers it.
    #[test]
    fn a_missing_root_keeps_inotify_live() {
        let t = tempfile::tempdir().unwrap();
        let mut w = waiter();
        assert_eq!(w.follow(&t.path().join("absent"), None), None);
        assert_eq!(w.mode(), Detect::Inotify);
    }
}
