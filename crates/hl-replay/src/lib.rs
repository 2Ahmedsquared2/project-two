//! End-to-end replay harness (stub: the API is declared, nothing works yet).
#![allow(clippy::missing_panics_doc, clippy::must_use_candidate, missing_docs)]
#![allow(clippy::unused_self, clippy::needless_pass_by_value, unused_variables)]

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub use hl_latency::Observation;

pub const GIT_SHA: &str = "stub";
pub const HIST_MAX_NS: u64 = 60_000_000_000;

#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("not implemented")]
    Stub,
}

#[derive(Debug, Clone, Copy)]
pub struct Clock;
impl Clock {
    pub fn new() -> Self {
        Self
    }
    pub fn now_ns(&self) -> u64 {
        0
    }
}
impl Default for Clock {
    fn default() -> Self {
        Self
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Summary {
    pub count: u64,
    pub p50: u64,
    pub p90: u64,
    pub p99: u64,
    pub p999: u64,
    pub p9999: u64,
    pub max: u64,
}

#[derive(Debug, Clone, Default)]
pub struct LatencyHist;
impl LatencyHist {
    pub fn new() -> Self {
        Self
    }
    pub fn record(&mut self, ns: u64) {}
    pub fn count(&self) -> u64 {
        0
    }
    pub fn merge(&mut self, other: &Self) {}
    pub fn value_at_per_100k(&self, q: u32) -> u64 {
        0
    }
    pub fn max(&self) -> u64 {
        0
    }
    pub fn summary(&self) -> Summary {
        Summary::default()
    }
    pub fn write_v2<W: Write>(&self, w: &mut W) -> Result<(), ReplayError> {
        Err(ReplayError::Stub)
    }
    pub fn read_v2<R: Read>(r: &mut R) -> Result<Self, ReplayError> {
        Err(ReplayError::Stub)
    }
    pub fn write_buckets<W: Write>(&self, w: &mut W) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Debug)]
pub struct LineBuf;
impl LineBuf {
    pub fn with_capacity(cap: usize) -> Self {
        Self
    }
    pub fn spare_mut(&mut self) -> Result<&mut [u8], ReplayError> {
        Err(ReplayError::Stub)
    }
    pub fn advance(&mut self, n: usize) {}
    pub fn push(&mut self, bytes: &[u8]) -> Result<(), ReplayError> {
        Err(ReplayError::Stub)
    }
    pub fn fill_from<R: Read>(&mut self, r: &mut R) -> Result<usize, ReplayError> {
        Err(ReplayError::Stub)
    }
    pub fn next_line(&mut self) -> Option<&[u8]> {
        None
    }
    pub fn buffered(&self) -> usize {
        0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageLabel {
    Real,
    Stub,
    Prototype,
}
impl std::fmt::Display for StageLabel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("?")
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StageInfo {
    pub stage: &'static str,
    pub name: &'static str,
    pub label: StageLabel,
}
pub trait Stage {
    const NAME: &'static str;
    const LABEL: StageLabel;
}
pub trait Parser: Stage {
    fn parse(&mut self, line: &str, recv_ns: u64) -> Result<Observation, ReplayError>;
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TopOfBook {
    pub bid_ticks: u64,
    pub ask_ticks: u64,
    pub seq: u64,
    pub time_ms: u64,
}
pub trait Book: Stage {
    fn apply(&mut self, obs: &Observation) -> TopOfBook;
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OrderIntent {
    pub asset: u32,
    pub is_buy: bool,
    pub px_ticks: u64,
    pub sz_lots: u64,
    pub nonce: u64,
}
pub trait Decision: Stage {
    fn decide(&mut self, top: &TopOfBook) -> Option<OrderIntent>;
}
pub trait Signer: Stage {
    fn sign(&mut self, intent: &OrderIntent, out: &mut Vec<u8>) -> Result<(), ReplayError>;
}

#[derive(Debug, Default)]
pub struct NodeLineParser;
impl Stage for NodeLineParser {
    const NAME: &'static str = "stub";
    const LABEL: StageLabel = StageLabel::Stub;
}
impl Parser for NodeLineParser {
    fn parse(&mut self, line: &str, recv_ns: u64) -> Result<Observation, ReplayError> {
        Err(ReplayError::Stub)
    }
}
#[derive(Debug, Default)]
pub struct StubBook;
impl Stage for StubBook {
    const NAME: &'static str = "stub";
    const LABEL: StageLabel = StageLabel::Stub;
}
impl Book for StubBook {
    fn apply(&mut self, obs: &Observation) -> TopOfBook {
        TopOfBook::default()
    }
}
#[derive(Debug, Default)]
pub struct StubDecision;
impl Stage for StubDecision {
    const NAME: &'static str = "stub";
    const LABEL: StageLabel = StageLabel::Stub;
}
impl Decision for StubDecision {
    fn decide(&mut self, top: &TopOfBook) -> Option<OrderIntent> {
        None
    }
}
#[derive(Debug)]
pub struct PrototypeSigner;
impl PrototypeSigner {
    pub fn new(secret: &[u8; 32]) -> Result<Self, ReplayError> {
        Ok(Self)
    }
    pub fn throwaway() -> Self {
        Self
    }
    pub fn digest(&mut self, intent: &OrderIntent) -> [u8; 32] {
        [0; 32]
    }
    pub fn public_key(&self) -> [u8; 33] {
        [0; 33]
    }
}
impl Stage for PrototypeSigner {
    const NAME: &'static str = "stub";
    const LABEL: StageLabel = StageLabel::Stub;
}
impl Signer for PrototypeSigner {
    fn sign(&mut self, intent: &OrderIntent, out: &mut Vec<u8>) -> Result<(), ReplayError> {
        Err(ReplayError::Stub)
    }
}
pub type ActiveSigner = PrototypeSigner;
pub type ActivePipeline = Pipeline<NodeLineParser, StubBook, StubDecision, ActiveSigner>;
pub fn active_pipeline() -> ActivePipeline {
    Pipeline::new(NodeLineParser, StubBook, StubDecision, ActiveSigner::throwaway())
}
pub fn stage_table() -> [StageInfo; 6] {
    [StageInfo {
        stage: "?",
        name: "?",
        label: StageLabel::Stub,
    }; 6]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineOutcome {
    pub t1_ns: u64,
    pub order: bool,
}
pub trait Handle {
    fn on_line(
        &mut self,
        line: &[u8],
        t0_ns: u64,
        clock: &Clock,
        out: &mut Vec<u8>,
    ) -> Result<LineOutcome, ReplayError>;
}
#[derive(Debug)]
pub struct Pipeline<P, B, D, S> {
    pub parser: P,
    pub book: B,
    pub decision: D,
    pub signer: S,
}
impl<P, B, D, S> Pipeline<P, B, D, S> {
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
    fn on_line(
        &mut self,
        line: &[u8],
        t0_ns: u64,
        clock: &Clock,
        out: &mut Vec<u8>,
    ) -> Result<LineOutcome, ReplayError> {
        Err(ReplayError::Stub)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoopbackAddr(SocketAddr);
impl LoopbackAddr {
    pub fn new(addr: SocketAddr) -> Result<Self, ReplayError> {
        Ok(Self(addr))
    }
    pub fn ephemeral() -> Self {
        Self(SocketAddr::from(([127, 0, 0, 1], 0)))
    }
    pub fn addr(&self) -> SocketAddr {
        self.0
    }
}
impl std::str::FromStr for LoopbackAddr {
    type Err = ReplayError;
    fn from_str(s: &str) -> Result<Self, ReplayError> {
        Ok(Self::ephemeral())
    }
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SinkStats {
    pub bytes: u64,
    pub orders: u64,
}
#[derive(Debug)]
pub struct Sink;
impl Sink {
    pub fn spawn(bind: LoopbackAddr) -> Result<Self, ReplayError> {
        Err(ReplayError::Stub)
    }
    pub fn addr(&self) -> LoopbackAddr {
        LoopbackAddr::ephemeral()
    }
    pub fn connect(&self) -> Result<TcpStream, ReplayError> {
        Err(ReplayError::Stub)
    }
    pub fn finish(self) -> Result<SinkStats, ReplayError> {
        Err(ReplayError::Stub)
    }
}
pub fn loopback_pair() -> Result<(TcpStream, TcpStream), ReplayError> {
    Err(ReplayError::Stub)
}

#[derive(Debug, Default)]
pub struct Hists {
    pub parse: LatencyHist,
    pub e2e: LatencyHist,
    pub detect: LatencyHist,
}
#[derive(Debug)]
pub struct Core<H> {
    handler: H,
    hists: Hists,
}
impl<H: Handle> Core<H> {
    pub fn new(handler: H, clock: Clock, warmup: u64, line_capacity: usize) -> Self {
        Self {
            handler,
            hists: Hists::default(),
        }
    }
    pub fn hists(&self) -> &Hists {
        &self.hists
    }
    pub fn into_hists(self) -> Hists {
        self.hists
    }
    pub fn lines_seen(&self) -> u64 {
        0
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Polled {
    Idle,
    Lines(u64),
    Eof,
}
#[derive(Debug)]
pub struct BusyTick<R, H> {
    source: R,
    core: Core<H>,
}
impl<R: Read, H: Handle> BusyTick<R, H> {
    pub fn new(source: R, sink: TcpStream, core: Core<H>) -> Self {
        Self { source, core }
    }
    pub fn poll(&mut self) -> Result<Polled, ReplayError> {
        Err(ReplayError::Stub)
    }
    pub fn core(&self) -> &Core<H> {
        &self.core
    }
    pub fn into_core(self) -> Core<H> {
        self.core
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Arm {
    BusyPoll,
    TokioCurrentThread,
    TokioMultiThread,
    Compio,
    CompioSqpoll,
    FileBusyPoll,
    FilePoll10us,
    FilePoll100us,
    FileInotify,
}
impl Arm {
    pub const RUNTIME: [Arm; 5] = [
        Arm::BusyPoll,
        Arm::TokioCurrentThread,
        Arm::TokioMultiThread,
        Arm::Compio,
        Arm::CompioSqpoll,
    ];
    pub const DETECT: [Arm; 4] = [
        Arm::FileBusyPoll,
        Arm::FilePoll10us,
        Arm::FilePoll100us,
        Arm::FileInotify,
    ];
    pub fn name(self) -> &'static str {
        "?"
    }
    pub fn is_io_uring(self) -> bool {
        matches!(self, Arm::Compio | Arm::CompioSqpoll)
    }
}
impl std::str::FromStr for Arm {
    type Err = ReplayError;
    fn from_str(s: &str) -> Result<Self, ReplayError> {
        Err(ReplayError::Stub)
    }
}
#[derive(Debug, Clone)]
pub struct RoundConfig {
    pub lines: Arc<Vec<String>>,
    pub measured: u64,
    pub warmup: u64,
    pub interval: Duration,
    pub feeder_core: Option<usize>,
    pub hot_core: Option<usize>,
    pub tail_dir: PathBuf,
    pub sink_bind: LoopbackAddr,
}
#[derive(Debug)]
pub struct RoundResult {
    pub hists: Hists,
    pub sink: SinkStats,
    pub fed: u64,
    pub hot_pinned: Option<bool>,
}
#[derive(Debug)]
pub enum RoundOutcome {
    Ran(Box<RoundResult>),
    Unavailable(String),
}
pub fn run_round(arm: Arm, cfg: &RoundConfig) -> Result<RoundOutcome, ReplayError> {
    Err(ReplayError::Stub)
}
#[derive(Debug)]
pub struct ArmReport {
    pub arm: Arm,
    pub rounds_run: u32,
    pub hists: Hists,
    pub orders: u64,
    pub hot_pinned: Option<bool>,
    pub unavailable: Option<String>,
}
pub fn run_interleaved(
    arms: &[Arm],
    rounds: u32,
    cfg: &RoundConfig,
) -> Result<Vec<ArmReport>, ReplayError> {
    Err(ReplayError::Stub)
}
pub fn node_line(seq: u64, events: usize) -> String {
    String::new()
}
pub fn generated_lines(n: usize, events: usize) -> Vec<String> {
    Vec::new()
}
pub fn load_lines(path: &Path) -> Result<Vec<String>, ReplayError> {
    Err(ReplayError::Stub)
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostInfo {
    pub kernel: String,
    pub cpu_model: String,
    pub cores: usize,
}
impl HostInfo {
    pub fn detect() -> Self {
        Self::default()
    }
}
#[derive(Debug, Clone)]
pub struct RunMeta {
    pub commit: String,
    pub host: HostInfo,
    pub feeder_core: Option<usize>,
    pub hot_core: Option<usize>,
    pub rounds: u32,
    pub measured_per_round: u64,
    pub warmup_per_round: u64,
    pub interval: Duration,
    pub input: String,
}
pub fn render_report(meta: &RunMeta, arms: &[ArmReport]) -> String {
    String::new()
}
pub fn write_artifacts(
    dir: &Path,
    report: &str,
    arms: &[ArmReport],
) -> Result<Vec<PathBuf>, ReplayError> {
    Err(ReplayError::Stub)
}
