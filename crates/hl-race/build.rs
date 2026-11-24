//! Declares the `kani` cfg so the `#[cfg(kani)]` proof module does not warn.
fn main() {
    println!("cargo::rustc-check-cfg=cfg(kani)");
}
