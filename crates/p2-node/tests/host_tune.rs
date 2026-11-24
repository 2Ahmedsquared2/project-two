//! `host/tune.sh` state handling (CI/CD review of PR #10, findings 1, 5, 6),
//! exercised against a fake sysfs/procfs tree as a non-root user through the
//! script's test seams (`P2_TUNE_ROOT`, `P2_TUNE_STATE_DIR`, `P2_TUNE_ALLOW_NONROOT`).

#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../host/tune.sh")
}

struct Fake {
    dir: PathBuf,
}

impl Fake {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("p2-tune-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        for f in ["proc/sys/net/core/busy_poll", "proc/sys/vm/nr_hugepages"] {
            let p = dir.join("root").join(f);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, "0").unwrap();
        }
        fs::create_dir_all(dir.join("state")).unwrap();
        #[allow(clippy::permissions_set_readonly_false)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(dir.join("state"), fs::Permissions::from_mode(0o700)).unwrap();
        }
        Fake { dir }
    }
    fn root(&self) -> PathBuf {
        self.dir.join("root")
    }
    fn state(&self) -> PathBuf {
        self.dir.join("state/p2-tune.state")
    }
    fn run(&self, args: &[&str]) -> Output {
        Command::new("bash")
            .arg(script())
            .args(args)
            .env("P2_TUNE_ROOT", self.root())
            .env("P2_TUNE_STATE_DIR", self.dir.join("state"))
            .env("P2_TUNE_ALLOW_NONROOT", "1")
            .output()
            .unwrap()
    }
    fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.root().join(rel)).unwrap()
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

#[test]
fn tune_then_undo_restores() {
    let f = Fake::new("roundtrip");
    let o = f.run(&["--hugepages", "8"]);
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(f.read("proc/sys/net/core/busy_poll"), "50");
    assert_eq!(f.read("proc/sys/vm/nr_hugepages"), "8");
    let o = f.run(&["--undo"]);
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(f.read("proc/sys/net/core/busy_poll"), "0");
    assert_eq!(f.read("proc/sys/vm/nr_hugepages"), "0");
    assert!(!f.state().exists());
}

/// Finding 5: a second tune must not overwrite the saved originals.
#[test]
fn second_tune_refuses_and_keeps_originals() {
    let f = Fake::new("twice");
    assert!(f.run(&[]).status.success());
    let o = f.run(&[]);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(text(&o).contains("already tuned"), "{}", text(&o));
    assert!(f.run(&["--undo"]).status.success());
    assert_eq!(f.read("proc/sys/net/core/busy_poll"), "0");
}

/// Finding 1: undo writes only to allowed sysfs/procfs paths named by the
/// script, never to an arbitrary path written into the state file.
#[test]
fn undo_refuses_a_path_outside_the_allowed_set() {
    let f = Fake::new("evil");
    let target = f.dir.join("victim");
    fs::write(&target, "keep").unwrap();
    fs::write(f.state(), format!("{}|pwned\n", target.display())).unwrap();
    let o = f.run(&["--undo"]);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert_eq!(fs::read_to_string(&target).unwrap(), "keep");
    assert!(text(&o).contains("refusing"), "{}", text(&o));
}

/// Finding 1: a state file that is a symlink (or a state dir others can
/// write) is refused.
#[test]
fn undo_refuses_symlinked_state_or_open_state_dir() {
    let f = Fake::new("symlink");
    let other = f.dir.join("other");
    fs::write(&other, "").unwrap();
    std::os::unix::fs::symlink(&other, f.state()).unwrap();
    let o = f.run(&["--undo"]);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    fs::remove_file(f.state()).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(f.dir.join("state"), fs::Permissions::from_mode(0o777)).unwrap();
    }
    let o = f.run(&[]);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(text(&o).contains("state dir"), "{}", text(&o));
}

/// Finding 6: a failed restore keeps the state file and exits non-zero.
#[test]
fn failed_undo_keeps_state_and_fails() {
    let f = Fake::new("failundo");
    assert!(f.run(&[]).status.success());
    // Make one restore target unwritable by replacing it with a directory.
    let p = f.root().join("proc/sys/net/core/busy_poll");
    fs::remove_file(&p).unwrap();
    fs::create_dir(&p).unwrap();
    let o = f.run(&["--undo"]);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(f.state().exists(), "state deleted after a failed undo");
    assert_eq!(f.read("proc/sys/vm/nr_hugepages"), "0");
}
