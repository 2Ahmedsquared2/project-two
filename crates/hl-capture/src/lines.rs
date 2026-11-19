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

/// Accumulates partial lines across reads.
#[derive(Debug, Default)]
pub struct LineSplitter {
    partial: Vec<u8>,
}

impl LineSplitter {
    /// Creates an empty splitter.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds the bytes of one read, stamped `recv_ns`, and appends every line
    /// it completes to `out`.
    pub fn push(&mut self, chunk: &[u8], recv_ns: u64, out: &mut Vec<StampedLine>) {
        let mut rest = chunk;
        while let Some(i) = rest.iter().position(|&b| b == b'\n') {
            let (head, tail) = rest.split_at(i);
            rest = &tail[1..];
            let mut bytes = if self.partial.is_empty() {
                head.to_vec()
            } else {
                let mut v = std::mem::take(&mut self.partial);
                v.extend_from_slice(head);
                v
            };
            if bytes.last() == Some(&b'\r') {
                bytes.pop();
            }
            if !bytes.is_empty() {
                out.push(StampedLine { bytes, recv_ns });
            }
        }
        self.partial.extend_from_slice(rest);
    }

    /// Drops an unfinished line (used when the file it came from is
    /// abandoned). Returns whether one was held.
    pub fn clear(&mut self) -> bool {
        let had = !self.partial.is_empty();
        self.partial.clear();
        had
    }

    /// Bytes of an unfinished line held back.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.partial.len()
    }
}
