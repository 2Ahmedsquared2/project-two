//! The send stage's only destination: a counting sink on loopback.

use std::io::Read;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use crate::ReplayError;

/// A socket address that is proven to be loopback (`127.0.0.0/8` or `::1`).
///
/// This is the only address type the harness will bind a sink to or connect
/// the send stage to. It cannot be built from a host name (nothing is ever
/// resolved) or from any routable, wildcard or IPv4-mapped address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoopbackAddr(SocketAddr);

impl LoopbackAddr {
    /// Accepts `addr` only if its IP is loopback.
    ///
    /// # Errors
    ///
    /// [`ReplayError::NotLoopback`] otherwise.
    pub fn new(addr: SocketAddr) -> Result<Self, ReplayError> {
        if addr.ip().is_loopback() {
            Ok(Self(addr))
        } else {
            Err(ReplayError::NotLoopback(addr.to_string()))
        }
    }

    /// `127.0.0.1` on a port the kernel picks.
    #[must_use]
    pub fn ephemeral() -> Self {
        Self(SocketAddr::from(([127, 0, 0, 1], 0)))
    }

    /// The address.
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.0
    }
}

impl std::str::FromStr for LoopbackAddr {
    type Err = ReplayError;

    /// Parses `ip:port`. Host names are refused: resolving one could yield
    /// anything.
    fn from_str(s: &str) -> Result<Self, ReplayError> {
        let addr: SocketAddr = s
            .parse()
            .map_err(|_| ReplayError::NotLoopback(format!("{s:?}")))?;
        Self::new(addr)
    }
}

/// Refuses a connected stream whose other end is not loopback. Every engine
/// calls this before its first write.
pub(crate) fn ensure_loopback_peer(peer: std::io::Result<SocketAddr>) -> Result<(), ReplayError> {
    LoopbackAddr::new(peer?).map(|_| ())
}

/// What the sink received.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SinkStats {
    /// Bytes read.
    pub bytes: u64,
    /// Newline-terminated bodies, one per order.
    pub orders: u64,
}

/// A thread that accepts one loopback connection and reads it to the end,
/// counting bytes and orders. It stands where the exchange would be.
#[derive(Debug)]
pub struct Sink {
    addr: LoopbackAddr,
    stop: Arc<AtomicBool>,
    thread: JoinHandle<std::io::Result<SinkStats>>,
}

impl Sink {
    /// Binds `bind` and starts the sink thread.
    pub fn spawn(bind: LoopbackAddr) -> Result<Self, ReplayError> {
        Self::spawn_on(bind, None)
    }

    /// Like [`Sink::spawn`], with the sink thread pinned to `core` so it
    /// stays off the hot core.
    pub fn spawn_on(bind: LoopbackAddr, core: Option<usize>) -> Result<Self, ReplayError> {
        let listener = TcpListener::bind(bind.addr())?;
        let addr = LoopbackAddr::new(listener.local_addr()?)?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("hl-replay-sink".to_owned())
            .spawn(move || {
                if let Some(id) = core {
                    core_affinity::set_for_current(core_affinity::CoreId { id });
                }
                let Some(mut stream) = accept_one(&listener, &stopped, || {})? else {
                    return Ok(SinkStats::default());
                };
                stream.set_nonblocking(false)?;
                let mut stats = SinkStats::default();
                let mut buf = vec![0_u8; 64 * 1024];
                loop {
                    let n = match stream.read(&mut buf) {
                        Ok(0) => return Ok(stats),
                        Ok(n) => n,
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(e) => return Err(e),
                    };
                    stats.bytes += u64::try_from(n).unwrap_or(u64::MAX);
                    for b in &buf[..n] {
                        stats.orders += u64::from(*b == b'\n');
                    }
                }
            })?;
        Ok(Self { addr, stop, thread })
    }

    /// Where the sink listens.
    #[must_use]
    pub fn addr(&self) -> LoopbackAddr {
        self.addr
    }

    /// Connects the send stage to the sink, with Nagle off.
    pub fn connect(&self) -> Result<TcpStream, ReplayError> {
        let s = TcpStream::connect(self.addr.addr())?;
        s.set_nodelay(true)?;
        ensure_loopback_peer(s.peer_addr())?;
        Ok(s)
    }

