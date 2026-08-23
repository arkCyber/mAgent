#!/bin/bash
# Build script for mAgent

set -e

echo "Building mAgent for nRF52840..."

# Set environment variables
export DEFMT_LOG=info

# Build release
cargo build --release --bin magent-app

echo "Build complete!"
echo "Binary: target/thumbv7em-none-eabihf/release/magent-app"

# Show size
cargo size --release --bin magent-app

echo "To flash to nRF52840:"
echo "  cargo flash --chip nRF52840_xxAA --release --bin magent-app"
