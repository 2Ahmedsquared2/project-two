//! `host/capture-host.sh`, the per-section host snapshot a benchmark run
//! attaches (docs/bench/METHOD.md): it runs to its last section whatever
//! shell options its caller exports, and a non-zero exit is never silent (#83).
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(target_os = "linux")]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../host/capture-host.sh")
}
fn bash() -> &'static str {
    ["/bin/bash", "/usr/bin/bash"]
        .into_iter()
        .find(|p| Path::new(p).exists())
        .expect("bash")
}
fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("hl-host-capture-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A PATH with only the six programs the script itself needs, and an `lscpu`
/// that fails with status 3. No `vmstat`, `top` or `chronyc`, so a capture
/// takes milliseconds, and one tool is known to fail on every host.
fn bare_path(dir: &Path) -> PathBuf {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    for tool in ["date", "mkdir", "cat", "ls", "grep", "basename"] {
        let real = ["/bin", "/usr/bin"]
            .into_iter()
            .map(|d| Path::new(d).join(tool))
            .find(|p| p.exists())
            .expect(tool);
        std::os::unix::fs::symlink(real, bin.join(tool)).unwrap();
    }
    let lscpu = bin.join("lscpu");
    std::fs::write(&lscpu, "#!/bin/sh\necho no cpu for you\nexit 3\n").unwrap();
    std::fs::set_permissions(&lscpu, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

fn capture(dir: &Path, shellopts: Option<&str>, bash_env: Option<&Path>) -> (Output, PathBuf) {
    let out = dir.join("capture");
    let mut c = Command::new(bash());
    c.arg(script()).arg(&out).env("PATH", bare_path(dir));
    c.env_remove("SHELLOPTS").env_remove("BASH_ENV");
    if let Some(s) = shellopts {
        c.env("SHELLOPTS", s);
    }
    if let Some(e) = bash_env {
        c.env("BASH_ENV", e);
    }
    (c.output().unwrap(), out)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Every section file a complete capture on this PATH leaves, sorted.
fn sections(out: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(out)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

// A capture that works: exit 0, one line on stdout naming the directory,
// nothing on stderr, and a tool that fails is recorded with its status.
#[test]
fn a_capture_that_works_says_only_where_it_wrote() {
    let dir = tmp("quiet");
    let (o, out) = capture(&dir, None, None);
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(text(&o.stderr), "");
    assert_eq!(
        text(&o.stdout),
        format!("capture written to {}\n", out.display())
    );
    assert_eq!(
        std::fs::read_to_string(out.join("lscpu.txt")).unwrap(),
        "$ lscpu\nno cpu for you\nexit=3\n"
    );
    assert!(out.join("imds-hint.txt").exists());
}

// #83: a caller that runs `set -e` with SHELLOPTS exported hands errexit to
// every bash it starts. The script sets its own options, so a tool that fails
// or a file that cannot be read is still recorded and the capture still runs
// to its last section; under the inherited errexit it would end right there
// with nothing on stderr.
#[test]
fn an_errexit_inherited_from_the_caller_does_not_end_the_capture_early() {
    let plain_dir = tmp("plain");
    let (_, plain) = capture(&plain_dir, None, None);
    let want = sections(&plain);
    for inherited in ["errexit", "errexit:pipefail", "errexit:nounset:pipefail"] {
        let dir = tmp("inherited");
        let (o, out) = capture(&dir, Some(inherited), None);
        assert_eq!(
            o.status.code(),
            Some(0),
            "{inherited}: stderr {:?}",
            text(&o.stderr)
        );
        assert_eq!(
            text(&o.stdout),
            format!("capture written to {}\n", out.display()),
            "{inherited}"
        );
        assert_eq!(sections(&out), want, "{inherited}");
        assert_eq!(
            std::fs::read_to_string(out.join("lscpu.txt")).unwrap(),
            "$ lscpu\nno cpu for you\nexit=3\n",
            "{inherited}"
        );
    }
}

// #83: a capture that ends non-zero says on stderr its status, its directory,
// the section it had reached and the command it ended on. Here it is ended
// from outside (a DEBUG trap, handed in through BASH_ENV, exits 9 on the
// first file a `cat_files` section reads, which is /proc/cmdline).
#[test]
fn a_capture_that_ends_non_zero_says_where_on_stderr() {
    let dir = tmp("ended");
    let env = dir.join("bash-env");
    std::fs::write(
        &env,
        "set -T\ntrap 'case \"$BASH_COMMAND\" in \"cat \\\"\\$f\\\" 2>&1\") exit 9 ;; esac' DEBUG\n",
    )
    .unwrap();
    let (o, out) = capture(&dir, None, Some(&env));
    assert_eq!(
        o.status.code(),
        Some(9),
        "the read of /proc/cmdline was not reached: {:?}",
        text(&o.stderr)
    );
    let err = text(&o.stderr);
    let last = err.lines().last().unwrap_or_default();
    assert!(
        last.starts_with(&format!(
            "capture-host.sh: exit 9; directory: {}; last section: cmdline; last command: ",
            out.display()
        )),
        "{last:?}"
    );
    assert_eq!(text(&o.stdout), "", "it did not reach its last line");
}
