//! Build script for nRF52840 firmware

use std::env;

fn main() {
    // Tell cargo to look for memory.x in the source directory
    println!("cargo:rustc-link-search={}", env::var("CARGO_MANIFEST_DIR").unwrap());
    
    // cortex-m-rt's link.x already includes memory.x, so we don't need to
    // add it again. Just ensure the search path is correct.
    
    // Tell linker to include defmt.x
    println!("cargo:rustc-link-arg=-Tdefmt.x");
    
    // Rebuild if memory.x changes
    println!("cargo:rerun-if-changed=memory.x");
}
