//! How the reader waits for new bytes between polls.
//!
//! | mode | waits by | cost |
//! |---|---|---|
//! | `busy-poll` (default) | not waiting: spin, read again | one core at 100% |
//! | `poll` | sleeping `--poll-us` | a timer wake-up per interval |
//! | `inotify` (Linux) | sleeping until the kernel reports a write | a wake-up per write |
//!
//! Fallbacks, each reported to the caller as a note (the binary prints it
//! on stderr with the live mode):
//!
//! - `busy-poll` on a machine with one usable CPU becomes `poll`: the
//!   spinning reader would starve the writer thread that empties the ring.
//! - `inotify` off Linux, or when the inotify instance cannot be created,
//!   becomes `poll`.
//! - `inotify` whose watch on the followed file is refused later becomes
//!   `poll` at that point.
//!
//! In `inotify` mode the watch on a file is added after the caller has read
//! that file to its end, and a write between the two raises no event. So the
//! first wait after a file is followed does not sleep: the caller reads once
//! more with the watch in place, and a line written in that gap is stamped
//! at once instead of at the next write.

use std::path::Path;
use std::time::{Duration, Instant};

/// A way to wait for the node to write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detect {
    /// Do not wait: spin and read again. Burns one core.
    BusyPoll,
    /// Sleep a fixed interval between reads.
    Poll,
    /// Sleep until the kernel reports a write (Linux only).
    Inotify,
}

impl Detect {
    /// Parses a `--detect` value: `busy-poll`, `poll` or `inotify`.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        [Self::BusyPoll, Self::Poll, Self::Inotify]
            .into_iter()
            .find(|m| m.name() == s)
    }

    /// The `--detect` spelling.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::BusyPoll => "busy-poll",
            Self::Poll => "poll",
            Self::Inotify => "inotify",
        }
    }
}

/// What one [`Waiter::wait`] learned.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Wake {
    /// A directory changed: look for a newer file.
    pub rescan: bool,
}

#[derive(Debug)]
enum Kind {
    Busy,
    Poll,
    #[cfg(target_os = "linux")]
    Inotify(Box<linux::Watcher>),
}

/// Waits between polls in the live detect mode.
#[derive(Debug)]
pub struct Waiter {
    kind: Kind,
    poll: Duration,
}

impl Waiter {
    /// A waiter for `requested`, or for the mode it falls back to, with the
    /// reason for the fallback. `poll` is the sleep of [`Detect::Poll`];
    /// `cpus` is how many CPUs this process may run on.
    #[must_use]
    pub fn new(requested: Detect, poll: Duration, cpus: usize) -> (Self, Option<String>) {
        let (kind, note) = match requested {
            Detect::BusyPoll if cpus < 2 => (
                Kind::Poll,
                Some(format!(
                    "busy-poll needs a second CPU for the writer thread and only {cpus} CPU is available"
                )),
            ),
            Detect::BusyPoll => (Kind::Busy, None),
            Detect::Poll => (Kind::Poll, None),
            Detect::Inotify => inotify_kind(),
        };
        (Self { kind, poll }, note)
    }

    /// The mode in use now.
    #[must_use]
    pub fn mode(&self) -> Detect {
        match self.kind {
            Kind::Busy => Detect::BusyPoll,
            Kind::Poll => Detect::Poll,
            #[cfg(target_os = "linux")]
            Kind::Inotify(_) => Detect::Inotify,
        }
    }

    /// Tells the waiter which file is followed now. Call it after any poll
    /// that may have switched files; it does nothing unless the file
    /// changed (and nothing at all outside inotify mode). Returns a note if
    /// the mode had to change. In inotify mode, the first [`Self::wait`]
    /// after the file changed returns without sleeping: poll the file again
    /// after it, since a write made before the watch existed raised no
    /// event.
    pub fn follow(&mut self, root: &Path, current: Option<&Path>) -> Option<String> {
        #[cfg(target_os = "linux")]
        if let Kind::Inotify(w) = &mut self.kind {
            if let Err(e) = w.follow(root, current) {
                self.kind = Kind::Poll;
                return Some(format!("inotify watch failed: {e}"));
            }
        }
        let _ = (root, current);
        None
    }

    /// Waits for new bytes, at most until `until`. In busy-poll mode it
    /// returns at once.
    pub fn wait(&mut self, until: Instant) -> Wake {
        match &mut self.kind {
            Kind::Busy => std::hint::spin_loop(),
            Kind::Poll => {
                let left = until.saturating_duration_since(Instant::now());
                std::thread::sleep(self.poll.min(left));
            }
            #[cfg(target_os = "linux")]
            Kind::Inotify(w) => return w.wait(until),
        }
        Wake::default()
    }
}

#[cfg(target_os = "linux")]
fn inotify_kind() -> (Kind, Option<String>) {
    match linux::Watcher::new() {
        Ok(w) => (Kind::Inotify(Box::new(w)), None),
        Err(e) => (Kind::Poll, Some(format!("inotify setup failed: {e}"))),
    }
}

#[cfg(not(target_os = "linux"))]
fn inotify_kind() -> (Kind, Option<String>) {
    (
        Kind::Poll,
        Some("inotify is only available on Linux".to_owned()),
    )
}

#[cfg(target_os = "linux")]
mod linux {
    use std::io;
    use std::path::{Path, PathBuf};
    use std::time::Instant;

