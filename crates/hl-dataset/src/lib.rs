//! Versioned on-disk format for a recorded order-book dataset.
//!
//! A dataset is one asset's contiguous run of blocks together with the full
//! resting book after each block (or after every k-th block). It is the ground
//! truth that `book-check` replays a candidate book against. The format is
//! specified in `docs/dataset-format.md` at the repository root; this crate is
//! its reader and writer and enforces the structural rules:
//!
//! 1. The first line is a [`Header`] with a supported `format_version`.
//! 2. Every later line is a [`Block`] or a book record.
//! 3. Block heights are contiguous and start at `header.first_height`.
//! 4. A book record carries the height of the block it directly follows, and
//!    at most one book record follows a block.
//!
//! The encoding is JSON Lines (one JSON object per line, `\n` terminated).
//! That is not a hot-path format and is not meant to be: this file is read by
//! a checker, written by a capture tool (possibly in Python), and inspected by
//! people. Every number is an integer: prices in ticks, sizes in lots.

use std::io::{BufRead, Write};

use hl_book_ref::{BookOp, BookSnapshot};
pub use records::Header;
use records::Record;
use serde::{Deserialize, Serialize};

/// The format version this crate writes and the only one it reads.
pub const FORMAT_VERSION: u32 = 1;

mod records {
    //! `#[serde(flatten)]` expands to a visitor over arbitrary JSON values whose
    //! generated code names `f32`/`f64`, which the workspace `clippy.toml` bans.
    //! No float reaches any field of ours; the ban is lifted for this module only.
    #![allow(clippy::disallowed_types)]

    use std::collections::BTreeMap;

    use hl_book_ref::BookSnapshot;
    use serde::{Deserialize, Serialize};

    use super::Block;

    /// The first record of a dataset: what was captured and how to read the integers.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct Header {
        /// Format version; see [`FORMAT_VERSION`](super::FORMAT_VERSION).
        pub format_version: u32,
        /// Chain the blocks come from, e.g. `mainnet` or `testnet`.
        pub chain: String,
        /// Asset (coin) whose book this is, e.g. `BTC`.
        pub asset: String,
        /// Decimal places of the price grid: `price = px_ticks / 10^px_decimals`.
        pub px_decimals: u32,
        /// Decimal places of the size grid: `size = sz_lots / 10^sz_decimals`.
        pub sz_decimals: u32,
        /// Height of the first block in the file.
        pub first_height: u64,
        /// Free-text provenance: capture tool, hl-node binary, dates. Never parsed.
        pub source: String,
        /// Any other header fields. Preserved verbatim so a capture tool can add
        /// provenance before this crate knows the field.
        #[serde(flatten)]
        pub extra: BTreeMap<String, serde_json::Value>,
    }

    #[derive(Serialize, Deserialize)]
    #[serde(tag = "kind", rename_all = "snake_case")]
    pub(crate) enum Record {
        Header(Header),
        Block(Block),
        Book {
            height: u64,
            #[serde(flatten)]
            book: BookSnapshot,
        },
    }
}

/// One block's effect on the resting book, in the order the effects were applied.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Block {
    /// Block height.
    pub height: u64,
    /// Block time in milliseconds since the Unix epoch, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_ms: Option<u64>,
    /// Per-order effects, in application order.
    pub ops: Vec<BookOp>,
}

/// One block and, when recorded, the full book after it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    /// The block.
    pub block: Block,
    /// The recorded book after the block, if this block has a book record.
    pub book: Option<BookSnapshot>,
}

/// Why a dataset could not be read. `line` is 1-based.
#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    /// Underlying I/O failure.
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    /// A line is not a valid record.
    #[error("line {line}: invalid record: {source}")]
    Json {
        /// 1-based line number.
        line: usize,
        /// The JSON error.
        source: serde_json::Error,
    },
    /// The file is empty or its first record is not a header.
    #[error("the first line must be a header record")]
    MissingHeader,
    /// A second header appeared after the first.
    #[error("line {line}: a second header record")]
    DuplicateHeader {
        /// 1-based line number.
        line: usize,
    },
    /// The header's version is not one this crate reads.
    #[error("unsupported format_version {0} (this reader supports {FORMAT_VERSION})")]
    UnsupportedVersion(u32),
    /// Block heights are not contiguous from `first_height`.
    #[error("line {line}: expected block height {expected}, found {found}")]
    HeightGap {
        /// The height the reader expected next.
        expected: u64,
        /// The height the record carried.
        found: u64,
        /// 1-based line number.
        line: usize,
    },
    /// A book record does not directly follow the block with its height.
    #[error("line {line}: book record for height {height} does not follow its block")]
    BookWithoutBlock {
        /// The height the book record carried.
        height: u64,
        /// 1-based line number.
        line: usize,
    },
}

/// Why a record could not be written.
#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    /// Underlying I/O failure.
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    /// Serialisation failure (should not happen for these types).
    #[error("serialisation error: {0}")]
    Json(#[from] serde_json::Error),
    /// A block's height does not continue the run.
    #[error("expected block height {expected}, found {found}")]
    HeightGap {
        /// The height the writer expected next.
        expected: u64,
        /// The height given.
        found: u64,
    },
    /// A book was written before any block.
    #[error("a book record must follow a block")]
    BookWithoutBlock,
    /// A second book was written for the same block.
    #[error("a block has at most one book record")]
    DuplicateBook,
}

/// Streaming dataset writer. Enforces the structural rules as it goes.
#[derive(Debug)]
pub struct Writer<W: Write> {
    out: W,
    next_height: u64,
    last_block: Option<u64>,
    book_written: bool,
}

