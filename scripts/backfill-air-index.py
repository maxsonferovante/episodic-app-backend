#!/usr/bin/env python3
"""
Backfill the GSI2 air-date index keys on existing `EP#` episode rows.

`upsert_episodes` now writes `GSI2PK = "AIR#<YYYY-MM>"` and
`GSI2SK = "<date>#<series>#S<ss>#E<ee>"` so `/releases` can answer a window
with one bounded index Query per month. Episodes persisted before that change
have no GSI2 keys, so the new read path would never see them. This one-off
fills the keys in.

Additive and idempotent: it only writes the two attributes on rows that lack
`GSI2PK` (and have an `airDate`), using exactly the same derivation as
`upsert_episodes`. Re-running touches nothing.

Usage:
    python3 scripts/backfill-air-index.py             # apply
    DRY_RUN=1 python3 scripts/backfill-air-index.py   # preview, write nothing

Env overrides: REGION (sa-east-1), TABLE (EpisodicEpisodes).
"""

import os

import boto3

REGION = os.environ.get("REGION", "sa-east-1")
TABLE = os.environ.get("TABLE", "EpisodicEpisodes")
DRY_RUN = os.environ.get("DRY_RUN", "0") == "1"

ddb = boto3.client("dynamodb", region_name=REGION)


def main():
    scanned = updated = skipped = 0
    start = None

    while True:
        kwargs = {
            "TableName": TABLE,
            "FilterExpression": "begins_with(SK, :sk) AND attribute_not_exists(GSI2PK)",
            "ExpressionAttributeValues": {":sk": {"S": "EP#"}},
            "ProjectionExpression": "PK, SK, seasonNumber, episodeNumber, airDate",
        }
        if start:
            kwargs["ExclusiveStartKey"] = start

        resp = ddb.scan(**kwargs)

        for item in resp.get("Items", []):
            scanned += 1
            air_date = item.get("airDate", {}).get("S", "")
            try:
                season = int(item.get("seasonNumber", {}).get("N", ""))
                episode = int(item.get("episodeNumber", {}).get("N", ""))
            except (TypeError, ValueError):
                skipped += 1
                continue
            if len(air_date) < 10:
                skipped += 1
                continue

            series_id = item["PK"]["S"].removeprefix("SER#")
            gsi2pk = f"AIR#{air_date[:7]}"
            gsi2sk = f"{air_date}#{series_id}#S{season:02d}#E{episode:02d}"

            if not DRY_RUN:
                ddb.update_item(
                    TableName=TABLE,
                    Key={"PK": item["PK"], "SK": item["SK"]},
                    UpdateExpression="SET GSI2PK = :pk, GSI2SK = :sk",
                    ExpressionAttributeValues={
                        ":pk": {"S": gsi2pk},
                        ":sk": {"S": gsi2sk},
                    },
                )

            updated += 1
            if updated % 500 == 0:
                print(f"  {'would update' if DRY_RUN else 'updated'} {updated}")

        start = resp.get("LastEvaluatedKey")
        if not start:
            break

    verb = "would update" if DRY_RUN else "updated"
    print(f"scanned {scanned} episode rows; {verb} {updated}; skipped {skipped}")


if __name__ == "__main__":
    main()
