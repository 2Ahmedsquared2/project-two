//! Kani harnesses for `hl-exec`, driven only through its public API.
//!
//! A passing harness proves its property for every input inside the bounds
//! written on it (orders, levels, operations or actions, value ranges), on
//! this engine's code. It says nothing about speed and nothing about
//! equivalence to the native node. See `README.md` for the list, the bounds,
//! the commands and the mutants each harness kills.
//!
//! The helpers here are plain Rust so the unit tests can run them on concrete
//! inputs; the harnesses themselves are in `proofs` (only under `cfg(kani)`).
//! The descoped harnesses (a Kani memory/time limit, not proofs) are kept in
//! `descoped`, compiled only with `cfg(kani)` and the `descoped` feature.

pub mod model;

#[cfg(kani)]
mod proofs;

#[cfg(all(kani, feature = "descoped"))]
mod descoped;
