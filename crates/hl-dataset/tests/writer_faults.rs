//! CI/CD re-review of PR #1 at fc5adf2c: the writer must not commit
//! validation state for a record whose bytes did not reach the sink, and must
//! poison itself once a record may be torn. Written before the fix.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, VecDeque};
use std::io::{self, Cursor, ErrorKind, Write};

use hl_dataset::{
    AssetMeta, Block, BookState, Header, L2Book, L2Level, ReadError, Reader, WriteError, Writer,
    FORMAT_VERSION,
};
use proptest::prelude::*;

/// What the sink does on one `write` call.
#[derive(Clone, Copy, Debug)]
enum Step {
    /// Accept every byte offered.
    Accept,
    /// Accept at most this many bytes (a short write).
    Short(usize),
    /// Fail with this kind, writing nothing.
    Fail(ErrorKind),
}

/// A test double: a byte sink that follows a script of per-call results, then
/// accepts everything once the script runs out.
#[derive(Debug, Default)]
struct ScriptedSink {
    bytes: Vec<u8>,
    writes: VecDeque<Step>,
    flushes: VecDeque<Option<ErrorKind>>,
}

impl Write for ScriptedSink {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        match self.writes.pop_front().unwrap_or(Step::Accept) {
            Step::Accept => {
                self.bytes.extend_from_slice(data);
                Ok(data.len())
            }
            Step::Short(n) => {
                let n = n.min(data.len());
                self.bytes.extend_from_slice(&data[..n]);
                Ok(n)
            }
            Step::Fail(kind) => Err(io::Error::from(kind)),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.flushes.pop_front().flatten() {
            None => Ok(()),
            Some(kind) => Err(io::Error::from(kind)),
        }
    }
}

fn btc() -> AssetMeta {
    AssetMeta::new(0, "BTC", 5, false).unwrap()
}

fn header(first_height: u64) -> Header {
    Header {
        format_version: FORMAT_VERSION,
        chain: "m".to_owned(),
        first_height,
        source: "s".to_owned(),
        assets: vec![btc()],
        extra: BTreeMap::default(),
    }
}

fn block(height: u64) -> Block {
    Block {
        height,
        time_ms: None,
        hash: None,
        proposer: None,
        txs: vec![],
        ops: vec![],
    }
}

fn l2(height: u64) -> BookState {
    BookState::L2(L2Book {
        height,
        asset: 0,
        time_ms: None,
        depth: None,
        bids: vec![L2Level {
            px_ticks: 1000,
            sz_lots: 1,
            n: 1,
            px: None,
            sz: None,
        }],
        asks: vec![],
    })
}

fn read_all(bytes: &[u8]) -> Result<usize, ReadError> {
    let r = Reader::new(Cursor::new(bytes))?;
    let mut n = 0;
    for s in r.steps() {
        s?;
        n += 1;
    }
    Ok(n)
}

/// The header goes out first; start the script after it.
fn writer(script: impl IntoIterator<Item = Step>) -> Writer<ScriptedSink> {
    let mut w = Writer::new(ScriptedSink::default(), &header(1)).unwrap();
    w.get_mut().writes = script.into_iter().collect();
    w
}

/// The exact CI/CD sequence: a block write fails with `WouldBlock` before any
/// byte, the retry must succeed (or report `Poisoned`), never `HeightGap`, and the
/// finished file must be readable.
#[test]
fn a_block_whose_write_failed_before_any_byte_can_be_retried() {
    let mut w = writer([Step::Fail(ErrorKind::WouldBlock)]);
    let err = w.write_block(&block(1)).unwrap_err();
    assert!(
        matches!(&err, WriteError::Io(e) if e.kind() == ErrorKind::WouldBlock),
        "{err:?}"
    );
    match w.write_block(&block(1)) {
        Ok(()) => {
            w.write_block(&block(2)).unwrap();
            let sink = w.finish().unwrap();
            assert_eq!(read_all(&sink.bytes).unwrap(), 2);
        }
        Err(WriteError::Poisoned(ErrorKind::WouldBlock)) => {}
        Err(other) => panic!("retry must succeed or be Poisoned, got {other:?}"),
    }
}

