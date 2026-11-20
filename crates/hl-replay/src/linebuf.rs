//! The line reader: a fixed buffer that turns reads of any size into lines.

use std::io::Read;

use crate::ReplayError;

/// A fixed-capacity byte buffer that yields complete `\n`-terminated lines.
///
/// It never grows: a line longer than the buffer is an error, not a
/// reallocation in the middle of a run. Read straight into
/// [`LineBuf::spare_mut`] and call [`LineBuf::advance`], then drain with
/// [`LineBuf::next_line`].
#[derive(Debug)]
pub struct LineBuf {
    buf: Box<[u8]>,
    /// The storage's size; `buf` is empty while the storage is lent out.
    cap: usize,
    start: usize,
    end: usize,
    /// Bytes in `start..scanned` are known to hold no newline.
    scanned: usize,
    /// Where the line last returned by `next_line` sits in `buf`.
    last: (usize, usize),
    /// `(end, scanned)` saved while the storage is lent out.
    lent: (usize, usize),
}

impl LineBuf {
    /// A buffer that can hold `cap` bytes of pending input.
    #[must_use]
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            buf: vec![0; cap].into_boxed_slice(),
            cap,
            start: 0,
            end: 0,
            scanned: 0,
            last: (0, 0),
            lent: (0, 0),
        }
    }

    /// The free space to read into. Moves a pending partial line to the front
    /// first if that frees room.
    ///
    /// # Errors
    ///
    /// [`ReplayError::LineTooLong`] when the buffer is full of one unfinished
    /// line.
    pub fn spare_mut(&mut self) -> Result<&mut [u8], ReplayError> {
        if self.start > 0 {
            self.buf.copy_within(self.start..self.end, 0);
            self.end -= self.start;
            self.scanned -= self.start;
            self.start = 0;
            // The last line's bytes may just have been overwritten.
            self.last = (0, 0);
        }
        if self.end == self.buf.len() {
            return Err(ReplayError::LineTooLong { capacity: self.cap });
        }
        Ok(&mut self.buf[self.end..])
    }

    /// Marks `n` bytes of [`LineBuf::spare_mut`] as filled (clamped to the
    /// space there is).
    pub fn advance(&mut self, n: usize) {
        self.end = self.end.saturating_add(n).min(self.buf.len());
    }

    /// Copies `bytes` in. For sources that hand over their own buffer; a
    /// reader that can read into [`LineBuf::spare_mut`] should.
    ///
    /// # Errors
    ///
    /// [`ReplayError::LineTooLong`] when they do not fit; nothing is copied.
    pub fn push(&mut self, bytes: &[u8]) -> Result<(), ReplayError> {
        let capacity = self.cap;
        if bytes.is_empty() {
            return Ok(());
        }
        let free = self.buf.len() - (self.end - self.start);
        if bytes.len() > free {
            return Err(ReplayError::LineTooLong { capacity });
        }
        match self.spare_mut() {
            Ok(spare) if spare.len() >= bytes.len() => {
                spare[..bytes.len()].copy_from_slice(bytes);
                self.advance(bytes.len());
                Ok(())
            }
            _ => Err(ReplayError::LineTooLong { capacity }),
        }
    }

    /// One `read` from `r` into the free space; returns the bytes read.
    pub fn fill_from<R: Read>(&mut self, r: &mut R) -> Result<usize, ReplayError> {
        let n = r.read(self.spare_mut()?)?;
        self.advance(n);
        Ok(n)
    }

    /// The next complete line, without its newline, or `None` when only a
    /// partial line (or nothing) is buffered.
    pub fn next_line(&mut self) -> Option<&[u8]> {
        let from = self.scanned.max(self.start);
        let Some(i) = self.buf[from..self.end].iter().position(|&b| b == b'\n') else {
            self.scanned = self.end;
            return None;
        };
        let (a, b) = (self.start, from + i);
        self.start = b + 1;
        self.scanned = self.start;
        if self.start == self.end {
            // Empty: the next read starts at the front, no copy needed.
            self.start = 0;
            self.end = 0;
            self.scanned = 0;
        }
        self.last = (a, b);
        Some(&self.buf[a..b])
    }

    /// The line [`LineBuf::next_line`] last returned, for as long as nothing
    /// has been read into the buffer since; empty after that.
    #[must_use]
    pub fn last_line(&self) -> &[u8] {
        &self.buf[self.last.0..self.last.1]
    }

    /// Lends the storage out, for a runtime whose reads must own their
    /// buffer (`io_uring`). Returns it with the offset to read at: fill
    /// `storage[offset..]`, then give it back with
    /// [`LineBuf::restore_storage`]. Until then the reader is empty and
    /// refuses input. No allocation and no copy beyond moving a pending
    /// partial line to the front, as [`LineBuf::spare_mut`] does.
    ///
    /// # Errors
    ///
    /// [`ReplayError::LineTooLong`] when the buffer is full of one
    /// unfinished line (or is already lent).
    pub fn take_storage(&mut self) -> Result<(Box<[u8]>, usize), ReplayError> {
        self.spare_mut()?;
        self.lent = (self.end, self.scanned);
        self.end = 0;
        self.scanned = 0;
        self.last = (0, 0);
        Ok((std::mem::take(&mut self.buf), self.lent.0))
    }

    /// Takes back the storage lent by [`LineBuf::take_storage`], with
    /// `filled` new bytes written at the offset that call returned.
    pub fn restore_storage(&mut self, storage: Box<[u8]>, filled: usize) {
        self.buf = storage;
        self.end = self.lent.0.saturating_add(filled).min(self.buf.len());
        self.scanned = self.lent.1;
        self.lent = (0, 0);
    }

    /// Bytes buffered and not yet returned as a line.
    #[must_use]
    pub fn buffered(&self) -> usize {
        self.end - self.start
    }
}