    /// Waits for the connection to close and returns what arrived. Close the
    /// sending stream first. A sink nobody connected to returns zeros.
    pub fn finish(self) -> Result<SinkStats, ReplayError> {
        self.stop.store(true, Ordering::Release);
        match self.thread.join() {
            Ok(r) => Ok(r?),
            Err(_) => Err(ReplayError::Thread("sink thread panicked".to_owned())),
        }
    }
}

/// Waits for the one connection on a non-blocking `listener`, or for `stop`.
/// `None` means stop was set and nobody had connected.
///
/// `after_would_block` runs after each `accept` that found nothing and
/// before `stop` is read. The sink passes a no-op; a test uses it to stand
/// in the gap between those two steps.
fn accept_one(
    listener: &TcpListener,
    stop: &AtomicBool,
    mut after_would_block: impl FnMut(),
) -> std::io::Result<Option<TcpStream>> {
    loop {
        match listener.accept() {
            Ok((s, _)) => return Ok(Some(s)),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                after_would_block();
                if stop.load(Ordering::Acquire) {
                    return Ok(None);
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(e) => return Err(e),
        }
    }
}

/// A connected TCP pair on `127.0.0.1` with Nagle off on both ends: the feed
/// into a socket-reading arm.
pub fn loopback_pair() -> Result<(TcpStream, TcpStream), ReplayError> {
    let listener = TcpListener::bind(LoopbackAddr::ephemeral().addr())?;
    let a = TcpStream::connect(listener.local_addr()?)?;
    let (b, _) = listener.accept()?;
    a.set_nodelay(true)?;
    b.set_nodelay(true)?;
    ensure_loopback_peer(a.peer_addr())?;
    Ok((a, b))
}

#[cfg(test)]
mod tests {
    use super::{accept_one, ensure_loopback_peer, LoopbackAddr};
    use std::io::Write;
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, Ordering};

    fn listener() -> TcpListener {
        let Ok(l) = TcpListener::bind(LoopbackAddr::ephemeral().addr()) else {
            panic!("a loopback listener");
        };
        assert!(l.set_nonblocking(true).is_ok());
        l
    }

    /// Issue #63: the accept that found nothing can predate a connection
    /// made before `finish()` set stop. That connection must still be read.
    #[test]
    fn a_connection_made_between_the_empty_accept_and_stop_is_still_accepted() {
        let l = listener();
        let Ok(addr) = l.local_addr() else {
            panic!("a local address");
        };
        let stop = AtomicBool::new(false);
        let mut clients = Vec::new();
        let got = accept_one(&l, &stop, || {
            if clients.is_empty() {
                let Ok(mut c) = TcpStream::connect(addr) else {
                    panic!("connect to the listener");
                };
                assert!(c.write_all(b"x\n").is_ok());
                clients.push(c);
                stop.store(true, Ordering::Release);
            }
        });
        assert!(
            matches!(got, Ok(Some(_))),
            "the connection was made before stop was set: {got:?}"
        );
    }

    #[test]
    fn stop_with_nobody_connected_gives_no_stream() {
        let l = listener();
        let stop = AtomicBool::new(true);
        let mut calls = 0_u32;
        let got = accept_one(&l, &stop, || calls += 1);
        assert!(matches!(got, Ok(None)), "{got:?}");
        assert_eq!(calls, 1);
    }

    #[test]
    fn a_non_loopback_peer_is_refused() {
        for bad in ["8.8.8.8:443", "10.1.2.3:80", "[2001:db8::1]:443"] {
            let Ok(addr) = bad.parse() else {
                panic!("{bad} is a socket address");
            };
            assert!(ensure_loopback_peer(Ok(addr)).is_err(), "{bad}");
        }
        let Ok(ok) = "127.0.0.1:9".parse() else {
            panic!("a socket address");
        };
        assert!(ensure_loopback_peer(Ok(ok)).is_ok());
        assert!(ensure_loopback_peer(Err(std::io::Error::other("no peer"))).is_err());
    }
}
