//! Arms that need a Linux-only kernel interface say so instead of failing the
//! run (or the build) on another platform.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use hl_replay::{generated_lines, run_round, Arm, LoopbackAddr, RoundConfig, RoundOutcome};

fn every_arm() -> impl Iterator<Item = Arm> {
    Arm::RUNTIME.into_iter().chain(Arm::DETECT)
}

/// The seam: which arms a platform cannot run at all, and the reason the
/// report prints. Takes the platform as an argument so the non-Linux answer
/// is checked on Linux too.
#[test]
fn inotify_is_the_only_arm_a_platform_rules_out_and_only_off_linux() {
    for arm in every_arm() {
        assert_eq!(arm.unsupported_on("linux"), None, "{arm:?}");
    }
    for os in ["macos", "windows", "freebsd"] {
        for arm in every_arm() {
            let why = arm.unsupported_on(os);
            if arm == Arm::FileInotify {
                let why = why.expect("inotify does not exist off Linux");
                assert!(why.contains("inotify is Linux-only"), "{why}");
                assert!(why.contains(os), "the reason names the platform: {why}");
            } else {
                assert_eq!(why, None, "{arm:?} on {os}");
            }
        }
    }
}

/// On the platform the tests run on, the answer is the one for that platform.
#[test]
fn unsupported_here_is_the_answer_for_this_platform() {
    for arm in every_arm() {
        assert_eq!(
            arm.unsupported_here(),
            arm.unsupported_on(std::env::consts::OS),
            "{arm:?}"
        );
    }
    if cfg!(target_os = "linux") {
        assert_eq!(Arm::FileInotify.unsupported_here(), None);
    } else {
        assert!(Arm::FileInotify.unsupported_here().is_some());
    }
}

/// A round of the inotify arm runs where inotify exists and is reported
/// unavailable, with the seam's reason, where it does not. Either way it is
/// not an error, so the other arms of a run still count.
#[test]
fn the_inotify_round_runs_or_reports_the_platform_reason() {
    let tail_dir: PathBuf =
        std::env::temp_dir().join(format!("hl-replay-platform-{}", std::process::id()));
    let cfg = RoundConfig {
        lines: Arc::new(generated_lines(64, 20)),
        measured: 100,
        warmup: 20,
        interval: Duration::from_micros(50),
        feeder_core: None,
        hot_core: None,
        tail_dir: tail_dir.clone(),
        sink_bind: LoopbackAddr::ephemeral(),
    };
    let outcome = run_round(Arm::FileInotify, &cfg).unwrap();
    match (outcome, Arm::FileInotify.unsupported_here()) {
        (RoundOutcome::Ran(r), None) => assert_eq!(r.sink.orders, 120),
        (RoundOutcome::Unavailable(why), Some(expected)) => assert_eq!(why, expected),
        (RoundOutcome::Ran(_), Some(why)) => panic!("ran although unsupported: {why}"),
        (RoundOutcome::Unavailable(why), None) => panic!("supported yet unavailable: {why}"),
    }
    // Nothing is left behind in the tail directory either way.
    assert_eq!(std::fs::read_dir(&tail_dir).unwrap().count(), 0);
}
