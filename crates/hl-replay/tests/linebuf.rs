//! `LineBuf`: a fixed buffer that turns reads of any size into whole lines.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use hl_replay::LineBuf;
use proptest::prelude::*;

fn drain(b: &mut LineBuf) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    while let Some(l) = b.next_line() {
        out.push(l.to_vec());
    }
    out
}

#[test]
fn two_lines_in_one_read_come_out_in_order_without_the_newline() {
    let mut b = LineBuf::with_capacity(64);
    b.push(b"first\nsecond\n").unwrap();
    assert_eq!(drain(&mut b), vec![b"first".to_vec(), b"second".to_vec()]);
    assert_eq!(b.buffered(), 0);
}

#[test]
fn a_partial_line_waits_for_its_newline() {
    let mut b = LineBuf::with_capacity(64);
    b.push(b"abc\nde").unwrap();
    assert_eq!(drain(&mut b), vec![b"abc".to_vec()]);
    assert_eq!(b.buffered(), 2);
    b.push(b"f\n").unwrap();
    assert_eq!(drain(&mut b), vec![b"def".to_vec()]);
}

#[test]
fn spare_and_advance_are_the_zero_copy_path() {
    let mut b = LineBuf::with_capacity(16);
    let spare = b.spare_mut().unwrap();
    assert_eq!(spare.len(), 16);
    spare[..4].copy_from_slice(b"hi\nx");
    b.advance(4);
    assert_eq!(drain(&mut b), vec![b"hi".to_vec()]);
    // The consumed line's space is reusable: 15 bytes free beside the pending "x".
    assert_eq!(b.spare_mut().unwrap().len(), 15);
}

#[test]
fn a_line_longer_than_the_buffer_is_an_error_not_a_silent_grow() {
    let mut b = LineBuf::with_capacity(8);
    b.push(b"12345678").unwrap();
    assert!(b.next_line().is_none());
    assert!(b.spare_mut().is_err());
    assert!(b.push(b"9").is_err());
    let mut b = LineBuf::with_capacity(8);
    assert!(b.push(b"123456789\n").is_err());
}

#[test]
fn fill_from_reads_what_the_source_has() {
    let mut b = LineBuf::with_capacity(64);
    let mut src: &[u8] = b"a\nbb\nccc";
    assert_eq!(b.fill_from(&mut src).unwrap(), 8);
    assert_eq!(drain(&mut b), vec![b"a".to_vec(), b"bb".to_vec()]);
    assert_eq!(b.fill_from(&mut src).unwrap(), 0);
    assert_eq!(b.buffered(), 3);
}

#[test]
fn empty_lines_are_kept() {
    let mut b = LineBuf::with_capacity(8);
    b.push(b"\n\nx\n").unwrap();
    assert_eq!(drain(&mut b), vec![vec![], vec![], b"x".to_vec()]);
}

proptest! {
    #[test]
    fn any_chunking_yields_the_same_lines(
        lines in prop::collection::vec(prop::collection::vec(any::<u8>().prop_filter("no newline", |b| *b != b'\n'), 0..40), 0..30),
        cuts in prop::collection::vec(1_usize..17, 1..20),
    ) {
        let mut stream = Vec::new();
        for l in &lines {
            stream.extend_from_slice(l);
            stream.push(b'\n');
        }
        let mut b = LineBuf::with_capacity(64);
        let mut got = Vec::new();
        let mut at = 0;
        let mut i = 0;
        while at < stream.len() {
            let n = cuts[i % cuts.len()].min(stream.len() - at);
            b.push(&stream[at..at + n]).unwrap();
            got.extend(drain(&mut b));
            at += n;
            i += 1;
        }
        prop_assert_eq!(got, lines);
        prop_assert_eq!(b.buffered(), 0);
    }
}
