//! Zero allocation after warmup: each stage alone, then the whole tick.
//!
//! Counts are per thread, so every tick here runs on the test thread: the
//! test writes one line into the source socket, then polls the busy-poll
//! engine until the signed order has been handed to the sink socket.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::hint::black_box;
use std::io::Write;
use std::net::TcpStream;

use hl_alloc_gate::{assert_no_alloc_after_warmup, count_ticks};
use hl_replay::{
    generated_lines, loopback_pair, ActiveSigner, Book, BusyTick, Clock, Core, Decision, Handle,
    LatencyHist, LineBuf, NodeLineParser, Observation, OrderIntent, Parser, Pipeline, Polled,
    ReplayError, Signer, Sink, Stage, StageLabel, StubBook, StubDecision, LoopbackAddr,
};

const WARMUP: u64 = 64;
const TICKS: u64 = 2000;

fn lines() -> Vec<String> {
    generated_lines(64, 20)
}
fn pick(lines: &[String], i: u64) -> &str {
    &lines[usize::try_from(i).unwrap() % lines.len()]
}

#[test]
fn gate_reader() {
    let lines = lines();
    let mut buf = LineBuf::with_capacity(1 << 16);
    assert_no_alloc_after_warmup("reader: LineBuf push + next_line", WARMUP, TICKS, |i| {
        buf.push(pick(&lines, i).as_bytes()).unwrap();
        let l = buf.next_line().unwrap();
        black_box(l);
    });
}

#[test]
fn gate_book() {
    let obs: Vec<Observation> = lines()
        .iter()
        .map(|l| hl_latency::parse_node_line(l.trim_end(), 1).unwrap())
        .collect();
    let mut book = StubBook::default();
    assert_no_alloc_after_warmup("book: StubBook::apply", WARMUP, TICKS, |i| {
        black_box(book.apply(&obs[usize::try_from(i).unwrap() % obs.len()]));
    });
}

#[test]
fn gate_decision() {
    let mut book = StubBook::default();
    let tops: Vec<_> = lines()
        .iter()
        .map(|l| book.apply(&hl_latency::parse_node_line(l.trim_end(), 1).unwrap()))
        .collect();
    let mut d = StubDecision::default();
    assert_no_alloc_after_warmup("decision: StubDecision::decide", WARMUP, TICKS, |i| {
        black_box(d.decide(&tops[usize::try_from(i).unwrap() % tops.len()]));
    });
}

#[test]
fn gate_signer() {
    let mut s = ActiveSigner::throwaway();
    let mut out = Vec::new();
    assert_no_alloc_after_warmup("signer: prototype keccak + libsecp256k1", WARMUP, TICKS, |i| {
        out.clear();
        let intent = OrderIntent {
            asset: 0,
            is_buy: i % 2 == 0,
            px_ticks: 670_000 + i % 5000,
            sz_lots: 100 + i % 7,
            nonce: 1_753_606_210_051 + i,
        };
        s.sign(&intent, &mut out).unwrap();
        black_box(&out);
    });
}

#[test]
fn gate_send() {
    let sink = Sink::spawn(LoopbackAddr::ephemeral()).unwrap();
    let mut s = sink.connect().unwrap();
    let body = [b'x'; 400];
    assert_no_alloc_after_warmup("send: write to the loopback sink", WARMUP, TICKS, |_| {
        s.write_all(&body).unwrap();
    });
    drop(s);
    assert_eq!(sink.finish().unwrap().bytes, 400 * (WARMUP + TICKS));
}

#[test]
fn gate_histogram_record_and_clock() {
    let clock = Clock::new();
    let mut h = LatencyHist::new();
    assert_no_alloc_after_warmup("record: clock + LatencyHist::record", WARMUP, TICKS, |i| {
        let t = clock.now_ns();
        h.record(clock.now_ns() - t + i * 1_000_003);
    });
    assert_eq!(h.count(), WARMUP + TICKS);
}

/// The Value-based parser allocates on every line. This is the measured,
/// expected finding: the gate below must panic until the zero-allocation
/// parser lands, then this attribute comes off.
#[test]
#[should_panic(expected = "parser: hl_latency::parse_node_line")]
fn gate_parser_fails_today_because_parse_node_line_builds_a_value_tree() {
    let lines = lines();
    let mut p = NodeLineParser;
    assert_no_alloc_after_warmup("parser: hl_latency::parse_node_line", WARMUP, TICKS, |i| {
        black_box(p.parse(pick(&lines, i).trim_end(), 1).unwrap());
    });
}

