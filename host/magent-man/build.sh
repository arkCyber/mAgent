#!/bin/bash
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "========================================="
echo "  mAgent-Man Build Script"
echo "========================================="

# Build Swift BLE Helper
echo ""
echo "[1/2] Building Swift BLE Helper..."
cd "$SCRIPT_DIR/ble-helper"
swift build -c release
SWIFT_BIN=$(swift build -c release --show-bin-path 2>/dev/null || echo ".build/x86_64-apple-macosx/release")
cp "$SWIFT_BIN/ble-helper" "$SCRIPT_DIR/" 2>/dev/null || cp ".build/release/ble-helper" "$SCRIPT_DIR/" 2>/dev/null
echo "  ✓ BLE Helper built"

# Build Tauri App
echo ""
echo "[2/2] Building Tauri App..."
cd "$SCRIPT_DIR"
bun run tauri build

echo ""
echo "========================================="
echo "  Build Complete!"
echo "========================================="
echo ""
echo "Outputs:"
echo "  - Tauri App: $SCRIPT_DIR/src-tauri/target/release/bundle/macos/"
echo "  - BLE Helper: $SCRIPT_DIR/ble-helper"
echo ""
