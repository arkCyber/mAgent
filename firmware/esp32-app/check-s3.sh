#!/bin/bash
# Host-side compile check for the ESP32-S3 firmware (Xtensa).
#
# This is the S3 counterpart of `test-workspace.sh`'s host sweep: it does not
# flash or run anything, it only proves that the `board-s3` code paths (dual-core
# scheduling, native USB-Serial/JTAG ingress, S3-only watchdog, `config_nvs`,
# and — with `--lua` — the Lua host) still compile for the real target.
#
# Usage:
#   ./check-s3.sh                 # quad-PSRAM dev board (sdkconfig.defaults)
#   ./check-s3.sh --lua           # also compile the Lua host (still unstable
#                                 # on hardware; see docs/LUA_SCRIPTING_S3.md)
#   ./check-s3.sh --octal8mb      # N8R8 8 MB octal module profile
#
# Requires an Xtensa Rust toolchain: `+esp` from `espup install`, or a manual
# esp-rs release linked as `+esp-rust` (the script detects both). The script
# sources `$HOME/export-esp.sh` when present, which is what `espup install`
# writes.
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$DIR"

LUA=0
OCTAL=0
for arg in "$@"; do
    case "$arg" in
        --lua) LUA=1 ;;
        --octal8mb) OCTAL=1 ;;
        -h|--help)
            # `$0` may be a relative path and we already changed directory.
            sed -n '2,18p' "$DIR/check-s3.sh" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "unknown argument: $arg (try --lua or --octal8mb)" >&2
            exit 2
            ;;
    esac
done

# `espup install` writes this; it puts the Xtensa GCC and libclang on PATH.
if [ -f "$HOME/export-esp.sh" ]; then
    # shellcheck disable=SC1091
    . "$HOME/export-esp.sh"
fi

if ! command -v xtensa-esp32s3-elf-gcc >/dev/null 2>&1; then
    echo "error: xtensa-esp32s3-elf-gcc not found." >&2
    echo "       Install the esp-rs toolchain: cargo install espup && espup install" >&2
    exit 1
fi

export MCU=ESP32S3
export BINDGEN_EXTRA_CLANG_ARGS="-target xtensa-esp32s3-none-elf"
export CC
CC="$(command -v xtensa-esp32s3-elf-gcc)"
export CXX
CXX="$(command -v xtensa-esp32s3-elf-g++ || true)"
if [ -z "$CXX" ]; then
    echo "error: xtensa-esp32s3-elf-g++ not found (needed by C dependencies)" >&2
    exit 1
fi

FEATURES="board-s3,wifi,uart"
if [ "$LUA" -eq 1 ]; then
    FEATURES="$FEATURES,lua"
fi

# Memory profile and sdkconfig must agree: the quad dev board is 2 MB PSRAM
# and uses sdkconfig.defaults; the N8R8 module is 8 MB octal and uses
# sdkconfig.s3.defaults with the `s3-8mb-psram` ceiling profile.
#
# PlatformIO's idf.py only reads the standard `sdkconfig.defaults` filename
# (the C61 script swaps for the same reason), so the octal overlay is swapped
# in for the duration of the check and the tracked file restored on exit.
export ESP_IDF_SDKCONFIG_DEFAULTS="$DIR/sdkconfig.defaults"
if [ "$OCTAL" -eq 1 ]; then
    FEATURES="$FEATURES,s3-8mb-psram"
    S3_SAVED="$(mktemp "${TMPDIR:-/tmp}/magent-check-s3-quad.XXXXXX")"
    cp "$DIR/sdkconfig.defaults" "$S3_SAVED"
    restore_config() {
        cp "$S3_SAVED" "$DIR/sdkconfig.defaults" 2>/dev/null || true
        rm -f "$S3_SAVED"
    }
    trap restore_config EXIT
    cp "$DIR/sdkconfig.s3.defaults" "$DIR/sdkconfig.defaults"
fi

echo "=== ESP32-S3 check ==="
echo "  target:   xtensa-esp32s3-espidf"
echo "  features: $FEATURES"
echo "  sdkconfig: ${ESP_IDF_SDKCONFIG_DEFAULTS##*/}"
echo ""

# An Xtensa-enabled Rust toolchain is required: building `std` for an Xtensa
# target needs the esp-rs patched standard library. (Upstream `build-std`
# fails with internal `core` specialisation errors.) `espup` names it `esp`;
# an install done by hand from the esp-rs release is usually `esp-rust`.
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

RUSTC_BOOTSTRAP=1 cargo "${TOOLCHAIN[@]}" check \
    --target xtensa-esp32s3-espidf \
    --no-default-features \
    --features "$FEATURES"

echo ""
echo "S3 check passed."
