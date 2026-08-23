#!/bin/bash
# Patch ESP-IDF toolchain headers to fix clang compatibility issues on macOS
# This script should be run once after installing/updating the ESP-IDF toolchain

TOOLCHAIN_DIR="$HOME/.platformio/packages/toolchain-riscv32-esp"
REENT_H="$TOOLCHAIN_DIR/riscv32-esp-elf/sys-include/sys/reent.h"

if [ ! -f "$REENT_H" ]; then
    echo "Error: reent.h not found at $REENT_H"
    exit 1
fi

# Backup original
if [ ! -f "$REENT_H.orig" ]; then
    cp "$REENT_H" "$REENT_H.orig"
    echo "Backed up original reent.h to $REENT_H.orig"
fi

# Apply patch: comment out the conflicting typedef
# The issue is that reent.h defines __FILE as either __sFILE64 or __sFILE
# depending on __LARGE64_FILES, but clang on macOS sees both definitions
sed -i.bak 's/^typedef struct __sFILE   __FILE;$/\/* PATCHED: typedef struct __sFILE   __FILE; \*\//' "$REENT_H"

echo "Patched $REENT_H to fix __FILE typedef conflict"
