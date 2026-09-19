# Episodic — Backend

Serverless API for **Episodic**, a TV series tracker. A Rust workspace compiled
to AWS Lambda functions behind API Gateway, backed by DynamoDB and TMDB.

## Architecture

Cargo workspace (`crates/`):

| Crate | Responsibility |
| --- | --- |
| `shared` | Models, DynamoDB access, JWT auth, enums, helpers |
| `google-auth-lambda` | Contact point with Google: validates the `id_token` and issues JWT access/refresh tokens |
| `authorizer-lambda` | API Gateway `TOKEN` authorizer: validates the access token and returns an IAM policy |
| `catalog-lambda` | TMDB search and series/seasons/episodes, cached in DynamoDB |
| `library-lambda` | User library CRUD and per-series progress |
| `progress-lambda` | Mark/unmark episodes (single or whole season) and watch events |
| `dashboard-lambda` | History, calendar and release windows |
| `sync-job` | Daily **scheduler**: scans library series and enqueues a hydrate message for the ones needing a refresh (never fetches TMDB itself) |
| `hydrate-worker` | SQS consumer: fully hydrates one series from TMDB (details, seasons, episodes, providers) and persists it canonically |

`google-auth`, `authorizer`, `catalog`, `library`, `progress`, `dashboard` are
`bootstrap` binaries built with `lambda_http`. `sync-job` and `hydrate-worker`
are `bootstrap` binaries built with `lambda_runtime` (EventBridge and SQS
events, not HTTP).

## Metadata hydration

TMDB is queried once per series and reused by every user:

- Adding a series to a library (`PUT /api/v1/library/{id}`) enqueues a message
  on the `episodic-hydrate` SQS queue (best-effort).
- `hydrate-worker` consumes it and runs `shared::hydrate::hydrate_series`, which
  makes **1 + N** TMDB calls (details + one per aired season) and writes the
  canonical `SER#ser_<tmdb>` rows: `META`, `SN#<ss>`, `EP#<ss>#<ee>` and
  `PROVIDERS`.
- **Finished** series (`Ended`/`Canceled`) are frozen with a far-future TTL and
  never re-fetched. **On-air** series get a 7-day TTL; the daily `sync-job`
  re-enqueues a series once its `nextAirDate` has passed or its cache expired.
- The hydrate routine is idempotent and guarded by a conditional lock, so
  duplicate SQS deliveries are no-ops and concurrent workers don't stampede TMDB.
- Specials (season 0) are hydrated and count towards progress, so they are
  part of the canonical episode total.

## DynamoDB access pattern

Read paths are shaped to cost a small, constant number of round trips:

- **Progress**: one `PROG#<series>#` Query (`get_series_progress_map`) plus one
  paginated `SER#<series>` partition read (`get_series_partition`: `META` +
  `SN#` + `EP#`). The next unwatched episode and the watched counts are derived
  in memory — no per-episode lookup.
- **Library**: one `LIB#` Query + one `PROG#` Query
  (`get_user_progress_counts`) + one `BatchGetItem` of the series `META`
  (`get_series_meta_bulk`).
- **Releases**: one `LIB#` Query + one bounded `GSI2` Query per month
  (`get_releases`); the index keys are written by `upsert_episodes`.
- **Calendar**: one key-range Query (`SK BETWEEN EVT#<from> AND EVT#<to>`).
- **Search**: never cached — every search hits TMDB.
- Season mark/unmark writes with `BatchWriteItem` (25 per batch, bounded retry
  on unprocessed items) instead of one `PutItem` per episode.

Totals: `META.totalEpisodes` is the sum of every season (specials included) and
is the canonical denominator everywhere; `META.numberOfEpisodes` (TMDB,
specials excluded) is only a fallback. `SN#` rows carry an `airedCount` rollup.
Completion percentages use two decimals (`models::progress::completion_percentage`).

## Requirements

- Rust (edition 2021, `rust-version = "1.75"`)
- `cargo-zigbuild` + the `aarch64-unknown-linux-musl` target (see `.cargo/config.toml`)
- AWS CLI credentials with permission to update Lambda functions

## Environment variables (per function)

| Variable | Used by | Description |
| --- | --- | --- |
| `DYNAMODB_TABLE_NAME` | all | DynamoDB table name |
| `JWT_SECRET` | auth, authorizer, library, progress, dashboard, catalog | HS256 secret for the API's own JWTs |
| `GOOGLE_CLIENT_ID` | google-auth | Google OAuth client id |
| `TMDB_API_KEY` | catalog, hydrate-worker | TMDB API key |
| `TMDB_BASE_URL` | catalog, hydrate-worker | TMDB base URL (optional, defaults to `https://api.themoviedb.org/3`) |
| `TMDB_COUNTRY` | catalog, hydrate-worker | Watch-provider region (defaults to `US`, defined in `shared::config`) |
| `HYDRATE_QUEUE_URL` | library | SQS queue URL for hydrate jobs; unset = no-op (local dev) |

## Build & deploy

One command builds every lambda for `aarch64-unknown-linux-musl` and deploys it
with `aws lambda update-function-code` (region `sa-east-1`):

