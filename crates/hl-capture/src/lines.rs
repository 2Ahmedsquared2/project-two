//! Splits a byte stream into newline-terminated lines, keeping each line's
//! completion stamp.

/// One complete line (without its `\n`, and without a trailing `\r`) and the
/// stamp of the read that completed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StampedLine {
    /// Line bytes.
    pub bytes: Vec<u8>,
    /// Stamp of the read that delivered the newline.
    pub recv_ns: u64,
}

/// Default longest line kept, in bytes (1 MiB). A longer line is dropped and
/// counted, so a stream without newlines cannot grow memory without bound.
pub const DEFAULT_MAX_LINE: usize = 1024 * 1024;

/// Accumulates partial lines across reads, holding at most `max_line` bytes.
#[derive(Debug)]
pub struct LineSplitter {
    partial: Vec<u8>,
    max_line: usize,
    /// Inside an oversized line: drop bytes up to its newline.
    discarding: bool,
    oversized: u64,
}

impl Default for LineSplitter {
    fn default() -> Self {
        Self::with_max_line(DEFAULT_MAX_LINE)
    }
}

impl LineSplitter {
    /// Creates an empty splitter with [`DEFAULT_MAX_LINE`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates an empty splitter that drops lines longer than `max_line`
    /// bytes (counting a trailing `\r`, not the `\n`). A `max_line` of 0 is treated as 1.
    #[must_use]
    pub fn with_max_line(max_line: usize) -> Self {
        Self {
            partial: Vec::new(),
            max_line: max_line.max(1),
            discarding: false,
            oversized: 0,
        }
    }

    /// Feeds the bytes of one read, stamped `recv_ns`, and appends every line
    /// it completes to `out`. Lines longer than the cap are dropped and
    /// counted in [`Self::oversized`]; at most the cap is ever held back.
    pub fn push(&mut self, chunk: &[u8], recv_ns: u64, out: &mut Vec<StampedLine>) {
        let mut rest = chunk;
        while let Some(i) = rest.iter().position(|&b| b == b'\n') {
            let (head, tail) = rest.split_at(i);
            rest = tail.get(1..).unwrap_or_default();
            if std::mem::take(&mut self.discarding) {
                continue;
            }
            if self.partial.len() + head.len() > self.max_line {
                self.partial.clear();
                self.oversized += 1;
                continue;
            }
            let mut bytes = std::mem::take(&mut self.partial);
            bytes.extend_from_slice(head);
            if bytes.last() == Some(&b'\r') {
                bytes.pop();
            }
            if !bytes.is_empty() {
                out.push(StampedLine { bytes, recv_ns });
            }
        }
        if self.discarding {
            return;
        }
        if self.partial.len() + rest.len() > self.max_line {
            self.partial.clear();
            self.discarding = true;
            self.oversized += 1;
        } else {
            self.partial.extend_from_slice(rest);
        }
    }

    /// Lines dropped for exceeding the cap.
    #[must_use]
    pub fn oversized(&self) -> u64 {
        self.oversized
    }

    /// Drops an unfinished line (used when the file it came from is
    /// abandoned). Returns whether one was held.
    pub fn clear(&mut self) -> bool {
        let had = !self.partial.is_empty() || self.discarding;
        self.partial.clear();
        self.discarding = false;
        had
    }

    /// Bytes of an unfinished line held back.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.partial.len()
    }
}
