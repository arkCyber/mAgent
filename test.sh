#!/bin/bash
# Test script for mAgent

set -e

echo "Running mAgent tests..."

# Run unit tests
echo "Running unit tests..."
cargo test --lib

# Run doc tests
echo "Running doc tests..."
cargo test --doc

# Check code style
echo "Checking code style..."
cargo fmt -- --check

# Run clippy
echo "Running clippy..."
cargo clippy -- -D warnings

echo "All tests passed!"