```bash
./scripts/deploy-all.sh                   # build + deploy all lambdas
./scripts/deploy-all.sh catalog progress  # only the given crates
DEPLOY_ONLY=1 ./scripts/deploy-all.sh      # deploy the existing zips
TERRAFORM=1  ./scripts/deploy-all.sh       # also run terraform apply on the infra
```

Local checks:

```bash
cargo check --workspace
cargo build --workspace
```

## API

Base URL: `https://<api-id>.execute-api.<region>.amazonaws.com/prod`
(the infra repo exposes it as `api_endpoint`).

Authenticated endpoints expect `Authorization: Bearer <accessToken>`. Every
route except `POST /api/v1/auth/google` and `POST /api/v1/auth/refresh` goes
through the API Gateway JWT authorizer, so the backend always resolves the
caller's user id.

| Method | Path | Description |
| --- | --- | --- |
| POST | `/api/v1/auth/google` | Exchange a Google `id_token` for JWTs |
| POST | `/api/v1/auth/refresh` | Refresh an access token |
| GET | `/api/v1/series/search?q=&page=` | Search series (each item carries `inLibrary`) |
| GET | `/api/v1/series/{id}` | Series detail (accepts `1399` or `ser_1399`, carries `inLibrary`) |
| GET | `/api/v1/series/{id}/seasons` | Season list |
| GET | `/api/v1/series/{id}/seasons/{n}` | Season detail with your watch status |
| GET | `/api/v1/library` | Library with per-series progress |
| PUT | `/api/v1/library/{seriesId}` | Add a series to the library |
| DELETE | `/api/v1/library/{seriesId}` | Remove a series from the library |
| GET | `/api/v1/episodes/{id}/progress` | Episode progress |
| PUT | `/api/v1/episodes/{id}/progress` | Mark/unmark an episode — `{ "watched": true }` |
| PUT | `/api/v1/episodes/season/{seriesId}/{n}/progress` | Mark/unmark a whole season (aired episodes only) |
| GET | `/api/v1/releases?from=&to=` | Library episodes airing in the window (also `month=YYYY-MM`) |
| GET | `/api/v1/history?cursor=&limit=` | Paginated watch history |
| GET | `/api/v1/calendar?from=&to=` | Calendar of episodes (also accepts `month=YYYY-MM`) |

## Data model (DynamoDB)

Single table with `PK` / `SK` and two indexes: `GSI1` (`GSI1PK` / `GSI1SK`) and
`GSI2` (`GSI2PK` / `GSI2SK`, the air-date index). Highlights:

- `USR#<id>` / `PROFILE` — user (`GSI1PK = EMAIL#<email>`)
- `USR#<id>` / `LIB#<seriesId>` — library item
- `USR#<id>` / `PROG#<seriesId>#<ss>#<ee>` — watch progress
- `USR#<id>` / `EVT#<ts>#<episodeId>` — watch events (history)
- `SER#ser_<tmdb>` / `META` — canonical series metadata plus hydrate
  bookkeeping (`hydrationStatus`, `hydratedAt`, `nextAirDate`, `expiresAt`) and
  the canonical episode total `totalEpisodes`
- `SER#ser_<tmdb>` / `SN#<ss>` — season metadata (`episodeCount` + `airedCount`)
- `SER#ser_<tmdb>` / `EP#<ss>#<ee>` — episodes (`GSI1PK = <episodeId>` so the
  progress lambda can resolve an episode id back to series/season/episode;
  `GSI2PK = AIR#<YYYY-MM>` / `GSI2SK = <date>#<series>#S#E` for release windows)
- `SER#ser_<tmdb>` / `PROVIDERS` — watch providers for `TMDB_COUNTRY`

All series data is global and keyed by the deterministic id `ser_<tmdb>`; there
is no per-user copy. Search results are **not** cached.

## Scripts

| Script | Purpose |
| --- | --- |
| `scripts/deploy-all.sh` | Build (musl) + deploy the lambdas |
| `scripts/backfill-specials.sh` | Force-rehydrate series so specials are persisted |
| `scripts/backfill-season-rollups.py` | Fill `SN#.airedCount` and `META.totalEpisodes` |
| `scripts/backfill-air-index.py` | Fill `GSI2PK`/`GSI2SK` on existing `EP#` rows |

The backfills are additive and idempotent; run them with `DRY_RUN=1` first.

## Conventions

- Episode ids are deterministic: `epi_<tmdbEpisodeId>`.
- Series ids are deterministic: `ser_<tmdbId>`; season ids `sea_<tmdbId>_<ss>`.
- Status strings live in `crates/shared/src/enums.rs` (no raw literals).
- Series identifiers accept both the TMDB id and the internal `ser_<tmdb>` form.
- TMDB is never queried on the read path once a series is hydrated; hydration
  is idempotent and guarded by a conditional lock.
- Watch-provider country is a single shared constant (`shared::config`).

## Related repos

- [episodic-app-web](https://github.com/maxsonferovante/episodic-app-web) — frontend
- [episodic-app-infra-cloud](https://github.com/maxsonferovante/episodic-app-infra-cloud) — AWS infrastructure

## License

MIT © Maxson Almeida. Metadata from the TMDB API; not endorsed or certified by TMDB.
