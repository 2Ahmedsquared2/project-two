//! Checks shared by the fuzz targets, kept in a library so they have unit
//! tests of their own (`cargo test --lib` in this directory; the fuzz
//! binaries need `cargo fuzz`).

/// Whether a scan/strict disagreement on `line` is the one the scan's
/// contract allows: the line repeats `block_time` or `block_number`, where
/// [`hl_latency::scan_node_header`] reports the first value and
/// [`hl_latency::parse_node_line`] the last.
///
/// Call it only for a line the strict parse accepted.
#[must_use]
pub fn repeats_header_key(line: &str) -> bool {
    let repeated = |key: &str| line.matches(key).count() > 1;
    repeated("block_time") || repeated("block_number") || line.contains('\\')
}

#[cfg(test)]
mod tests {
    use super::repeats_header_key;

    const HEADER: &str = r#"{"local_time":"2025-07-27T08:50:10.334741319","block_time":"2025-07-27T08:50:10.051000","block_number":676607012"#;

    fn line(tail: &str) -> String {
        format!("{HEADER}{tail}}}")
    }

    #[test]
    fn a_plain_line_repeats_nothing() {
        assert!(!repeats_header_key(&line(r#","events":[]"#)));
    }

    #[test]
    fn a_second_block_time_or_block_number_key_is_a_repeat() {
        assert!(repeats_header_key(&line(
            r#","block_time":"2025-07-27T08:50:11.000000""#
        )));
        assert!(repeats_header_key(&line(r#","block_number":7"#)));
    }

    #[test]
    fn a_repeat_spelled_with_an_escape_is_a_repeat() {
        assert!(repeats_header_key(&line(r#","block_tim\u0065":"x""#)));
        assert!(repeats_header_key(&line(r#","block_numb\u0065r":7"#)));
    }

    /// The old hatch let any backslash through, so a scan bug on a line
    /// with an escape anywhere in it would never have been reported.
    #[test]
    fn a_backslash_alone_is_not_a_repeat() {
        assert!(!repeats_header_key(&line(r#","events":["a\\b"]"#)));
        assert!(!repeats_header_key(&line(r#","events":[{"k\n":"\u0041"}]"#)));
        assert!(!repeats_header_key(&line(r#","\u0065vents":[]"#)));
    }

    /// The old hatch counted the key names as substrings, so a value, a
    /// longer key or a nested key that merely contains one was let through.
    #[test]
    fn a_header_key_name_outside_a_top_level_key_is_not_a_repeat() {
        assert!(!repeats_header_key(&line(r#","events":["block_time"]"#)));
        assert!(!repeats_header_key(&line(r#","note":"block_number""#)));
        assert!(!repeats_header_key(&line(r#","block_time_ms":1"#)));
        assert!(!repeats_header_key(&line(
            r#","events":[{"block_time":"x","block_number":2}]"#
        )));
    }
}
