#!/bin/bash
# ==============================================================================
# mAgent PRODUCTION flash — Secure Boot v2 + Flash Encryption.
# ==============================================================================
# The one-time, IRREVERSIBLE provisioning path for shipping devices.
#   1. Generates (or reuses) Secure Boot v2 + Flash Encryption keys.
#   2. Builds firmware with the production overlay (`sdkconfig.prod.defaults`).
#   3. Signs bootloader + app with `espsecure.py`.
#   4. Burns eFuses with `espefuse.py` — ONLY when `--apply` is given.
#   5. Flashes signed + encrypted images with `esptool.py`.
#
# WARNING: once eFuses are burned there is NO software recovery. Run WITHOUT
# --apply first (prepares keys + images, prints steps). Back up the key dir
# (`build/secure/`) off-device before --apply.
#
# Usage: ./flash-secure.sh [--chip esp32c61] [--keydir <dir>] [--apply] [--port <port>]
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$DIR/../.." && pwd)"
cd "$DIR"

CHIP="esp32c61"
PORT="${PORT:-/dev/cu.usbserial-10}"
BAUD="${BAUD:-460800}"
APPLY=0
KEYDIR=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --chip)   CHIP="$2"; shift 2 ;;
        --port)   PORT="$2"; shift 2 ;;
        --keydir) KEYDIR="$2"; shift 2 ;;
        --apply)  APPLY=1; shift ;;
        -h|--help) sed -n '1,22p' "$DIR/flash-secure.sh" | sed 's/^# \?//'; exit 0 ;;
        *) echo "unknown arg: $1"; exit 2 ;;
    esac
done

# Locate the esptool suite (PlatformIO package or PATH).
ESPTOOL="$(command -v esptool.py || true)"
if [[ -z "$ESPTOOL" ]]; then
    ESPTOOL="$HOME/.platformio/packages/tool-esptoolpy/esptool.py"
fi
ESPTOOL_DIR="$(dirname "$ESPTOOL")"
ESPSECURE="${ESPTOOL_DIR}/espsecure.py"
ESPEFUSE="${ESPTOOL_DIR}/espefuse.py"

# Build output locations (per-chip toolchain target dir).
case "$CHIP" in
    esp32c61) TGT_DIR="riscv32imac-esp-espidf" ;;
    esp32s3)  TGT_DIR="xtensa-esp32s3-espidf" ;;
    *) echo "unsupported chip: $CHIP"; exit 2 ;;
esac
TGT="$REPO/target/$TGT_DIR/release"
ELF="$TGT/magent-esp32-app"
APP_BIN="$TGT/magent-esp32-app.bin"
OUT="$(ls -dt "$TGT"/build/esp-idf-sys-*/out/esp-idf/.pio/build/release 2>/dev/null | head -1 || true)"
if [[ -z "$OUT" ]]; then
    OUT="$(ls -dt "$REPO"/target/$TGT_DIR/release/build/esp-idf-sys-*/out/esp-idf/.pio/build/release 2>/dev/null | head -1 || true)"
fi
BOOT="$OUT/bootloader.bin"
PARTS="$OUT/partitions.bin"

KEYDIR="${KEYDIR:-$DIR/build/secure}"
mkdir -p "$KEYDIR"

# Detect whether the production overlay enables Flash Encryption. When on, the
# images must be written with esptool `--encrypt` (the flash-encryption key is
# eFuse-protected, so esptool cannot read it back and needs the key file).
FLASH_ENC=0
if grep -q '^CONFIG_SECURE_FLASH_ENC_ENABLED=y' "$DIR/sdkconfig.prod.defaults"; then
    FLASH_ENC=1
fi

echo "== mAgent Secure Boot v2 + Flash Encryption provisioning (dry-run unless --apply) =="
echo "   chip=$CHIP port=$PORT keydir=$KEYDIR apply=$APPLY flash_enc=$FLASH_ENC"
echo ""

# ---------------------------------------------------------------------------
# 1) Keys — generate once, reuse if present. MUST be backed up off-device.
# ---------------------------------------------------------------------------
if [[ ! -f "$KEYDIR/bootloader_secure_boot_signing_key.pem" ]]; then
    echo ">> [1/5] generating Secure Boot v2 signing keys"
    python3 "$ESPSECURE" generate_signing_key \
        "$KEYDIR/bootloader_secure_boot_signing_key.pem"
fi
if [[ ! -f "$KEYDIR/flash_encryption_key.bin" ]]; then
    echo ">> generating flash-encryption key"
    python3 "$ESPSECURE" generate_flash_encryption_key \
        "$KEYDIR/flash_encryption_key.bin" >/dev/null
fi
echo ">> keys ready in: $KEYDIR  (BACK THESE UP — the only way to update a field device)"
echo ""