/// The same rule for book records: a failed book write is not recorded as
/// written, so the retry is not a `DuplicateBook`.
#[test]
fn a_book_whose_write_failed_before_any_byte_can_be_retried() {
    let mut w = writer([Step::Accept, Step::Fail(ErrorKind::WouldBlock)]);
    w.write_block(&block(1)).unwrap();
    w.write_book(&l2(1)).unwrap_err();
    w.write_book(&l2(1)).unwrap();
    let sink = w.finish().unwrap();
    assert_eq!(read_all(&sink.bytes).unwrap(), 1);
}

/// A sink that takes some bytes of a record and then fails leaves a torn
/// line: the writer reports the error once, then is poisoned for good.
#[test]
fn a_partial_write_poisons_the_writer() {
    let mut w = writer([Step::Short(3), Step::Fail(ErrorKind::BrokenPipe)]);
    let err = w.write_block(&block(1)).unwrap_err();
    assert!(
        matches!(&err, WriteError::Io(e) if e.kind() == ErrorKind::BrokenPipe),
        "{err:?}"
    );
    for retry in [
        w.write_block(&block(1)),
        w.write_block(&block(2)),
        w.write_book(&l2(1)),
    ] {
        assert!(
            matches!(retry, Err(WriteError::Poisoned(ErrorKind::BrokenPipe))),
            "{retry:?}"
        );
    }
    assert!(matches!(
        w.finish(),
        Err(WriteError::Poisoned(ErrorKind::BrokenPipe))
    ));
}

/// A sink that accepts zero bytes without an error cannot make progress: that
/// is a `WriteZero` error, and nothing was written, so it is retryable.
#[test]
fn a_zero_length_write_is_a_retryable_write_zero_error() {
    let mut w = writer([Step::Short(0)]);
    let err = w.write_block(&block(1)).unwrap_err();
    assert!(
        matches!(&err, WriteError::Io(e) if e.kind() == ErrorKind::WriteZero),
        "{err:?}"
    );
    w.write_block(&block(1)).unwrap();
    assert_eq!(read_all(&w.finish().unwrap().bytes).unwrap(), 1);
}

/// `Interrupted` is retried inside the writer, as `write_all` does.
#[test]
fn an_interrupted_write_is_retried_transparently() {
    let mut w = writer([Step::Fail(ErrorKind::Interrupted), Step::Short(2)]);
    w.write_block(&block(1)).unwrap();
    assert_eq!(read_all(&w.finish().unwrap().bytes).unwrap(), 1);
}

#[derive(Clone, Debug)]
enum Rec {
    Block(u64),
    Book(u64),
}

fn rec() -> impl Strategy<Value = Rec> {
    prop_oneof![
        4 => (0u64..5).prop_map(Rec::Block),
        2 => (0u64..5).prop_map(Rec::Book),
    ]
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        4 => Just(Step::Accept),
        2 => (0usize..40).prop_map(Step::Short),
        2 => prop_oneof![
            Just(ErrorKind::WouldBlock),
            Just(ErrorKind::Interrupted),
            Just(ErrorKind::BrokenPipe),
            Just(ErrorKind::Other),
        ]
        .prop_map(Step::Fail),
    ]
}

proptest! {
    /// For any scripted fault sequence and any retry policy, whatever bytes
    /// the sink holds after `finish()` returns Ok are a dataset the reader
    /// accepts, with every block the writer reported as written.
    #[test]
    fn after_any_faults_a_finished_dataset_is_readable(
        recs in prop::collection::vec((rec(), any::<bool>()), 0..16),
        writes in prop::collection::vec(step(), 0..24),
        flushes in prop::collection::vec(prop::option::weighted(0.2, Just(ErrorKind::Other)), 0..2),
    ) {
        let mut w = writer(writes);
        w.get_mut().flushes = flushes.into_iter().collect();
        let mut blocks = 0usize;
        'recs: for (r, retry) in &recs {
            // One try, plus one retry when asked.
            for _ in 0..=usize::from(*retry) {
                let res = match r {
                    Rec::Block(h) => w.write_block(&block(*h)),
                    Rec::Book(h) => w.write_book(&l2(*h)),
                };
                match res {
                    Ok(()) => {
                        if matches!(r, Rec::Block(_)) { blocks += 1; }
                        continue 'recs;
                    }
                    Err(WriteError::Poisoned(_)) => break 'recs,
                    Err(_) => {}
                }
            }
        }
        if let Ok(sink) = w.finish() {
            prop_assert_eq!(read_all(&sink.bytes).map_err(|e| e.to_string()), Ok(blocks));
        }
    }
}