/// A parser that does not allocate, used only to prove that the parser is the
/// sole allocating stage of the whole tick. Reads the two header fields by
/// byte scan; it is a test double, not the replacement parser.
#[derive(Default)]
struct ScanParser;
impl Stage for ScanParser {
    const NAME: &'static str = "test header scan";
    const LABEL: StageLabel = StageLabel::Stub;
}
impl Parser for ScanParser {
    fn parse(&mut self, line: &str, recv_ns: u64) -> Result<Observation, ReplayError> {
        fn after<'a>(line: &'a str, key: &str) -> &'a str {
            &line[line.find(key).unwrap() + key.len()..]
        }
        let bt = &after(line, "\"block_time\":\"")[..23];
        let bn = after(line, "\"block_number\":");
        let bn = &bn[..bn.find(|c: char| !c.is_ascii_digit()).unwrap()];
        Ok(Observation {
            source: hl_latency::Source::Node,
            block_time_ms: hl_latency::parse_naive_utc_ms(bt).unwrap(),
            block_number: Some(bn.parse().unwrap()),
            recv_ns,
        })
    }
}

struct Rig<H: Handle> {
    feed: TcpStream,
    tick: BusyTick<TcpStream, H>,
    sink: Sink,
    lines: Vec<String>,
}

fn rig<H: Handle>(handler: H) -> Rig<H> {
    let sink = Sink::spawn(LoopbackAddr::ephemeral()).unwrap();
    let (feed, source) = loopback_pair().unwrap();
    source.set_nonblocking(true).unwrap();
    let core = Core::new(handler, Clock::new(), WARMUP, 1 << 16);
    let tick = BusyTick::new(source, sink.connect().unwrap(), core);
    Rig { feed, tick, sink, lines: lines() }
}

impl<H: Handle> Rig<H> {
    /// One whole tick: a line appears on the source socket, the reader sees
    /// it (t0), it is parsed (t1), booked, decided, signed, written to the
    /// sink socket (t2) and both latencies are recorded.
    fn tick(&mut self, i: u64) {
        self.feed.write_all(pick(&self.lines, i).as_bytes()).unwrap();
        loop {
            match self.tick.poll().unwrap() {
                Polled::Lines(n) => {
                    assert_eq!(n, 1);
                    return;
                }
                Polled::Idle => std::hint::spin_loop(),
                Polled::Eof => panic!("source closed"),
            }
        }
    }
    fn finish(self) -> (u64, u64, u64) {
        let Rig { feed, tick, sink, .. } = self;
        drop(feed);
        let hists = tick.into_core().into_hists();
        (hists.e2e.count(), hists.parse.count(), sink.finish().unwrap().orders)
    }
}

/// The whole-tick gate as it will stand: reader to sink on the busy-poll
/// engine, with the real pipeline. Today it must panic, and only because of
/// the parser (the two tests below pin that down). When the zero-allocation
/// `parse_node_line` lands, delete `#[should_panic]`: the gate is then live.
#[test]
#[should_panic(expected = "whole tick, busy-poll, real pipeline")]
fn gate_whole_tick_fails_today_because_of_the_parser() {
    let mut r = rig(hl_replay::active_pipeline());
    assert_no_alloc_after_warmup("whole tick, busy-poll, real pipeline", WARMUP, TICKS, |i| {
        r.tick(i);
    });
}

/// Same tick, same engine, parser swapped for a non-allocating one: zero.
/// So reader, book, decision, signer, send and recording are clean together.
#[test]
fn gate_whole_tick_with_a_non_allocating_parser() {
    let mut r = rig(Pipeline::new(
        ScanParser,
        StubBook::default(),
        StubDecision::default(),
        ActiveSigner::throwaway(),
    ));
    assert_no_alloc_after_warmup("whole tick, busy-poll, scan parser", WARMUP, TICKS, |i| {
        r.tick(i);
    });
    assert_eq!(r.finish(), (TICKS, TICKS, WARMUP + TICKS));
}

/// Documents the measured count: the whole tick allocates exactly what
/// `hl_latency::parse_node_line` allocates on the same lines, no more.
#[test]
fn whole_tick_allocations_are_exactly_the_parsers() {
    let lines = lines();
    let mut p = NodeLineParser;
    let parser = count_ticks(WARMUP, TICKS, |i| {
        black_box(p.parse(pick(&lines, i).trim_end(), 1).unwrap());
    });
    let mut r = rig(hl_replay::active_pipeline());
    let whole = count_ticks(WARMUP, TICKS, |i| r.tick(i));
    println!(
        "allocations per tick (20-event line, {} bytes): parser alone {} ({} bytes), whole tick {} ({} bytes)",
        lines[0].len(),
        parser.total.count / TICKS,
        parser.total.bytes / TICKS,
        whole.total.count / TICKS,
        whole.total.bytes / TICKS,
    );
    assert_eq!(whole.total, parser.total, "the parser is the only allocating stage");
    assert_eq!(whole.allocating_ticks, parser.allocating_ticks);
    assert_eq!(r.finish(), (TICKS, TICKS, WARMUP + TICKS));
}
