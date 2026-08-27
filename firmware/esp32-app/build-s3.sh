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
export ESP_IDF_SDKCONFIG_DEFAULTS="/Users/arksong/MicroAgent/firmware/esp32-app/sdkconfig.defaults"
# Xtensa bindgen: the esp-clang defaults to RISC-V; force the matching
# Xtensa target so __XTENSA__ is defined (fixes riscv/csr.h + l32r asm).
export BINDGEN_EXTRA_CLANG_ARGS="-target xtensa-esp32s3-none-elf"
# S3 is Xtensa; force C crates (secp256k1_sys etc.) to build for Xtensa, not
# the RISC-V CC set in .cargo/config.toml (else EM:RISCV objects fail to link).
export CC="/Users/arksong/.platformio/packages/toolchain-xtensa-esp-elf/bin/xtensa-esp32s3-elf-gcc"
# WiFi provisioning (baked into NVS on first boot if unset)
export MAGENT_WIFI_SSID="arkSong@iPhone"
export MAGENT_WIFI_PASS="Ark314159"
export CXX="/Users/arksong/.platformio/packages/toolchain-xtensa-esp-elf/bin/xtensa-esp32s3-elf-g++"

RUSTC_BOOTSTRAP=1 cargo +esp build --target "${TARGET}" --no-default-features --features board-s3,wifi,uart --"${PROFILE}"

echo ""
echo "Build complete!"
echo "  ELF: target/${TARGET}/${PROFILE}/magent-esp32-app"
