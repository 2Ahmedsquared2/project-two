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
    /// Position of the line's first byte in the stream (see
    /// [`LineSplitter::set_position`]).
    pub offset: u64,
}

/// Receives each complete line a [`LineSplitter`] finds. The bytes are
/// borrowed from the splitter (or from the chunk being pushed) and are only
/// valid for the call, so a sink that keeps a line copies it.
pub trait LineSink {
    /// One complete, non-empty line without its `\n` or trailing `\r`, the
    /// stamp of the read that completed it, and the position of its first
    /// byte in the stream (see [`LineSplitter::set_position`]).
    fn line(&mut self, bytes: &[u8], recv_ns: u64, offset: u64);
}

/// Collects owned lines: one heap allocation per line. For tests and cold
/// callers; the capture path uses a sink that parses the borrowed bytes.
impl LineSink for Vec<StampedLine> {
    fn line(&mut self, bytes: &[u8], recv_ns: u64, offset: u64) {
        self.push(StampedLine {
            bytes: bytes.to_vec(),
            recv_ns,
            offset,
        });
    }
}

/// Default longest line kept, in bytes (1 MiB). A longer line is dropped and
/// counted, so a stream without newlines cannot grow memory without bound.
pub const DEFAULT_MAX_LINE: usize = 1024 * 1024;

/// Accumulates partial lines across reads, holding at most `max_line` bytes.
///
/// The buffer for a line split across reads is reserved once, at
/// construction, at its full `max_line` size, so [`Self::push`] never
/// allocates: a line that lies wholly inside one chunk is handed to the sink
/// straight from that chunk, and a split line is assembled in the reserved
/// buffer and handed out from there.
#[derive(Debug)]
pub struct LineSplitter {
    partial: Vec<u8>,
    max_line: usize,
    /// Inside an oversized line: drop bytes up to its newline.
    discarding: bool,
    oversized: u64,
    /// Stream position of the next byte [`Self::push`] will be given.
    next: u64,
    /// Stream position of the first byte of the line being assembled.
    start: u64,
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
    /// bytes (counting a trailing `\r`, not the `\n`). A `max_line` of 0 is
    /// treated as 1. Reserves `max_line` bytes now; if that reservation is
    /// refused (an absurd cap), the buffer grows on demand instead.
    #[must_use]
    pub fn with_max_line(max_line: usize) -> Self {
        let max_line = max_line.max(1);
        let mut partial = Vec::new();
        // Refused only for a cap no machine can hold; growing later is then
        // the best that can be done.
        let _ = partial.try_reserve_exact(max_line);
        Self {
            partial,
            max_line,
            discarding: false,
            oversized: 0,
            next: 0,
            start: 0,
        }
    }

    /// Says where in its stream the next byte pushed lies (for a file, the
    /// file position the next read starts at): line offsets count from
    /// there. A new splitter is at 0. Every byte pushed advances the
    /// position, whether its line is handed out, dropped as oversized or
    /// empty. Call it when the stream changes, after [`Self::clear`].
    pub fn set_position(&mut self, position: u64) {
        self.next = position;
        self.start = position;
    }

    /// Feeds the bytes of one read, stamped `recv_ns`, and hands every line
    /// it completes to `out`. Lines longer than the cap are dropped and
    /// counted in [`Self::oversized`]; at most the cap is ever held back.
    pub fn push<S: LineSink + ?Sized>(&mut self, chunk: &[u8], recv_ns: u64, out: &mut S) {
        let mut rest = chunk;
        while let Some(i) = memchr::memchr(b'\n', rest) {
            let (head, tail) = rest.split_at(i);
            rest = tail.get(1..).unwrap_or_default();
            // One add per line: where this line began, and where the next
            // one does (past the newline).
            let offset = self.start;
            self.next = self.next.wrapping_add(i as u64).wrapping_add(1);
            self.start = self.next;
            if std::mem::take(&mut self.discarding) {
                continue;
            }
            if self.partial.len() + head.len() > self.max_line {
                self.partial.clear();
                self.oversized += 1;
                continue;
            }
            let line = if self.partial.is_empty() {
                head
            } else {
                // Within the reserved capacity: the cap was checked above.
                self.partial.extend_from_slice(head);
                self.partial.as_slice()
            };
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if !line.is_empty() {
                // Not implemented yet (red): no offset.
                let _ = offset;
                out.line(line, recv_ns, 0);
            }
            self.partial.clear();
        }
        // The unfinished rest belongs to the line that began at `start`.
        self.next = self.next.wrapping_add(rest.len() as u64);
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
