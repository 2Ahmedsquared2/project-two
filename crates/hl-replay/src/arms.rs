//! The arms of the A/B and the driver that runs them, interleaved.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::core::{BusyTick, Core, Hists, Polled, Step};
use crate::sink::{ensure_loopback_peer, loopback_pair, LoopbackAddr, Sink, SinkStats};
use crate::stage::{active_pipeline, ActivePipeline};
use crate::{Clock, ReplayError};

/// One way of getting lines in and orders out. The pipeline between is the
/// same in every arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Arm {
    /// A pinned thread spinning on a non-blocking socket.
    BusyPoll,
    /// tokio `current_thread`, pinned.
    TokioCurrentThread,
    /// tokio multi-thread (2 workers, connection task spawned), not pinned:
    /// the default most code starts from.
    TokioMultiThread,
    /// compio on `io_uring`, pinned.
    Compio,
    /// compio on `io_uring` with a kernel submission-queue polling thread.
    CompioSqpoll,
    /// Tail a file by spinning on `read`.
    FileBusyPoll,
    /// Tail a file, sleeping 10 µs when there is nothing new.
    FilePoll10us,
    /// Tail a file, sleeping 100 µs when there is nothing new.
    FilePoll100us,
    /// Tail a file, sleeping on inotify `IN_MODIFY`.
    FileInotify,
}

impl Arm {
    /// The runtime arms: lines arrive on a loopback socket.
    pub const RUNTIME: [Arm; 5] = [
        Arm::BusyPoll,
        Arm::TokioCurrentThread,
        Arm::TokioMultiThread,
        Arm::Compio,
        Arm::CompioSqpoll,
    ];

    /// The detect-mode arms: lines are appended to a file the reader tails,
    /// which is how the node's output is actually read.
    pub const DETECT: [Arm; 4] = [
        Arm::FileBusyPoll,
        Arm::FilePoll10us,
        Arm::FilePoll100us,
        Arm::FileInotify,
    ];

    /// The arm's name on the command line, in the report and in file names.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Arm::BusyPoll => "busy-poll",
            Arm::TokioCurrentThread => "tokio-current-thread",
            Arm::TokioMultiThread => "tokio-multi-thread",
            Arm::Compio => "compio",
            Arm::CompioSqpoll => "compio-sqpoll",
            Arm::FileBusyPoll => "file-busy-poll",
            Arm::FilePoll10us => "file-poll-10us",
            Arm::FilePoll100us => "file-poll-100us",
            Arm::FileInotify => "file-inotify",
        }
    }

    /// Whether the arm needs `io_uring` and may therefore be unavailable.
    #[must_use]
    pub fn is_io_uring(self) -> bool {
        matches!(self, Arm::Compio | Arm::CompioSqpoll)
    }

    fn is_file(self) -> bool {
        Arm::DETECT.contains(&self)
    }
}

impl std::str::FromStr for Arm {
    type Err = ReplayError;

    fn from_str(s: &str) -> Result<Self, ReplayError> {
        Arm::RUNTIME
            .iter()
            .chain(Arm::DETECT.iter())
            .copied()
            .find(|a| a.name() == s)
            .ok_or_else(|| ReplayError::UnknownArm(s.to_owned()))
    }
}

/// What one round of one arm replays, and how.
#[derive(Debug, Clone)]
pub struct RoundConfig {
    /// The lines, each ending in a newline. Replayed in order, cycling.
    pub lines: Arc<Vec<String>>,
    /// Lines whose latencies are recorded.
    pub measured: u64,
    /// Lines replayed first and not recorded.
    pub warmup: u64,
    /// Time between lines. The feeder is open-loop: it keeps this schedule
    /// whether or not the previous line has been answered.
    pub interval: Duration,
    /// Core for the feeder and the sink thread, or `None` to leave them free.
    pub feeder_core: Option<usize>,
    /// Core for the hot thread, or `None`.
    pub hot_core: Option<usize>,
    /// Directory for the file the detect-mode arms tail (`/dev/shm` is the
    /// honest choice: the node's output is read from the page cache).
    pub tail_dir: PathBuf,
    /// Where the sink listens. Loopback by construction.
    pub sink_bind: LoopbackAddr,
}

