//! Contract of the strict pass library: what a record must be, what is
//! counted and what is listed.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead, BufReader, ErrorKind, Read};

use hl_latency::{scan_node_header, CompleteLine};
use hl_strict_pass::{check_record, Counts, Options, StrictPass};
use proptest::prelude::*;

/// A line as hl-node writes it, without its newline. The block time moves
/// one millisecond per height so time order follows height order.
fn line(height: u64) -> String {
    format!(
        r#"{{"local_time":"2025-07-27T08:50:10.334741319","block_time":"2025-07-27T08:50:{:02}.{:03}000","block_number":{height},"events":[{{"user":"0xab","raw_book_diff":{{"new":{{"sz":"1.0"}}}}}}]}}"#,
        (height / 1000) % 60,
        height % 1000
    )
}

fn file(lines: &[String]) -> Vec<u8> {
    let mut out = Vec::new();
    for l in lines {
        out.extend_from_slice(l.as_bytes());
        out.push(b'\n');
    }
    out
}

fn scan(bytes: &[u8]) -> Option<(u64, u64)> {
    scan_node_header(CompleteLine::assume_newline_seen(bytes))
}

fn pass_one(bytes: &[u8]) -> (StrictPass, hl_strict_pass::FileReport) {
    let mut pass = StrictPass::new(Options::default());
    let report = pass.check_file("f", bytes, false).unwrap();
    (pass, report)
}

#[test]
fn clean_file_passes_with_its_block_range() {
    let (pass, report) = pass_one(&file(&[line(10), line(11), line(12)]));
    let want = Counts {
        records: 3,
        ..Counts::default()
    };
    assert_eq!(report.counts, want);
    assert_eq!(pass.total(), want);
    assert_eq!(pass.failures(), []);
    assert_eq!(report.path, "f");
    assert_eq!(
        (report.first_block_number, report.last_block_number),
        (Some(10), Some(12))
    );
    let (t10, _) = scan(line(10).as_bytes()).unwrap();
    assert_eq!(
        (report.first_block_time_ms, report.last_block_time_ms),
        (Some(t10), Some(t10 + 2))
    );
}

#[test]
fn torn_line_that_still_ends_with_a_brace_fails_and_is_located() {
    // The writer tore the line inside its events: it ends with a nested `}`
    // and its newline, so the hot path's header scan stamps it.
    let whole = line(11);
    let torn = whole.strip_suffix("}]}").unwrap().to_owned();
    assert!(torn.ends_with('}'));
    let scanned = scan(torn.as_bytes()).expect("premise: the scan accepts the torn line");
    let first = line(10);
    let (pass, report) = pass_one(&file(&[first.clone(), torn.clone(), line(12)]));
    assert_eq!(report.counts.records, 3);
    assert_eq!(report.counts.failed, 1);
    assert_eq!(report.counts.scan_accepted_failed, 1);
    let [f] = pass.failures() else {
        panic!("one failure expected: {:?}", pass.failures())
    };
    assert_eq!(f.path, "f");
    assert_eq!(f.line, 2);
    assert_eq!(f.offset, u64::try_from(first.len()).unwrap() + 1);
    assert_eq!(f.len, u64::try_from(torn.len()).unwrap());
    assert!(f.scan_accepted);
    assert_eq!(
        (f.block_time_ms, f.block_number),
        (Some(scanned.0), Some(11))
    );
    assert_ne!(f.reason, "");
    // The failed record is not a height reference: 10 then 12 is a gap.
    assert_eq!(report.counts.height_gaps, 1);
    assert_eq!(report.counts.height_breaks, 1);
}

#[test]
fn torn_line_with_the_next_line_joined_on_fails() {
    let whole = line(11);
    let torn = format!("{}{}", &whole[..whole.len() - 3], line(12));
    assert!(scan(torn.as_bytes()).is_some(), "premise");
    let (pass, report) = pass_one(&file(&[torn]));
    assert_eq!((report.counts.records, report.counts.failed), (1, 1));
    assert_eq!(report.counts.scan_accepted_failed, 1);
    assert_eq!(pass.failures()[0].block_number, Some(11));
}

