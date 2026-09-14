#!/usr/bin/env bash
set -euo pipefail

# Build a single lambda for deployment
# Usage: ./scripts/build-lambda.sh <crate-name>
# Example: ./scripts/build-lambda.sh auth-lambda

CRATE_NAME="${1:?Usage: build-lambda.sh <crate-name>}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"
TARGET_DIR="$ROOT_DIR/target/lambda"
CRATE_DIR="$ROOT_DIR/crates/$CRATE_NAME"

if [ ! -d "$CRATE_DIR" ]; then
    echo "Error: Crate directory not found: $CRATE_DIR"
    exit 1
fi

echo "Building $CRATE_NAME for Lambda (aarch64-unknown-linux-musl)..."

# Ensure target exists
mkdir -p "$TARGET_DIR"

# Build for Lambda (ARM64)
cargo build --release --target aarch64-unknown-linux-musl --bin bootstrap --manifest-path "$CRATE_DIR/Cargo.toml" 2>&1

# The binary should be at target/aarch64-unknown-linux-musl/release/bootstrap
BINARY_PATH="$ROOT_DIR/target/aarch64-unknown-linux-musl/release/bootstrap"

if [ ! -f "$BINARY_PATH" ]; then
    echo "Error: Binary not found at $BINARY_PATH"
    exit 1
fi

# Create zip for deployment
ZIP_PATH="$TARGET_DIR/$CRATE_NAME.zip"
cd "$ROOT_DIR/target/aarch64-unknown-linux-musl/release"
zip -j "$ZIP_PATH" bootstrap

echo "Built: $ZIP_PATH"
ls -lh "$ZIP_PATH"
