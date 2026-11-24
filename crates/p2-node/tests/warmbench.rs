//! Cold-vs-warm bench pieces.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use hl_book_ref::{Book, Px, Side};
use p2_node::replay::load_native_diffs;
use p2_node::warmbench::{apply_diffs, fixed, steps, summarize, CONFIGS};

#[test]
fn fixed_point() {
    assert_eq!(fixed("0.012", 3), Some(12));
    assert_eq!(fixed("1.5", 3), Some(1500));
    assert_eq!(fixed("100500", 0), Some(100_500));
    assert_eq!(fixed("0.0125", 3), None);
    assert_eq!(fixed(".5", 3), None);
    assert_eq!(fixed("x", 0), None);
}

#[test]
fn every_config_disables_exactly_what_it_says() {
    assert_eq!(steps("cold"), Some((false, false, false)));
    assert_eq!(steps("warm"), Some((true, true, true)));
    for c in CONFIGS.iter().filter(|c| c.starts_with("warm-no-")) {
        let (p, a, w) = steps(c).unwrap();
        assert_eq!([p, a, w].iter().filter(|x| !**x).count(), 1, "{c}");
    }
    assert_eq!(steps("bogus"), None);
}

#[test]
fn synthetic_diffs_build_the_book() {
    let (_, d) = p2_node::gen::generate(20, 4, 3);
    let diffs = load_native_diffs(d.as_bytes()).unwrap();
    let mut book = Book::new();
    let mut n = 0;
    for h in 1..=20u64 {
        n += apply_diffs(&mut book, &diffs[&h].1);
    }
    assert_eq!(n, 80);
    assert_eq!(book.order_count(), 80);
    assert!(book.best(Side::Bid).unwrap() >= Px(100_000));
}

#[test]
fn summary_rows() {
    let trials = vec![vec![100, 10, 10, 10], vec![300, 20, 20, 20]];
    let s = summarize("cold", &trials, 4);
    let first = s.lines().next().unwrap();
    // event 0 over 2 trials: p50 = 100, max = 300
    assert!(first.contains("event 0"), "{s}");
    let cols: Vec<&str> = first.split_whitespace().collect();
    assert_eq!(cols[3], "2");
    assert_eq!(cols[4], "100");
    assert_eq!(*cols.last().unwrap(), "300");
}