/// One round's result.
#[derive(Debug)]
pub struct RoundResult {
    /// The round's histograms (warmup excluded).
    pub hists: Hists,
    /// What the sink counted.
    pub sink: SinkStats,
    /// Lines fed, warmup included.
    pub fed: u64,
    /// `Some(true)` if the hot thread was pinned as asked, `Some(false)` if
    /// pinning was refused, `None` if this arm was not asked to pin.
    pub hot_pinned: Option<bool>,
}

/// A round either ran or could not on this host.
#[derive(Debug)]
pub enum RoundOutcome {
    /// It ran.
    Ran(Box<RoundResult>),
    /// The arm cannot run here (for example `io_uring` is blocked); the
    /// string says why. Not an error: the other arms still count.
    Unavailable(String),
}

enum Ready {
    Go,
    Unavailable(String),
}

enum Source {
    Tcp(TcpStream),
    File(PathBuf),
}

type HotResult = Result<(Option<Hists>, Option<bool>), ReplayError>;

fn pin(core: Option<usize>) -> Option<bool> {
    core.map(|id| core_affinity::set_for_current(core_affinity::CoreId { id }))
}

fn closed(core: &Core<ActivePipeline>, expected: u64) -> ReplayError {
    ReplayError::SourceClosed {
        seen: core.lines_seen(),
        expected,
    }
}

fn busy_arm(
    source: TcpStream,
    sink: TcpStream,
    core: Core<ActivePipeline>,
    total: u64,
    ready: &SyncSender<Ready>,
) -> Result<Hists, ReplayError> {
    source.set_nonblocking(true)?;
    let mut tick = BusyTick::new(source, sink, core);
    let _ = ready.send(Ready::Go);
    while tick.core().lines_seen() < total {
        match tick.poll()? {
            Polled::Lines(_) => {}
            Polled::Idle => std::hint::spin_loop(),
            Polled::Eof => return Err(closed(tick.core(), total)),
        }
    }
    Ok(tick.into_core().into_hists())
}

fn file_arm(
    arm: Arm,
    path: &Path,
    sink: TcpStream,
    core: Core<ActivePipeline>,
    total: u64,
    ready: &SyncSender<Ready>,
    abort: &AtomicBool,
) -> Result<Hists, ReplayError> {
    let file = File::open(path)?;
    let mut watch = if arm == Arm::FileInotify {
        let ino = inotify::Inotify::init()?;
        ino.watches().add(path, inotify::WatchMask::MODIFY)?;
        Some(ino)
    } else {
        None
    };
    let mut events = [0_u8; 4096];
    let mut tick = BusyTick::tail(file, sink, core);
    let _ = ready.send(Ready::Go);
    while tick.core().lines_seen() < total {
        if let Polled::Lines(_) = tick.poll()? {
            continue;
        }
        if abort.load(Ordering::Relaxed) {
            return Err(closed(tick.core(), total));
        }
        match (arm, watch.as_mut()) {
            (Arm::FilePoll10us, _) => std::thread::sleep(Duration::from_micros(10)),
            (Arm::FilePoll100us, _) => std::thread::sleep(Duration::from_micros(100)),
            // Events queue, so a write between the empty read above and this
            // call still wakes it.
            (Arm::FileInotify, Some(ino)) => drop(ino.read_events_blocking(&mut events)?),
            _ => std::hint::spin_loop(),
        }
    }
    Ok(tick.into_core().into_hists())
}

