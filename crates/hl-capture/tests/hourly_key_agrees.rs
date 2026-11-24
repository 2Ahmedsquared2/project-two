//! hl-latency matches a strict-pass failure to an observation by the date
//! and hour its path ends in. It cannot call [`hl_capture::hourly_key`]
//! (hl-capture depends on hl-latency), so it carries its own copy of the
//! rule; this holds the two together.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use hl_capture::hourly_key;
use hl_latency::{Observation, Source, StrictDropError, StrictFailures};
use proptest::prelude::*;

/// What hl-latency reads from `path`: drops the observation of line 0 of
/// the file `(date, hour)` exactly when the path names that file.
fn latency_key(path: &str, date: u32, hour: u8) -> Result<bool, StrictDropError> {
    let obs = Observation {
        source: Source::Node,
        block_time_ms: 5,
        block_number: Some(1),
        recv_ns: 1,
        file_date: Some(date),
        file_hour: Some(hour),
        line_offset: Some(0),
    };
    let failure = serde_json::json!({"failure": {
        "path": path, "line": 1, "offset": 0, "len": 2, "reason": "torn",
        "scan_accepted": true, "block_number": 1, "block_time_ms": 5,
    }});
    let text = format!("{failure}\n{{\"total\":{{\"failures_omitted\":0}}}}\n");
    let (kept, _) = StrictFailures::read(text.as_bytes())
        .unwrap()
        .drop_from(vec![obs])?;
    Ok(kept.is_empty())
}

fn agree(path: &str) {
    match hourly_key(Path::new(path)) {
        Some((date, hour)) => {
            assert_eq!(latency_key(path, date, hour), Ok(true), "{path:?}");
            // Another hour of the same day is another file.
            assert_eq!(
                latency_key(path, date, (hour + 1) % 24),
                Ok(false),
                "{path:?}"
            );
        }
        None => assert_eq!(
            latency_key(path, 20_250_727, 8),
            Err(StrictDropError::FailureWithoutFile {
                path: path.to_owned()
            }),
            "{path:?}"
        ),
    }
}

#[test]
fn both_read_the_same_date_and_hour_from_a_path() {
    for path in [
        "/data/hourly/20250727/8",
        "/data/hourly/20250727/08",
        "/data/hourly/20250727/23",
        "/data/hourly/20250727/24",
        "/data/hourly/20250727/008",
        "/data/hourly/2025072/8",
        "/data/hourly/202507270/8",
        "/data/hourly/20250727/8/",
        "/data/hourly/20250727/+8",
        "/data/hourly/2025072a/8",
        "20250727/8",
        "./20250727/8",
        "00000000/0",
        "99999999/00",
        "8",
        "/8",
        "",
        "f",
    ] {
        agree(path);
    }
}

proptest! {
    #[test]
    fn both_agree_on_arbitrary_paths(
        root in "(/[a-z]{0,3}){0,2}",
        date in "[0-9+a]{7,9}",
        hour in "[0-9+a]{0,3}",
        tail in "(/)?",
    ) {
        agree(&format!("{root}/{date}/{hour}{tail}"));
        agree(&format!("{date}/{hour}"));
    }
}
