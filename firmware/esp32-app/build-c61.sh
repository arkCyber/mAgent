#!/bin/bash
# Build the mAgent firmware for the ESP32-C61 (compile-time board switch).
# PlatformIO idf.py only reads the standard-named sdkconfig.defaults, so we
# temporarily swap in the C61 config (sdkconfig.c61.defaults), build, then
# restore the S3 sdkconfig.defaults on exit.
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$DIR"

S3_DEFAULTS="$DIR/sdkconfig.defaults"
C61_DEFAULTS="$DIR/sdkconfig.c61.defaults"
S3_SAVED="/tmp/sdkconfig.defaults.s3.backup"

restore() { cp "$S3_SAVED" "$S3_DEFAULTS" 2>/dev/null || true; }
trap restore EXIT

# Save the S3 config, swap in the C61 config
cp "$S3_DEFAULTS" "$S3_SAVED"
cp "$C61_DEFAULTS" "$S3_DEFAULTS"

export MCU="ESP32C61"
export ESP_IDF_SDKCONFIG_DEFAULTS="$S3_DEFAULTS"
echo "=== mAgent ESP32-C61 build ==="
RUSTC_BOOTSTRAP=1 cargo build --release

echo ""
echo "Build complete! ELF: target/riscv32imac-esp-espidf/release/magent-esp32-app"
