//! Fuzzes the hl-latency parsers: node lines, public WS messages, timestamps
//! and observation files. Any panic is a bug; errors are fine.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = hl_latency::read_observations(data);
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = hl_latency::parse_naive_utc_ms(s);
        let _ = hl_latency::parse_node_line(s, 0);
        if let Ok(obs) = hl_latency::parse_ws_message(s, 0) {
            let _ = hl_latency::summarize(&hl_latency::pair(obs));
        }
    }
});
