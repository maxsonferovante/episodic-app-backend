use lambda_http::{Body, Request, Response};
use shared::auth::extract_user_id;
use shared::db::{self, ProgressEntry, SeriesPartition, WatchEventSeed};
use shared::error::{AppError, app_error_response, add_cors};
use shared::models::progress::{
    EpisodeProgress, MarkWatchedRequest, ProgressResponse, SeriesProgress, WatchStatus,
};
use serde_json::json;
use std::collections::HashMap;

fn parse_episode_id(path: &str) -> Option<&str> {
    let prefix = "/api/v1/episodes/";
    let suffix = "/progress";
    let rest = path.strip_prefix(prefix)?;
    rest.strip_suffix(suffix)
}

/// Matches `/api/v1/episodes/season/{seriesId}/{seasonNumber}/progress`.
fn parse_season_path(path: &str) -> Option<(&str, i32)> {
    let rest = path.strip_prefix("/api/v1/episodes/season/")?;
    let rest = rest.strip_suffix("/progress")?;
    let mut parts = rest.split('/');
    let series_id = parts.next()?;
    let season_number: i32 = parts.next()?.parse().ok()?;
    if series_id.is_empty() || parts.next().is_some() {
        return None;
    }
    Some((series_id, season_number))
}

/// Canonicalise a series id to the stored `ser_<tmdb>` form, accepting either
/// `1399` or `ser_1399`.
fn normalize_series_id(series_id: &str) -> String {
    if series_id.starts_with("ser_") {
        series_id.to_string()
    } else {
        format!("ser_{}", series_id)
    }
}

fn json_response(body: String) -> Response<Body> {
    let mut resp = Response::builder()
        .status(200)
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();
    add_cors(&mut resp);
    resp
}

/// Build the progress response purely from an already-fetched progress map and
/// series partition — no additional reads.
fn progress_response(
    episode_id: &str,
    series_id: &str,
    season_number: i32,
    episode_number: i32,
    progress_map: &HashMap<(i32, i32), ProgressEntry>,
    partition: &SeriesPartition,
) -> ProgressResponse {
    let entry = progress_map.get(&(season_number, episode_number));
    let status = entry
        .map(|e| WatchStatus::from_db(&e.status))
        .unwrap_or(WatchStatus::Unwatched);
    let watched_at = entry.and_then(|e| e.watched_at.clone());

    let watched_count = progress_map.values().filter(|e| e.is_watched()).count() as i32;
    let total_episodes = partition.total_episodes();
    let season_watched = progress_map
        .iter()
        .filter(|((season, _), entry)| *season == season_number && entry.is_watched())
        .count() as i32;
    let season_total = partition.season_episode_count(season_number);

    let series_pct = shared::models::progress::completion_percentage(watched_count, total_episodes);
    let season_pct = shared::models::progress::completion_percentage(season_watched, season_total);

    ProgressResponse {
        episode: EpisodeProgress {
            episode_id: episode_id.to_string(),
            status,
            watched_at,
        },
        progress: SeriesProgress {
            series_percentage: series_pct,
            season_percentage: season_pct,
            watched_episodes: watched_count,
            total_episodes,
            season_watched_episodes: season_watched,
            season_total_episodes: season_total,
        },
        next_episode: db::next_unwatched_episode(series_id, partition, progress_map),
    }
}

/// One progress map + series partition per request. Both are needed by every
/// branch below, and fetching them together keeps the request at three reads
/// regardless of how many episodes the series has.
async fn load_progress_context(
    client: &db::Client,
    table: &str,
    user_id: &str,
    series_id: &str,
) -> Result<(HashMap<(i32, i32), ProgressEntry>, SeriesPartition), AppError> {
    tokio::try_join!(
        db::get_series_progress_map(client, table, user_id, series_id),
        db::get_series_partition(client, table, series_id),
    )
    .map_err(|e| AppError::Internal(e.to_string()))
}

pub async fn handle_request(req: Request) -> Result<Response<Body>, Box<dyn std::error::Error + Send + Sync>> {
    let method = req.method().as_str();
    let path = req.uri().path();

    // Resolve auth once and answer properly (401) instead of letting the error
    // escape handle_request, which made the runtime panic with a 502.
    let user_id = match extract_user_id(&req) {
        Ok(user_id) => user_id,
        Err(e) => return Ok(app_error_response(e)),
    };

    let table = std::env::var("DYNAMODB_TABLE_NAME")
        .unwrap_or_else(|_| "EpisodicEpisodes".to_string());
    let client = db::get_client().await;

    let result = match method {
        "GET" if parse_episode_id(path).is_some() => {
            let episode_id = parse_episode_id(path).unwrap();
            handle_get_progress(&client, &table, &user_id, episode_id).await
        }
        "PUT" if parse_season_path(path).is_some() => {
            let (series_id, season_number) = parse_season_path(path).unwrap();
            match serde_json::from_slice::<MarkWatchedRequest>(req.body().as_ref()) {
                Ok(request) => handle_season_progress(&client, &table, &user_id, series_id, season_number, request.watched).await,
                Err(_) => Err(AppError::Internal("Invalid request body".into())),
            }
        }
        "PUT" if parse_episode_id(path).is_some() => {
            let episode_id = parse_episode_id(path).unwrap();
            match serde_json::from_slice::<MarkWatchedRequest>(req.body().as_ref()) {
                Ok(request) => handle_put_progress(&client, &table, &user_id, episode_id, request).await,
                Err(_) => Err(AppError::Internal("Invalid request body".into())),
            }
        }
        _ => Err(AppError::Internal("Not found".into())),
    };

    match result {
        Ok(resp) => Ok(resp),
        Err(e) => Ok(app_error_response(e)),
    }
}

