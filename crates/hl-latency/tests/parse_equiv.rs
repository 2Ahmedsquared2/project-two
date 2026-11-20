//! The zero-allocation parsers must agree with the old `serde_json::Value`
//! parsers (`tests/oracle`) on every input: same `Ok`, same error variant,
//! same error text.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod oracle;

use hl_latency::{
    parse_node_line, parse_ws_message, parse_ws_message_into, scan_node_header, Observation,
    ParseError, Source,
};
use proptest::prelude::*;
use proptest::sample::{select, Index};

// ---- the comparisons ----------------------------------------------------------

fn same_node(s: &str) -> Result<(), TestCaseError> {
    prop_assert_eq!(
        parse_node_line(s, 42),
        oracle::parse_node_line(s, 42),
        "input {:?}",
        s
    );
    Ok(())
}

/// A buffer that is not empty before the call: `_into` must clear it.
fn dirty() -> Vec<Observation> {
    vec![
        Observation {
            source: Source::Node,
            block_time_ms: 1,
            block_number: Some(2),
            recv_ns: 3,
        };
        3
    ]
}

fn same_ws(s: &str) -> Result<(), TestCaseError> {
    let want = oracle::parse_ws_message(s, 7);
    prop_assert_eq!(&parse_ws_message(s, 7), &want, "input {:?}", s);
    let mut buf = dirty();
    let got = parse_ws_message_into(s, 7, &mut buf);
    match want {
        Ok(v) => {
            prop_assert_eq!(got, Ok(()), "input {:?}", s);
            prop_assert_eq!(buf, v, "input {:?}", s);
        }
        Err(e) => {
            prop_assert_eq!(got, Err(e), "input {:?}", s);
            prop_assert!(buf.is_empty(), "buffer not cleared on error, input {:?}", s);
        }
    }
    Ok(())
}

fn same_both(s: &str) -> Result<(), TestCaseError> {
    same_node(s)?;
    same_ws(s)
}

/// `Some(x)` from the header scan implies the oracle accepts with `x`.
fn scan_implies_oracle(s: &str) -> Result<(), TestCaseError> {
    if let Some((t, n)) = scan_node_header(s.as_bytes()) {
        let o = oracle::parse_node_line(s, 0);
        prop_assert_eq!(
            o.as_ref().map(|o| (o.block_time_ms, o.block_number)),
            Ok((t, Some(n))),
            "input {:?}",
            s
        );
    }
    Ok(())
}

// ---- hand-picked inputs ---------------------------------------------------------

const TS: &str = "2025-07-27T08:50:10.051000";

