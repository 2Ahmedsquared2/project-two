//! Fuzzes the hl-latency parsers: node lines, public WS messages, timestamps
//! and observation files. Any panic is a bug; errors are fine.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = hl_latency::read_observations(data);
    let _ = hl_latency::scan_node_header(data);
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = hl_latency::parse_naive_utc_ms(s);
        let _ = hl_latency::parse_node_line(s, 0);
        let parsed = hl_latency::parse_ws_message(s, 0);
        // The buffer-reusing parser must agree with the allocating one, and
        // leave nothing behind from an earlier message.
        let mut out = vec![hl_latency::Observation {
            source: hl_latency::Source::Node,
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
    }
});
