//! A parser only ever sees a complete line, and a header-scan parser is
//! checked against the strict parse off the timed path.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::cell::Cell;
use std::io::Write;
use std::rc::Rc;
use std::time::Duration;

use hl_replay::{
    node_line, render_report, ActiveSigner, BusyTick, Clock, CompleteLine, Core, HostInfo,
    LoopbackAddr, Observation, Parser, ParserMode, Pipeline, Polled, ReplayError, RunMeta, Sink,
    Stage, StageLabel, Step, StubBook, StubDecision,
};

/// A header-scan test double: reads the two header fields and nothing else,
/// so (like any scan) it accepts a line that was cut after a nested `}`.
/// It records every line it is handed.
#[derive(Clone, Default)]
struct CountingScan {
    calls: Rc<Cell<u64>>,
    saw_partial: Rc<Cell<bool>>,
}
impl Stage for CountingScan {
    const NAME: &'static str = "test header scan";
    const LABEL: StageLabel = StageLabel::Stub;
}
impl Parser for CountingScan {
    const MODE: ParserMode = ParserMode::HeaderScan;
    fn parse(&mut self, line: CompleteLine<'_>, recv_ns: u64) -> Result<Observation, ReplayError> {
        let line = std::str::from_utf8(line.as_bytes()).unwrap();
        self.calls.set(self.calls.get() + 1);
        if !line.ends_with('}') {
            self.saw_partial.set(true);
        }
        let after = |key: &str| &line[line.find(key).unwrap() + key.len()..];
        let bn = after("\"block_number\":");
        let bn = &bn[..bn.find(|c: char| !c.is_ascii_digit()).unwrap()];
        Ok(Observation {
            source: hl_latency::Source::Node,
            block_time_ms: hl_latency::parse_naive_utc_ms(&after("\"block_time\":\"")[..23])
                .unwrap(),
            block_number: Some(bn.parse().unwrap()),
            recv_ns,
        })
    }
}

type ScanPipeline = Pipeline<CountingScan, StubBook, StubDecision, ActiveSigner>;

fn pipeline(scan: &CountingScan) -> ScanPipeline {
    Pipeline::new(
        scan.clone(),
        StubBook::default(),
        StubDecision::default(),
        ActiveSigner::throwaway(),
    )
}

#[test]
fn a_partial_line_in_the_file_is_never_handed_to_the_parser() {
    let dir = std::env::temp_dir().join(format!("hl-replay-partial-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("node.jsonl");
    let mut writer = std::fs::File::create(&path).unwrap();
    let scan = CountingScan::default();
    let sink = Sink::spawn(LoopbackAddr::ephemeral()).unwrap();
    let core = Core::new(pipeline(&scan), Clock::new(), 0, 1 << 16);
    let mut tick = BusyTick::tail(
        std::fs::File::open(&path).unwrap(),
        sink.connect().unwrap(),
        core,
    );

    let line = node_line(7, 20);
    // Cut right after the first event object: the bytes so far end in a
    // nested '}' and carry the whole header, which is exactly what a scan
    // would accept if it were shown them.
    let cut = line.find("}}}").unwrap() + 3;
    writer.write_all(&line.as_bytes()[..cut]).unwrap();
    writer.flush().unwrap();
    for _ in 0..50 {
        assert_eq!(tick.poll().unwrap(), Polled::Idle);
    }
    assert_eq!(scan.calls.get(), 0, "a partial line reached the parser");

    writer.write_all(&line.as_bytes()[cut..]).unwrap();
    writer.flush().unwrap();
    let mut got = Polled::Idle;
    for _ in 0..50 {
        got = tick.poll().unwrap();
        if got != Polled::Idle {
            break;
        }
    }
    assert_eq!(got, Polled::Lines(1));
    assert_eq!(
        scan.calls.get(),
        1,
        "exactly one parse, of the finished line"
    );
    assert!(!scan.saw_partial.get());
    for _ in 0..10 {
        assert_eq!(tick.poll().unwrap(), Polled::Idle);
    }
    assert_eq!(scan.calls.get(), 1);
    drop(tick);
    assert_eq!(sink.finish().unwrap().orders, 1);
}

fn run_core(validate_every: u64, lines: &[&str]) -> hl_replay::Hists {
    let scan = CountingScan::default();
    let mut core =
        Core::new(pipeline(&scan), Clock::new(), 0, 1 << 16).with_strict_validation(validate_every);
    for l in lines {
        core.push(l.as_bytes()).unwrap();
        core.push(b"\n").unwrap();
        assert_eq!(core.step(1).unwrap(), Step::Order);
        core.sent();
    }
    core.into_hists()
}

#[test]
fn the_strict_validator_counts_lines_the_scan_took_and_strict_refuses() {
    let good = node_line(1, 3);
    let good = good.trim_end();
    // A whole "line" (newline-terminated) whose content was cut after a
    // nested '}': the scan says Some, the strict parse says Err.
    let cut = &good[..good.find("}}}").unwrap() + 3];
    assert!(hl_latency::parse_node_line(cut, 0).is_err());

    let h = run_core(1, &[good, cut, good]);
    assert_eq!(h.validated, 3);
    assert_eq!(h.scan_strict_mismatches, 1);
    assert_eq!(
        h.e2e.count(),
        3,
        "validation is beside the timing, not instead of it"
    );

    let sampled = run_core(2, &[good, cut, good, cut]);
    assert_eq!(sampled.validated, 2, "every 2nd line: lines 0 and 2");
    assert_eq!(sampled.scan_strict_mismatches, 0);

    let off = run_core(0, &[good, cut]);
    assert_eq!((off.validated, off.scan_strict_mismatches), (0, 0));
}

#[test]
fn the_report_says_what_was_checked_and_what_is_still_unmeasured() {
    let meta = RunMeta {
        commit: "0".repeat(40),
        host: HostInfo::default(),
        feeder_core: None,
        hot_core: None,
        rounds: 1,
        measured_per_round: 1,
        warmup_per_round: 0,
        interval: Duration::from_micros(1),
        input: "none".to_owned(),
    };
    let text = render_report(&meta, &[]);
    assert!(
        text.contains("scan-vs-strict: every 16th line is re-parsed strictly after t2; any mismatch fails the run"),
        "{text}"
    );
    // So an older Value-parser run is never compared with a later one
    // unknowingly: the header names the parser and how complete lines are
    // enforced.
    assert!(
        text.contains("parser: hl_latency::scan_node_header, falling back to strict parse_node_line (header scan)"),
        "{text}"
    );
    assert!(
        text.contains(
            "complete lines: enforced by type (hl_latency::CompleteLine, made only where the reader has seen the newline)"
        ),
        "{text}"
    );
    assert!(
        text.contains("unmeasured: last-byte cache miss on real ~100 KB lines"),
        "{text}"
    );
}
