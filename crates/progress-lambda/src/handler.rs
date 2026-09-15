use lambda_http::{Body, Request, Response};
use shared::auth::extract_user_id;
use shared::db;
use shared::error::{AppError, app_error_response, add_cors};
use shared::models::progress::{MarkWatchedRequest, ProgressResponse, EpisodeProgress, SeriesProgress};
use serde_json::json;

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

    let progress = db::get_episode_progress(client, table, user_id, &series_id, season_number, episode_number)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    let status = progress
        .as_ref()
        .map(|p| p.status.clone())
        .unwrap_or(shared::models::progress::WatchStatus::Unwatched);
    let watched_at = progress.as_ref().and_then(|p| p.watched_at.clone());

    let watched_count = db::count_watched_in_series(client, table, user_id, &series_id)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let total_episodes = db::get_series_total_episodes(client, table, &series_id)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let season_watched = db::count_watched_in_season(client, table, user_id, &series_id, season_number)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let season_total = db::get_season_episode_count(client, table, &series_id, season_number)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    let series_pct = if total_episodes > 0 {
        (watched_count as f64 / total_episodes as f64) * 100.0
    } else {
        0.0
    };
    let season_pct = if season_total > 0 {
        (season_watched as f64 / season_total as f64) * 100.0
    } else {
        0.0
    };

    let next_episode = db::get_next_unwatched_episode(client, table, user_id, &series_id)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    let response = ProgressResponse {
        episode: EpisodeProgress {
            episode_id: episode_id.to_string(),
            status,
            watched_at,
        },
        progress: SeriesProgress {
            series_percentage: (series_pct * 10.0).round() / 10.0,
            season_percentage: (season_pct * 10.0).round() / 10.0,
        },
        next_episode,
    };

    let body = serde_json::to_string(&response)
        .map_err(|e| AppError::Internal(e.to_string()))?;

    let mut resp = Response::builder()
        .status(200)
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();
    add_cors(&mut resp);
    Ok(resp)
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

    let current = db::get_episode_progress(client, table, user_id, &series_id, season_number, episode_number)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    let already_watched = current
        .as_ref()
        .map(|p| p.status == shared::models::progress::WatchStatus::Watched)
        .unwrap_or(false);

    if request.watched && already_watched {
        let response = build_progress_response(
            client, table, user_id, episode_id, &series_id, season_number, episode_number,
        ).await?;
        let body = serde_json::to_string(&response)
            .map_err(|e| AppError::Internal(e.to_string()))?;
        let mut resp = Response::builder()
            .status(200)
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();
        add_cors(&mut resp);
        return Ok(resp);
    }

    if !request.watched && !already_watched {
        let response = build_progress_response(
            client, table, user_id, episode_id, &series_id, season_number, episode_number,
        ).await?;
        let body = serde_json::to_string(&response)
            .map_err(|e| AppError::Internal(e.to_string()))?;
        let mut resp = Response::builder()
            .status(200)
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();
        add_cors(&mut resp);
        return Ok(resp);
    }

    if request.watched {
        db::mark_episode(client, table, user_id, &series_id, season_number, episode_number)
            .await
            .map_err(|e| AppError::Internal(e.to_string()))?;
        db::create_watch_event(client, table, user_id, episode_id, shared::enums::watch_event_type::MARK_WATCHED)
            .await
            .map_err(|e| AppError::Internal(e.to_string()))?;
    } else {
        db::unmark_episode(client, table, user_id, &series_id, season_number, episode_number)
            .await
            .map_err(|e| AppError::Internal(e.to_string()))?;
        db::create_watch_event(client, table, user_id, episode_id, shared::enums::watch_event_type::UNMARK_WATCHED)
            .await
            .map_err(|e| AppError::Internal(e.to_string()))?;
    }

    let response = build_progress_response(
        client, table, user_id, episode_id, &series_id, season_number, episode_number,
    ).await?;

    let body = serde_json::to_string(&response)
        .map_err(|e| AppError::Internal(e.to_string()))?;

    let mut resp = Response::builder()
        .status(200)
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();
    add_cors(&mut resp);
    Ok(resp)
}

/// Mark/unmark every episode of a season in one request.
async fn handle_season_progress(
    client: &db::Client,
    table: &str,
    user_id: &str,
    series_id: &str,
    season_number: i32,
    watched: bool,
) -> Result<Response<Body>, AppError> {
    let series_id = normalize_series_id(series_id);
    let episodes = db::list_season_episode_numbers(client, table, &series_id, season_number)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    for episode_number in &episodes {
        let result = if watched {
            db::mark_episode(client, table, user_id, &series_id, season_number, *episode_number).await
        } else {
            db::unmark_episode(client, table, user_id, &series_id, season_number, *episode_number).await
        };
        result.map_err(|e| AppError::Internal(e.to_string()))?;
    }

    let body = json!({
        "seriesId": series_id,
        "seasonNumber": season_number,
        "watched": watched,
        "updatedEpisodes": episodes.len(),
    });

    let mut resp = Response::builder()
        .status(200)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    add_cors(&mut resp);
    Ok(resp)
}

async fn build_progress_response(
    client: &db::Client,
    table: &str,
    user_id: &str,
    episode_id: &str,
    series_id: &str,
    season_number: i32,
    episode_number: i32,
) -> Result<ProgressResponse, AppError> {
    let progress = db::get_episode_progress(client, table, user_id, series_id, season_number, episode_number)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    let status = progress
        .as_ref()
        .map(|p| p.status.clone())
        .unwrap_or(shared::models::progress::WatchStatus::Unwatched);
    let watched_at = progress.as_ref().and_then(|p| p.watched_at.clone());

    let watched_count = db::count_watched_in_series(client, table, user_id, series_id)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let total_episodes = db::get_series_total_episodes(client, table, series_id)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let season_watched = db::count_watched_in_season(client, table, user_id, series_id, season_number)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let season_total = db::get_season_episode_count(client, table, series_id, season_number)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    let series_pct = if total_episodes > 0 {
        (watched_count as f64 / total_episodes as f64) * 100.0
    } else {
        0.0
    };
    let season_pct = if season_total > 0 {
        (season_watched as f64 / season_total as f64) * 100.0
    } else {
        0.0
    };

    let next_episode = db::get_next_unwatched_episode(client, table, user_id, series_id)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    Ok(ProgressResponse {
        episode: EpisodeProgress {
            episode_id: episode_id.to_string(),
            status,
            watched_at,
        },
        progress: SeriesProgress {
            series_percentage: (series_pct * 10.0).round() / 10.0,
            season_percentage: (season_pct * 10.0).round() / 10.0,
        },
        next_episode,
    })
}
