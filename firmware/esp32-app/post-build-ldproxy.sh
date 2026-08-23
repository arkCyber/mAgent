#!/bin/bash
# Post-build script to emit ldproxy linker path as a cargo directive.
# This runs after esp-idf-sys's build completes, reading the scons dump
# to find the linker path and emitting it for subsequent builds.
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BUILD_OUT_DIR="$SCONS_BUILD_DIR"

# Find the scons dump file
SCONS_DUMP=""
if [ -n "$BUILD_OUT_DIR" ]; then
    SCONS_DUMP="$BUILD_OUT_DIR/__pio_scons_dump.json"
fi

if [ -z "$SCONS_DUMP" ] || [ ! -f "$SCONS_DUMP" ]; then
    exit 0
fi

# Read the "link" field from the JSON dump
LINK=$(python3 -c "import json,sys; d=json.load(open('$SCONS_DUMP')); print(d.get('link',''))" 2>/dev/null)

if [ "$LINK" = "ldproxy" ]; then
    # Emit the linker path for ldproxy
    python3 -c "
import json, subprocess, sys, os

dump = json.load(open('$SCONS_DUMP'))
path_env = dump.get('path', '')
link_name = dump.get('link', '')

# Find ldproxy in PATH
env_paths = path_env.split(':')
for p in env_paths:
    candidate = os.path.join(p, link_name)
    if os.path.isfile(candidate) and os.access(candidate, os.X_OK):
        print(f'cargo:rustc-link-arg=--ldproxy-linker={candidate}')
        break
" 2>/dev/null
fi