async fn tokio_conn(
    source: TcpStream,
    sink: TcpStream,
    mut core: Core<ActivePipeline>,
    total: u64,
    ready: SyncSender<Ready>,
) -> Result<Hists, ReplayError> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    ensure_loopback_peer(sink.peer_addr())?;
    source.set_nonblocking(true)?;
    sink.set_nonblocking(true)?;
    let mut source = tokio::net::TcpStream::from_std(source)?;
    let mut sink = tokio::net::TcpStream::from_std(sink)?;
    let _ = ready.send(Ready::Go);
    while core.lines_seen() < total {
        let n = source.read(core.spare_mut()?).await?;
        if n == 0 {
            return Err(closed(&core, total));
        }
        let t0 = core.clock().now_ns();
        core.advance(n);
        loop {
            match core.step(t0)? {
                Step::NoLine => break,
                Step::Order => {
                    sink.write_all(core.out()).await?;
                    core.sent();
                }
                Step::NoOrder => {}
            }
        }
    }
    Ok(core.into_hists())
}

fn tokio_arm(
    multi_thread: bool,
    source: TcpStream,
    sink: TcpStream,
    core: Core<ActivePipeline>,
    total: u64,
    ready: SyncSender<Ready>,
) -> Result<Hists, ReplayError> {
    let conn = tokio_conn(source, sink, core, total, ready);
    if multi_thread {
        // The stock setup: a multi-thread runtime, the connection on a
        // spawned task so it runs on a worker and can be moved between them.
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_io()
            .build()?;
        rt.block_on(async { tokio::spawn(conn).await })
            .map_err(|e| ReplayError::Thread(e.to_string()))?
    } else {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()?;
        rt.block_on(conn)
    }
}

#[cfg(feature = "compio")]
fn compio_arm(
    sqpoll: bool,
    source: TcpStream,
    sink: TcpStream,
    mut core: Core<ActivePipeline>,
    total: u64,
    ready: &SyncSender<Ready>,
) -> Result<Option<Hists>, ReplayError> {
    use compio::driver::{DriverType, ProactorBuilder};
    use compio::io::{AsyncRead, AsyncWriteExt};
    use compio::BufResult;

    let mut proactor = ProactorBuilder::new();
    proactor.driver_type(DriverType::IoUring);
    if sqpoll {
        proactor.sqpoll_idle(Duration::from_secs(10));
    }
    let unavailable = |why: String| {
        let _ = ready.send(Ready::Unavailable(why));
        Ok(None)
    };
    let rt = match compio::runtime::RuntimeBuilder::new()
        .with_proactor(proactor)
        .build()
    {
        Ok(rt) => rt,
        Err(e) => return unavailable(format!("io_uring could not be set up on this host: {e}")),
    };
    if rt.driver_type() != DriverType::IoUring {
        return unavailable(format!(
            "compio fell back to the {:?} driver: io_uring is not usable on this host",
            rt.driver_type()
        ));
    }
    rt.block_on(async move {
        ensure_loopback_peer(sink.peer_addr())?;
        let mut source = compio::net::TcpStream::from_std(source)?;
        let mut sink = compio::net::TcpStream::from_std(sink)?;
        // io_uring reads fill a buffer the kernel owns for the duration, so
        // this arm reads into its own buffer and copies into the line reader:
        // one extra memcpy per read that the other arms do not pay.
        let mut chunk: Vec<u8> = Vec::with_capacity(core.line_capacity() / 2);
        let _ = ready.send(Ready::Go);
        while core.lines_seen() < total {
            let BufResult(read, buf) = source.read(chunk).await;
            chunk = buf;
            let n = read?;
            if n == 0 {
                return Err(closed(&core, total));
            }
            let t0 = core.clock().now_ns();
            core.push(&chunk[..n])?;
            loop {
                match core.step(t0)? {
                    Step::NoLine => break,
                    Step::Order => {
                        let BufResult(wrote, out) = sink.write_all(core.take_out()).await;
                        core.restore_out(out);
                        wrote?;
                        core.sent();
                    }
                    Step::NoOrder => {}
                }
            }
        }
        Ok(Some(core.into_hists()))
    })
}

