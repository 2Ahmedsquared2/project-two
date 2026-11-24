//! The send stage may only ever reach a loopback sink.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write;
use std::net::SocketAddr;

use hl_replay::{loopback_pair, LoopbackAddr, Sink, SinkStats};

#[test]
fn only_loopback_addresses_are_accepted() {
    for ok in ["127.0.0.1:0", "127.0.0.1:9000", "127.8.9.10:1", "[::1]:0"] {
        let a: LoopbackAddr = ok.parse().unwrap_or_else(|e| panic!("{ok}: {e}"));
        assert!(a.addr().ip().is_loopback());
    }
    for bad in [
        "8.8.8.8:443",
        "0.0.0.0:0",
        "10.0.0.1:80",
        "192.168.1.1:80",
        "[::]:0",
        "[2001:db8::1]:443",
        "[::ffff:8.8.8.8]:443",
        "localhost:80",
        "api.hyperliquid.xyz:443",
        "",
    ] {
        assert!(
            bad.parse::<LoopbackAddr>().is_err(),
            "{bad} must be refused"
        );
    }
    let public: SocketAddr = "1.1.1.1:443".parse().unwrap();
    let err = LoopbackAddr::new(public).unwrap_err().to_string();
    assert!(err.contains("loopback"), "{err}");
    assert!(LoopbackAddr::ephemeral().addr().ip().is_loopback());
}

#[test]
fn the_sink_counts_bytes_and_newline_terminated_orders() {
    let sink = Sink::spawn(LoopbackAddr::ephemeral()).unwrap();
    assert!(sink.addr().addr().ip().is_loopback());
    assert_ne!(sink.addr().addr().port(), 0);
    let mut s = sink.connect().unwrap();
    assert!(s.nodelay().unwrap());
    assert!(s.peer_addr().unwrap().ip().is_loopback());
    s.write_all(b"one\ntwo\nthree\n").unwrap();
    drop(s);
    assert_eq!(
        sink.finish().unwrap(),
        SinkStats {
            bytes: 14,
            orders: 3
        }
    );
}

#[test]
fn a_sink_nobody_connected_to_finishes_empty() {
    let sink = Sink::spawn(LoopbackAddr::ephemeral()).unwrap();
    assert_eq!(sink.finish().unwrap(), SinkStats::default());
}

#[test]
fn loopback_pair_is_connected_both_ways_on_loopback() {
    use std::io::Read;
    let (mut a, mut b) = loopback_pair().unwrap();
    assert!(a.peer_addr().unwrap().ip().is_loopback());
    assert!(b.local_addr().unwrap().ip().is_loopback());
    a.write_all(b"ping").unwrap();
    let mut buf = [0; 4];
    b.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, b"ping");
}

/// A tripwire, not a proof: the crate's sources must never name a Hyperliquid
/// host or the order endpoint.
#[test]
fn the_sources_name_no_exchange_host_or_endpoint() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = 0;
    let mut stack = vec![src];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            files += 1;
            let text = std::fs::read_to_string(&p).unwrap().to_lowercase();
            for banned in [
                "hyperliquid.xyz",
                "/exchange",
                "hyperliquid-testnet",
                "wss://",
                "https://api.",
            ] {
                assert!(!text.contains(banned), "{} names {banned}", p.display());
            }
        }
    }
    assert!(files >= 2);
}
