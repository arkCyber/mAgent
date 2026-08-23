//! mAgent dev tools.
//!
//! This crate bundles the host-only demo / benchmark / smoke-test
//! programs that were previously loose `*.rs` files at the workspace
//! root. See the package-level docs in `Cargo.toml` for the rationale.
//!
//! Each binary lives in `src/bin/<name>.rs`. Run any of them with:
//!
//! ```sh
//! cargo run -p magent-tools --bin benchmarks
//! cargo run -p magent-tools --bin algorithm-tests
//! cargo run -p magent-tools --bin integration-tests
//! cargo run -p magent-tools --bin module-integration-test
//! cargo run -p magent-tools --bin config-validation-test
//! cargo run -p magent-tools --bin e2e-agent-test
//! ```
//!
//! Or list them all at once:
//!
//! ```sh
//! cargo run -p magent-tools --bin
//! ```

// Intentionally empty — the binaries don't share code (they were
// written as standalone demos). Kept as a library so the crate has
// at least one `lib.rs` for `cargo doc` to render.