async fn handle_get_progress(
    client: &db::Client,
    table: &str,
    user_id: &str,
    episode_id: &str,
) -> Result<Response<Body>, AppError> {
    let (series_id, season_number, episode_number) = db::get_episode_by_id(client, table, episode_id)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?
        .ok_or(AppError::EpisodeNotFound)?;

    let (progress_map, partition) =
        load_progress_context(client, table, user_id, &series_id).await?;

    let response = progress_response(
        episode_id,
        &series_id,
        season_number,
        episode_number,
        &progress_map,
        &partition,
    );

    let body = serde_json::to_string(&response)
        .map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(json_response(body))
}

async fn handle_put_progress(
    client: &db::Client,
    table: &str,
    user_id: &str,
    episode_id: &str,
    request: MarkWatchedRequest,
) -> Result<Response<Body>, AppError> {
    let (series_id, season_number, episode_number) = db::get_episode_by_id(client, table, episode_id)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?
        .ok_or(AppError::EpisodeNotFound)?;

    let (mut progress_map, partition) =
        load_progress_context(client, table, user_id, &series_id).await?;

    let already_watched = progress_map
        .get(&(season_number, episode_number))
        .map(ProgressEntry::is_watched)
        .unwrap_or(false);

    // A no-op toggle writes nothing: the existing map already answers.
    if request.watched != already_watched {
        let seed = WatchEventSeed {
            series_id: &series_id,
            season_number,
            episode_number,
            episode_name: partition.episode_name(season_number, episode_number),
            series_name: Some(partition.name.as_str()),
            poster_path: partition.poster_path.as_deref(),
        };

        let (status, event_type, watched_at) = if request.watched {
            db::mark_episode(client, table, user_id, &series_id, season_number, episode_number)
                .await
                .map_err(|e| AppError::Internal(e.to_string()))?;
            (
                WatchStatus::Watched,
                shared::enums::watch_event_type::MARK_WATCHED,
                Some(chrono::Utc::now().to_rfc3339()),
            )
        } else {
            db::unmark_episode(client, table, user_id, &series_id, season_number, episode_number)
                .await
                .map_err(|e| AppError::Internal(e.to_string()))?;
            (
                WatchStatus::Unwatched,
                shared::enums::watch_event_type::UNMARK_WATCHED,
                None,
            )
        };

        db::create_watch_event(client, table, user_id, episode_id, event_type, &seed)
            .await
            .map_err(|e| AppError::Internal(e.to_string()))?;

        progress_map.insert(
            (season_number, episode_number),
            ProgressEntry {
                status: status.as_str().to_string(),
                watched_at,
            },
        );
    }

    let response = progress_response(
        episode_id,
        &series_id,
        season_number,
        episode_number,
        &progress_map,
        &partition,
    );

    let body = serde_json::to_string(&response)
        .map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(json_response(body))
}

/// Mark/unmark every aired episode of a season in one request.
async fn handle_season_progress(
    client: &db::Client,
    table: &str,
    user_id: &str,
    series_id: &str,
    season_number: i32,
    watched: bool,
) -> Result<Response<Body>, AppError> {
    let series_id = normalize_series_id(series_id);

    let (mut progress_map, partition) =
        load_progress_context(client, table, user_id, &series_id).await?;

    // Only aired episodes are touched, matching the previous behaviour.
    let aired = partition.aired_episode_numbers(season_number);

    // Write only the episodes whose state actually changes; the response still
    // reports every aired episode so the client can flip them all locally.
    let to_change: Vec<i32> = aired
        .iter()
        .copied()
        .filter(|episode_number| {
            let is_watched = progress_map
                .get(&(season_number, *episode_number))
                .map(ProgressEntry::is_watched)
                .unwrap_or(false);
            is_watched != watched
        })
        .collect();

    db::mark_episodes_bulk(client, table, user_id, &series_id, season_number, &to_change, watched)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    let now = chrono::Utc::now().to_rfc3339();
    for episode_number in &to_change {
        progress_map.insert(
            (season_number, *episode_number),
            ProgressEntry {
                status: if watched {
                    WatchStatus::Watched.as_str().to_string()
                } else {
                    WatchStatus::Unwatched.as_str().to_string()
                },
                watched_at: if watched { Some(now.clone()) } else { None },
            },
        );
    }

    // Recompute the caller's totals so the client can update every progress
    // surface without refetching the series and season.
    let watched_count = progress_map.values().filter(|e| e.is_watched()).count() as i32;
    let total_episodes = partition.total_episodes();
    let season_watched = progress_map
        .iter()
        .filter(|((season, _), entry)| *season == season_number && entry.is_watched())
        .count() as i32;
    let season_total = partition.season_episode_count(season_number);
    let percentage =
        shared::models::progress::completion_percentage(watched_count, total_episodes);

    let body = json!({
        "seriesId": series_id,
        "seasonNumber": season_number,
        "watched": watched,
        "updatedEpisodes": aired.len(),
        "updatedEpisodeNumbers": aired,
        "progress": {
            "watchedEpisodes": watched_count,
            "totalEpisodes": total_episodes,
            "percentage": percentage,
        },
        "season": {
            "seasonNumber": season_number,
            "watchedEpisodes": season_watched,
            "episodeCount": season_total,
        },
    });

    Ok(json_response(body.to_string()))
}
