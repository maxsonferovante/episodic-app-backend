#!/usr/bin/env python3
"""
Backfill the `airedCount` rollup on `SN#` season rows.

`get_aired_counts` prefers the `airedCount` attribute that hydrate and the
season-detail read write onto each `SN#` row. Seasons persisted before that
rollup existed have no `airedCount`, so the read path falls back to scanning
every `EP#` row for the series. This one-off fills the rollup for those legacy
seasons so the read path stops scanning episodes.

The operation is additive and idempotent: it only SETs `airedCount` on rows
that lack it, and only when at least one episode has actually aired (a season
with 0 aired episodes keeps its existing fallback, so nothing regresses).

Usage:
    python3 scripts/backfill-aired-count.py             # apply
    DRY_RUN=1 python3 scripts/backfill-aired-count.py   # preview, write nothing

Env overrides: REGION (sa-east-1), TABLE (EpisodicEpisodes).
"""

import os
from collections import defaultdict
from datetime import datetime, timezone

import boto3

REGION = os.environ.get("REGION", "sa-east-1")
TABLE = os.environ.get("TABLE", "EpisodicEpisodes")
DRY_RUN = os.environ.get("DRY_RUN", "0") == "1"

ddb = boto3.client("dynamodb", region_name=REGION)
TODAY = datetime.now(timezone.utc).strftime("%Y-%m-%d")


def scan(filter_expression, names, values, projection):
    """Yield every item matching `filter_expression`, paging through the table."""
    start = None
    while True:
        kwargs = {
            "TableName": TABLE,
            "FilterExpression": filter_expression,
            "ProjectionExpression": projection,
        }
        if names:
            kwargs["ExpressionAttributeNames"] = names
        if values:
            kwargs["ExpressionAttributeValues"] = values
        if start:
            kwargs["ExclusiveStartKey"] = start

        resp = ddb.scan(**kwargs)
        yield from resp.get("Items", [])

        start = resp.get("LastEvaluatedKey")
        if not start:
            break


def main():
    # 1. Aired episode count per (series, season) from the existing EP# rows.
    aired = defaultdict(int)
    episodes = 0
    for item in scan(
        "begins_with(SK, :sk)",
        None,
        {":sk": {"S": "EP#"}},
        "PK, seasonNumber, airDate",
    ):
        episodes += 1
        air_date = item.get("airDate", {}).get("S")
        season = item.get("seasonNumber", {}).get("N")
        if air_date and air_date <= TODAY and season is not None:
            aired[(item["PK"]["S"], season)] += 1

    print(
        f"scanned {episodes} episode rows; "
        f"{len(aired)} (series, season) pairs with aired episodes"
    )

    # 2. Write the rollup onto SN# rows that don't have it yet.
    updated = skipped = 0
    for item in scan(
        "begins_with(SK, :sk) AND attribute_not_exists(airedCount)",
        None,
        {":sk": {"S": "SN#"}},
        "PK, SK, seasonNumber",
    ):
        season = item.get("seasonNumber", {}).get("N")
        if season is None:
            continue

        count = aired.get((item["PK"]["S"], season), 0)
        if count <= 0:
            skipped += 1
            continue

        if not DRY_RUN:
            ddb.update_item(
                TableName=TABLE,
                Key={"PK": item["PK"], "SK": item["SK"]},
                UpdateExpression="SET airedCount = :ac",
                ExpressionAttributeValues={":ac": {"N": str(count)}},
            )

        updated += 1
        if updated % 200 == 0:
            print(f"  {'would update' if DRY_RUN else 'updated'} {updated}")

    verb = "would update" if DRY_RUN else "updated"
    print(f"{verb} {updated} season rows; skipped {skipped} (no aired episodes)")


if __name__ == "__main__":
    main()