    use inotify::{EventMask, Inotify, WatchDescriptor, WatchMask};
    use rustix::event::{poll, PollFd, PollFlags, Timespec};

    use super::Wake;

    /// Room for a burst of events; more are read on the next wait.
    const EVENT_BUF: usize = 4096;

    /// The inotify instance and what it watches: the followed file for
    /// writes, and the directories a newer hourly file can appear in
    /// (the file's date directory, the `hourly` directory above it, and
    /// the root) for creations.
    pub(super) struct Watcher {
        ino: Inotify,
        file: Option<(WatchDescriptor, PathBuf)>,
        dirs: Vec<(WatchDescriptor, PathBuf)>,
        followed_once: bool,
        /// A file was followed since the last wait: what the node wrote to
        /// it before the watch existed raised no event, so the next wait
        /// must not sleep.
        unseen_writes: bool,
        events: Box<[u8; EVENT_BUF]>,
    }

    impl std::fmt::Debug for Watcher {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("Watcher")
                .field("file", &self.file)
                .field("dirs", &self.dirs)
                .finish_non_exhaustive()
        }
    }

    impl Watcher {
        pub(super) fn new() -> io::Result<Self> {
            Ok(Self {
                ino: Inotify::init()?,
                file: None,
                dirs: Vec::new(),
                followed_once: false,
                unseen_writes: false,
                events: Box::new([0; EVENT_BUF]),
            })
        }

        /// Watches `dir` for creations, unless it is watched already. A
        /// directory that does not exist (yet) is skipped: the caller's
        /// rescan timer covers what cannot be watched.
        fn watch_dir(&mut self, dir: &Path) -> io::Result<()> {
            if self.dirs.iter().any(|(_, p)| p == dir) {
                return Ok(());
            }
            let mask = WatchMask::CREATE | WatchMask::MOVED_TO | WatchMask::ONLYDIR;
            match self.ino.watches().add(dir, mask) {
                Ok(wd) => {
                    self.dirs.push((wd, dir.to_path_buf()));
                    Ok(())
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                    ) =>
                {
                    Ok(())
                }
                Err(e) => Err(e),
            }
        }

        /// Cold: runs when the followed file changes, once per hour.
        pub(super) fn follow(&mut self, root: &Path, current: Option<&Path>) -> io::Result<()> {
            let same = self.file.as_ref().map(|(_, p)| p.as_path()) == current;
            if same && self.followed_once {
                return Ok(());
            }
            self.followed_once = true;
            if let Some((wd, _)) = self.file.take() {
                // The old file may be gone already; either way it is no
                // longer watched.
                let _ = self.ino.watches().remove(wd);
            }
            if let Some(file) = current {
                let wd = self.ino.watches().add(file, WatchMask::MODIFY)?;
                self.file = Some((wd, file.to_path_buf()));
                self.unseen_writes = true;
                // Stop watching date directories that are no longer on the
                // path to the followed file.
                let mut i = 0;
                while let Some((_, dir)) = self.dirs.get(i) {
                    if file.starts_with(dir) {
                        i += 1;
                    } else {
                        let (wd, _) = self.dirs.swap_remove(i);
                        let _ = self.ino.watches().remove(wd);
                    }
                }
                // `{root}/hourly/{date}/{hour}`: the date directory and the
                // one above it.
                for dir in file.ancestors().skip(1).take(2) {
                    self.watch_dir(dir)?;
                }
            }
            self.watch_dir(root)?;
            // Before the first file exists the node may only have made
            // `{root}/hourly`.
            self.watch_dir(&root.join("hourly"))
        }

        pub(super) fn wait(&mut self, until: Instant) -> Wake {
            // The file was read to its end before its watch was added, and
            // the node may have written in between: do not sleep, only
            // collect the events already queued, so the caller reads again
            // at once. Cold: once per followed file.
            let left = if std::mem::take(&mut self.unseen_writes) {
                std::time::Duration::ZERO
            } else {
                until.saturating_duration_since(Instant::now())
            };
            let timeout = Timespec::try_from(left).ok();
            let ready = {
                let mut fds = [PollFd::new(&self.ino, PollFlags::IN)];
                // An interrupted or failed wait is treated as a wake-up: the
                // caller polls the file, which is always safe.
                poll(&mut fds, timeout.as_ref()).unwrap_or(1)
            };
            let mut wake = Wake::default();
            if ready == 0 {
                return wake;
            }
            // A directory created under a watched one is watched at once,
            // then a rescan is asked for: a file made in it before the watch
            // existed is found by the rescan, one made after by the watch.
            let mut new_dirs: Vec<PathBuf> = Vec::new();
            if let Ok(events) = self.ino.read_events(self.events.as_mut_slice()) {
                for ev in events {
                    if !ev.mask.intersects(EventMask::CREATE | EventMask::MOVED_TO) {
                        continue;
                    }
                    wake.rescan = true;
                    if !ev.mask.contains(EventMask::ISDIR) {
                        continue;
                    }
                    let parent = self.dirs.iter().find(|(wd, _)| *wd == ev.wd);
                    if let (Some((_, parent)), Some(name)) = (parent, ev.name) {
                        new_dirs.push(parent.join(name));
                    }
                }
            }
            for dir in new_dirs {
                // Refused: the rescan timer still finds files in it.
                let _ = self.watch_dir(&dir);
            }
            wake
        }
    }
}
