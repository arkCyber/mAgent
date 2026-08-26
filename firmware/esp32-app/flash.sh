#!/bin/bash
# Flash the ESP32-C61 mAgent firmware (with the fixed BLE advertising + GATT).
#
# Prerequisites:
#   1. The C61 dev kit connected over USB (auto-detected / see below).
#   2. NO serial monitor is using the port (close it!).
#   3. Build first:  cd firmware/esp32-app && RUSTC_BOOTSTRAP=1 cargo build --release
#
# The full mAgent build (web3+wallet+ingress) is ~2.3 MB, so it needs a
# factory app partition >= 2.5 MB. The default crate partitions.csv gives
# factory only 1.5 MB — use the 4 MB test layout here (see /tmp) until the
# crate partition table is enlarged.
#
# Usage:  ./flash.sh [port]
set -euo pipefail

PORT="${1:-/dev/cu.usbserial-10}"
PIO="/Users/arksong/MicroAgent/target/riscv32imac-esp-espidf/release/build/esp-idf-sys-"
PIO="$(ls -d ${PIO}*/out/esp-idf/.pio/build/release | head -1)"
ELF="/Users/arksong/MicroAgent/target/riscv32imac-esp-espidf/release/magent-esp32-app"
PART="/tmp/bletest_partitions.csv"   # 4 MB factory app partition

echo "Flashing mAgent (ESP32-C61) to ${PORT}"
echo "  ELF:      ${ELF}"
echo "  bootloader: ${PIO}/bootloader.bin"
echo "  partitions: ${PART} (4 MB factory)"
echo "  - close any serial monitor on ${PORT} first!"

espflash flash "${ELF}" \
  --port "${PORT}" \
  --baud 460800 \
  --chip esp32c61 \
  --bootloader "${PIO}/bootloader.bin" \
  --partition-table "${PART}" \
  --confirm-port

echo "Flash complete. Device will reset and start BLE advertising as 'mAgent'."
echo "Verify with a BLE scan for name 'mAgent' / service 0x1850."

