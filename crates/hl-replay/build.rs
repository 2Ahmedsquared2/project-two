//! Bakes the commit the harness was built from into the binary, so every
//! report names the code that produced its numbers.

use std::path::Path;
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8(out.stdout).ok()?.trim().to_owned())
}

fn main() {
    println!("cargo:rerun-if-env-changed=HL_REPLAY_GIT_SHA");
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=Cargo.toml");
    if let Some(dir) = git(&["rev-parse", "--absolute-git-dir"]) {
        for f in ["HEAD", "index"] {
            println!(
                "cargo:rerun-if-changed={}",
                Path::new(&dir).join(f).display()
            );
        }
    }
    let sha = std::env::var("HL_REPLAY_GIT_SHA").ok().or_else(|| {
        let head = git(&["rev-parse", "HEAD"])?;
        // Tracked files only: a run's own output directory must not mark it dirty.
        let dirty =
            git(&["status", "--porcelain", "--untracked-files=no"]).is_none_or(|s| !s.is_empty());
        Some(if dirty { format!("{head}-dirty") } else { head })
    });
    println!(
        "cargo:rustc-env=HL_REPLAY_GIT_SHA={}",
        sha.unwrap_or_else(|| "unknown".to_owned())
    );
}
