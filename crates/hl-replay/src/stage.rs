//! The pluggable stages and the pipeline that chains them.
//!
//! Every stage is a trait with one implementation chosen at compile time:
//! the pipeline is generic, so calls are statically dispatched and inlined.

use hl_latency::{CompleteLine, Observation};

use crate::signer::PrototypeSigner;
use crate::{Clock, ReplayError};

/// How much a stage's numbers may be trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageLabel {
    /// The production code path.
    Real,
    /// A placeholder doing a fixed small amount of work.
    Stub,
    /// Plausible work of the right shape, not yet checked against the
    /// protocol: its cost is indicative only.
    Prototype,
}

impl std::fmt::Display for StageLabel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            StageLabel::Real => "real",
            StageLabel::Stub => "stub",
            StageLabel::Prototype => "prototype",
        })
    }
}

/// One row of the report's stage table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StageInfo {
    /// Position in the pipeline: `reader`, `parser`, `book`, `decision`,
    /// `signer` or `send`.
    pub stage: &'static str,
    /// What implements it.
    pub name: &'static str,
    /// How real it is.
    pub label: StageLabel,
}

/// What every stage says about itself.
pub trait Stage {
    /// What implements the stage, for the report.
    const NAME: &'static str;
    /// How real it is.
    const LABEL: StageLabel;
}

/// How much of a line a parser reads. Printed beside every histogram row, so
/// the cost of a header scan is never read as the cost of a full parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParserMode {
    /// Parses and validates the whole line.
    Strict,
    /// Reads only the header fields, for stamping and routing. A decision
    /// stage must not take prices or sizes from it.
    HeaderScan,
}

impl std::fmt::Display for ParserMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ParserMode::Strict => "strict",
            ParserMode::HeaderScan => "header scan",
        })
    }
}

/// Turns one node output line into an [`Observation`].
pub trait Parser: Stage {
    /// How much of the line this parser reads.
    const MODE: ParserMode = ParserMode::Strict;

    /// Parses `line`, stamped `recv_ns`. Taking a [`CompleteLine`] means a
    /// parser cannot be handed a partial read.
    fn parse(&mut self, line: CompleteLine<'_>, recv_ns: u64) -> Result<Observation, ReplayError>;

    /// Lines a header-scan parser could not scan and parsed strictly
    /// instead. Their timings are the strict parser's.
    fn fallbacks(&self) -> u64 {
        0
    }
}

/// The best prices after a book update, in integer ticks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TopOfBook {
    /// Best bid, ticks.
    pub bid_ticks: u64,
    /// Best ask, ticks.
    pub ask_ticks: u64,
    /// The block the update came from.
    pub seq: u64,
    /// That block's time, Unix milliseconds.
    pub time_ms: u64,
}

/// Applies an observation to book state.
pub trait Book: Stage {
    /// Applies `obs` and returns the new top of book.
    fn apply(&mut self, obs: &Observation) -> TopOfBook;
}

/// An order the decision stage wants sent, in integer ticks and lots.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OrderIntent {
    /// Asset index.
    pub asset: u32,
    /// Buy or sell.
    pub is_buy: bool,
    /// Limit price, in ticks of 0.1.
    pub px_ticks: u64,
    /// Size, in lots of 0.00001.
    pub sz_lots: u64,
    /// Nonce (milliseconds, strictly increasing).
    pub nonce: u64,
}

/// Decides whether a book update produces an order.
pub trait Decision: Stage {
    /// The order to send for this update, if any.
    fn decide(&mut self, top: &TopOfBook) -> Option<OrderIntent>;
}

/// Turns an order into the signed bytes the send stage writes.
pub trait Signer: Stage {
    /// Appends one signed, newline-terminated body for `intent` to `out`.
    /// Must not allocate once `out` and the signer's own buffers are warm.
    fn sign(&mut self, intent: &OrderIntent, out: &mut Vec<u8>) -> Result<(), ReplayError>;
}

