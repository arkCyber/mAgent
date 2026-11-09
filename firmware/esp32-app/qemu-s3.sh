#!/bin/bash
# Run the ESP32-S3 firmware under Espressif's QEMU (`esp32s3` machine).
#
# This is the no-hardware debugging path for the S3, including the Lua host
# (whose on-device Core0 crash is still being root-caused). It builds a
# **release** image with Wi-Fi disabled and — by default — PSRAM disabled,
# because QEMU's esp32s3 machine has no PSRAM model:
#
#   E (…) quad_psram: PSRAM chip is not connected, or wrong PSRAM line mode
#   E cpu_start: Failed to init external RAM!
#
# With `--psram` the tracked sdkconfig is used as-is (for a QEMU fork that
# models PSRAM); the default temporarily swaps in `CONFIG_SPIRAM=n`.
#
# Usage:
#   ./qemu-s3.sh                 # build + run, Lua host included, no PSRAM
#   ./qemu-s3.sh --no-build      # reuse the last release ELF
#   ./qemu-s3.sh --timeout 60    # seconds before QEMU is killed (default 25)
#   ./qemu-s3.sh --psram         # keep CONFIG_SPIRAM=y
#   ./qemu-s3.sh --gdb           # pause at reset with a GDB server (:1234)
#
# Requirements:
#   * the esp-rs Xtensa toolchain (`+esp` or `+esp-rust`; see check-s3.sh)
#   * `espflash` (image creation) and `qemu-system-xtensa`
#     (`python $IDF_PATH/tools/idf_tools.py install qemu-xtensa`)
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$DIR"

NO_BUILD=0
KEEP_PSRAM=0
GDB=0
TIMEOUT=25
while [ $# -gt 0 ]; do
    case "$1" in
        --no-build) NO_BUILD=1 ;;
        --psram) KEEP_PSRAM=1 ;;
        --gdb) GDB=1 ;;
        --timeout) shift; TIMEOUT="${1:?--timeout needs seconds}" ;;
        -h|--help) sed -n '2,25p' "$DIR/qemu-s3.sh" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown argument: $1 (try --no-build, --psram, --gdb, --timeout N)" >&2; exit 2 ;;
    esac
    shift
done

if [ -f "$HOME/export-esp.sh" ]; then
    # shellcheck disable=SC1091
    . "$HOME/export-esp.sh"
fi

QEMU="$(command -v qemu-system-xtensa || true)"
if [ -z "$QEMU" ]; then
    QEMU="$(find "$HOME/.espressif/tools/qemu-xtensa" -name qemu-system-xtensa -type f 2>/dev/null | head -1)"
fi
if [ -z "$QEMU" ]; then
    echo "error: qemu-system-xtensa not found." >&2
    echo "       Install it: python \$IDF_PATH/tools/idf_tools.py install qemu-xtensa" >&2
    exit 1
fi
if ! command -v espflash >/dev/null 2>&1; then
    echo "error: espflash not found (needed to merge the flash image)." >&2
    exit 1
fi
if ! command -v xtensa-esp32s3-elf-gcc >/dev/null 2>&1; then
    echo "error: xtensa-esp32s3-elf-gcc not found — install espup (see check-s3.sh)." >&2
    exit 1
fi

TOOLCHAIN=()
if rustc +esp --version >/dev/null 2>&1; then
    TOOLCHAIN=(+esp)
elif rustc +esp-rust --version >/dev/null 2>&1; then
    TOOLCHAIN=(+esp-rust)
else
    echo "error: no Xtensa Rust toolchain found (+esp / +esp-rust)." >&2
    exit 1
fi

export MCU=ESP32S3
export BINDGEN_EXTRA_CLANG_ARGS="-target xtensa-esp32s3-none-elf"
export CC="$(command -v xtensa-esp32s3-elf-gcc)"
export CXX="$(command -v xtensa-esp32s3-elf-g++)"
export ESP_IDF_SDKCONFIG_DEFAULTS="$DIR/sdkconfig.defaults"

