//! The ECDSA backend choice, pinned: `secp256k1` (libsecp256k1 bindings) signs,
//! and `k256` is only the test oracle. A change of backend has to come through
//! this test, with the byte-identity proptest in `src/ecdsa.rs`.
#![allow(clippy::expect_used)]

/// The names declared in one `[section]` of this crate's manifest.
fn section_keys(section: &str) -> Vec<String> {
    let manifest = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
        .expect("read Cargo.toml");
    let mut inside = false;
    let mut keys = Vec::new();
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            inside = line == format!("[{section}]");
        } else if inside && !line.starts_with('#') {
            if let Some((key, _)) = line.split_once('=') {
                keys.push(key.trim().to_owned());
            }
        }
    }
    keys
}

#[test]
fn libsecp256k1_signs_and_k256_is_a_dev_dependency_only() {
    let deps = section_keys("dependencies");
    let dev = section_keys("dev-dependencies");
    assert!(deps.iter().any(|k| k == "secp256k1"), "deps: {deps:?}");
    assert!(!deps.iter().any(|k| k == "k256"), "deps: {deps:?}");
    assert!(dev.iter().any(|k| k == "k256"), "dev-deps: {dev:?}");
}
