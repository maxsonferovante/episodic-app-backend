#!/usr/bin/env bash
set -euo pipefail

# Build and deploy every Episodic lambda in one shot.
#
# Usage:
#   ./scripts/deploy-all.sh                  # build + deploy all lambdas
#   ./scripts/deploy-all.sh catalog progress # only the given crates
#   DEPLOY_ONLY=1 ./scripts/deploy-all.sh    # skip build, deploy existing zips
#   TERRAFORM=1 ./scripts/deploy-all.sh      # also `terraform apply` the infra
#
# Lambda function names are `episodic-<name>` (crate name minus the -lambda
# suffix). The zips land in target/lambda/<crate>.zip.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"
TARGET_DIR="$ROOT_DIR/target/lambda"
TARGET_TRIPLE="aarch64-unknown-linux-musl"
INFRA_DIR="$ROOT_DIR/../episodic-app-infra-cloud"
# Lambdas live in sa-east-1; override with AWS_REGION if ever needed.
REGION="${AWS_REGION:-sa-east-1}"

ALL_CRATES=(google-auth-lambda authorizer-lambda catalog-lambda library-lambda progress-lambda dashboard-lambda sync-job hydrate-worker)

function_name() {
    echo "episodic-${1%-lambda}"
}

build_one() {
    local crate="$1"
    echo "=== Building $crate ($TARGET_TRIPLE) ==="
    cargo build \
        --release \
        --target "$TARGET_TRIPLE" \
        --bin bootstrap \
        --manifest-path "$ROOT_DIR/crates/$crate/Cargo.toml"

    local binary="$ROOT_DIR/target/$TARGET_TRIPLE/release/bootstrap"
    if [ ! -f "$binary" ]; then
        echo "Error: binary not found at $binary" >&2
        exit 1
    fi

    mkdir -p "$TARGET_DIR"
    local zip="$TARGET_DIR/$crate.zip"
    rm -f "$zip"
    (cd "$(dirname "$binary")" && zip -j -q "$zip" bootstrap)
    echo "Built: $zip"
}

deploy_one() {
    local crate="$1"
    local function_name
    function_name="$(function_name "$crate")"
    local zip="$TARGET_DIR/$crate.zip"

    if [ ! -f "$zip" ]; then
        echo "Error: zip not found at $zip (run the build first)" >&2
        exit 1
    fi

    echo "=== Deploying $crate -> $function_name ($REGION) ==="
    aws lambda update-function-code \
        --function-name "$function_name" \
        --region "$REGION" \
        --zip-file "fileb://$zip" \
        --no-cli-pager >/dev/null
    echo "Deployed: $function_name"
}

if [ "$#" -gt 0 ]; then
    SELECTED=("$@")
else
    SELECTED=("${ALL_CRATES[@]}")
fi

for crate in "${SELECTED[@]}"; do
    if [ "${DEPLOY_ONLY:-0}" != "1" ]; then
        build_one "$crate"
    fi
    deploy_one "$crate"
done

if [ "${TERRAFORM:-0}" = "1" ]; then
    echo "=== terraform apply ($INFRA_DIR) ==="
    (cd "$INFRA_DIR" && terraform apply -auto-approve)
fi

echo ""
echo "=== Done ==="
