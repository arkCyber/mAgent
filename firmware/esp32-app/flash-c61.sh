#!/bin/bash
# Flash the mAgent ESP32-C61 firmware (bootloader + partition table + app).
# Usage: ./flash-c61.sh [--port /dev/cu.usbserial-XXX]
set -euo pipefail

CHIP="esp32c61"
BAUD="${BAUD:-460800}"
PORT="${PORT:-/dev/cu.usbserial-10}"
if [[ "${1:-}" == "--port" && -n "${2:-}" ]]; then PORT="$2"; fi

DIR="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$DIR/../.." && pwd)"
cd "$DIR"

TGT="$REPO/target/riscv32imac-esp-espidf/release"
ELF="$TGT/magent-esp32-app"
APP_BIN="$TGT/magent-esp32-app.bin"

# 1) Build (idempotent, stable toolchain + build-std, C61 default)
cargo build --release

# 2) Locate the latest esp-idf-sys build (bootloader.bin + partitions.bin)
OUT="$(ls -dt "$REPO"/target/riscv32imac-esp-espidf/release/build/esp-idf-sys-*/out/esp-idf/.pio/build/release 2>/dev/null | head -1)"
[ -n "$OUT" ] || { echo "error: no esp-idf-sys build dir"; exit 1; }
BOOT="$OUT/bootloader.bin"
PARTS="$OUT/partitions.bin"
[ -f "$BOOT" ] || { echo "error: bootloader.bin not found"; exit 1; }
[ -f "$PARTS" ] || { echo "error: partitions.bin not found"; exit 1; }

# 3) Generate the app image
espflash save-image --chip "$CHIP" "$ELF" "$APP_BIN"

# 4) Write bootloader @0x0, partition table @0x8000, app @0x10000
echo "Flashing ${CHIP} on ${PORT} @${BAUD}:"
esptool.py --chip "$CHIP" --port "$PORT" --baud "$BAUD" write_flash 0x0 "$BOOT" 0x8000 "$PARTS" 0x10000 "$APP_BIN"

echo ""
echo "Done. Reset the board and watch the serial console (115200)."