/// The real parser: [`hl_latency::parse_node_line`], unchanged. Today it
/// builds a `serde_json::Value` tree and so allocates on every line; its
/// zero-allocation replacement keeps the signature and drops in here.
#[derive(Debug, Default, Clone, Copy)]
pub struct NodeLineParser;

impl Stage for NodeLineParser {
    const NAME: &'static str = "hl_latency::parse_node_line";
    const LABEL: StageLabel = StageLabel::Real;
}

impl Parser for NodeLineParser {
    #[inline]
    fn parse(&mut self, line: CompleteLine<'_>, recv_ns: u64) -> Result<Observation, ReplayError> {
        let line = std::str::from_utf8(line.as_bytes()).map_err(|_| ReplayError::NotUtf8)?;
        Ok(hl_latency::parse_node_line(line, recv_ns)?)
    }
}

/// Stub.
#[derive(Debug, Default, Clone, Copy)]
pub struct HeaderScanParser;

impl Stage for HeaderScanParser {
    const NAME: &'static str = "stub";
    const LABEL: StageLabel = StageLabel::Stub;
}

impl Parser for HeaderScanParser {
    fn parse(
        &mut self,
        _line: CompleteLine<'_>,
        _recv_ns: u64,
    ) -> Result<Observation, ReplayError> {
        Err(ReplayError::Config("stub".to_owned()))
    }
}

const STUB_LEVELS: usize = 16;

/// A stand-in book: a fixed 16-level array per side that each update pokes
/// and rescans for the best prices. No heap, a fixed handful of cache lines.
/// It is not a book: prices are derived from the block number.
#[derive(Debug, Clone)]
pub struct StubBook {
    bids: [u64; STUB_LEVELS],
    asks: [u64; STUB_LEVELS],
}

impl Default for StubBook {
    fn default() -> Self {
        Self {
            bids: [0; STUB_LEVELS],
            asks: [0; STUB_LEVELS],
        }
    }
}

impl Stage for StubBook {
    const NAME: &'static str = "StubBook: fixed 16-level arrays, poke one level and rescan";
    const LABEL: StageLabel = StageLabel::Stub;
}

impl Book for StubBook {
    #[inline]
    fn apply(&mut self, obs: &Observation) -> TopOfBook {
        let seq = obs.block_number.unwrap_or(0);
        // The level is always < 16, so the cast cannot truncate.
        let level = usize::try_from(seq % 16).unwrap_or(0);
        // A mid that wanders in a 64-tick band around 67,123.0.
        let mid = 671_230 + seq % 64;
        self.bids[level] = mid - 1 - (seq % 3);
        self.asks[level] = mid + 1 + (seq % 5);
        let mut bid = 0;
        let mut ask = u64::MAX;
        for i in 0..STUB_LEVELS {
            if self.bids[i] > bid && self.bids[i] < mid {
                bid = self.bids[i];
            }
            if self.asks[i] > mid && self.asks[i] < ask {
                ask = self.asks[i];
            }
        }
        TopOfBook {
            bid_ticks: bid,
            ask_ticks: ask,
            seq,
            time_ms: obs.block_time_ms,
        }
    }
}

/// A stand-in decision: quote one side of every uncrossed book, alternating
/// by block, with a nonce that never repeats.
#[derive(Debug, Default, Clone, Copy)]
pub struct StubDecision {
    last_nonce: u64,
}

impl Stage for StubDecision {
    const NAME: &'static str = "StubDecision: one ALO order per uncrossed update";
    const LABEL: StageLabel = StageLabel::Stub;
}

impl Decision for StubDecision {
    #[inline]
    fn decide(&mut self, top: &TopOfBook) -> Option<OrderIntent> {
        if top.bid_ticks == 0 || top.bid_ticks >= top.ask_ticks {
            return None;
        }
        let nonce = top.time_ms.max(self.last_nonce.saturating_add(1));
        self.last_nonce = nonce;
        let is_buy = top.seq.is_multiple_of(2);
        Some(OrderIntent {
            asset: 0,
            is_buy,
            px_ticks: if is_buy { top.bid_ticks } else { top.ask_ticks },
            sz_lots: 100,
            nonce,
        })
    }
}

