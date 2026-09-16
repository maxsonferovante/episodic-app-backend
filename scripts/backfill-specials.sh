#!/usr/bin/env bash
set -euo pipefail

# Force-rehydrate series so specials (TMDB season 0) get persisted and start
# counting towards watch progress and rendering in the UI.
#
# TMDB asks callers to stay around ~40 req/s and to respect 429. Three layers
# keep us under that:
#   1. the hydrate-worker consumes with batch_size=1 and maximum_concurrency=5,
#      and each invocation fetches a series' seasons sequentially — so at most
#      ~5 TMDB requests are ever in flight;
#   2. the shared TMDB client throttles per process (sliding 1s window) and
#      retries 429 honouring Retry-After;
#   3. this script paces the *enqueue* so the queue never bursts.
#
# Usage:
#   ./scripts/backfill-specials.sh                  # every series in every library
#   ./scripts/backfill-specials.sh ser_1399 ser_123 # only the given series ids
#   RATE=5 ./scripts/backfill-specials.sh           # 5 messages/second (default)
#   DRY_RUN=1 ./scripts/backfill-specials.sh        # list, send nothing
#
# Env overrides: REGION (sa-east-1), TABLE (EpisodicEpisodes),
#                QUEUE_NAME (episodic-hydrate), QUEUE_URL, RATE.

REGION="${REGION:-sa-east-1}"
TABLE="${TABLE:-EpisodicEpisodes}"
QUEUE_NAME="${QUEUE_NAME:-episodic-hydrate}"
RATE="${RATE:-5}"
DRY_RUN="${DRY_RUN:-0}"

if [ "$RATE" -le 0 ]; then RATE=1; fi
SLEEP=$(awk "BEGIN {printf \"%.3f\", 1/$RATE}")

QUEUE_URL="${QUEUE_URL:-$(aws sqs get-queue-url --queue-name "$QUEUE_NAME" --region "$REGION" --query QueueUrl --output text)}"

echo "backfill-specials: table=$TABLE region=$REGION queue=$QUEUE_NAME rate=${RATE}/s"
echo "queue: $QUEUE_URL"

tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT

if [ "$#" -gt 0 ]; then
    # Explicit series ids take precedence over the library scan.
    printf '%s\n' "$@" > "$tmp"
else
    echo "Scanning user libraries..."

    last_key=""
    while :; do
        args=(
            dynamodb scan
            --table-name "$TABLE"
            --region "$REGION"
            --filter-expression "begins_with(PK, :pk) AND begins_with(SK, :sk)"
            --expression-attribute-values '{":pk":{"S":"USR#"},":sk":{"S":"LIB#"}}'
            --projection-expression "seriesId"
            --output json
        )
        if [ -n "$last_key" ]; then
            args+=(--exclusive-start-key "$last_key")
        fi

        page=$(aws "${args[@]}")
        echo "$page" | jq -r '.Items[] | select(.seriesId.S) | .seriesId.S' >> "$tmp"
        last_key=$(echo "$page" | jq -c '.LastEvaluatedKey // empty')
        [ -z "$last_key" ] && break
    done
fi

sort -u "$tmp" -o "$tmp"
total=$(wc -l < "$tmp" | tr -d ' ')
echo "distinct series: $total"

if [ "$total" -eq 0 ]; then
    echo "Nothing to do."
    exit 0
fi

if [ "$DRY_RUN" = "1" ]; then
    echo "DRY_RUN=1 — nothing sent. First 10:"
    head -n 10 "$tmp"
    exit 0
fi

sent=0
while IFS= read -r series_id; do
    [ -z "$series_id" ] && continue
    aws sqs send-message \
        --queue-url "$QUEUE_URL" \
        --region "$REGION" \
        --message-body "{\"seriesId\":\"$series_id\",\"force\":true}" \
        --no-cli-pager >/dev/null
    sent=$((sent + 1))
    if [ $((sent % 25)) -eq 0 ]; then
        echo "  enqueued $sent/$total"
    fi
    sleep "$SLEEP"
done < "$tmp"

echo "Done. Enqueued $sent forced hydrations."
