//! Looking for a newer hourly file, off the reader thread.
//!
//! The look is a directory walk, and a thread that walks cannot read. So the
//! walk runs on the writer thread ([`Scout`]) and the reader only takes what
//! it found ([`Arrivals`]): one wait-free pop from a one-slot ring when the
//! file it follows is at its end. The scout opens the file too, so the
//! reader's side of a rotation makes no directory or `open` call.
//!
//! The scout looks on a coarse timer, and at once when the reader asks
//! ([`NodeTailer::request_rescan`](crate::NodeTailer::request_rescan), an
//! atomic store). What it hands over is the newest file it sees that is
//! newer than the last one it handed over. While the slot is still full it
//! hands over nothing, so the reader always takes files in the order they
//! were found; the newer one is found again by the next look.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::tail::{hourly_key, newest_hourly_file};

/// Longest gap between looks while the scout has found no file yet.
const NO_FILE: Duration = Duration::from_millis(10);

/// An hourly file the scout found and opened.
#[derive(Debug)]
pub(crate) struct Found {
    pub(crate) path: PathBuf,
    pub(crate) key: (u32, u8),
    pub(crate) file: File,
}

/// What the two ends share: how many looks the reader has asked for, and
/// the request count the scout's latest finished look had seen.
#[derive(Debug, Default)]
struct Shared {
    asked: AtomicU64,
    answered: AtomicU64,
}

/// The walking end: lives on the writer thread.
#[derive(Debug)]
pub struct Scout {
    root: PathBuf,
    tx: rtrb::Producer<Found>,
    shared: Arc<Shared>,
    /// Key of the last file handed over.
    last: Option<(u32, u8)>,
    every: Duration,
    next: Option<Instant>,
    answered: u64,
    looks: u64,
}

/// The reader's end: takes the files the [`Scout`] found.
#[derive(Debug)]
pub struct Arrivals {
    rx: rtrb::Consumer<Found>,
    shared: Arc<Shared>,
    asked: u64,
}

/// A scout over `root` that looks every `every` (every 10 ms at most until
/// it has found a first file) and whenever asked, and the reader's end to
/// give to [`NodeTailer::with_scout`](crate::NodeTailer::with_scout).
#[must_use]
pub fn scout(root: impl Into<PathBuf>, every: Duration) -> (Scout, Arrivals) {
    let (tx, rx) = rtrb::RingBuffer::new(1);
    let shared = Arc::new(Shared::default());
    (
        Scout {
            root: root.into(),
            tx,
            shared: Arc::clone(&shared),
            last: None,
            every,
            next: None,
            answered: 0,
            looks: 0,
        },
        Arrivals {
            rx,
            shared,
            asked: 0,
        },
    )
}

impl Scout {
    /// Looks if the reader asked since the last look or the timer is due.
    /// Returns whether it looked. Otherwise one atomic load.
    pub fn tick(&mut self, now: Instant) -> std::io::Result<bool> {
        let asked = self.shared.asked.load(Ordering::Acquire);
        let due = self.next.is_none_or(|at| now >= at);
        if asked == self.answered && !due {
            return Ok(false);
        }
        let looked = self.look();
        // After the hand-over, so a reader that sees the answer also sees
        // the file in the slot.
        self.answered = asked;
        self.shared.answered.store(asked, Ordering::Release);
        let every = if self.last.is_some() {
            self.every
        } else {
            self.every.min(NO_FILE)
        };
        self.next = now.checked_add(every);
        looked.map(|()| true)
    }

    /// Walks the directory once and hands the newest file to the reader if
    /// it is newer than the last one handed over and the slot is free.
    pub fn look(&mut self) -> std::io::Result<()> {
        self.looks += 1;
        // Not implemented yet (red): finds nothing.
        let _ = (
            &self.tx,
            hourly_key,
            newest_hourly_file,
            File::open::<&Path>,
        );
        Ok(())
    }

    /// Directory walks so far.
    #[must_use]
    pub fn looks(&self) -> u64 {
        self.looks
    }

    /// The directory searched.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl Arrivals {
    /// Asks the scout to look now. One atomic store.
    pub(crate) fn ask(&mut self) {
        self.asked = self.asked.wrapping_add(1);
        self.shared.asked.store(self.asked, Ordering::Release);
    }

    /// True while a look that was asked for has not finished. One atomic
    /// load.
    pub(crate) fn pending(&self) -> bool {
        self.shared.answered.load(Ordering::Acquire) != self.asked
    }

    /// The next file the scout found, if any. Wait-free.
    pub(crate) fn pop(&mut self) -> Option<Found> {
        self.rx.pop().ok()
    }
}
