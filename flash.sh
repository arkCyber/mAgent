#!/bin/bash
# Flash script for mAgent

set -e

echo "Flashing mAgent to nRF52840..."

# Build first
./build.sh

# Flash to device
cargo flash --chip nRF52840_xxAA --release --bin magent-app

echo "Flash complete!"
echo "Monitor output with:"
echo "  cargo embed --chip nRF52840_xxAA"
