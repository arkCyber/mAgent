#!/bin/bash
# Build the mAgent firmware for the ESP32-S3 (compile-time board switch).
#
# This uses the `board-s3` Cargo feature + the Xtensa target + the S3
# sdkconfig. Requires the ESP32-S3 toolchain:
#   cargo install espup && espup install
#
# Usage:  ./build-s3.sh [--debug] [--octal8mb] [--lua]
#
#   --octal8mb  build for the ESP32-S3-WROOM-1-N8R8 (8 MB octal PSRAM) module:
#               enables `s3-8mb-psram` and swaps in `sdkconfig.s3.defaults`.
#               Without it the quad-PSRAM 2 MB dev board profile is used.
#   --lua       include the Lua application host (still crashes Core0 on
#               hardware; use this build to capture the boot log for
#               docs/BACKLOG.md). Reads the default passive app unless an NVS
#               `main.lua` was uploaded with AT+LUAAPP.
set -euo pipefail

PROFILE=release
OCTAL=0
LUA=0
FEATURES="board-s3,wifi,uart"
for arg in "$@"; do
    case "$arg" in
        --debug) PROFILE=debug ;;
        --octal8mb) OCTAL=1 ;;
        --lua) LUA=1 ;;
        -h|--help)
            sed -n '2,16p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "unknown argument: $arg (try --debug, --octal8mb or --lua)" >&2
            exit 2
            ;;
    esac
done
if [ "$OCTAL" -eq 1 ]; then
    FEATURES="$FEATURES,s3-8mb-psram"
fi
if [ "$LUA" -eq 1 ]; then
    FEATURES="$FEATURES,lua"
fi

DIR="$(cd "$(dirname "$0")" && pwd)"
TARGET="xtensa-esp32s3-espidf"
CFG="$DIR/.cargo/config.toml"

echo "=== mAgent ESP32-S3 build (${PROFILE}) ==="
echo "  target:   ${TARGET}"
echo "  features: ${FEATURES} (BLE off)"

cd "$DIR"

# `espup install` writes this; it puts the Xtensa GCC + libclang on PATH.
if [ -f "$HOME/export-esp.sh" ]; then
    # shellcheck disable=SC1091
    . "$HOME/export-esp.sh"
fi

# Override the C61-specific cargo-config env (real shell env takes precedence
# over cargo config `[env]` unless `force = true`).
export MCU="ESP32S3"
# The connected S3 board is the QUAD-PSRAM dev board (4 MB flash, 2 MB quad
# PSRAM per eFuse), which is exactly the profile `sdkconfig.defaults` targets
# ("ESP32-S3 (quad-PSRAM dev board)"). `sdkconfig.s3.defaults` is for the
# different N8R8 module (8 MB octal PSRAM) — only pass `--octal8mb` for that
# module, or the PSRAM mode mismatches the board.
#
# Memory profile: `board-s3` intentionally does NOT imply the 8 MB
# `s3-8mb-psram` ceilings (4 MiB configurable / 2 MiB context). On this 2 MB
# quad board the default 1 MiB / 512 KiB profile is the honest one. For the
# N8R8 module build with `--features board-s3,s3-8mb-psram` and
# `sdkconfig.s3.defaults`.
export ESP_IDF_SDKCONFIG_DEFAULTS="$DIR/sdkconfig.defaults"
# PlatformIO only reads the standard `sdkconfig.defaults` filename, so the
# octal overlay is swapped in for the build and the tracked file restored.
if [ "$OCTAL" -eq 1 ]; then
    S3_SAVED="$(mktemp "${TMPDIR:-/tmp}/magent-build-s3-quad.XXXXXX")"
    cp sdkconfig.defaults "$S3_SAVED"
    restore_config() {
        cp "$S3_SAVED" sdkconfig.defaults 2>/dev/null || true
        rm -f "$S3_SAVED"
    }
    trap restore_config EXIT
    cp sdkconfig.s3.defaults sdkconfig.defaults
fi
# Xtensa bindgen: the esp-clang defaults to RISC-V; force the matching
# Xtensa target so __XTENSA__ is defined (fixes riscv/csr.h + l32r asm).
export BINDGEN_EXTRA_CLANG_ARGS="-target xtensa-esp32s3-none-elf"
# S3 is Xtensa; force C crates (secp256k1_sys etc.) to build for Xtensa, not
# the RISC-V CC set in .cargo/config.toml (else EM:RISCV objects fail to link).
# Prefer whatever the shell/espup export provides; fall back to the macOS
# PlatformIO install the repo has historically used.
if command -v xtensa-esp32s3-elf-gcc >/dev/null 2>&1; then
    export CC="$(command -v xtensa-esp32s3-elf-gcc)"
else
    export CC="/Users/arksong/.platformio/packages/toolchain-xtensa-esp-elf/bin/xtensa-esp32s3-elf-gcc"
fi
# WiFi provisioning (optional — baked into NVS on first boot if set).
# These MUST come from the environment, never from this file: it is tracked in
# git, so a literal passphrase here leaks into the repository and its history.
# Pass them inline when you need a pre-provisioned image:
#   MAGENT_WIFI_SSID=... MAGENT_WIFI_PASS=... ./build-s3.sh
# Otherwise leave them unset and provision at runtime with
# `AT+CWJAP="<ssid>","<pass>"` over UART0, or from the HTTP admin panel.
if command -v xtensa-esp32s3-elf-g++ >/dev/null 2>&1; then
    export CXX="$(command -v xtensa-esp32s3-elf-g++)"
else
    export CXX="/Users/arksong/.platformio/packages/toolchain-xtensa-esp-elf/bin/xtensa-esp32s3-elf-g++"
fi

TOOLCHAIN=()
if rustc +esp --version >/dev/null 2>&1; then
    TOOLCHAIN=(+esp)
elif rustc +esp-rust --version >/dev/null 2>&1; then
    TOOLCHAIN=(+esp-rust)
else
    echo "error: no Xtensa Rust toolchain found (looked for +esp / +esp-rust)." >&2
    echo "       Install it with: cargo install espup && espup install" >&2
    exit 1
fi
RUSTC_BOOTSTRAP=1 cargo "${TOOLCHAIN[@]}" build --target "${TARGET}" --no-default-features --features "$FEATURES" --"${PROFILE}"

echo ""
echo "Build complete!"
# Cargo's target dir lives at the workspace root, not under this member
# crate; printing a firmware-relative path sent the user to a nonexistent file.
WORKSPACE_ROOT="$(cd "$DIR/../.." && pwd)"
ELF_PATH="$WORKSPACE_ROOT/target/${TARGET}/${PROFILE}/magent-esp32-app"
echo "  ELF: ${ELF_PATH}"
echo ""
echo "Flash (link the S3 first):"
echo "  espflash flash --monitor ${ELF_PATH}"
if [ "$LUA" -eq 1 ]; then
    echo "Lua host INCLUDED. The default app is passive at boot; the known"
    echo "Core0 crash is still open. Capture the boot log (see docs/BACKLOG.md)."
else
    echo "This build omits the 'lua' feature (the Lua VM crashes Core0 on hardware)."
    echo "To build the Lua path instead: ./build-s3.sh --lua"
fi