#[test]
fn height_breaks_are_counted_by_kind() {
    // 10, 11 ok; 11 repeat; 14 gap; 13 backward; 14 follows on but was seen.
    let lines = [10, 11, 11, 14, 13, 14].map(line);
    let (_, report) = pass_one(&file(&lines));
    let c = report.counts;
    assert_eq!((c.records, c.failed), (6, 0));
    assert_eq!(c.height_repeats, 1);
    assert_eq!(c.height_gaps, 1);
    assert_eq!(c.height_backwards, 1);
    assert_eq!(c.height_breaks, 3);
    assert_eq!(
        c.duplicate_block_numbers, 1,
        "the second 14; 11,11 is a repeat"
    );
    assert_eq!(c.block_time_backwards, 1, "14 then 13");
}

#[test]
fn heights_and_duplicates_carry_across_files() {
    let mut pass = StrictPass::new(Options::default());
    let a = pass
        .check_file("a", &file(&[line(1), line(2)])[..], false)
        .unwrap();
    let b = pass
        .check_file("b", &file(&[line(4), line(5)])[..], false)
        .unwrap();
    // An overlapping file: 2 was already seen in `a`.
    let c = pass.check_file("c", &file(&[line(2)])[..], false).unwrap();
    assert_eq!(a.counts.height_breaks, 0);
    assert_eq!((b.counts.height_gaps, b.counts.height_breaks), (1, 1));
    assert_eq!(c.counts.height_backwards, 1);
    assert_eq!(c.counts.duplicate_block_numbers, 1);
    let t = pass.total();
    assert_eq!(t.records, 5);
    assert_eq!(
        (t.height_gaps, t.height_backwards, t.height_breaks),
        (1, 1, 2)
    );
    assert_eq!(t.duplicate_block_numbers, 1);
}

