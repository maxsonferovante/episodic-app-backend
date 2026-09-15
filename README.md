# Episodic — Backend

Serverless API for **Episodic**, a TV series tracker. A Rust workspace compiled
to AWS Lambda functions behind API Gateway, backed by DynamoDB and TMDB.

## Architecture

Cargo workspace (`crates/`):

| Crate | Responsibility |
| --- | --- |
| `shared` | Models, DynamoDB access, JWT auth, enums, helpers |
| `auth-lambda` | Validates the Google `id_token` and issues JWT access/refresh tokens |
| `catalog-lambda` | TMDB search and series/seasons/episodes, cached in DynamoDB |
| `library-lambda` | User library CRUD and per-series progress |
| `progress-lambda` | Mark/unmark episodes (single or whole season) and watch events |
| `dashboard-lambda` | Dashboard (continue watching, upcoming), history, calendar |
| `sync-job` | Scheduled sync of library series metadata/episodes from TMDB |

Every Lambda is a `bootstrap` binary built with `lambda_http`.

## Requirements

- Rust (edition 2021, `rust-version = "1.75"`)
- `cargo-zigbuild` + the `aarch64-unknown-linux-musl` target (see `.cargo/config.toml`)
- AWS CLI credentials with permission to update Lambda functions

## Environment variables (per function)

| Variable | Used by | Description |
| --- | --- | --- |
| `DYNAMODB_TABLE_NAME` | all | DynamoDB table name |
| `JWT_SECRET` | auth, library, progress, dashboard, catalog | HS256 secret for the API's own JWTs |
| `GOOGLE_CLIENT_ID` | auth | Google OAuth client id |
| `TMDB_API_KEY` | catalog, sync-job | TMDB API key |
| `TMDB_BASE_URL` | catalog | TMDB base URL |

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

Authenticated endpoints expect `Authorization: Bearer <accessToken>`.

| Method | Path | Description |
| --- | --- | --- |
| POST | `/api/v1/auth/google` | Exchange a Google `id_token` for JWTs |
| POST | `/api/v1/auth/refresh` | Refresh an access token |
| GET | `/api/v1/series/search?q=&page=` | Search series |
| GET | `/api/v1/series/{id}` | Series detail (accepts `1399` or `ser_1399`) |
| GET | `/api/v1/series/{id}/seasons` | Season list |
| GET | `/api/v1/series/{id}/seasons/{n}` | Season detail with your watch status |
| GET | `/api/v1/library` | Library with per-series progress |
| PUT | `/api/v1/library/{seriesId}` | Add a series to the library |
| DELETE | `/api/v1/library/{seriesId}` | Remove a series from the library |
| GET | `/api/v1/episodes/{id}/progress` | Episode progress |
| PUT | `/api/v1/episodes/{id}/progress` | Mark/unmark an episode — `{ "watched": true }` |
| PUT | `/api/v1/episodes/season/{seriesId}/{n}/progress` | Mark/unmark a whole season (aired episodes only) |
| GET | `/api/v1/dashboard` | Continue watching, upcoming, recent history |
| GET | `/api/v1/history?cursor=&limit=` | Paginated watch history |
| GET | `/api/v1/calendar?month=YYYY-MM` | Calendar of episodes |

## Data model (DynamoDB)

Single table with `PK` / `SK` and a `GSI1` (`GSI1PK` / `GSI1SK`). Highlights:

- `USR#<id>` / `PROFILE` — user (`GSI1PK = EMAIL#<email>`)
- `USR#<id>` / `LIB#<seriesId>` — library item
- `USR#<id>` / `PROG#<seriesId>#<ss>#<ee>` — watch progress
- `USR#<id>` / `EVT#<ts>#<episodeId>` — watch events (history)
- `SER#<seriesId>` / `META`, `SN#<ss>`, `EP#<ss>#<ee>` — synced catalog data
  (`EP#` rows are indexed on `GSI1` by episode id)
- `SERIES#<tmdbId>` / `META`, `SEASON#..`, `EPISODE#..` — catalog cache

## Conventions

- Episode ids are deterministic: `epi_<tmdbEpisodeId>`.
- Status strings live in `crates/shared/src/enums.rs` (no raw literals).
- Series identifiers accept both the TMDB id and the internal `ser_<tmdb>` form.

## Related repos

- [episodic-app-web](https://github.com/maxsonferovante/episodic-app-web) — frontend
- [episodic-app-infra-cloud](https://github.com/maxsonferovante/episodic-app-infra-cloud) — AWS infrastructure

## License

MIT © Maxson Almeida. Metadata from the TMDB API; not endorsed or certified by TMDB.
