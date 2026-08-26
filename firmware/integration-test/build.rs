//! Build script for the nRF52840 integration-test firmware.
//!
//! Mirrors `firmware/nrf52-app/build.rs`: `cortex-m-rt`'s generated
//! `link.x` does `INCLUDE memory.x`, and rust-lld only searches the
//! `-L` library paths (not the crate root) when resolving that include.
//! This script adds the crate directory to the linker search path so the
//! crate-local `memory.x` (the nRF52840 memory map) is found, and passes
//! `-Tdefmt.x` for the `defmt` log formatting tables.

use std::env;

fn main() {
    // Tell cargo to look for memory.x in the source directory.
    println!("cargo:rustc-link-search={}", env::var("CARGO_MANIFEST_DIR").unwrap());

    // cortex-m-rt's link.x already includes memory.x, so we don't need to
    // add it again. Just ensure the search path is correct.

    // Tell linker to include defmt.x.
    println!("cargo:rustc-link-arg=-Tdefmt.x");

    // Rebuild if memory.x changes.
    println!("cargo:rerun-if-changed=memory.x");
}
