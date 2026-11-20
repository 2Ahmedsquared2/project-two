//! End-to-end replay harness for the hot path.
//!
//! Replays node output lines through `reader -> parser -> book -> decision ->
//! signer -> send` in one process and times every line with one monotonic
//! clock:
//!
//! - `t0`: the read that delivered the line returned (line seen),
//! - `t1`: the parser returned,
//! - `t2`: the write of the signed order to the socket returned.
//!
//! `t2 - t0` and `t1 - t0` go into HDR histograms, per arm. An arm is one way
//! of doing the I/O around the same pipeline: a busy-poll pinned thread,
//! tokio `current_thread`, tokio multi-thread, compio on `io_uring` with and
//! without SQPOLL, and four ways of tailing a file. Rounds interleave the
//! arms so each sees the same machine and load.
//!
//! The send stage writes to a counting sink on loopback and nowhere else:
//! the only address type it accepts is [`LoopbackAddr`], which cannot hold a
//! routable address or a host name. Nothing in this crate can reach an
//! exchange.
//!
//! Stages are generic parameters of [`Pipeline`] (static dispatch). The
//! active ones are the `Active*` type aliases; each reports whether it is
//! real, a stub or a prototype, and the report prints that beside the numbers.

mod arms;
mod clock;
mod core;
mod error;
mod hist;
mod input;
mod linebuf;
mod report;
mod signer;
mod sink;
mod stage;

pub use hl_latency::{CompleteLine, Observation};

pub use crate::arms::{
    run_interleaved, run_round, run_shuffled, Arm, ArmReport, RoundConfig, RoundLog, RoundOutcome,
    RoundResult, Run,
};
pub use crate::clock::Clock;
pub use crate::core::{BusyTick, Core, Hists, Polled, Step};
pub use crate::error::ReplayError;
pub use crate::hist::{LatencyHist, Summary, HIST_MAX_NS};
pub use crate::input::{generated_lines, load_lines, node_line};
pub use crate::linebuf::LineBuf;
pub use crate::report::{render_report, render_run, write_artifacts, HostInfo, RunMeta, GIT_SHA};
pub use crate::signer::PrototypeSigner;
pub use crate::sink::{loopback_pair, LoopbackAddr, Sink, SinkStats};
pub use crate::stage::{
    active_parser_mode, active_pipeline, stage_table, ActiveBook, ActiveDecision, ActiveParser,
    ActivePipeline, ActiveSigner, Book, Decision, Handle, HeaderScanParser, LineOutcome,
    NodeLineParser, OrderIntent, Parser, ParserMode, Pipeline, Signer, Stage, StageInfo,
    StageLabel, StubBook, StubDecision, TopOfBook,
};