// Same signature as the real arm above, which can fail.
#[cfg(not(feature = "compio"))]
#[allow(clippy::unnecessary_wraps)]
fn compio_arm(
    _sqpoll: bool,
    _source: TcpStream,
    _sink: TcpStream,
    _core: Core<ActivePipeline>,
    _total: u64,
    ready: &SyncSender<Ready>,
) -> Result<Option<Hists>, ReplayError> {
    let _ = ready.send(Ready::Unavailable(
        "built without the `compio` cargo feature".to_owned(),
    ));
    Ok(None)
}

#[allow(clippy::too_many_arguments)]
fn hot_main(
    arm: Arm,
    source: Source,
    sink: TcpStream,
    core: Core<ActivePipeline>,
    total: u64,
    hot_core: Option<usize>,
    ready: SyncSender<Ready>,
    abort: &AtomicBool,
) -> HotResult {
    // The multi-thread runtime is the unpinned baseline on purpose.
    let pinned = if arm == Arm::TokioMultiThread {
        None
    } else {
        pin(hot_core)
    };
    let hists = match (arm, source) {
        (Arm::BusyPoll, Source::Tcp(s)) => Some(busy_arm(s, sink, core, total, &ready)?),
        (Arm::TokioCurrentThread, Source::Tcp(s)) => {
            Some(tokio_arm(false, s, sink, core, total, ready)?)
        }
        (Arm::TokioMultiThread, Source::Tcp(s)) => {
            Some(tokio_arm(true, s, sink, core, total, ready)?)
        }
        (Arm::Compio, Source::Tcp(s)) => compio_arm(false, s, sink, core, total, &ready)?,
        (Arm::CompioSqpoll, Source::Tcp(s)) => compio_arm(true, s, sink, core, total, &ready)?,
        (_, Source::File(path)) => Some(file_arm(arm, &path, sink, core, total, &ready, abort)?),
        (_, Source::Tcp(_)) => {
            return Err(ReplayError::Config(format!(
                "{} reads a file, not a socket",
                arm.name()
            )))
        }
    };
    Ok((hists, pinned))
}

/// The open-loop feeder: one line every `interval`, stamped just before the
/// write. Sleeps while the next line is far off and spins the last stretch,
/// so the schedule is not at the mercy of timer slack.
fn feed<W: Write>(
    mut sink: W,
    lines: &[String],
    interval: Duration,
    clock: Clock,
    stamps: &[AtomicU64],
    core: Option<usize>,
) -> Result<W, ReplayError> {
    const SPIN: Duration = Duration::from_micros(200);
    pin(core);
    let mut next = Instant::now();
    for (line, stamp) in lines.iter().cycle().zip(stamps) {
        next += interval;
        loop {
            let now = Instant::now();
            let Some(left) = next.checked_duration_since(now).filter(|d| !d.is_zero()) else {
                // Behind schedule (we were descheduled): do not burst to catch up.
                if now.duration_since(next) > interval {
                    next = now;
                }
                break;
            };
            if left > SPIN {
                std::thread::sleep(left.checked_sub(SPIN / 2).unwrap_or_default());
            } else {
                std::hint::spin_loop();
            }
        }
        stamp.store(clock.now_ns(), Ordering::Relaxed);
        sink.write_all(line.as_bytes())?;
    }
    Ok(sink)
}

fn join<T>(h: std::thread::JoinHandle<T>, what: &str) -> Result<T, ReplayError> {
    h.join()
        .map_err(|_| ReplayError::Thread(format!("{what} thread panicked")))
}

static TAIL_FILES: AtomicU64 = AtomicU64::new(0);

/// The feeder's end of the source.
enum Feed {
    Tcp(TcpStream),
    File(File),
}

