//! Fuzzes the hl-latency parsers: node lines, public WS messages, timestamps
//! and observation files. Any panic is a bug; errors are fine.
//!
//! The strict parsers are also compared with the old `serde_json::Value`
//! parsers (the oracle the proptests use): any difference, error text
//! included, is a bug.
#![no_main]

#[path = "../../crates/hl-latency/tests/oracle/mod.rs"]
mod oracle;

use hl_latency::{CompleteLine, Observation, Source};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = hl_latency::read_observations(data);
    // The fuzzer's bytes are the whole input, so the claim holds here.
    let scanned = hl_latency::scan_node_header(CompleteLine::assume_newline_seen(data));
    let Ok(s) = std::str::from_utf8(data) else {
        // Not UTF-8: the scan may still answer (it does not validate
        // `local_time` or the tail); it must only not panic.
        return;
    };
    let _ = hl_latency::parse_naive_utc_ms(s);

    let node = hl_latency::parse_node_line(s, 0);
    assert_eq!(node, oracle::parse_node_line(s, 0), "node line {s:?}");
    // The scan's contract: when it declines, nothing is claimed. When it
    // answers and the strict parse accepts, either they agree or the line
    // repeats a header key (first wins in the scan, last in the parse).
    if let (Some((t, n)), Ok(o)) = (scanned, &node) {
        let agree = (o.block_time_ms, o.block_number) == (t, Some(n));
        assert!(
            agree || project_two_fuzz::repeats_header_key(s),
            "scan {scanned:?} vs strict {o:?} on {s:?}"
        );
    }

    let parsed = hl_latency::parse_ws_message(s, 0);
    assert_eq!(parsed, oracle::parse_ws_message(s, 0), "ws message {s:?}");
    // The buffer-reusing parser must agree with the allocating one, and
    // leave nothing behind from an earlier message.
    let mut out = vec![Observation {
        source: Source::Node,
        block_time_ms: 0,
        block_number: None,
        recv_ns: 0,
    }];
    let into = hl_latency::parse_ws_message_into(s, 0, &mut out);
    match &parsed {
        Ok(obs) => assert!(into.is_ok() && out == *obs),
        Err(e) => assert!(into.as_ref() == Err(e) && out.is_empty()),
    }
    if let Ok(obs) = parsed {
        let _ = hl_latency::summarize(&hl_latency::pair(obs));
    }
});
