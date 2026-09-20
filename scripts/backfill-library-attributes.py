#!/usr/bin/env python3
"""
Backfill the derived attributes on existing `LIB#` library rows.

`/library` now:
  * filters/counts by a denormalized `status` (copied from `SER#` META),
  * pages newest-first off GSI3 (`GSI3PK = USR#<uid>`, `GSI3SK = <addedAt>#<seriesId>`),
  * and fans status changes out via GSI1 (`GSI1PK = SERIES#<seriesId>`).

Rows written before those changes lack `status`, `GSI1SK`, `GSI3PK` and
`GSI3SK`, so they would be invisible to GSI3 and miscounted. This one-off
fills them in with exactly the same derivation the write path uses.

Additive and idempotent: it only writes attributes that are missing, so
re-running touches nothing.

Usage:
    python3 scripts/backfill-library-attributes.py             # apply
    DRY_RUN=1 python3 scripts/backfill-library-attributes.py   # preview, write nothing

Env overrides: REGION (sa-east-1), TABLE (EpisodicEpisodes).
"""

import os

import boto3

REGION = os.environ.get("REGION", "sa-east-1")
TABLE = os.environ.get("TABLE", "EpisodicEpisodes")
DRY_RUN = os.environ.get("DRY_RUN", "0") == "1"

ddb = boto3.client("dynamodb", region_name=REGION)


def status_map(series_ids):
    """series_id -> META status for the given ids, via BatchGetItem."""
    metas = {}
    for i in range(0, len(series_ids), 100):
        chunk = series_ids[i : i + 100]
        resp = ddb.batch_get_item(
            RequestItems={
                TABLE: {
                    "Keys": [
                        {"PK": {"S": f"SER#{sid}"}, "SK": {"S": "META"}}
                        for sid in chunk
                    ],
                    "ProjectionExpression": "PK, #s",
                    "ExpressionAttributeNames": {"#s": "status"},
                }
            }
        )
        for item in resp.get("Responses", {}).get(TABLE, []):
            sid = item["PK"]["S"].removeprefix("SER#")
            status = item.get("status", {}).get("S", "")
            if status:
                metas[sid] = status
    return metas


def main():
    scanned = updated = skipped = 0
    start = None

    while True:
        kwargs = {
            "TableName": TABLE,
            "FilterExpression": "begins_with(SK, :sk)",
            "ExpressionAttributeValues": {":sk": {"S": "LIB#"}},
            "ProjectionExpression": (
                "PK, SK, seriesId, addedAt, #s, GSI1SK, GSI3PK, GSI3SK"
            ),
            "ExpressionAttributeNames": {"#s": "status"},
        }
        if start:
            kwargs["ExclusiveStartKey"] = start

        resp = ddb.scan(**kwargs)
        rows = resp.get("Items", [])
        scanned += len(rows)

        series_ids = [r.get("seriesId", {}).get("S", "") for r in rows]
        metas = status_map([s for s in series_ids if s])

        for row in rows:
            pk = row.get("PK", {}).get("S", "")
            sk = row.get("SK", {}).get("S", "")
            series_id = row.get("seriesId", {}).get("S", "")
            added_at = row.get("addedAt", {}).get("S", "")
            user_id = pk.removeprefix("USR#")

            sets, names, values = [], {}, {}
            if "status" not in row:
                status = metas.get(series_id)
                if status:
                    sets.append("#s = :s")
                    names["#s"] = "status"
                    values[":s"] = {"S": status}
            if "GSI1SK" not in row:
                sets.append("GSI1SK = :g1")
                values[":g1"] = {"S": f"LIB#{user_id}#{series_id}"}
            if "GSI3PK" not in row:
                sets.append("GSI3PK = :g3p")
                values[":g3p"] = {"S": f"USR#{user_id}"}
            if "GSI3SK" not in row:
                sets.append("GSI3SK = :g3s")
                values[":g3s"] = {"S": f"{added_at}#{series_id}"}

            if not sets:
                skipped += 1
                continue

            if not DRY_RUN:
                kwargs_update = {
                    "TableName": TABLE,
                    "Key": {"PK": row["PK"], "SK": row["SK"]},
                    "UpdateExpression": "SET " + ", ".join(sets),
                    "ExpressionAttributeValues": values,
                }
                if names:
                    kwargs_update["ExpressionAttributeNames"] = names
                ddb.update_item(**kwargs_update)

            updated += 1
            if updated % 500 == 0:
                print(f"  {'would update' if DRY_RUN else 'updated'} {updated}")

        start = resp.get("LastEvaluatedKey")
        if not start:
            break

    verb = "would update" if DRY_RUN else "updated"
    print(f"scanned {scanned} library rows; {verb} {updated}; skipped {skipped}")


if __name__ == "__main__":
    main()
