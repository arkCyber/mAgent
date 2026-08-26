#!/bin/bash
# Build the mAgent firmware for the ESP32-S3 (compile-time board switch).
#
# This uses the `board-s3` Cargo feature + the Xtensa target + the S3
# sdkconfig. Requires the ESP32-S3 toolchain:
#   cargo install espup && espup install
#
# Usage:  ./build-s3.sh [--debug]
set -euo pipefail

PROFILE="${1:-release}"
TARGET="xtensa-esp32s3-espidf"
CFG="/Users/arksong/MicroAgent/firmware/esp32-app/.cargo/config.toml"

echo "=== mAgent ESP32-S3 build (${PROFILE}) ==="
echo "  target:   ${TARGET}"
echo "  features: board-s3"

cd /Users/arksong/MicroAgent/firmware/esp32-app

# Override the C61-specific cargo-config env (real shell env takes precedence
# over cargo config `[env]` unless `force = true`).
export MCU="ESP32S3"
export ESP_IDF_SDKCONFIG_DEFAULTS="/Users/arksong/MicroAgent/firmware/esp32-app/sdkconfig.s3.defaults"

RUSTC_BOOTSTRAP=1 cargo build --target "${TARGET}" --features board-s3 --"${PROFILE}"

echo ""
echo "Build complete!"
echo "  ELF: target/${TARGET}/${PROFILE}/magent-esp32-app"
