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
echo "  features: board-s3,wifi,uart (BLE off; Lua off — Lua crashes Core0 even after I2C pin fix)"

cd /Users/arksong/MicroAgent/firmware/esp32-app

# Override the C61-specific cargo-config env (real shell env takes precedence
# over cargo config `[env]` unless `force = true`).
export MCU="ESP32S3"
# The connected S3 board is the QUAD-PSRAM dev board (4 MB flash, quad PSRAM
# per eFuse), which is exactly the profile `sdkconfig.defaults` targets
# ("ESP32-S3 (quad-PSRAM dev board)"). `sdkconfig.s3.defaults` is for the
# different N8R8 module (8 MB octal PSRAM) — do NOT use it here or the PSRAM
# mode mismatches this board.
export ESP_IDF_SDKCONFIG_DEFAULTS="/Users/arksong/MicroAgent/firmware/esp32-app/sdkconfig.defaults"
# Xtensa bindgen: the esp-clang defaults to RISC-V; force the matching
# Xtensa target so __XTENSA__ is defined (fixes riscv/csr.h + l32r asm).
export BINDGEN_EXTRA_CLANG_ARGS="-target xtensa-esp32s3-none-elf"
# S3 is Xtensa; force C crates (secp256k1_sys etc.) to build for Xtensa, not
# the RISC-V CC set in .cargo/config.toml (else EM:RISCV objects fail to link).
export CC="/Users/arksong/.platformio/packages/toolchain-xtensa-esp-elf/bin/xtensa-esp32s3-elf-gcc"
# WiFi provisioning (optional — baked into NVS on first boot if set).
# These MUST come from the environment, never from this file: it is tracked in
# git, so a literal passphrase here leaks into the repository and its history.
# Pass them inline when you need a pre-provisioned image:
#   MAGENT_WIFI_SSID=... MAGENT_WIFI_PASS=... ./build-s3.sh
# Otherwise leave them unset and provision at runtime with
# `AT+CWJAP="<ssid>","<pass>"` over UART0, or from the HTTP admin panel.
export CXX="/Users/arksong/.platformio/packages/toolchain-xtensa-esp-elf/bin/xtensa-esp32s3-elf-g++"

RUSTC_BOOTSTRAP=1 cargo +esp build --target "${TARGET}" --no-default-features --features board-s3,wifi,uart --"${PROFILE}"

echo ""
echo "Build complete!"
echo "  ELF: target/${TARGET}/${PROFILE}/magent-esp32-app"
echo ""
echo "Flash (link the S3 first):"
echo "  espflash flash --monitor target/${TARGET}/${PROFILE}/magent-esp32-app"
echo "This build omits the 'lua' feature (the Lua VM crashes Core0 on hardware),"
echo "so there are no '[lua] <driver> ok/err' self-test lines — that is expected."
echo "To build the Lua path instead, add 'lua' to the feature list on line 45."