# ---------------------------------------------------------------------------
# 2) Build with the production security overlay.
# ---------------------------------------------------------------------------
echo ">> [2/5] building firmware with Secure Boot + Flash Encryption overlay"
# ESP-IDF reads exactly ONE sdkconfig.defaults file, selected via the
# ESP_IDF_SDKCONFIG_DEFAULTS env var (the .cargo/config.toml sets it to the
# S3 dev default; we override it). Merge the per-chip base config with the
# production overlay (Secure Boot / Flash Enc / OTA-only / rollback) into a
# single temp file and point ESP_IDF_SDKCONFIG_DEFAULTS at it.
BASE_DEFAULTS=""
case "$CHIP" in
    esp32c61) BASE_DEFAULTS="$DIR/sdkconfig.c61.defaults" ;;
    esp32s3)  BASE_DEFAULTS="$DIR/sdkconfig.s3.defaults" ;;
esac
MERGED="/tmp/magent-sdkconfig.prod.merged"
cat "$BASE_DEFAULTS" "$DIR/sdkconfig.prod.defaults" > "$MERGED"
export ESP_IDF_SDKCONFIG_DEFAULTS="$MERGED"
if ! cargo build --release; then
    echo "!! build failed; merged config at $MERGED (check partition/rollback settings)"
    exit 1
fi
[ -f "$ELF" ] || { echo "error: no ELF at $ELF"; exit 1; }
[ -f "$BOOT" ] || { echo "error: no bootloader at $BOOT"; exit 1; }
[ -f "$PARTS" ] || { echo "error: no partitions at $PARTS"; exit 1; }

# Generate the app image (Xtensa needs espflash; RISC-V accepts either).
"$ESPTOOL" --chip "$CHIP" elf2image --flash_mode dio --flash_freq 80m \
    --flash_size 8MB -o "$APP_BIN" "$ELF"

# ---------------------------------------------------------------------------
# 3) Sign bootloader + app with the Secure Boot v2 key.
# ---------------------------------------------------------------------------
echo ">> [3/5] signing bootloader + app"
python3 "$ESPSECURE" sign_data --version 2 \
    --keyfile "$KEYDIR/bootloader_secure_boot_signing_key.pem" \
    -o "$BOOT.signed" "$BOOT"
python3 "$ESPSECURE" sign_data --version 2 \
    --keyfile "$KEYDIR/bootloader_secure_boot_signing_key.pem" \
    -o "$APP_BIN.signed" "$APP_BIN"

# ---------------------------------------------------------------------------
# 4) Burn eFuses (IRREVERSIBLE) — only with --apply.
# ---------------------------------------------------------------------------
echo ">> [4/5] eFuse provisioning"
if [[ "$APPLY" == "1" ]]; then
    echo "!! BURNING eFUSES — this is IRREVERSIBLE."
    read -r -p "Type 'PROVISION' to confirm: " ans
    [[ "$ans" == "PROVISION" ]] || { echo "aborted."; exit 3; }
    "$ESPTOOL" --chip "$CHIP" --port "$PORT" --baud "$BAUD" \
        --before default_reset --after hard_reset \
        write_protect_efuse flash_crypt_cnt >/dev/null
    "$ESPTOOL" --chip "$CHIP" --port "$PORT" --baud "$BAUD" \
        --after no_reset burn_efuse secure_boot_v2 1 >/dev/null
    "$ESPTOOL" --chip "$CHIP" --port "$PORT" --baud "$BAUD" \
        --after no_reset burn_key flash_encryption \
        "$KEYDIR/flash_encryption_key.bin" >/dev/null
    # NOTE: the key is burned WITHOUT `--no-protect-key`, i.e. read-protected.
    # esptool therefore cannot read it back; the encrypted write below supplies
    # it via --flash-encrypt-key.
    "$ESPTOOL" --chip "$CHIP" --port "$PORT" --baud "$BAUD" \
        --after no_reset burn_efuse FLASH_CRYPT_CNT 1 >/dev/null
    echo ">> eFuses burned."
else
    echo "!! DRY-RUN — not burning eFuses. Re-run with --apply after backing up keys."
fi

# ---------------------------------------------------------------------------
# 5) Flash the signed images.
# ---------------------------------------------------------------------------
echo ">> [5/5] flashing"
FLASH_ARGS=()
if [[ "$APPLY" == "1" && "$FLASH_ENC" == "1" ]]; then
    # Encrypt-on-write. The key is eFuse read-protected, so supply the key file.
    FLASH_ARGS+=(--encrypt --flash-encrypt-key "$KEYDIR/flash_encryption_key.bin")
    echo ">> flash encryption enabled: writing encrypted images (--encrypt)"
fi
if [[ "$APPLY" == "1" ]]; then
    "$ESPTOOL" --chip "$CHIP" --port "$PORT" --baud "$BAUD" \
        write_flash "${FLASH_ARGS[@]}" \
        0x0      "$BOOT.signed" \
        0x8000   "$PARTS" \
        0x10000  "$APP_BIN.signed"
else
    echo ">> DRY-RUN — skipping flash (re-run with --apply to write the device)."
    echo "   Would run: $ESPTOOL --chip $CHIP --port $PORT --baud $BAUD \\"
    echo "     write_flash ${FLASH_ARGS[*]:-} 0x0 $BOOT.signed 0x8000 $PARTS 0x10000 $APP_BIN.signed"
fi

echo ""
echo "== done. Boot and verify 'secure boot enabled' / 'flash encryption' in the"
echo "   boot log. Keep $KEYDIR backed up securely. =="

