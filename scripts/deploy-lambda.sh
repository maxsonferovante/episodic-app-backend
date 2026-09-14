#!/usr/bin/env bash
set -euo pipefail

# Build and deploy a single lambda
# Usage: ./scripts/deploy-lambda.sh <crate-name> <function-name>
# Example: ./scripts/deploy-lambda.sh auth-lambda episodic-auth

CRATE_NAME="${1:?Usage: deploy-lambda.sh <crate-name> <function-name>}"
FUNCTION_NAME="${2:?Usage: deploy-lambda.sh <crate-name> <function-name>}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "Building $CRATE_NAME..."
"$SCRIPT_DIR/build-lambda.sh" "$CRATE_NAME"

ZIP_PATH="$(dirname "$SCRIPT_DIR")/target/lambda/$CRATE_NAME.zip"

echo "Deploying $FUNCTION_NAME..."
aws lambda update-function-code \
    --function-name "$FUNCTION_NAME" \
    --zip-file "fileb://$ZIP_PATH" \
    --no-cli-pager

echo "Deployed $FUNCTION_NAME successfully"