#[test]
fn record_must_have_the_documented_top_level_fields() {
    let ok = line(7);
    assert_eq!(check_record(ok.as_bytes()).map(|r| r.1), Ok(7));
    let header = r#""block_time":"2025-07-27T08:50:10.051000","block_number":7"#;
    for bad in [
        String::new(),
        "[]".to_owned(),
        "{}".to_owned(),
        // No events.
        format!(r#"{{"local_time":"x",{header}}}"#),
        // events is not an array.
        format!(r#"{{"local_time":"x",{header},"events":{{}}}}"#),
        // No local_time, and local_time that is not a string.
        format!(r#"{{{header},"events":[]}}"#),
        format!(r#"{{"local_time":1,{header},"events":[]}}"#),
        // block_number that is not a u64; block_time that is not a time.
        r#"{"local_time":"x","block_time":"2025-07-27T08:50:10.051000","block_number":-1,"events":[]}"#.to_owned(),
        r#"{"local_time":"x","block_time":"yesterday","block_number":1,"events":[]}"#.to_owned(),
        // Two values on one line.
        format!("{ok}{ok}"),
        // Valid header, damaged tail that ends with a brace.
        format!(r#"{{"local_time":"x",{header},"events":[nul}}"#),
    ] {
        assert!(check_record(bad.as_bytes()).is_err(), "accepted {bad:?}");
    }
    assert!(check_record(b"\xff\xfe").is_err());
    // A trailing carriage return is JSON whitespace.
    assert!(check_record(format!("{ok}\r").as_bytes()).is_ok());
}

#[test]
fn duplicate_key_where_scan_and_strict_disagree_fails() {
    // The scan trusts the first block_number (5); a Value parse takes the
    // last (6). hl-node never repeats a key, so the line is damaged.
    let dup = r#"{"local_time":"x","block_time":"2025-07-27T08:50:10.051000","block_number":5,"events":[],"block_number":6}"#;
    assert_eq!(scan(dup.as_bytes()).map(|s| s.1), Some(5));
    assert_eq!(
        hl_latency::parse_node_line(dup, 0).unwrap().block_number,
        Some(6)
    );
    let (pass, report) = pass_one(&file(&[dup.to_owned()]));
    assert_eq!(
        (report.counts.failed, report.counts.scan_accepted_failed),
        (1, 1)
    );
    assert_eq!(pass.failures()[0].block_number, Some(5));
    // The reason is the disagreement itself, with both readings: what
    // hl-capture stamped and what the line says. Only the scan-agreement
    // check gives it; without that check the line would fail as a repeated
    // key and this assertion would not hold.
    assert_eq!(
        pass.failures()[0].reason,
        "header scan reads block 5 at 1753606210051 ms, strict parse block 6 at 1753606210051 ms"
    );
    // The same for a repeated block_time.
    let dup = r#"{"local_time":"x","block_time":"2025-07-27T08:50:10.051000","block_number":5,"events":[],"block_time":"2025-07-27T08:50:10.052000"}"#;
    assert_eq!(
        check_record(dup.as_bytes()),
        Err(
            "header scan reads block 5 at 1753606210051 ms, strict parse block 5 at 1753606210052 ms"
                .to_owned()
        )
    );
}

#[test]
fn repeated_key_fails_even_when_both_values_agree() {
    // Scan and strict parse read the same block here, so only the
    // repeated-key refusal can fail these lines.
    let t = r#""block_time":"2025-07-27T08:50:10.051000""#;
    for (key, dup) in [
        (
            "local_time",
            format!(r#"{{"local_time":"x",{t},"block_number":5,"events":[],"local_time":"x"}}"#),
        ),
        (
            "block_time",
            format!(r#"{{"local_time":"x",{t},"block_number":5,"events":[],{t}}}"#),
        ),
        (
            "block_number",
            format!(r#"{{"local_time":"x",{t},"block_number":5,"events":[],"block_number":5}}"#),
        ),
        (
            "events",
            format!(r#"{{"local_time":"x",{t},"block_number":5,"events":[],"events":[]}}"#),
        ),
    ] {
        let strict = hl_latency::parse_node_line(&dup, 0).expect("premise: valid JSON");
        assert_eq!(
            scan(dup.as_bytes()),
            Some((strict.block_time_ms, 5)),
            "premise: scan and strict agree on {dup}"
        );
        assert_eq!(strict.block_number, Some(5));
        let reason = check_record(dup.as_bytes()).expect_err(&dup);
        assert!(
            reason.starts_with("schema: duplicate field") && reason.contains(key),
            "{key}: {reason}"
        );
    }
    // A repeated key that is not one of the four is not looked at.
    let extra = format!(r#"{{"local_time":"x",{t},"block_number":5,"events":[],"z":1,"z":2}}"#);
    assert_eq!(check_record(extra.as_bytes()).map(|r| r.1), Ok(5));
}

/// A reader whose `fill_buf` is interrupted by a signal (`EINTR`) before
/// every successful call.
struct Interrupted<R> {
    inner: R,
    interrupt_next: bool,
    interrupts: u32,
}

impl<R: BufRead> Read for Interrupted<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buf)
    }
}

impl<R: BufRead> BufRead for Interrupted<R> {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        self.interrupt_next = !self.interrupt_next;
        if self.interrupt_next {
            self.interrupts += 1;
            return Err(ErrorKind::Interrupted.into());
        }
        self.inner.fill_buf()
    }

    fn consume(&mut self, n: usize) {
        self.inner.consume(n);
    }
}

#[test]
fn an_interrupted_read_is_retried() {
    let bytes = file(&[line(1), line(2), line(3)]);
    let (_, want) = pass_one(&bytes);
    let mut reader = Interrupted {
        // Small chunks, so lines span several reads.
        inner: BufReader::with_capacity(16, &bytes[..]),
        interrupt_next: false,
        interrupts: 0,
    };
    let mut pass = StrictPass::new(Options::default());
    let report = pass
        .check_file("f", &mut reader, false)
        .expect("EINTR is not an error");
    assert!(reader.interrupts > 3, "premise: reads were interrupted");
    assert_eq!(report, want);
    assert_eq!((report.counts.records, report.counts.failed), (3, 0));
}

#[test]
fn a_read_error_other_than_an_interrupt_is_returned() {
    struct Broken;
    impl Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(ErrorKind::PermissionDenied.into())
        }
    }
    impl BufRead for Broken {
        fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
            Err(ErrorKind::PermissionDenied.into())
        }
        fn consume(&mut self, _: usize) {}
    }
    let err = StrictPass::new(Options::default())
        .check_file("f", Broken, false)
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::PermissionDenied);
}

#[test]
fn file_without_a_trailing_newline_fails_unless_allowed() {
    let mut bytes = file(&[line(1)]);
    bytes.extend_from_slice(line(2).as_bytes());
    let (pass, report) = pass_one(&bytes);
    assert_eq!(report.counts.unterminated_tail, 1);
    assert_eq!((report.counts.records, report.counts.failed), (2, 1));
    let f = &pass.failures()[0];
    assert_eq!(f.line, 2);
    assert_eq!(f.reason, "unterminated");
    // hl-capture never stamps a line whose newline it has not seen.
    assert!(!f.scan_accepted);

    let mut pass = StrictPass::new(Options::default());
    let report = pass.check_file("f", &bytes[..], true).unwrap();
    assert_eq!(report.counts.unterminated_tail, 1);
    assert_eq!((report.counts.records, report.counts.failed), (1, 0));
    assert_eq!(pass.failures(), []);
    assert_eq!(report.last_block_number, Some(1));
}

#[test]
fn oversized_line_fails_and_the_lines_after_it_are_still_checked() {
    let first = line(1);
    // Longer than its neighbours, and one byte over the cap.
    let long = format!("{}{}", line(2), " ".repeat(10));
    let mut pass = StrictPass::new(Options {
        max_line: long.len() - 1,
        ..Options::default()
    });
    let report = pass
        .check_file(
            "f",
            &file(&[first.clone(), long.clone(), line(3)])[..],
            false,
        )
        .unwrap();
    assert_eq!((report.counts.records, report.counts.failed), (3, 1));
    let f = &pass.failures()[0];
    assert_eq!(f.reason, "oversized");
    assert_eq!((f.line, f.len), (2, u64::try_from(long.len()).unwrap()));
    assert_eq!(f.offset, u64::try_from(first.len()).unwrap() + 1);
    assert_eq!(report.last_block_number, Some(3));
}

#[test]
fn listed_failures_are_capped_but_counts_are_not() {
    let bad = vec!["{".to_owned(); 5];
    let mut pass = StrictPass::new(Options {
        max_failures: 2,
        ..Options::default()
    });
    let report = pass.check_file("f", &file(&bad)[..], false).unwrap();
    assert_eq!(report.counts.failed, 5);
    assert_eq!(pass.failures().len(), 2);
    assert_eq!(pass.failures_omitted(), 3);
    assert_eq!(pass.failures()[1].line, 2);
}

proptest! {
    /// Valid lines always pass, and the break counters match a direct count.
    #[test]
    fn valid_lines_pass_and_breaks_match_a_direct_count(
        heights in proptest::collection::vec(0u64..40, 0..60),
    ) {
        let lines: Vec<String> = heights.iter().copied().map(line).collect();
        let (pass, report) = pass_one(&file(&lines));
        let c = report.counts;
        prop_assert_eq!(c.records, u64::try_from(heights.len()).unwrap());
        prop_assert_eq!(c.failed, 0);
        prop_assert_eq!(pass.failures(), []);
        let pairs = || heights.windows(2).map(|w| (w[0], w[1]));
        let count = |f: &dyn Fn(u64, u64) -> bool| {
            u64::try_from(pairs().filter(|&(a, b)| f(a, b)).count()).unwrap()
        };
        prop_assert_eq!(c.height_repeats, count(&|a, b| b == a));
        prop_assert_eq!(c.height_gaps, count(&|a, b| b > a + 1));
        prop_assert_eq!(c.height_backwards, count(&|a, b| b < a));
        prop_assert_eq!(c.height_breaks, c.height_repeats + c.height_gaps + c.height_backwards);
        let dups = (0..heights.len())
            .filter(|&i| i > 0 && heights[i] != heights[i - 1] && heights[..i].contains(&heights[i]))
            .count();
        prop_assert_eq!(c.duplicate_block_numbers, u64::try_from(dups).unwrap());
        prop_assert_eq!(report.first_block_number, heights.first().copied());
        prop_assert_eq!(report.last_block_number, heights.last().copied());
    }

    /// A line cut anywhere before its end never passes, wherever the cut
    /// lands (including right after a nested `}`).
    #[test]
    fn a_line_cut_short_never_passes(height in 0u64..1_000_000, cut in 0usize..10_000) {
        let whole = line(height);
        let cut = cut % whole.len();
        let torn = &whole.as_bytes()[..cut];
        prop_assert!(check_record(torn).is_err());
        let mut bytes = torn.to_vec();
        bytes.push(b'\n');
        let (pass, report) = pass_one(&bytes);
        prop_assert_eq!((report.counts.records, report.counts.failed), (1, 1));
        let f = &pass.failures()[0];
        prop_assert_eq!(f.scan_accepted, scan(torn).is_some());
        prop_assert_eq!(report.counts.scan_accepted_failed, u64::from(f.scan_accepted));
    }

    /// Any bytes: no panic, every newline is a record, failures are listed
    /// in file order with offsets that point at their lines.
    #[test]
    fn arbitrary_bytes_are_counted_line_by_line(
        bytes in proptest::collection::vec(prop_oneof![Just(b'\n'), Just(b'{'), Just(b'}'), any::<u8>()], 0..300),
        max_line in 1usize..64,
    ) {
        let mut pass = StrictPass::new(Options { max_line, max_failures: 1000 });
        let report = pass.check_file("f", &bytes[..], false).unwrap();
        let newlines = bytes.split(|&b| b == b'\n').count() - 1;
        let tail = usize::from(bytes.last().is_some_and(|&b| b != b'\n'));
        prop_assert_eq!(report.counts.records, u64::try_from(newlines + tail).unwrap());
        prop_assert_eq!(report.counts.unterminated_tail, u64::try_from(tail).unwrap());
        prop_assert_eq!(report.counts.failed, u64::try_from(pass.failures().len()).unwrap());
        let mut last = None;
        for f in pass.failures() {
            prop_assert!(last < Some(f.offset));
            last = Some(f.offset);
            let start = usize::try_from(f.offset).unwrap();
            let end = start + usize::try_from(f.len).unwrap();
            prop_assert!(!bytes[start..end].contains(&b'\n'));
            prop_assert!(start == 0 || bytes[start - 1] == b'\n');
            prop_assert!(end == bytes.len() || bytes[end] == b'\n');
        }
    }

    /// Reading in chunks of any size (a line then spans several `fill_buf`
    /// calls, and a chunk can end on either side of a newline) gives the
    /// report, failures and offsets of reading the bytes in one piece. The
    /// input mixes valid lines, cut lines, oversized lines and raw bytes.
    #[test]
    fn reading_in_chunks_of_any_size_gives_the_same_result(
        parts in proptest::collection::vec(
            prop_oneof![
                (0u64..50).prop_map(|h| { let mut l = line(h).into_bytes(); l.push(b'\n'); l }),
                (0u64..50, 0usize..10_000).prop_map(|(h, cut)| {
                    let l = line(h);
                    let mut l = l.as_bytes()[..cut % l.len()].to_vec();
                    l.push(b'\n');
                    l
                }),
                (0u64..50, 1usize..200).prop_map(|(h, pad)| {
                    let mut l = line(h).into_bytes();
                    l.extend(std::iter::repeat_n(b' ', pad));
                    l.push(b'\n');
                    l
                }),
                proptest::collection::vec(prop_oneof![Just(b'\n'), any::<u8>()], 0..40),
            ],
            0..12,
        ),
        capacity in 1usize..400,
        max_line in prop_oneof![1usize..64, Just(line(0).len() + 100), Just(1 << 20)],
        allow_tail in any::<bool>(),
    ) {
        let bytes = parts.concat();
        let opts = Options { max_line, max_failures: 1000 };
        let mut whole = StrictPass::new(opts);
        let want = whole.check_file("f", &bytes[..], allow_tail).unwrap();
        let mut chunked = StrictPass::new(opts);
        let got = chunked
            .check_file("f", BufReader::with_capacity(capacity, &bytes[..]), allow_tail)
            .unwrap();
        prop_assert_eq!(got, want);
        prop_assert_eq!(chunked.failures(), whole.failures());
        prop_assert_eq!(chunked.total(), whole.total());
        prop_assert_eq!(chunked.failures_omitted(), whole.failures_omitted());
    }
}