/// The parser the harness runs.
pub type ActiveParser = NodeLineParser;
/// The book the harness runs.
pub type ActiveBook = StubBook;
/// The decision stage the harness runs.
pub type ActiveDecision = StubDecision;
/// The signer the harness runs. **This line is the swap point**: point it at
/// the real signer's [`Signer`] impl once it passes the SDK vectors, and the
/// report's `signer=` label follows from that type's [`Stage::LABEL`].
pub type ActiveSigner = PrototypeSigner;

/// The pipeline every arm runs.
pub type ActivePipeline = Pipeline<ActiveParser, ActiveBook, ActiveDecision, ActiveSigner>;

/// Builds the pipeline every arm runs, with a throwaway signing key.
#[must_use]
pub fn active_pipeline() -> ActivePipeline {
    Pipeline::new(
        ActiveParser::default(),
        ActiveBook::default(),
        ActiveDecision::default(),
        ActiveSigner::throwaway(),
    )
}

/// How much of each line the active parser reads.
#[must_use]
pub fn active_parser_mode() -> ParserMode {
    <ActiveParser as Parser>::MODE
}

/// The report's stage table, read off the active stage types.
#[must_use]
pub fn stage_table() -> [StageInfo; 6] {
    fn row<S: Stage>(stage: &'static str) -> StageInfo {
        StageInfo {
            stage,
            name: S::NAME,
            label: S::LABEL,
        }
    }
    [
        StageInfo {
            stage: "reader",
            name: "LineBuf: fixed buffer, read() straight into it",
            label: StageLabel::Real,
        },
        row::<ActiveParser>("parser"),
        row::<ActiveBook>("book"),
        row::<ActiveDecision>("decision"),
        row::<ActiveSigner>("signer"),
        StageInfo {
            stage: "send",
            name: "plain TCP write to a loopback sink: no TLS, no WebSocket framing, no NIC",
            label: StageLabel::Prototype,
        },
    ]
}

/// What handling one line produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineOutcome {
    /// When the parser returned, on the caller's clock.
    pub t1_ns: u64,
    /// Whether `out` now holds an order to send.
    pub order: bool,
}

/// Everything between the reader and the send stage, for one line.
pub trait Handle {
    /// Handles one line seen at `t0_ns`. Clears `out`, then leaves the bytes
    /// to send in it when the outcome says there is an order.
    fn on_line(
        &mut self,
        line: CompleteLine<'_>,
        t0_ns: u64,
        clock: &Clock,
        out: &mut Vec<u8>,
    ) -> Result<LineOutcome, ReplayError>;

    /// See [`Parser::fallbacks`].
    fn scan_fallbacks(&self) -> u64 {
        0
    }
}

/// parser -> book -> decision -> signer, statically dispatched.
#[derive(Debug)]
pub struct Pipeline<P, B, D, S> {
    /// The parser.
    pub parser: P,
    /// The book.
    pub book: B,
    /// The decision stage.
    pub decision: D,
    /// The signer.
    pub signer: S,
}

impl<P, B, D, S> Pipeline<P, B, D, S> {
    /// Chains the four stages.
    pub fn new(parser: P, book: B, decision: D, signer: S) -> Self {
        Self {
            parser,
            book,
            decision,
            signer,
        }
    }
}

impl<P: Parser, B: Book, D: Decision, S: Signer> Handle for Pipeline<P, B, D, S> {
    #[inline]
    fn on_line(
        &mut self,
        line: CompleteLine<'_>,
        t0_ns: u64,
        clock: &Clock,
        out: &mut Vec<u8>,
    ) -> Result<LineOutcome, ReplayError> {
        out.clear();
        let obs = self.parser.parse(line, t0_ns)?;
        let t1_ns = clock.now_ns();
        let top = self.book.apply(&obs);
        let order = match self.decision.decide(&top) {
            Some(intent) => {
                self.signer.sign(&intent, out)?;
                true
            }
            None => false,
        };
        Ok(LineOutcome { t1_ns, order })
    }
}
