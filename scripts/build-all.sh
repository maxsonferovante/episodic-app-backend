#!/usr/bin/env bash
set -euo pipefail

# Global build script for all backend lambdas
# Usage: ./scripts/build-all.sh

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"
TARGET_DIR="$ROOT_DIR/target/lambda"

echo "Building all lambdas..."

CRATES=(
    "auth-lambda"
    "catalog-lambda"
    "library-lambda"
    "progress-lambda"
    "dashboard-lambda"
    "sync-job"
)

for crate in "${CRATES[@]}"; do
    echo ""
    echo "=== Building $crate ==="
    "$SCRIPT_DIR/build-lambda.sh" "$crate"
done

echo ""
echo "=== All lambdas built successfully ==="
ls -lh "$TARGET_DIR"/*.zip 2>/dev/null || echo "No zip files found"