fn validate(cfg: &RoundConfig) -> Result<(), ReplayError> {
    if cfg.measured == 0 {
        return Err(ReplayError::Config(
            "nothing to measure: 0 lines".to_owned(),
        ));
    }
    if cfg.lines.is_empty() {
        return Err(ReplayError::Config("no lines to replay".to_owned()));
    }
    if cfg.lines.iter().any(|l| l.find('\n') != Some(l.len() - 1)) {
        return Err(ReplayError::Config(
            "every line must end with exactly one newline".to_owned(),
        ));
    }
    Ok(())
}

/// Both ends of the arm's source: a loopback socket pair, or a fresh file to
/// append to and tail (its path is returned for cleanup).
fn open_source(
    arm: Arm,
    cfg: &RoundConfig,
) -> Result<(Feed, Source, Option<PathBuf>), ReplayError> {
    if !arm.is_file() {
        let (feed_end, hot_end) = loopback_pair()?;
        return Ok((Feed::Tcp(feed_end), Source::Tcp(hot_end), None));
    }
    std::fs::create_dir_all(&cfg.tail_dir)?;
    let path = cfg.tail_dir.join(format!(
        "hl-replay-{}-{}-{}.jsonl",
        std::process::id(),
        arm.name(),
        TAIL_FILES.fetch_add(1, Ordering::Relaxed)
    ));
    let file = OpenOptions::new().create(true).append(true).open(&path)?;
    Ok((Feed::File(file), Source::File(path.clone()), Some(path)))
}

/// Runs one round of one arm: starts a loopback sink, the hot thread and the
/// feeder, replays `warmup + measured` lines and checks that exactly that
/// many orders reached the sink.
///
/// # Errors
///
/// On a meaningless config, any I/O or parse failure, or a sink count that
/// does not match what was fed.
pub fn run_round(arm: Arm, cfg: &RoundConfig) -> Result<RoundOutcome, ReplayError> {
    validate(cfg)?;
    let total = cfg.warmup.saturating_add(cfg.measured);
    let total_lines = usize::try_from(total)
        .map_err(|_| ReplayError::Config(format!("{total} lines do not fit in memory")))?;
    let longest = cfg.lines.iter().map(String::len).max().unwrap_or(0);
    let line_capacity = longest.saturating_mul(2).max(64 * 1024);

    let clock = Clock::new();
    let stamps: Arc<[AtomicU64]> = (0..total_lines).map(|_| AtomicU64::new(0)).collect();
    let sink = Sink::spawn_on(cfg.sink_bind, cfg.feeder_core)?;
    let sink_stream = sink.connect()?;
    let core = Core::new(active_pipeline(), clock, cfg.warmup, line_capacity)
        .with_feed_stamps(Arc::clone(&stamps));

    let (feed_end, source, tail_path) = open_source(arm, cfg)?;

    let abort = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = sync_channel(1);
    let hot = {
        let abort = Arc::clone(&abort);
        let hot_core = cfg.hot_core;
        std::thread::Builder::new()
            .name(format!("hl-replay-{}", arm.name()))
            .spawn(move || {
                hot_main(
                    arm,
                    source,
                    sink_stream,
                    core,
                    total,
                    hot_core,
                    ready_tx,
                    &abort,
                )
            })?
    };

    let cleanup = |path: &Option<PathBuf>| {
        if let Some(p) = path {
            let _ = std::fs::remove_file(p);
        }
    };

    let ready = ready_rx.recv();
    if !matches!(ready, Ok(Ready::Go)) {
        // Unavailable, or the hot thread ended before it was ready (its
        // error says why). Either way nothing is fed.
        drop(feed_end);
        let hot_result = join(hot, "hot").and_then(|r| r);
        let _ = sink.finish();
        cleanup(&tail_path);
        hot_result?;
        return match ready {
            Ok(Ready::Unavailable(why)) => Ok(RoundOutcome::Unavailable(why)),
            _ => Err(ReplayError::Thread(
                "hot thread ended before it was ready".to_owned(),
            )),
        };
    }

    let feeder = {
        let lines = Arc::clone(&cfg.lines);
        let stamps = Arc::clone(&stamps);
        let (interval, core) = (cfg.interval, cfg.feeder_core);
        std::thread::Builder::new()
            .name("hl-replay-feeder".to_owned())
            .spawn(move || match feed_end {
                Feed::Tcp(s) => feed(s, &lines, interval, clock, &stamps, core).map(Feed::Tcp),
                Feed::File(f) => feed(f, &lines, interval, clock, &stamps, core).map(Feed::File),
            })?
    };
    let fed = join(feeder, "feeder").and_then(|r| r);
    if fed.is_err() {
        // Let a file-tailing hot thread stop: it checks this when idle, and a
        // bare newline wakes one that sleeps on inotify (and fails its parse).
        abort.store(true, Ordering::Relaxed);
        if let Some(p) = &tail_path {
            if let Ok(mut f) = OpenOptions::new().append(true).open(p) {
                let _ = f.write_all(b"\n");
            }
        }
    }
    let hot_result = join(hot, "hot").and_then(|r| r);
    // The feed end stays open until the hot thread is done, so a socket arm
    // never sees end-of-file early.
    let feed_end = fed;
    let sink_stats = sink.finish();
    cleanup(&tail_path);
    drop(feed_end?);
    let (hists, hot_pinned) = hot_result?;
    let sink_stats = sink_stats?;
    let Some(hists) = hists else {
        return Err(ReplayError::Thread(format!(
            "{} reported ready and then produced nothing",
            arm.name()
        )));
    };
    if sink_stats.orders != total {
        return Err(ReplayError::Mismatch {
            arm: arm.name(),
            fed: total,
            orders: sink_stats.orders,
        });
    }
    Ok(RoundOutcome::Ran(Box::new(RoundResult {
        hists,
        sink: sink_stats,
        fed: total,
        hot_pinned,
    })))
}