#[test]
fn node_line_edge_cases_match_the_oracle() {
    let deep = |n: usize| format!("{}{}", "[".repeat(n), "]".repeat(n));
    let cases = [
        String::new(),
        " ".into(),
        "null".into(),
        "[]".into(),
        "7".into(),
        "{}".into(),
        "{".into(),
        r#"{"a"}"#.into(),
        r#"{"a":1,}"#.into(),
        r"{1:2}".into(),
        "{} x".into(),
        format!(r#"{{"block_time":"{TS}","block_number":5}}"#),
        format!(r#" {{ "block_number" : 5 , "block_time" : "{TS}" }} "#),
        // Last duplicate wins, whatever came first.
        format!(r#"{{"block_time":1,"block_time":"{TS}","block_number":"x","block_number":5}}"#),
        format!(r#"{{"block_time":"{TS}","block_time":1,"block_number":5}}"#),
        format!(r#"{{"block_time":"{TS}","block_number":5,"block_number":-5}}"#),
        format!(r#"{{"block_time":"bad","block_time":"{TS}","block_number":5}}"#),
        // Escapes in keys and values are resolved before comparing.
        format!(r#"{{"block\u005ftime":"{TS}","block_numbe\u0072":5}}"#),
        r#"{"block_time":"2025-07-27T08:50:1\u0030.051","block_number":5}"#.into(),
        r#"{"block_time":"2025-07-27T08:50:10\n","block_number":5}"#.into(),
        // Error precedence: block_time, then block_number, then the timestamp.
        r#"{"block_time":"bad"}"#.into(),
        r#"{"block_time":"bad","block_number":1.5}"#.into(),
        r#"{"block_time":"bad","block_number":1}"#.into(),
        r#"{"block_number":"bad"}"#.into(),
        // Numbers that are not a u64.
        format!(r#"{{"block_time":"{TS}","block_number":-0}}"#),
        format!(r#"{{"block_time":"{TS}","block_number":5.0}}"#),
        format!(r#"{{"block_time":"{TS}","block_number":5e0}}"#),
        format!(r#"{{"block_time":"{TS}","block_number":18446744073709551615}}"#),
        format!(r#"{{"block_time":"{TS}","block_number":18446744073709551616}}"#),
        format!(r#"{{"block_time":"{TS}","block_number":05}}"#),
        format!(r#"{{"block_time":"{TS}","block_number":null}}"#),
        format!(r#"{{"block_time":"{TS}","block_number":[5]}}"#),
        // Errors only a full parse of ignored values finds.
        format!(r#"{{"block_time":"{TS}","block_number":5,"events":[1e400]}}"#),
        format!(r#"{{"block_time":"{TS}","block_number":5,"events":"\ud800"}}"#),
        format!(r#"{{"block_time":"{TS}","block_number":5,"events":"\ud83d\ude00"}}"#),
        format!(r#"{{"block_time":"{TS}","block_number":5,"events":"\x"}}"#),
        format!("{{\"block_time\":\"{TS}\",\"block_number\":5,\"events\":\"\u{1}\"}}"),
        format!(r#"{{"block_time":"{TS}","block_number":5,"events":[1,]}}"#),
        format!(r#"{{"block_time":"{TS}","block_number":5,"events":tru}}"#),
        format!(
            r#"{{"block_time":"{TS}","block_number":5,"events":{}}}"#,
            deep(126)
        ),
        format!(
            r#"{{"block_time":"{TS}","block_number":5,"events":{}}}"#,
            deep(127)
        ),
        format!(
            r#"{{"block_time":"{TS}","block_number":5,"events":{}}}"#,
            deep(128)
        ),
        deep(127),
        deep(128),
        deep(129),
    ];
    for c in &cases {
        same_both(c).unwrap();
    }
}

#[test]
fn ws_edge_cases_match_the_oracle() {
    let cases = [
        r#"{"channel":"l2Book","data":{"time":5}}"#,
        r#"{"data":{"time":5},"channel":"l2Book"}"#,
        r#"{"data":[{"time":5},{"time":6},{"time":5}],"channel":"trades"}"#,
        r#"{"channel":"trades","data":[{"time":5},{"time":6},{"time":5}]}"#,
        r#"{"channel":"trades","data":[]}"#,
        r#"{"channel":"trades"}"#,
        r#"{"channel":"trades","data":{"time":5}}"#,
        r#"{"channel":"trades","data":[{"time":5},7]}"#,
        r#"{"channel":"trades","data":[{"time":5},{"time":-6},{"time":7}]}"#,
        r#"{"channel":"trades","data":[{"time":5,"time":"x"}]}"#,
        r#"{"channel":"trades","data":[{"time":"x","time":5}]}"#,
        r#"{"channel":"l2Book"}"#,
        r#"{"channel":"l2Book","data":null}"#,
        r#"{"channel":"l2Book","data":[{"time":5}]}"#,
        r#"{"channel":"l2Book","data":{"time":5,"time":6}}"#,
        r#"{"channel":"l2Book","data":{"time":5,"time":null}}"#,
        r#"{"channel":"l2Book","data":{"time":5.0}}"#,
        r#"{"channel":"l2Book","data":{"time":18446744073709551616}}"#,
        // Repeated keys: the last of each wins.
        r#"{"channel":"trades","channel":"l2Book","data":[{"time":1}],"data":{"time":2}}"#,
        r#"{"channel":"l2Book","channel":"trades","data":{"time":2},"data":[{"time":1}]}"#,
        r#"{"data":[{"time":1},{}],"channel":"trades","data":[{"time":3}]}"#,
        r#"{"data":[{"time":3}],"channel":"trades","data":[{"time":1},{}]}"#,
        r#"{"channel":"trades","data":[{"time":1}],"data":7}"#,
        r#"{"channel":"trades","channel":7,"data":7}"#,
        r#"{"channel":"pong","data":[{"time":1}]}"#,
        r#"{"data":[{"time":1}]}"#,
        r#"{"ch\u0061nnel":"tr\u0061des","d\u0061ta":[{"t\u0069me":1}]}"#,
        r#"{"channel":"Trades","data":7}"#,
        r#"[{"channel":"trades","data":7}]"#,
        r#"{"channel":"trades","data":[{"time":1}],"x":1e999}"#,
        r#"{"channel":"trades","data":[{"time":1}]} x"#,
        "7",
        "",
    ];
    for c in cases {
        same_both(c).unwrap();
    }
}

// ---- the header scan ------------------------------------------------------------

#[test]
fn header_scan_reads_the_hl_node_prefix() {
    let line = r#"{"local_time":"2025-07-27T08:50:10.334741319","block_time":"2025-07-27T08:50:10.051000","block_number":676607012,"events":[]}"#;
    assert_eq!(
        scan_node_header(line.as_bytes()),
        Some((1_753_606_210_051, 676_607_012))
    );
    let no_events = r#"{"local_time":"x","block_time":"1970-01-01T00:00:00","block_number":0}"#;
    assert_eq!(scan_node_header(no_events.as_bytes()), Some((0, 0)));
    let max = r#"{"local_time":"","block_time":"1970-01-01T00:00:01","block_number":18446744073709551615,"events":[]}"#;
    assert_eq!(scan_node_header(max.as_bytes()), Some((1000, u64::MAX)));
}

#[test]
fn header_scan_declines_anything_else() {
    let lt = r#""local_time":"2025-07-27T08:50:10.334741319""#;
    let bt = format!(r#""block_time":"{TS}""#);
    let cases = [
        String::new(),
        "{".into(),
        "{}".into(),
        format!("{{{lt}"),
        format!("{{{lt},{bt}"),
        format!(r#"{{{lt},{bt},"block_number":"#),
        // No terminator after the digits: the number may be cut short.
        format!(r#"{{{lt},{bt},"block_number":5"#),
        format!(r#" {{{lt},{bt},"block_number":5}}"#),
        format!(r#"{{ {lt},{bt},"block_number":5}}"#),
        format!(r#"{{{lt}, {bt},"block_number":5}}"#),
        format!(r#"{{{lt},{bt},"block_number": 5}}"#),
        format!(r#"{{{lt},{bt},"block_number":5 }}"#),
        format!(r#"{{{bt},{lt},"block_number":5}}"#),
        format!(r#"{{{bt},"block_number":5}}"#),
        format!(r#"{{{lt},"block_number":5,{bt}}}"#),
        format!(r#"{{{lt},{bt},"block_number":-5}}"#),
        format!(r#"{{{lt},{bt},"block_number":5.0}}"#),
        format!(r#"{{{lt},{bt},"block_number":5e3}}"#),
        format!(r#"{{{lt},{bt},"block_number":05}}"#),
        format!(r#"{{{lt},{bt},"block_number":00}}"#),
        format!(r#"{{{lt},{bt},"block_number":}}"#),
        format!(r#"{{{lt},{bt},"block_number":"5"}}"#),
        format!(r#"{{{lt},{bt},"block_number":18446744073709551616}}"#),
        format!(r#"{{{lt},{bt},"block_number":99999999999999999999999}}"#),
        format!(r#"{{{lt},"block_time":"yesterday","block_number":5}}"#),
        format!(r#"{{{lt},"block_time":"2025-07-27T08:50:1\u0030.051","block_number":5}}"#),
        format!(r#"{{"local_time":"a\"b",{bt},"block_number":5}}"#),
        format!(r#"{{"local_time":5,{bt},"block_number":5}}"#),
        format!(r#"{{"local\u005ftime":"x",{bt},"block_number":5}}"#),
    ];
    for c in &cases {
        assert_eq!(scan_node_header(c.as_bytes()), None, "accepted {c:?}");
    }
    assert_eq!(scan_node_header(b"{\"local_time\":\"\xff\xfe"), None);
}

/// A complete line ends with `}`; JSON whitespace (the newline the reader
/// saw, a `\r` before it) may follow.
#[test]
fn header_scan_accepts_trailing_whitespace_after_the_closing_brace() {
    let line = format!(r#"{{"local_time":"x","block_time":"{TS}","block_number":5,"events":[]}}"#);
    for tail in ["", "\n", "\r\n", " \t\r\n"] {
        let l = format!("{line}{tail}");
        assert_eq!(
            scan_node_header(l.as_bytes()),
            Some((1_753_606_210_051, 5)),
            "line {l:?}"
        );
    }
}

/// The end-of-line check: a line whose last non-whitespace byte is not `}`
/// is not complete, so the scan declines it and the strict parser reports
/// the error.
#[test]
fn header_scan_declines_a_truncated_line() {
    let head = format!(r#"{{"local_time":"x","block_time":"{TS}","block_number":5"#);
    let full = format!(
        r#"{head},"events":[{{"time":"{TS}","hash":"0xab","inner":{{"coin":"BTC"}}}},[1,2]]}}"#
    );
    assert_eq!(
        scan_node_header(full.as_bytes()),
        Some((1_753_606_210_051, 5))
    );
    let cuts = [
        format!("{head},"),
        format!(r#"{head},"events":"#),
        format!(r#"{head},"events":["#),
        format!(r#"{head},"events":[{{"#),
        format!(r#"{head},"events":[{{"time":"{TS}"#),
        format!(
            r#"{head},"events":[{{"time":"{TS}","hash":"0xab","inner":{{"coin":"BTC"}}}},[1,2]"#
        ),
        format!(
            r#"{head},"events":[{{"time":"{TS}","hash":"0xab","inner":{{"coin":"BTC"}}}},[1,2]]"#
        ),
        format!("{head},\"events\":[\n"),
        // Bytes after the closing brace that are not JSON whitespace.
        format!("{full}x"),
        format!("{full}\0"),
        format!("{full}\n{{"),
    ];
    for cut in &cuts {
        assert_eq!(scan_node_header(cut.as_bytes()), None, "accepted {cut:?}");
        assert!(
            matches!(parse_node_line(cut, 0), Err(ParseError::Malformed(_))),
            "strict accepted {cut:?}"
        );
    }
}

/// What the end-of-line check does not catch: a cut that lands just after a
/// nested `}` still ends with `}`. The scan answers from the prefix; the
/// strict parser rejects the line. Callers pass complete lines only (after
/// the newline is seen), so this takes a writer that stops mid-line.
#[test]
fn header_scan_answers_for_a_line_cut_right_after_a_nested_closing_brace() {
    let cut = format!(
        r#"{{"local_time":"x","block_time":"{TS}","block_number":5,"events":[{{"inner":{{"coin":"BTC"}}"#
    );
    assert_eq!(
        scan_node_header(cut.as_bytes()),
        Some((1_753_606_210_051, 5))
    );
    assert!(matches!(
        parse_node_line(&cut, 0),
        Err(ParseError::Malformed(_))
    ));
}

/// The tail between the header and the closing brace is not read: a line
/// that is corrupt after `block_number` but still ends with `}` gives the
/// header, where the strict parser gives an error.
#[test]
fn header_scan_answers_for_a_line_corrupt_after_block_number() {
    let head = format!(r#"{{"local_time":"x","block_time":"{TS}","block_number":5"#);
    let corrupt = [
        format!(r#"{head},"events":[{{"time":!!,}}]}}"#),
        format!(r#"{head},"events":[1,,2]}}"#),
        format!(r#"{head},"events":[}}"#),
        format!(r#"{head},garbage}}"#),
        format!("{head},\"events\":[\"\u{1}\"]}}"),
        format!(r#"{head}}}}}"#),
    ];
    for line in &corrupt {
        assert_eq!(
            scan_node_header(line.as_bytes()),
            Some((1_753_606_210_051, 5)),
            "line {line:?}"
        );
        assert!(
            matches!(parse_node_line(line, 0), Err(ParseError::Malformed(_))),
            "strict accepted {line:?}"
        );
    }
}

/// The first occurrence of each key is trusted: with a later duplicate
/// `block_time` or `block_number` the scan gives the first value and the
/// strict parser the last.
#[test]
fn header_scan_takes_the_first_of_duplicate_keys() {
    let head = format!(r#"{{"local_time":"x","block_time":"{TS}","block_number":5"#);
    let first = Some((1_753_606_210_051, 5));

    let dup_number = format!(r#"{head},"block_number":6}}"#);
    assert_eq!(scan_node_header(dup_number.as_bytes()), first);
    let strict = parse_node_line(&dup_number, 0).unwrap();
    assert_eq!(
        (strict.block_time_ms, strict.block_number),
        (1_753_606_210_051, Some(6))
    );

    let dup_time = format!(r#"{head},"block_time":"1970-01-01T00:00:01"}}"#);
    assert_eq!(scan_node_header(dup_time.as_bytes()), first);
    let strict = parse_node_line(&dup_time, 0).unwrap();
    assert_eq!((strict.block_time_ms, strict.block_number), (1000, Some(5)));

    let dup_both =
        format!(r#"{head},"events":[],"block_number":6,"block_time":"1970-01-01T00:00:01"}}"#);
    assert_eq!(scan_node_header(dup_both.as_bytes()), first);
    let strict = parse_node_line(&dup_both, 0).unwrap();
    assert_eq!((strict.block_time_ms, strict.block_number), (1000, Some(6)));

    // A later duplicate the strict parser rejects outright.
    let dup_bad = format!(r#"{head},"block_number":"six"}}"#);
    assert_eq!(scan_node_header(dup_bad.as_bytes()), first);
    assert!(matches!(
        parse_node_line(&dup_bad, 0),
        Err(ParseError::Malformed(_))
    ));
}

// ---- generators -------------------------------------------------------------------

/// JSON string literals (quotes included), biased to the names and values the
/// parsers look at, with escaped spellings and broken escapes.
fn string_lit() -> impl Strategy<Value = String> {
    prop_oneof![
        4 => select(vec![
            r#""block_time""#, r#""block_number""#, r#""local_time""#, r#""events""#,
            r#""channel""#, r#""data""#, r#""time""#, r#""l2Book""#, r#""trades""#, r#""pong""#,
            r#""block\u005ftime""#, r#""block_numbe\u0072""#, r#""ch\u0061nnel""#, r#""d\u0061ta""#,
            r#""t\u0069me""#, r#""l2\u0042ook""#, r#""tr\u0061des""#, r#""Time""#, r#""""#,
            r#""2025-07-27T08:50:10.051000""#, r#""2025-07-27T08:50:10""#,
            r#""2024-02-29T23:59:59.9""#, r#""2025-07-27T08:50:1\u0030.5""#,
            r#""2025-02-30T00:00:00""#, r#""yesterday""#, r#""a\nb\"c\\d\/""#,
            r#""\ud83d\ude00""#, r#""\ud800""#, r#""\udc00\ud800""#, r#""\u00""#, r#""\q""#,
        ]).prop_map(str::to_owned),
        1 => valid_ts().prop_map(|t| format!("\"{t}\"")),
        1 => "[a-zT0-9:.-]{0,24}".prop_map(|s| format!("\"{s}\"")),
        1 => any::<String>().prop_map(|s| serde_json::to_string(&s).unwrap()),
    ]
}

fn valid_ts() -> impl Strategy<Value = String> {
    (
        1970u32..=9999,
        1u32..=12,
        1u32..=28,
        0u32..24,
        0u32..60,
        0u32..60,
        "(\\.[0-9]{1,9})?",
    )
        .prop_map(|(y, mo, d, h, mi, s, f)| {
            format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}{f}")
        })
}

fn number() -> impl Strategy<Value = String> {
    prop_oneof![
        2 => (0u64..2000).prop_map(|n| n.to_string()),
        2 => any::<u64>().prop_map(|n| n.to_string()),
        1 => any::<i64>().prop_map(|n| n.to_string()),
        2 => select(vec![
            "0", "-0", "1.5", "5.0", "1e3", "1E+2", "1e400", "-1e400", "1e-400", "0.0000001",
            "18446744073709551615", "18446744073709551616", "-9223372036854775808",
            "-9223372036854775809", "123456789012345678901234567890", "1e99999999999999999999",
            "01", "1.", ".5", "-", "+1", "0x10", "NaN",
        ]).prop_map(str::to_owned),
    ]
}

fn ws() -> impl Strategy<Value = &'static str> {
    prop_oneof![6 => Just(""), 1 => select(vec![" ", "\n", "\t ", "\r\n"])]
}

fn object(fields: Vec<(String, String, &'static str)>) -> String {
    let body: Vec<String> = fields
        .into_iter()
        .map(|(k, v, w)| format!("{w}{k}{w}:{w}{v}{w}"))
        .collect();
    format!("{{{}}}", body.join(","))
}

/// Arbitrary JSON text: nested, with duplicate keys, escapes, odd numbers and
/// (rarely) invalid leaves.
fn json() -> impl Strategy<Value = String> {
    let leaf = prop_oneof![
        1 => select(vec!["null", "true", "false"]).prop_map(str::to_owned),
        3 => number(),
        3 => string_lit(),
    ];
    leaf.prop_recursive(5, 48, 6, |inner| {
        prop_oneof![
            proptest::collection::vec((inner.clone(), ws()), 0..6).prop_map(|v| {
                let body: Vec<String> = v.into_iter().map(|(x, w)| format!("{w}{x}{w}")).collect();
                format!("[{}]", body.join(","))
            }),
            proptest::collection::vec((string_lit(), inner, ws()), 0..6).prop_map(object),
        ]
    })
}

/// `value` wrapped `depth` levels deep, to cross `serde_json`'s recursion limit
/// of 128 from both sides.
fn nested() -> impl Strategy<Value = String> {
    (
        0usize..200,
        select(vec![
            ("[", "]"),
            (r#"{"a":"#, "}"),
            (r#"{"data":"#, "}"),
            (r#"[{"time":"#, "}]"),
        ]),
        select(vec![
            ("", ""),
            (r#"{"channel":"trades","data":"#, "}"),
            (r#"{"channel":"l2Book","data":"#, "}"),
            (
                r#"{"block_time":"2025-07-27T08:50:10","block_number":1,"events":"#,
                "}",
            ),
        ]),
        json(),
    )
        .prop_map(|(depth, (open, close), (pre, post), v)| {
            format!(
                "{pre}{}{v}{}{post}",
                open.repeat(depth),
                close.repeat(depth)
            )
        })
}

/// A JSON text damaged at one position: truncated, one char removed, or one
/// structural char inserted. Exercises error line/column text.
fn damaged() -> impl Strategy<Value = String> {
    (
        prop_oneof![json(), ws_frame(), node_shaped()],
        any::<Index>(),
        0u8..3,
        select(vec![
            '"', '{', '}', '[', ']', ',', ':', '\\', '0', 'e', '-', ' ', '\n', 'é',
        ]),
    )
        .prop_map(|(s, at, op, ch)| {
            let cuts: Vec<usize> = s.char_indices().map(|(i, _)| i).chain([s.len()]).collect();
            let i = cuts[at.index(cuts.len())];
            let (head, tail) = s.split_at(i);
            match op {
                0 => head.to_owned(),
                1 => format!("{head}{}", tail.chars().skip(1).collect::<String>()),
                _ => format!("{head}{ch}{tail}"),
            }
        })
}

/// What hl-node writes: `local_time`, `block_time`, `block_number`, then
/// events (random here).
fn hl_node_line() -> impl Strategy<Value = String> {
    (
        valid_ts(),
        valid_ts(),
        any::<u64>(),
        proptest::option::of(proptest::collection::vec(json(), 0..5)),
    )
        .prop_map(|(lt, bt, n, events)| {
            let head = format!(r#"{{"local_time":"{lt}","block_time":"{bt}","block_number":{n}"#);
            match events {
                Some(e) => format!(r#"{head},"events":[{}]}}"#, e.join(",")),
                None => format!("{head}}}"),
            }
        })
}

/// Node-line-like objects: the three header keys with arbitrary values, in
/// any order, repeated or missing, among other fields.
fn node_shaped() -> impl Strategy<Value = String> {
    let field = prop_oneof![
        2 => (Just(r#""block_time""#.to_owned()), prop_oneof![valid_ts().prop_map(|t| format!("\"{t}\"")), json()]),
        2 => (Just(r#""block_number""#.to_owned()), prop_oneof![number(), json()]),
        1 => (Just(r#""local_time""#.to_owned()), string_lit()),
        1 => (string_lit(), json()),
    ];
    proptest::collection::vec((field, ws()), 0..6)
        .prop_map(|v| object(v.into_iter().map(|((k, x), w)| (k, x, w)).collect()))
}

/// A valid JSON object with unique keys whose first three members use the
/// hl-node key names but arbitrary values: as close as valid input gets to
/// fooling the header scan.
fn unique_key_object() -> impl Strategy<Value = String> {
    let value = || {
        prop_oneof![
            valid_ts().prop_map(serde_json::Value::from),
            any::<String>().prop_map(serde_json::Value::from),
            any::<u64>().prop_map(serde_json::Value::from),
            any::<i64>().prop_map(serde_json::Value::from),
            Just(serde_json::json!(1.5)),
            Just(serde_json::json!([1, {"block_number": 2}])),
            Just(serde_json::Value::Null),
        ]
    };
    (
        proptest::collection::vec(
            (
                select(vec!["local_time", "block_time", "block_number"]),
                value(),
            ),
            0..=3,
        ),
        proptest::collection::btree_map("[a-e]{1,3}", value(), 0..4),
    )
        .prop_map(|(head, rest)| {
            let mut seen = std::collections::BTreeSet::new();
            let mut parts = Vec::new();
            for (k, v) in head {
                if seen.insert(k) {
                    parts.push(format!("{}:{v}", serde_json::to_string(k).unwrap()));
                }
            }
            for (k, v) in rest {
                parts.push(format!("{}:{v}", serde_json::to_string(&k).unwrap()));
            }
            format!("{{{}}}", parts.join(","))
        })
}

/// `l2Book` / `trades` frames: `channel` and `data` in either order, repeated
/// or missing, with good and bad `time` fields.
fn ws_frame() -> impl Strategy<Value = String> {
    let time_obj = || {
        proptest::collection::vec(
            prop_oneof![
                3 => (Just(r#""time""#.to_owned()), number(), ws()),
                1 => (Just(r#""time""#.to_owned()), json(), ws()),
                2 => (string_lit(), json(), ws()),
            ],
            0..4,
        )
        .prop_map(object)
    };
    let trades = proptest::collection::vec(
        prop_oneof![
            8 => time_obj(),
            1 => json(),
        ],
        0..6,
    )
    .prop_map(|v| format!("[{}]", v.join(",")));
    let field = prop_oneof![
        3 => (Just(r#""channel""#.to_owned()), prop_oneof![
            4 => select(vec![r#""l2Book""#, r#""trades""#, r#""tr\u0061des""#, r#""pong""#]).prop_map(str::to_owned),
            1 => json(),
        ]),
        2 => (Just(r#""data""#.to_owned()), time_obj()),
        2 => (Just(r#""data""#.to_owned()), trades),
        1 => (string_lit(), json()),
    ];
    proptest::collection::vec((field, ws()), 0..5)
        .prop_map(|v| object(v.into_iter().map(|((k, x), w)| (k, x, w)).collect()))
}

// ---- properties -----------------------------------------------------------------

/// 1024 cases per property by default; `PROPTEST_CASES` overrides it for a
/// longer soak.
fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1024)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]

    #[test]
    fn arbitrary_strings_match_the_oracle(s in "\\PC{0,200}") {
        same_both(&s)?;
        scan_implies_oracle(&s)?;
    }

    #[test]
    fn arbitrary_json_matches_the_oracle(s in json()) {
        same_both(&s)?;
    }

    #[test]
    fn nested_json_matches_the_oracle(s in nested()) {
        same_both(&s)?;
    }

    #[test]
    fn damaged_json_matches_the_oracle(s in damaged()) {
        same_both(&s)?;
    }

    #[test]
    fn node_shaped_objects_match_the_oracle(s in node_shaped()) {
        same_both(&s)?;
    }

    #[test]
    fn ws_frames_match_the_oracle(s in ws_frame()) {
        same_both(&s)?;
    }

    /// hl-node shaped lines: the strict parser equals the oracle, and the
    /// header scan answers and agrees with both.
    #[test]
    fn hl_node_lines_strict_and_scan_match_the_oracle(s in hl_node_line()) {
        same_node(&s)?;
        // Events are random JSON text and may be invalid: the scan does not
        // read them, so compare on the header-only line as the oracle.
        let scanned = scan_node_header(s.as_bytes());
        prop_assert!(scanned.is_some(), "no header in {:?}", s);
        let head_end = s.find(r#","events""#).unwrap_or(s.len() - 1);
        let header_only = format!("{}}}", &s[..head_end]);
        let o = oracle::parse_node_line(&header_only, 0).unwrap();
        prop_assert_eq!(scanned, Some((o.block_time_ms, o.block_number.unwrap())));
        if let Ok(full) = oracle::parse_node_line(&s, 0) {
            prop_assert_eq!(scanned, Some((full.block_time_ms, full.block_number.unwrap())));
        }
    }

    /// For any valid JSON object with unique keys, a `Some` from the scan is
    /// what the oracle returns.
    #[test]
    fn scan_some_implies_oracle_ok_on_unique_key_objects(s in unique_key_object()) {
        prop_assert!(serde_json::from_str::<serde_json::Value>(&s).is_ok(), "generator bug {:?}", s);
        scan_implies_oracle(&s)?;
        same_node(&s)?;
    }

    #[test]
    fn scan_never_panics_on_bytes(b in proptest::collection::vec(any::<u8>(), 0..120), cut in any::<Index>()) {
        let _ = scan_node_header(&b);
        let line = br#"{"local_time":"2025-07-27T08:50:10.334741319","block_time":"2025-07-27T08:50:10.051000","block_number":676607012,"events":[]}"#;
        let mut v = line[..cut.index(line.len() + 1)].to_vec();
        v.extend_from_slice(&b);
        let _ = scan_node_header(&v);
    }

    /// Whatever the bytes, a `Some` means the line's last non-whitespace
    /// byte is `}`; and cutting an hl-node line anywhere else gives `None`.
    #[test]
    fn scan_some_implies_the_line_ends_with_a_closing_brace(
        s in hl_node_line(),
        cut in any::<Index>(),
        tail in "[ \\t\\r\\n]{0,3}",
        junk in proptest::collection::vec(any::<u8>(), 0..8),
    ) {
        let mut v = s.as_bytes()[..cut.index(s.len() + 1)].to_vec();
        v.extend_from_slice(tail.as_bytes());
        let mut w = v.clone();
        w.extend_from_slice(&junk);
        for line in [&v, &w] {
            let trimmed_end = line
                .iter()
                .rposition(|c| !matches!(c, b' ' | b'\t' | b'\r' | b'\n'));
            let closed = trimmed_end.is_some_and(|i| line[i] == b'}');
            if scan_node_header(line).is_some() {
                prop_assert!(closed, "answered for {:?}", String::from_utf8_lossy(line));
            }
        }
        let mut whole = s.clone().into_bytes();
        whole.extend_from_slice(tail.as_bytes());
        prop_assert!(scan_node_header(&whole).is_some());
    }
}
