//! Declares `cfg(kani)` so the Kani harness module does not trip `-D warnings`.
fn main() {
    println!("cargo::rustc-check-cfg=cfg(kani)");
}
