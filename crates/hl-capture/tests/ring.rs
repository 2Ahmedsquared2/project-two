//! The SPSC ring between the read/stamp thread and the disk writer.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use hl_capture::{ring, ObservationSink};
use hl_latency::{read_observations, Observation, Source};

fn obs(h: u64) -> Observation {
    Observation {
        source: Source::Node,
        block_time_ms: 1_795_075_200_000 + h,
        block_number: Some(h),
        recv_ns: 10 * h,
        file_date: None,
        file_hour: None,
        line_offset: None,
    }
}

fn heights(jsonl: &[u8]) -> Vec<u64> {
    read_observations(jsonl)
        .unwrap()
        .iter()
        .map(|o| o.block_number.unwrap())
        .collect()
}

#[test]
fn records_are_written_in_push_order_as_observation_lines() {
    let (mut sink, mut source) = ring(16);
    for h in 0..5 {
        sink.observe(obs(h));
    }
    let mut out = Vec::new();
    assert_eq!(source.write_available(&mut out).unwrap(), 5);
    assert_eq!(
        read_observations(&out[..]).unwrap(),
        (0..5).map(obs).collect::<Vec<_>>()
    );
    assert_eq!(
        source.write_available(&mut out).unwrap(),
        0,
        "ring is empty"
    );
    assert_eq!(sink.dropped(), 0);
}

#[test]
fn a_full_ring_drops_the_newest_record_and_counts_it() {
    let (mut sink, mut source) = ring(4);
    for h in 0..10 {
        sink.observe(obs(h)); // returns at once, full or not
    }
    assert_eq!(sink.dropped(), 6);
    let mut out = Vec::new();
    assert_eq!(source.write_available(&mut out).unwrap(), 4);
    assert_eq!(heights(&out), vec![0, 1, 2, 3], "the oldest four survive");
    // Room again once the writer has caught up.
    sink.observe(obs(10));
    assert_eq!(source.pop(), Some(obs(10)));
    assert_eq!(sink.dropped(), 6);
}

#[test]
fn the_writer_thread_writes_everything_pushed_before_the_sink_is_dropped() {
    let t = tempfile::tempdir().unwrap();
    let path = t.path().join("obs.jsonl");
    let file = std::fs::File::create(&path).unwrap();
    let (mut sink, source) = ring(4096);
    let writer = std::thread::spawn(move || {
        source.run(std::io::BufWriter::new(file), Duration::from_micros(200))
    });
    for h in 0..3000 {
        sink.observe(obs(h));
        if h % 500 == 0 {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    assert_eq!(sink.dropped(), 0);
    assert!(!sink.abandoned());
    drop(sink);
    assert_eq!(writer.join().unwrap().unwrap(), 3000);
    assert_eq!(
        heights(&std::fs::read(&path).unwrap()),
        (0..3000).collect::<Vec<_>>()
    );
}

/// A writer that fails ends its thread; the reader can see that and stop.
#[test]
fn the_sink_sees_a_writer_that_died() {
    struct Broken;
    impl std::io::Write for Broken {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("disk gone"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let (mut sink, source) = ring(8);
    let writer = std::thread::spawn(move || source.run(Broken, Duration::from_micros(200)));
    sink.observe(obs(1));
    assert!(writer.join().unwrap().is_err());
    assert!(sink.abandoned());
}
