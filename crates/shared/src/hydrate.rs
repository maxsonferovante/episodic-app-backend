//! Full-metadata hydration for a series: one TMDB details call plus one call
//! per aired season, persisted canonically so every user is served from the
//! database instead of TMDB.
//!
//! The routine is deliberately idempotent — SQS delivers at least once, so a
//! duplicate message inside the cache TTL must be a cheap no-op.

use aws_sdk_dynamodb::Client;
use chrono::{Duration, Utc};

use crate::models::episode::Episode;
use crate::models::season::Season;
use crate::models::series::{Provider, Series, WatchProviders};
use crate::{config, db, enums, tmdb};

/// A hydrate lock older than this is considered stale and may be taken over.
const LOCK_STALE_SECS: i64 = 600;

#[derive(Debug)]
pub struct HydrateOutcome {
    pub tmdb_id: i64,
    pub status: String,
    pub finished: bool,
    pub seasons_hydrated: usize,
    pub episodes_written: usize,
    pub skipped: bool,
}

/// Fetch and persist every piece of metadata for a series that is useful to
/// any user: series details, season list, episodes of aired seasons, and watch
/// providers.
///
/// When `force` is set the "already hydrated and fresh" short-circuit is
/// bypassed, so an operator-triggered backfill can refresh frozen (finished)
/// series too.
pub async fn hydrate_series(
    client: &Client,
    table: &str,
    tmdb_id: i64,
    force: bool,
) -> Result<HydrateOutcome, Box<dyn std::error::Error + Send + Sync>> {
    // Idempotency: a duplicate delivery inside the TTL is a no-op.
    if !force {
        if let Some(state) = db::get_hydration_state(client, table, tmdb_id).await? {
            let fresh = state.expires_at > Utc::now().timestamp();
            let complete = state.hydration_status.as_deref() == Some(enums::hydration_status::COMPLETE);
            if complete && fresh {
                return Ok(HydrateOutcome {
                    tmdb_id,
                    status: state.series_status.unwrap_or_default(),
                    finished: false,
                    seasons_hydrated: 0,
                    episodes_written: 0,
                    skipped: true,
                });
            }
        }
    }

    let stale_before = (Utc::now() - Duration::seconds(LOCK_STALE_SECS)).to_rfc3339();
    if !db::try_acquire_hydration_lock(client, table, tmdb_id, &stale_before).await? {
        tracing::info!("Hydrate skipped for {}: another worker holds the lock", tmdb_id);
        return Ok(HydrateOutcome {
            tmdb_id,
            status: String::new(),
            finished: false,
            seasons_hydrated: 0,
            episodes_written: 0,
            skipped: true,
        });
    }

    match hydrate_inner(client, table, tmdb_id).await {
        Ok(outcome) => Ok(outcome),
        Err(e) => {
            // Release the lock so a retry can hydrate this series.
            let _ = db::set_hydration_status(client, table, tmdb_id, enums::hydration_status::PENDING)
                .await;
            Err(e)
        }
    }
}

async fn hydrate_inner(
    client: &Client,
    table: &str,
    tmdb_id: i64,
) -> Result<HydrateOutcome, Box<dyn std::error::Error + Send + Sync>> {
    let details = tmdb::get_tv_details(tmdb_id)
        .await
        .map_err(|e| format!("TMDB details failed: {}", e))?;

    let status = details.status.clone().unwrap_or_default();
    let finished = enums::series_status::is_finished(&status);
    let now = Utc::now();
    let expires_at = now.timestamp() + db::ttl_for_series(&status, db::CACHE_TTL_HYDRATED_ONGOING);

    let imdb_id = details
        .external_ids
        .as_ref()
        .and_then(|external| external.imdb_id.clone())
        .or_else(|| details.imdb_id.clone());

    let canonical_id = db::series_id(tmdb_id);

    let series = Series {
        id: canonical_id.clone(),
        tmdb_id,
        imdb_id,
        name: details.name.clone(),
        original_name: details.original_name.clone(),
        overview: details.overview.clone().unwrap_or_default(),
        poster_path: details.poster_path.clone(),
        backdrop_path: details.backdrop_path.clone(),
        first_air_date: details.first_air_date.clone(),
        last_air_date: details.last_air_date.clone(),
        status: status.clone(),
        number_of_seasons: details.number_of_seasons.unwrap_or(0),
        number_of_episodes: details.number_of_episodes.unwrap_or(0),
        created_at: now.to_rfc3339(),
        updated_at: now.to_rfc3339(),
    };
    db::cache_series(client, table, &series, expires_at).await?;

    // Keep the library's denormalized status in sync for every user that has
    // this series. Best-effort: a fan-out failure must not fail the hydrate.
    if !status.is_empty() {
        if let Err(e) = db::propagate_series_status(client, table, &canonical_id, &status).await {
            tracing::warn!("Failed to propagate status for {}: {}", canonical_id, e);
        }
    }

    let today = now.format("%Y-%m-%d").to_string();

    // Season rows for every season, including specials (season 0). Specials are
    // persisted so they count towards watch progress and render in the UI.
    let season_rows: Vec<Season> = details
        .seasons
        .iter()
        .map(|s| Season {
            id: db::season_id(tmdb_id, s.season_number),
            series_id: canonical_id.clone(),
            tmdb_id: Some(s.id),
            season_number: s.season_number,
            name: s.name.clone(),
            overview: s.overview.clone(),
            poster_path: s.poster_path.clone(),
            air_date: s.air_date.clone(),
            episode_count: s.episode_count,
        })
        .collect();
    db::cache_seasons(client, table, tmdb_id, &season_rows, expires_at).await?;

    // Episodes only exist for seasons that have already aired. Specials (season
    // 0) carry no reliable season-level air date, so they are always fetched and
    // filtered per-episode at read time.
    let mut seasons_hydrated = 0usize;
    let mut episodes_written = 0usize;
    // Earliest season premiering after today (numbered only): fetched too so
    // release calendars have data even before anyone opens the season.
    let mut next_unaired: Option<&tmdb::TmdbSeason> = None;
    for season in details.seasons.iter() {
        let is_special = season.season_number == 0;
        let aired = is_special
            || match season.air_date.as_deref() {
                Some(air_date) => air_date <= today.as_str(),
                None => finished,
            };
        if !aired {
            if !is_special {
                let is_earlier = match (&next_unaired, &season.air_date) {
                    (None, Some(_)) => true,
                    (Some(current), Some(candidate)) => {
                        candidate.as_str() < current.air_date.as_deref().unwrap_or("")
                    }
                    _ => false,
                };
                if is_earlier {
                    next_unaired = Some(season);
                }
            }
            continue;
        }

        let (s, e) =
            fetch_and_persist_season(client, table, &canonical_id, tmdb_id, season).await?;
        seasons_hydrated += s;
        episodes_written += e;
    }

    if let Some(season) = next_unaired {
        let (s, e) =
            fetch_and_persist_season(client, table, &canonical_id, tmdb_id, season).await?;
        seasons_hydrated += s;
        episodes_written += e;
    }

    if let Some(providers) = country_providers(&details) {
        db::cache_providers(client, table, tmdb_id, &providers).await?;
    }

    let next_air_date = details
        .next_episode_to_air
        .as_ref()
        .and_then(|next| next.air_date.clone());

    db::mark_hydration_complete(
        client,
        table,
        tmdb_id,
        &now.to_rfc3339(),
        next_air_date.as_deref(),
        expires_at,
    )
    .await?;

    Ok(HydrateOutcome {
        tmdb_id,
        status,
        finished,
        seasons_hydrated,
        episodes_written,
        skipped: false,
    })
}