impl<W: Write> Writer<W> {
    /// Start a dataset by writing its header.
    pub fn new(mut out: W, header: &Header) -> Result<Self, WriteError> {
        Self::write_record(&mut out, &Record::Header(header.clone()))?;
        Ok(Self {
            out,
            next_height: header.first_height,
            last_block: None,
            book_written: false,
        })
    }

    /// Append a block. Its height must be the next in the run.
    pub fn write_block(&mut self, block: &Block) -> Result<(), WriteError> {
        if block.height != self.next_height {
            return Err(WriteError::HeightGap {
                expected: self.next_height,
                found: block.height,
            });
        }
        Self::write_record(&mut self.out, &Record::Block(block.clone()))?;
        self.last_block = Some(block.height);
        self.next_height = block.height + 1;
        self.book_written = false;
        Ok(())
    }

    /// Append the full book after the block just written.
    pub fn write_book(&mut self, book: &BookSnapshot) -> Result<(), WriteError> {
        let height = self.last_block.ok_or(WriteError::BookWithoutBlock)?;
        if self.book_written {
            return Err(WriteError::DuplicateBook);
        }
        Self::write_record(
            &mut self.out,
            &Record::Book {
                height,
                book: book.clone(),
            },
        )?;
        self.book_written = true;
        Ok(())
    }

    /// Flush and return the underlying writer.
    pub fn finish(mut self) -> Result<W, WriteError> {
        self.out.flush()?;
        Ok(self.out)
    }

    fn write_record(out: &mut W, record: &Record) -> Result<(), WriteError> {
        serde_json::to_writer(&mut *out, record)?;
        out.write_all(b"\n")?;
        Ok(())
    }
}

/// Streaming dataset reader. Validates the header on construction and the
/// structural rules as records are read.
#[derive(Debug)]
pub struct Reader<R: BufRead> {
    lines: std::io::Lines<R>,
    line_no: usize,
    header: Header,
    next_height: u64,
}

impl<R: BufRead> Reader<R> {
    /// Open a dataset: reads and validates the header line.
    pub fn new(input: R) -> Result<Self, ReadError> {
        let mut lines = input.lines();
        let first = match lines.next() {
            Some(line) => line?,
            None => return Err(ReadError::MissingHeader),
        };
        let Record::Header(header) = parse_record(&first, 1)? else {
            return Err(ReadError::MissingHeader);
        };
        if header.format_version != FORMAT_VERSION {
            return Err(ReadError::UnsupportedVersion(header.format_version));
        }
        Ok(Self {
            lines,
            line_no: 1,
            next_height: header.first_height,
            header,
        })
    }

    /// The dataset header.
    #[must_use]
    pub fn header(&self) -> &Header {
        &self.header
    }

    /// Iterate over blocks, each paired with its recorded book when present.
    /// Errors end the iteration.
    pub fn steps(self) -> Steps<R> {
        Steps {
            reader: self,
            pending: None,
            done: false,
        }
    }

    /// Read the next non-header record, validating heights. `Ok(None)` at end of file.
    fn next_record(&mut self) -> Result<Option<Record>, ReadError> {
        let Some(line) = self.lines.next() else {
            return Ok(None);
        };
        let line = line?;
        self.line_no += 1;
        let line_no = self.line_no;
        if line.trim().is_empty() {
            return self.next_record();
        }
        match parse_record(&line, line_no)? {
            Record::Header(_) => Err(ReadError::DuplicateHeader { line: line_no }),
            Record::Block(block) => {
                if block.height != self.next_height {
                    return Err(ReadError::HeightGap {
                        expected: self.next_height,
                        found: block.height,
                        line: line_no,
                    });
                }
                self.next_height = block.height + 1;
                Ok(Some(Record::Block(block)))
            }
            book @ Record::Book { .. } => Ok(Some(book)),
        }
    }
}

fn parse_record(line: &str, line_no: usize) -> Result<Record, ReadError> {
    serde_json::from_str(line).map_err(|source| ReadError::Json {
        line: line_no,
        source,
    })
}

/// Iterator over [`Step`]s; see [`Reader::steps`].
#[derive(Debug)]
pub struct Steps<R: BufRead> {
    reader: Reader<R>,
    pending: Option<Block>,
    done: bool,
}

impl<R: BufRead> Steps<R> {
    fn advance(&mut self) -> Result<Option<Step>, ReadError> {
        loop {
            match self.reader.next_record()? {
                None => {
                    return Ok(self.pending.take().map(|block| Step { block, book: None }));
                }
                Some(Record::Block(block)) => {
                    // A pending block with no book record ends when the next block starts.
                    if let Some(prev) = self.pending.replace(block) {
                        return Ok(Some(Step {
                            block: prev,
                            book: None,
                        }));
                    }
                }
                Some(Record::Book { height, book }) => {
                    let line = self.reader.line_no;
                    match self.pending.take() {
                        Some(block) if block.height == height => {
                            return Ok(Some(Step {
                                block,
                                book: Some(book),
                            }));
                        }
                        _ => return Err(ReadError::BookWithoutBlock { height, line }),
                    }
                }
                Some(Record::Header(_)) => unreachable!("next_record rejects headers"),
            }
        }
    }
}

impl<R: BufRead> Iterator for Steps<R> {
    type Item = Result<Step, ReadError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        match self.advance() {
            Ok(Some(step)) => Some(Ok(step)),
            Ok(None) => {
                self.done = true;
                None
            }
            Err(e) => {
                self.done = true;
                Some(Err(e))
            }
        }
    }
}
