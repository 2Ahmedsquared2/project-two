//! The harness error type.

use std::io;

/// Everything that can stop a replay.
#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    /// I/O failure on the source, the sink or an output file.
    #[error("io: {0}")]
    Io(#[from] io::Error),
    /// The send stage was pointed at something that is not loopback.
    #[error("{0}: not a loopback address; the send stage only ever writes to a loopback sink")]
    NotLoopback(String),
    /// A line did not fit the reader's fixed buffer.
    #[error("a line is longer than the reader's {capacity}-byte buffer")]
    LineTooLong {
        /// The buffer's size.
        capacity: usize,
    },
    /// A replayed line is not UTF-8.
    #[error("line is not UTF-8")]
    NotUtf8,
    /// The parser refused a line.
    #[error("parse: {0}")]
    Parse(#[from] hl_latency::ParseError),
    /// A recording that cannot be replayed.
    #[error("{path}: line {line}: {msg}")]
    BadInput {
        /// The file.
        path: String,
        /// 1-based line number (0 when the file as a whole is unusable).
        line: usize,
        /// What is wrong.
        msg: String,
    },
    /// A key that is not a valid secp256k1 secret.
    #[error("signer: {0}")]
    Signer(String),
    /// A host-facts file that is missing something the capture script writes.
    #[error("host facts: {0}")]
    HostFacts(String),
    /// A round was asked to do something meaningless.
    #[error("config: {0}")]
    Config(String),
    /// An unknown arm name.
    #[error("unknown arm {0:?}")]
    UnknownArm(String),
    /// The source ended before every line had been replayed.
    #[error("source closed after {seen} of {expected} lines")]
    SourceClosed {
        /// Lines replayed.
        seen: u64,
        /// Lines expected.
        expected: u64,
    },
    /// What reached the sink is not what was fed: the run's numbers are void.
    #[error("{arm}: fed {fed} lines but {orders} orders reached the sink")]
    Mismatch {
        /// The arm.
        arm: &'static str,
        /// Lines fed.
        fed: u64,
        /// Orders counted at the sink.
        orders: u64,
    },
    /// A header-scan parser accepted lines the strict parser refuses: the
    /// run's numbers are void.
    #[error("{arm}: the scan accepted {mismatches} of {validated} checked lines that the strict parser refuses")]
    ScanStrictMismatch {
        /// The arm.
        arm: &'static str,
        /// Lines the strict parser refused.
        mismatches: u64,
        /// Lines checked.
        validated: u64,
    },
    /// A harness thread panicked or vanished.
    #[error("thread: {0}")]
    Thread(String),
    /// A serialised histogram could not be written or read.
    #[error("histogram: {0}")]
    Histogram(String),
}
