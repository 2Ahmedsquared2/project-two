//! Checks shared by the fuzz targets, kept in a library so they have unit
//! tests of their own (`cargo test --lib` in this directory; the fuzz
//! binaries need `cargo fuzz`).

use std::fmt;

use serde::de::{Deserialize, Deserializer, IgnoredAny, MapAccess, Visitor};

/// Whether a scan/strict disagreement on `line` is the one the scan's
/// contract allows: the line's top-level object has a second `block_time`
/// or `block_number` key, where [`hl_latency::scan_node_header`] reports the
/// first value and [`hl_latency::parse_node_line`] the last.
///
/// Keys are compared after JSON unescaping, so `"block_tim\u0065"` counts.
/// Nothing else does: not a backslash elsewhere in the line, and not a key
/// name that appears in a value, in a nested object or as part of a longer
/// key.
///
/// Call it only for a line the strict parse accepted; a line that is not a
/// JSON object gives `false`.
#[must_use]
pub fn repeats_header_key(line: &str) -> bool {
    serde_json::from_str::<HeaderKeyCounts>(line)
        .is_ok_and(|c| c.block_time > 1 || c.block_number > 1)
}

/// How many times each header key appears in the top-level object.
struct HeaderKeyCounts {
    block_time: u32,
    block_number: u32,
}

impl<'de> Deserialize<'de> for HeaderKeyCounts {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Counter;
        impl<'de> Visitor<'de> for Counter {
            type Value = HeaderKeyCounts;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a JSON object")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut counts = HeaderKeyCounts {
                    block_time: 0,
                    block_number: 0,
                };
                // An owned key: serde_json hands it over already unescaped.
                while let Some(key) = map.next_key::<String>()? {
                    map.next_value::<IgnoredAny>()?;
                    match key.as_str() {
                        "block_time" => counts.block_time += 1,
                        "block_number" => counts.block_number += 1,
                        _ => {}
                    }
                }
                Ok(counts)
            }
        }
        d.deserialize_map(Counter)
    }
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
        assert!(!repeats_header_key("[]"));
        assert!(!repeats_header_key("not json"));
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
        assert!(!repeats_header_key(&line(
            r#","events":[{"k\n":"\u0041"}]"#
        )));
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
