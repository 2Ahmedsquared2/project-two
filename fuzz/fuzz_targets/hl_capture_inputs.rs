//! Fuzzes the hl-capture input paths: the line splitter fed in arbitrary
//! chunks under a small cap, and the WebSocket filter on each resulting line.
//! Invariants: no panic, nothing held back beyond the cap, no emitted line
//! longer than the cap or containing a newline, and the filter's counters add
//! up.
#![no_main]

use hl_capture::{LineSplitter, WsFilter};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&cfg, rest)) = data.split_first() else {
        return;
    };
    let cap = usize::from(cfg % 64) + 1;
    let step = usize::from(cfg / 64) * 7 + 1;
    let mut splitter = LineSplitter::with_max_line(cap);
    let mut lines = Vec::new();
    for chunk in rest.chunks(step) {
        splitter.push(chunk, 0, &mut lines);
        assert!(splitter.pending() <= cap);
    }
    let mut filter = WsFilter::new();
    let mut frames = 0u64;
    for line in &lines {
        assert!(line.bytes.len() <= cap && !line.bytes.contains(&b'\n'));
        if let Ok(s) = std::str::from_utf8(&line.bytes) {
            let _ = filter.on_frame(s, 0);
            frames += 1;
        }
    }
    // Whole frames too, beyond the small cap.
    if let Ok(s) = std::str::from_utf8(rest) {
        let _ = filter.on_frame(s, 0);
        let _ = filter.on_frame(s, 0);
        frames += 2;
    }
    let st = filter.stats();
    assert_eq!(st.frames, frames);
    assert!(st.bad_frames + st.snapshots_dropped <= st.frames);
});