/// One arm's merged result over all its rounds.
#[derive(Debug)]
pub struct ArmReport {
    /// The arm.
    pub arm: Arm,
    /// Rounds that ran.
    pub rounds_run: u32,
    /// All rounds' samples, merged (never averaged).
    pub hists: Hists,
    /// Orders counted at the sink over all rounds, warmup included.
    pub orders: u64,
    /// Whether the hot thread was pinned; see [`RoundResult::hot_pinned`].
    pub hot_pinned: Option<bool>,
    /// Why the arm could not run here, if it could not.
    pub unavailable: Option<String>,
}

/// Runs `rounds` rounds, each going through `arms` in order, so every arm
/// sees the same machine under the same load: round 1 of every arm, then
/// round 2 of every arm, and so on. An arm that is unavailable is skipped
/// from then on and reported as such.
pub fn run_interleaved(
    arms: &[Arm],
    rounds: u32,
    cfg: &RoundConfig,
) -> Result<Vec<ArmReport>, ReplayError> {
    let mut reports: Vec<ArmReport> = arms
        .iter()
        .map(|&arm| ArmReport {
            arm,
            rounds_run: 0,
            hists: Hists::default(),
            orders: 0,
            hot_pinned: None,
            unavailable: None,
        })
        .collect();
    for _ in 0..rounds {
        for report in &mut reports {
            if report.unavailable.is_some() {
                continue;
            }
            match run_round(report.arm, cfg)? {
                RoundOutcome::Ran(r) => {
                    report.rounds_run += 1;
                    report.hists.merge(&r.hists);
                    report.orders += r.sink.orders;
                    report.hot_pinned = r.hot_pinned;
                }
                RoundOutcome::Unavailable(why) => report.unavailable = Some(why),
            }
        }
    }
    Ok(reports)
}
