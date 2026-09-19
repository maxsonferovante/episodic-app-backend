#!/usr/bin/env python3
"""
Backfill the season rollups on the series rows.

Two denormalised attributes keep the read paths cheap and consistent:

  * `SN#.airedCount`      — aired episodes for the season, so `get_aired_counts`
                            doesn't scan every `EP#` row.
  * `META.totalEpisodes`  — the sum of every season (specials included): the
                            canonical denominator shared by `/library`,
                            `/series/{id}` and the progress endpoints.

Rows written before these existed fall back to slower / different definitions
(TMDB's specials-excluded `numberOfEpisodes`, or an `EP#` scan). This one-off
fills them in. Additive and idempotent: it only writes attributes that are
missing, and only when the computed value is > 0.

Usage:
    python3 scripts/backfill-season-rollups.py             # apply
    DRY_RUN=1 python3 scripts/backfill-season-rollups.py   # preview, write nothing

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


def scan(filter_expression, values, projection):
    """Yield every item matching `filter_expression`, paging through the table."""
    start = None
    while True:
        kwargs = {
            "TableName": TABLE,
            "FilterExpression": filter_expression,
            "ProjectionExpression": projection,
        }
        if values:
            kwargs["ExpressionAttributeValues"] = values
        if start:
            kwargs["ExclusiveStartKey"] = start

        resp = ddb.scan(**kwargs)
        yield from resp.get("Items", [])

        start = resp.get("LastEvaluatedKey")
        if not start:
            break


def set_attribute(key, name, value):
    ddb.update_item(
        TableName=TABLE,
        Key=key,
        UpdateExpression=f"SET {name} = :v",
        ExpressionAttributeValues={":v": {"N": str(value)}},
    )


def main():
    # 1. Aired episode count per (series, season) from the existing EP# rows.
    aired = defaultdict(int)
    episodes = 0
    for item in scan(
        "begins_with(SK, :sk)",
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

    # 2. Season rows: accumulate the canonical total, and fill `airedCount`
    #    where it's missing and at least one episode has aired.
    total_by_series = defaultdict(int)
    aired_written = aired_skipped = 0
    for item in scan(
        "begins_with(SK, :sk)",
        {":sk": {"S": "SN#"}},
        "PK, SK, seasonNumber, episodeCount, airedCount",
    ):
        pk = item["PK"]["S"]
        season = item.get("seasonNumber", {}).get("N")
        if season is None:
            continue

        total_by_series[pk] += int(item.get("episodeCount", {}).get("N", 0))

        if "airedCount" in item:
            continue
        count = aired.get((pk, season), 0)
        if count <= 0:
            aired_skipped += 1
            continue
        if not DRY_RUN:
            set_attribute({"PK": item["PK"], "SK": item["SK"]}, "airedCount", count)
        aired_written += 1

    print(
        f"{'would set' if DRY_RUN else 'set'} airedCount on {aired_written} season rows; "
        f"skipped {aired_skipped} (no aired episodes)"
    )

    # 3. Series META rows: fill the canonical total when missing.
    total_written = total_skipped = 0
    for item in scan(
        "begins_with(SK, :sk) AND attribute_not_exists(totalEpisodes)",
        {":sk": {"S": "META"}},
        "PK, SK",
    ):
        total = total_by_series.get(item["PK"]["S"], 0)
        if total <= 0:
            total_skipped += 1
            continue
        if not DRY_RUN:
            set_attribute({"PK": item["PK"], "SK": item["SK"]}, "totalEpisodes", total)
        total_written += 1

    print(
        f"{'would set' if DRY_RUN else 'set'} totalEpisodes on {total_written} series rows; "
        f"skipped {total_skipped} (no seasons)"
    )


if __name__ == "__main__":
    main()