/// Fetch one season from TMDB and persist its episodes + meta.
/// Returns `(seasons_hydrated, episodes_written)` — `(0, 0)` when the TMDB
/// call fails (the season is skipped, never fatal). Errors from DynamoDB
/// itself still propagate.
/// Fetch one season from TMDB and persist its episodes + meta.
/// A failed TMDB call skips the season (`Ok((0, 0))`); DynamoDB errors
/// propagate so the hydrate run fails visibly instead of stamping COMPLETE
/// with missing seasons.
async fn fetch_and_persist_season(
    client: &Client,
    table: &str,
    canonical_id: &str,
    tmdb_id: i64,
    season: &tmdb::TmdbSeason,
) -> Result<(usize, usize), Box<dyn std::error::Error + Send + Sync>> {
    let detail = match tmdb::get_tv_season_detail(tmdb_id, season.season_number).await {
        Ok(detail) => detail,
        Err(e) => {
            tracing::warn!(
                "Season {}/{} hydrate failed: {}",
                tmdb_id,
                season.season_number,
                e
            );
            return Ok((0, 0));
        }
    };

    let season_id = db::season_id(tmdb_id, season.season_number);
    let episodes: Vec<Episode> = detail
        .episodes
        .into_iter()
        .map(|e| Episode {
            id: format!("epi_{}", e.id),
            series_id: canonical_id.to_string(),
            season_id: season_id.clone(),
            tmdb_id: Some(e.id),
            episode_number: e.episode_number,
            name: e.name,
            overview: e.overview,
            still_path: e.still_path,
            air_date: e.air_date,
            runtime: e.runtime,
            vote_average: e.vote_average,
            status: None,
        })
        .collect();

    let count = episodes.len();
    let today = Utc::now().format("%Y-%m-%d").to_string();
    let aired = episodes
        .iter()
        .filter(|e| {
            e.air_date
                .as_deref()
                .map(|date| date <= today.as_str())
                .unwrap_or(false)
        })
        .count() as i32;

    db::upsert_episodes(client, table, canonical_id, &season_id, season.season_number, &episodes)
        .await?;
    db::upsert_season_meta(
        client,
        table,
        canonical_id,
        season.season_number,
        count as i32,
        aired,
    )
    .await?;

    Ok((1, count))
}

/// Providers for the configured country, in the canonical structured shape.
fn country_providers(details: &tmdb::TmdbTvDetails) -> Option<WatchProviders> {
    let results = details.watch_providers.as_ref()?.results.as_ref()?;
    let country = config::provider_country();
    let country_providers = results.get(&country)?;

    Some(WatchProviders {
        flatrate: country_providers.flatrate.as_ref().map(|items| map_providers(items)),
        rent: country_providers.rent.as_ref().map(|items| map_providers(items)),
        buy: country_providers.buy.as_ref().map(|items| map_providers(items)),
        free: country_providers.free.as_ref().map(|items| map_providers(items)),
    })
}

fn map_providers(items: &[tmdb::TmdbProvider]) -> Vec<Provider> {
    items
        .iter()
        .map(|p| Provider {
            provider_id: p.provider_id,
            provider_name: p.provider_name.clone(),
            logo_path: p.logo_path.clone(),
        })
        .collect()
}