# QEMU has no PSRAM model; swap in a SPIRAM=n variant for the build and
# restore the tracked file on every exit path.
if [ "$KEEP_PSRAM" -eq 0 ]; then
    S3_SAVED="$(mktemp "${TMPDIR:-/tmp}/magent-qemu-s3.XXXXXX")"
    cp sdkconfig.defaults "$S3_SAVED"
    restore() {
        cp "$S3_SAVED" sdkconfig.defaults 2>/dev/null || true
        rm -f "$S3_SAVED"
    }
    trap restore EXIT
    sed 's/^CONFIG_SPIRAM=y/CONFIG_SPIRAM=n/' "$S3_SAVED" > sdkconfig.defaults
    echo "[qemu-s3] CONFIG_SPIRAM disabled for the emulator"
fi

if [ "$NO_BUILD" -eq 0 ]; then
    echo "[qemu-s3] building release (board-s3,uart,lua; no Wi-Fi)…"
    RUSTC_BOOTSTRAP=1 cargo "${TOOLCHAIN[@]}" build --release \
        --target xtensa-esp32s3-espidf \
        --no-default-features \
        --features board-s3,uart,lua
fi

# Cargo's target dir belongs to the workspace root, not this member crate.
WORKSPACE_ROOT="$(cd "$DIR/../.." && pwd)"
TARGET_DIR="${CARGO_TARGET_DIR:-$WORKSPACE_ROOT/target}"
ELF="$TARGET_DIR/xtensa-esp32s3-espidf/release/magent-esp32-app"
# `|| true` keeps `set -o pipefail` from aborting silently when the release
# build tree does not exist yet; the explicit check below reports it.
BOOTLOADER="$(find "$TARGET_DIR/xtensa-esp32s3-espidf/release/build" \
    -path '*esp-idf/.pio/build/release*' -name bootloader.bin -type f 2>/dev/null | head -1 || true)"
if [ ! -f "$ELF" ] || [ -z "$BOOTLOADER" ]; then
    echo "error: build artifacts missing ($ELF / bootloader.bin)" >&2
    exit 1
fi

IMAGE="${TMPDIR:-/tmp}/magent-s3-qemu.bin"
echo "[qemu-s3] merging flash image…"
espflash save-image --merge --chip esp32s3 --flash-size 4mb \
    --flash-mode dio --flash-freq 80mhz \
    --bootloader "$BOOTLOADER" \
    --partition-table "$DIR/partitions.s3.csv" \
    "$ELF" "$IMAGE" >/dev/null

QEMU_ARGS=(-nographic -machine esp32s3 -no-reboot
    -drive "file=$IMAGE,if=mtd,format=raw")

if [ "$GDB" -eq 1 ]; then
    # `-s` serves GDB on tcp::1234, `-S` pauses at reset. Run in the
    # foreground: the caller drives QEMU from GDB, so the timeout watchdog
    # would be wrong here (Ctrl-C returns to the shell, and the EXIT trap
    # restores sdkconfig.defaults).
    echo "[qemu-s3] GDB mode — QEMU paused at reset, server on :1234"
    echo "  xtensa-esp32s3-elf-gdb $ELF"
    echo "  (gdb) target remote :1234"
    echo "  (gdb) continue"
    set +e
    "$QEMU" "${QEMU_ARGS[@]}" -s -S
    STATUS=$?
    set -e
    echo "[qemu-s3] QEMU exited (status $STATUS)"
    exit 0
fi

echo "[qemu-s3] booting (timeout ${TIMEOUT}s, -no-reboot)…"
# `-no-reboot` makes a panic/abort/reset loop exit QEMU immediately, so a
# crash ends the run instead of spinning. The watchdog covers a healthy boot,
# which never exits on its own.
"$QEMU" "${QEMU_ARGS[@]}" &
QPID=$!
( sleep "$TIMEOUT"; kill "$QPID" 2>/dev/null ) &
WPID=$!
set +e
wait "$QPID"
STATUS=$?
set -e
kill "$WPID" 2>/dev/null || true
echo "[qemu-s3] QEMU exited (status $STATUS)"
