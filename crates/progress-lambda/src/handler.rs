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

pub async fn handle_request(req: Request) -> Result<Response<Body>, Box<dyn std::error::Error + Send + Sync>> {
    let method = req.method().as_str();
    let path = req.uri().path();

    let result = match method {
        "GET" if parse_episode_id(path).is_some() => {
            let user_id = extract_user_id(&req)?;
            let episode_id = parse_episode_id(path).unwrap();
            let table = std::env::var("DYNAMODB_TABLE_NAME")
                .unwrap_or_else(|_| "EpisodicEpisodes".to_string());
            let client = db::get_client().await;
            handle_get_progress(&client, &table, &user_id, episode_id).await
        }
        "PUT" if parse_episode_id(path).is_some() => {
            let user_id = extract_user_id(&req)?;
            let episode_id = parse_episode_id(path).unwrap();
            let body = req.body();
            let request: MarkWatchedRequest = serde_json::from_slice(body.as_ref())
                .map_err(|_| AppError::Internal("Invalid request body".into()))?;
            let table = std::env::var("DYNAMODB_TABLE_NAME")
                .unwrap_or_else(|_| "EpisodicEpisodes".to_string());
            let client = db::get_client().await;
            handle_put_progress(&client, &table, &user_id, episode_id, request).await
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
