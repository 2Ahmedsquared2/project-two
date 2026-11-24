//! The arms of the A/B and the driver that runs them, interleaved.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::Arc;
use std::time::Duration;

use crate::core::{BusyTick, Core, Hists, Polled, Step};
use crate::sink::{ensure_loopback_peer, loopback_pair, LoopbackAddr, Sink, SinkStats};
use crate::stage::{active_parser_mode, active_pipeline, ActivePipeline, ParserMode};
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
    /// Pinned, it needs [`RoundConfig::sqpoll_core`]: a core for that thread
    /// other than the hot core. Without one it is unavailable.
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

    /// Why this arm cannot run at all on the platform named `os` (a value of
    /// [`std::env::consts::OS`]), or `None` if the platform does not rule it
    /// out. Such an arm is reported as unavailable with this reason, like an
    /// `io_uring` arm on a host that blocks `io_uring`.
    #[must_use]
    pub fn unsupported_on(self, os: &str) -> Option<String> {
        (self == Arm::FileInotify && os != "linux")
            .then(|| format!("inotify is Linux-only and this build is for {os}"))
    }

    /// [`Arm::unsupported_on`] for the platform this binary was built for.
    #[must_use]
    pub fn unsupported_here(self) -> Option<String> {
        self.unsupported_on(std::env::consts::OS)
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
    /// Core for the kernel's submission-queue polling thread of the
    /// `compio-sqpoll` arm, or `None`.
    pub sqpoll_core: Option<usize>,
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
    /// The CPUs the kernel's submission-queue polling thread may run on
    /// (its `Cpus_allowed_list`, read from `/proc` once the ring exists).
    /// `None` for an arm without such a thread, or when it could not be read.
    pub sqpoll_cpus: Option<String>,
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

/// What the hot thread hands back: its histograms (`None` when the arm was
/// unavailable), whether it was pinned, and its polling thread's CPUs.
struct HotOut {
    hists: Option<Hists>,
    pinned: Option<bool>,
    sqpoll_cpus: Option<String>,
}

type HotResult = Result<HotOut, ReplayError>;

/// A kernel submission-queue polling thread, and the core it is given.
///
/// Without the `compio` feature no ring is made, so nothing reads the core
/// and the field is not built.
#[derive(Clone, Copy)]
struct SqPoll {
    #[cfg(feature = "compio")]
    core: Option<usize>,
}

/// Why the `compio-sqpoll` arm cannot run with these cores, if it cannot.
///
/// The kernel's polling thread (`iou-sqp`) is created by the hot thread and
/// spins. Given no CPU of its own it starts beside its creator: on Linux 6.1
/// it was seen on the pinned hot thread's core in 4 of 5 samples although it
/// was allowed on every core, and on a hot core the scheduler does not
/// balance (`isolcpus`) nothing would move it off. Two spinning threads on
/// one core are time-sliced against each other, milliseconds where the other
/// arms take microseconds (the Tokyo runs of d7f9087; that this is their
/// cause is inferred, not yet confirmed there). A pinned run therefore needs
/// a core for the polling thread that is not the hot core.
fn sqpoll_core_refusal(hot_core: Option<usize>, sqpoll_core: Option<usize>) -> Option<String> {
    let hot = hot_core?;
    let what = match sqpoll_core {
        Some(c) if c != hot => return None,
        Some(_) => "was given the hot thread's core",
        None => "was given no core, so it would start beside the hot thread",
    };
    Some(format!(
        "the kernel's polling thread {what} (core {hot}) and would spin there against it: pass --sqpoll-core N with another core"
    ))
}

/// The CPUs of the polling thread that appeared since `before` was read.
///
/// The thread names itself and takes its CPU when it first runs, not when
/// the ring is made, so this waits (at most 200 ms, before the round starts,
/// so nothing measured is delayed) until it shows and, when it was given a
/// core, until the kernel reports that core. It returns the last reading
/// either way: what the kernel says, not what was asked. `None` if the
/// thread never shows.
#[cfg(feature = "compio")]
fn new_sqpoll_thread_cpus(before: &[(String, String)], asked: Option<usize>) -> Option<String> {
    let asked = asked.map(|c| c.to_string());
    let mut last = None;
    for _ in 0..200 {
        let new: Vec<String> = sqpoll_threads()
            .into_iter()
            .filter(|(tid, _)| !before.iter().any(|(old, _)| old == tid))
            .map(|(_, cpus)| cpus)
            .collect();
        if !new.is_empty() {
            last = Some(new.join(" | "));
            if asked.is_none() || last == asked {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    last
}

/// The `iou-sqp` kernel threads of this process: `(tid, Cpus_allowed_list)`.
/// Empty where `/proc` has no such thing.
#[cfg(feature = "compio")]
fn sqpoll_threads() -> Vec<(String, String)> {
    let Ok(tasks) = std::fs::read_dir("/proc/self/task") else {
        return Vec::new();
    };
    tasks
        .filter_map(|task| {
            let task = task.ok()?;
            let comm = std::fs::read_to_string(task.path().join("comm")).ok()?;
            if !comm.starts_with("iou-sqp") {
                return None;
            }
            let status = std::fs::read_to_string(task.path().join("status")).ok()?;
            let cpus = status
                .lines()
                .find_map(|l| l.strip_prefix("Cpus_allowed_list:"))?;
            Some((
                task.file_name().to_string_lossy().into_owned(),
                cpus.trim().to_owned(),
            ))
        })
        .collect()
}

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

/// Sleeps until the tailed file is next modified.
#[cfg(target_os = "linux")]
struct ModifyWatch {
    inotify: inotify::Inotify,
    events: [u8; 4096],
}

#[cfg(target_os = "linux")]
impl ModifyWatch {
    fn open(path: &Path) -> Result<Self, ReplayError> {
        let inotify = inotify::Inotify::init()?;
        inotify.watches().add(path, inotify::WatchMask::MODIFY)?;
        Ok(Self {
            inotify,
            events: [0; 4096],
        })
    }

    /// Events queue, so a write between the caller's empty read and this
    /// call still wakes it.
    fn wait(&mut self) -> Result<(), ReplayError> {
        drop(self.inotify.read_events_blocking(&mut self.events)?);
        Ok(())
    }
}

/// There is no inotify off Linux, so one is never made: `file_arm` reports
/// the arm unavailable ([`Arm::unsupported_on`]) before it would open one.
#[cfg(not(target_os = "linux"))]
struct ModifyWatch(std::convert::Infallible);

#[cfg(not(target_os = "linux"))]
impl ModifyWatch {
    fn open(_path: &Path) -> Result<Self, ReplayError> {
        Err(ReplayError::Config(
            "inotify is Linux-only: the file-inotify arm cannot run on this platform".to_owned(),
        ))
    }

    fn wait(&mut self) -> Result<(), ReplayError> {
        match self.0 {}
    }
}

fn file_arm(
    arm: Arm,
    path: &Path,
    sink: TcpStream,
    core: Core<ActivePipeline>,
    total: u64,
    ready: &SyncSender<Ready>,
    abort: &AtomicBool,
) -> Result<Option<Hists>, ReplayError> {
    if let Some(why) = arm.unsupported_here() {
        let _ = ready.send(Ready::Unavailable(why));
        return Ok(None);
    }
    let file = File::open(path)?;
    let mut watch = if arm == Arm::FileInotify {
        Some(ModifyWatch::open(path)?)
    } else {
        None
    };
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
            (Arm::FileInotify, Some(watch)) => watch.wait()?,
            _ => std::hint::spin_loop(),
        }
    }
    Ok(Some(tick.into_core().into_hists()))
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

/// Whether this host sets up an `io_uring` with a polling thread when no CPU
/// is named for it. The ring, and its thread, are gone on return.
#[cfg(feature = "compio")]
fn sqpoll_ring_without_cpu() -> bool {
    use compio::driver::{DriverType, ProactorBuilder};

    let mut proactor = ProactorBuilder::new();
    proactor.driver_type(DriverType::IoUring);
    proactor.sqpoll_idle(Duration::from_secs(10));
    compio::runtime::RuntimeBuilder::new()
        .with_proactor(proactor)
        .build()
        .is_ok_and(|rt| rt.driver_type() == DriverType::IoUring)
}

/// Why setting up the ring failed, as the arm's "unavailable" reason.
///
/// The kernel answers `EINVAL` both to a host without a polling thread and
/// to a polling-thread CPU it will not use (not possible, offline, or
/// outside the task's cpuset: v6.1 `io_sq_offload_create`).
/// `ring_without_cpu` says whether a ring that polls with no CPU named is
/// accepted, which tells the two apart; it builds a ring, so it is asked
/// only when a core was named and the error is `EINVAL`.
#[cfg(any(feature = "compio", test))]
fn ring_setup_refusal(
    core: Option<usize>,
    e: &std::io::Error,
    ring_without_cpu: impl FnOnce() -> bool,
) -> String {
    match core {
        Some(core) if e.kind() == std::io::ErrorKind::InvalidInput && ring_without_cpu() => {
            format!(
                "io_uring refused --sqpoll-core {core} ({e}): the core is offline, not a CPU of this host, or outside this process's cpuset; a polling thread with no core named was accepted"
            )
        }
        _ => format!("io_uring could not be set up on this host: {e}"),
    }
}

#[cfg(feature = "compio")]
fn compio_arm(
    sqpoll: Option<SqPoll>,
    source: TcpStream,
    sink: TcpStream,
    mut core: Core<ActivePipeline>,
    total: u64,
    ready: &SyncSender<Ready>,
    sqpoll_cpus: &mut Option<String>,
) -> Result<Option<Hists>, ReplayError> {
    use compio::buf::{IntoInner, IoBuf};
    use compio::driver::{DriverType, ProactorBuilder};
    use compio::io::{AsyncRead, AsyncWriteExt};
    use compio::BufResult;

    let mut proactor = ProactorBuilder::new();
    proactor.driver_type(DriverType::IoUring);
    let unavailable = |why: String| {
        let _ = ready.send(Ready::Unavailable(why));
        Ok(None)
    };
    if let Some(sq) = sqpoll {
        proactor.sqpoll_idle(Duration::from_secs(10));
        if let Some(core) = sq.core {
            let Ok(cpu) = u32::try_from(core) else {
                return unavailable(format!("--sqpoll-core {core} is not a CPU number"));
            };
            proactor.sqpoll_cpu(cpu);
        }
    }
    // The polling thread is the `iou-sqp` task that appears with the ring.
    let before = sqpoll.map(|_| sqpoll_threads());
    let rt = match compio::runtime::RuntimeBuilder::new()
        .with_proactor(proactor)
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            return unavailable(ring_setup_refusal(
                sqpoll.and_then(|sq| sq.core),
                &e,
                sqpoll_ring_without_cpu,
            ));
        }
    };
    if rt.driver_type() != DriverType::IoUring {
        return unavailable(format!(
            "compio fell back to the {:?} driver: io_uring is not usable on this host",
            rt.driver_type()
        ));
    }
    if let Some(before) = before {
        *sqpoll_cpus = new_sqpoll_thread_cpus(&before, sqpoll.and_then(|sq| sq.core));
    }
    rt.block_on(async move {
        ensure_loopback_peer(sink.peer_addr())?;
        let mut source = compio::net::TcpStream::from_std(source)?;
        let mut sink = compio::net::TcpStream::from_std(sink)?;
        let _ = ready.send(Ready::Go);
        while core.lines_seen() < total {
            // An io_uring read must own its buffer for the duration, so the
            // reader lends its storage and the kernel fills it in place: the
            // same single copy (kernel to reader) every other arm makes.
            let (storage, at) = core.take_reader()?;
            let BufResult(read, filled) = source.read(storage.slice(at..)).await;
            let storage = filled.into_inner();
            let n = match read {
                Ok(n) => n,
                Err(e) => {
                    core.restore_reader(storage, 0);
                    return Err(e.into());
                }
            };
            core.restore_reader(storage, n);
            if n == 0 {
                return Err(closed(&core, total));
            }
            let t0 = core.clock().now_ns();
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
    _sqpoll: Option<SqPoll>,
    _source: TcpStream,
    _sink: TcpStream,
    _core: Core<ActivePipeline>,
    _total: u64,
    ready: &SyncSender<Ready>,
    _sqpoll_cpus: &mut Option<String>,
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
    sqpoll_core: Option<usize>,
    ready: SyncSender<Ready>,
    abort: &AtomicBool,
) -> HotResult {
    if arm == Arm::CompioSqpoll {
        if let Some(why) = sqpoll_core_refusal(hot_core, sqpoll_core) {
            let _ = ready.send(Ready::Unavailable(why));
            return Ok(HotOut {
                hists: None,
                pinned: None,
                sqpoll_cpus: None,
            });
        }
    }
    // The multi-thread runtime is the unpinned baseline on purpose.
    let pinned = if arm == Arm::TokioMultiThread {
        None
    } else {
        pin(hot_core)
    };
    let mut sqpoll_cpus = None;
    let sqpoll = SqPoll {
        #[cfg(feature = "compio")]
        core: sqpoll_core,
    };
    let hists = match (arm, source) {
        (Arm::BusyPoll, Source::Tcp(s)) => Some(busy_arm(s, sink, core, total, &ready)?),
        (Arm::TokioCurrentThread, Source::Tcp(s)) => {
            Some(tokio_arm(false, s, sink, core, total, ready)?)
        }
        (Arm::TokioMultiThread, Source::Tcp(s)) => {
            Some(tokio_arm(true, s, sink, core, total, ready)?)
        }
        (Arm::Compio, Source::Tcp(s)) => {
            compio_arm(None, s, sink, core, total, &ready, &mut sqpoll_cpus)?
        }
        (Arm::CompioSqpoll, Source::Tcp(s)) => {
            compio_arm(Some(sqpoll), s, sink, core, total, &ready, &mut sqpoll_cpus)?
        }
        (_, Source::File(path)) => file_arm(arm, &path, sink, core, total, &ready, abort)?,
        (_, Source::Tcp(_)) => {
            return Err(ReplayError::Config(format!(
                "{} reads a file, not a socket",
                arm.name()
            )))
        }
    };
    Ok(HotOut {
        hists,
        pinned,
        sqpoll_cpus,
    })
}

/// The open-loop feeder. Line `i` is due at `start + (i + 1) * interval` on
/// the run's clock, whatever happened to the lines before it: a feeder that
/// falls behind sends late lines at once rather than pushing the schedule
/// back. Each line gets two stamps before its write: when it was due
/// (`scheduled`) and when the write actually began (`actual`). Sleeps while
/// the next line is far off and spins the last stretch, so the schedule is
/// not at the mercy of timer slack.
fn feed<W: Write>(
    mut sink: W,
    lines: &[String],
    interval: Duration,
    clock: Clock,
    stamps: &FeedStamps,
    core: Option<usize>,
) -> Result<W, ReplayError> {
    const SPIN_NS: u64 = 200_000;
    pin(core);
    let interval_ns = u64::try_from(interval.as_nanos()).unwrap_or(u64::MAX);
    let mut due = clock.now_ns();
    for ((line, scheduled), actual) in lines
        .iter()
        .cycle()
        .zip(stamps.scheduled.iter())
        .zip(stamps.actual.iter())
    {
        due = due.saturating_add(interval_ns);
        let now = loop {
            let now = clock.now_ns();
            if now >= due {
                break now;
            }
            if due - now > SPIN_NS {
                std::thread::sleep(Duration::from_nanos(due - now - SPIN_NS / 2));
            } else {
                std::hint::spin_loop();
            }
        };
        scheduled.store(due, Ordering::Relaxed);
        actual.store(now, Ordering::Relaxed);
        sink.write_all(line.as_bytes())?;
    }
    Ok(sink)
}

/// The feeder's two stamps per line, shared with the hot thread.
struct FeedStamps {
    scheduled: Arc<[AtomicU64]>,
    actual: Arc<[AtomicU64]>,
}

impl FeedStamps {
    fn new(lines: usize) -> Self {
        let zeros = || (0..lines).map(|_| AtomicU64::new(0)).collect();
        Self {
            scheduled: zeros(),
            actual: zeros(),
        }
    }
}

fn join<T>(h: std::thread::JoinHandle<T>, what: &str) -> Result<T, ReplayError> {
    h.join()
        .map_err(|_| ReplayError::Thread(format!("{what} thread panicked")))
}

/// With a header-scan parser, every this-many-th line is re-parsed by the
/// strict parser after its `t2` (see [`Core::with_strict_validation`]).
const VALIDATE_EVERY: u64 = 16;

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
    let stamps = Arc::new(FeedStamps::new(total_lines));
    let sink = Sink::spawn_on(cfg.sink_bind, cfg.feeder_core)?;
    let sink_stream = sink.connect()?;
    let core = Core::new(active_pipeline(), clock, cfg.warmup, line_capacity)
        .with_feed_stamps(Arc::clone(&stamps.actual))
        .with_schedule_stamps(Arc::clone(&stamps.scheduled))
        .with_strict_validation(match active_parser_mode() {
            // The strict parser is its own validator.
            ParserMode::Strict => 0,
            ParserMode::HeaderScan => VALIDATE_EVERY,
        });

    let (feed_end, source, tail_path) = open_source(arm, cfg)?;

    let abort = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = sync_channel(1);
    let hot = {
        let abort = Arc::clone(&abort);
        let (hot_core, sqpoll_core) = (cfg.hot_core, cfg.sqpoll_core);
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
                    sqpoll_core,
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
    checked(arm, total, hot_result?, sink_stats?)
}

/// A round counts only if what reached the sink is what was fed and no
/// scanned line failed the strict check.
fn checked(
    arm: Arm,
    total: u64,
    hot: HotOut,
    sink: SinkStats,
) -> Result<RoundOutcome, ReplayError> {
    let HotOut {
        hists,
        pinned: hot_pinned,
        sqpoll_cpus,
    } = hot;
    let Some(hists) = hists else {
        return Err(ReplayError::Thread(format!(
            "{} reported ready and then produced nothing",
            arm.name()
        )));
    };
    if hists.scan_strict_mismatches > 0 {
        return Err(ReplayError::ScanStrictMismatch {
            arm: arm.name(),
            mismatches: hists.scan_strict_mismatches,
            validated: hists.validated,
        });
    }
    if sink.orders != total {
        return Err(ReplayError::Mismatch {
            arm: arm.name(),
            fed: total,
            orders: sink.orders,
        });
    }
    Ok(RoundOutcome::Ran(Box::new(RoundResult {
        hists,
        sink,
        fed: total,
        hot_pinned,
        sqpoll_cpus,
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
    /// See [`RoundResult::sqpoll_cpus`]; the last round's.
    pub sqpoll_cpus: Option<String>,
    /// Why the arm could not run here, if it could not.
    pub unavailable: Option<String>,
}

/// Runs `rounds` rounds, each going through `arms` in the order given, so
/// every arm sees the same machine under the same load: round 1 of every arm,
/// then round 2 of every arm, and so on. An arm that is unavailable is
/// skipped from then on and reported as such. [`run_shuffled`] is the same
/// with a random order per round, and is what the binary uses.
pub fn run_interleaved(
    arms: &[Arm],
    rounds: u32,
    cfg: &RoundConfig,
) -> Result<Vec<ArmReport>, ReplayError> {
    Ok(run_rounds(arms, rounds, cfg, None)?.arms)
}

/// What one round did besides measure.
#[derive(Debug, Clone)]
pub struct RoundLog {
    /// The order the arms ran in.
    pub order: Vec<Arm>,
    /// `/proc/loadavg` just before the round: the 1, 5 and 15 minute load,
    /// then runnable/total scheduling entities. Kept as the kernel printed it.
    pub load_before: String,
    /// `/proc/loadavg` just after it.
    pub load_after: String,
}

/// A whole run: per-arm results and a log of each round.
#[derive(Debug)]
pub struct Run {
    /// The seed the arm orders came from.
    pub seed: u64,
    /// One merged report per arm, in the order the arms were asked for.
    pub arms: Vec<ArmReport>,
    /// One entry per round.
    pub rounds: Vec<RoundLog>,
}

/// Like [`run_interleaved`], but every round runs the arms in its own random
/// order drawn from `seed`, so no arm always follows the same neighbour or
/// always runs first on a cold machine. The same seed gives the same orders.
/// The machine's load is read before and after each round.
pub fn run_shuffled(
    arms: &[Arm],
    rounds: u32,
    cfg: &RoundConfig,
    seed: u64,
) -> Result<Run, ReplayError> {
    run_rounds(arms, rounds, cfg, Some(seed))
}

fn loadavg() -> String {
    std::fs::read_to_string("/proc/loadavg")
        .map_or_else(|_| "unavailable".to_owned(), |s| s.trim().to_owned())
}

/// `SplitMix64`: small, seedable and good enough to shuffle a handful of arms.
fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn run_rounds(
    arms: &[Arm],
    rounds: u32,
    cfg: &RoundConfig,
    seed: Option<u64>,
) -> Result<Run, ReplayError> {
    let mut reports: Vec<ArmReport> = arms
        .iter()
        .map(|&arm| ArmReport {
            arm,
            rounds_run: 0,
            hists: Hists::default(),
            orders: 0,
            hot_pinned: None,
            sqpoll_cpus: None,
            unavailable: None,
        })
        .collect();
    let mut rng = seed.unwrap_or(0);
    let mut logs = Vec::new();
    for _ in 0..rounds {
        let mut order: Vec<usize> = (0..reports.len()).collect();
        if seed.is_some() {
            // Fisher-Yates.
            for i in (1..order.len()).rev() {
                let bound = u64::try_from(i + 1).unwrap_or(u64::MAX);
                let j = usize::try_from(splitmix64(&mut rng) % bound).unwrap_or(0);
                order.swap(i, j);
            }
        }
        let load_before = loadavg();
        for &i in &order {
            let report = &mut reports[i];
            if report.unavailable.is_some() {
                continue;
            }
            match run_round(report.arm, cfg)? {
                RoundOutcome::Ran(r) => {
                    report.rounds_run += 1;
                    report.hists.merge(&r.hists);
                    report.orders += r.sink.orders;
                    report.hot_pinned = r.hot_pinned;
                    report.sqpoll_cpus = r.sqpoll_cpus;
                }
                RoundOutcome::Unavailable(why) => report.unavailable = Some(why),
            }
        }
        logs.push(RoundLog {
            order: order.iter().map(|&i| reports[i].arm).collect(),
            load_before,
            load_after: loadavg(),
        });
    }
    Ok(Run {
        seed: seed.unwrap_or(0),
        arms: reports,
        rounds: logs,
    })
}

#[cfg(test)]
mod tests {
    use super::ring_setup_refusal;
    use std::cell::Cell;
    use std::io::{Error, ErrorKind};

    const EINVAL: i32 = 22;
    const ENOMEM: i32 = 12;

    /// The message, and how many times the probe ring was asked for.
    fn refusal(core: Option<usize>, errno: i32, ring_without_cpu: bool) -> (String, u32) {
        let asked = Cell::new(0);
        let e = Error::from_raw_os_error(errno);
        let why = ring_setup_refusal(core, &e, || {
            asked.set(asked.get() + 1);
            ring_without_cpu
        });
        (why, asked.get())
    }

    #[test]
    fn einval_is_the_kernels_invalid_input() {
        assert_eq!(
            Error::from_raw_os_error(EINVAL).kind(),
            ErrorKind::InvalidInput
        );
        assert_ne!(
            Error::from_raw_os_error(ENOMEM).kind(),
            ErrorKind::InvalidInput
        );
    }

    #[test]
    fn a_named_core_is_blamed_when_a_ring_without_it_is_accepted() {
        let (why, asked) = refusal(Some(99), EINVAL, true);
        assert!(
            why.starts_with("io_uring refused --sqpoll-core 99 ("),
            "{why}"
        );
        assert!(why.contains("os error 22"), "{why}");
        assert!(
            why.ends_with("a polling thread with no core named was accepted"),
            "{why}"
        );
        assert_eq!(asked, 1);
    }

    #[test]
    fn a_host_with_no_polling_thread_is_not_blamed_on_the_core() {
        let (why, asked) = refusal(Some(99), EINVAL, false);
        assert!(
            why.starts_with("io_uring could not be set up on this host: "),
            "{why}"
        );
        assert!(why.contains("os error 22"), "{why}");
        assert!(!why.contains("--sqpoll-core"), "{why}");
        assert_eq!(asked, 1, "only the probe ring tells the two EINVALs apart");
    }

    #[test]
    fn an_error_other_than_einval_is_not_blamed_on_the_core_and_asks_for_no_ring() {
        let (why, asked) = refusal(Some(99), ENOMEM, true);
        assert!(
            why.starts_with("io_uring could not be set up on this host: "),
            "{why}"
        );
        assert!(why.contains("os error 12"), "{why}");
        assert!(!why.contains("--sqpoll-core"), "{why}");
        assert_eq!(asked, 0);
    }

    #[test]
    fn with_no_core_named_the_host_is_the_reason_and_no_ring_is_asked_for() {
        for errno in [EINVAL, ENOMEM] {
            let (why, asked) = refusal(None, errno, true);
            assert!(
                why.starts_with("io_uring could not be set up on this host: "),
                "{why}"
            );
            assert!(!why.contains("--sqpoll-core"), "{why}");
            assert_eq!(asked, 0, "errno {errno}");
        }
    }
}
