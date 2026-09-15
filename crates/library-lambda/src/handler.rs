use lambda_http::{Body, Request, Response};
use shared::auth::extract_user_id;
use shared::db::{get_client, get_library_item, add_to_library, remove_from_library, list_library, get_cached_series, count_watched_in_series, get_series_total_episodes};
use shared::error::{AppError, app_error_response as error_response, add_cors};
use shared::id;
use shared::models::library::LibraryItem;
use serde_json::json;

fn get_table_name() -> Result<String, AppError> {
    std::env::var("DYNAMODB_TABLE_NAME")
        .map_err(|_| AppError::Internal("DYNAMODB_TABLE_NAME not set".into()))
}

fn extract_series_id(path: &str) -> Result<&str, AppError> {
    path.strip_prefix("/api/v1/library/")
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::Internal("Invalid path".into()))
}

fn normalize_series_id(id: &str) -> String {
    if id.starts_with("ser_") {
        id.to_string()
    } else {
        format!("ser_{}", id)
    }
}

fn extract_tmdb_id(series_id: &str) -> Option<i64> {
    series_id.strip_prefix("ser_")?.parse().ok()
}

pub async fn handle_request(req: Request) -> Result<Response<Body>, Box<dyn std::error::Error + Send + Sync>> {
    let method = req.method();
    let path = req.uri().path();

    let result = match (method.as_str(), path) {
        ("GET", "/api/v1/library") => handle_list_library(req).await,
        ("PUT", p) if p.starts_with("/api/v1/library/") => handle_add_to_library(req).await,
        ("DELETE", p) if p.starts_with("/api/v1/library/") => handle_remove_from_library(req).await,
        _ => Err(AppError::Internal("Not found".into())),
    };

    match result {
        Ok(resp) => Ok(resp),
        Err(e) => Ok(error_response(e)),
    }
}

async fn handle_list_library(req: Request) -> Result<Response<Body>, AppError> {
    let user_id = extract_user_id(&req)?;
    let table = get_table_name()?;
    let client = get_client().await;

    let items = list_library(&client, &table, &user_id).await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    let body: Vec<_> = items.iter().map(|item| {
        let tmdb_id = extract_tmdb_id(&item.series_id).unwrap_or(0);
        json!({
            "id": item.id,
            "seriesId": item.series_id,
            "tmdbId": tmdb_id,
            "addedAt": item.added_at,
        })
    }).collect();

    let items_with_details = futures::future::join_all(body.into_iter().map(|mut item| {
        let client = &client;
        let table = &table;
        let user_id = user_id.clone();
        let series_id = item["seriesId"].as_str().unwrap_or("").to_string();
        let tmdb_id = item["tmdbId"].as_i64().unwrap_or(0);
        async move {
            if tmdb_id > 0 {
                if let Ok(Some(series)) = get_cached_series(client, table, tmdb_id).await {
                    item["name"] = json!(series.name);
                    item["posterPath"] = json!(series.poster_path);
                    item["firstAirDate"] = json!(series.first_air_date);
                }
            }

            // Watch progress for the card stats.
            let watched = count_watched_in_series(client, table, &user_id, &series_id)
                .await
                .unwrap_or(0);
            let total = get_series_total_episodes(client, table, &series_id)
                .await
                .unwrap_or(0);
            let percentage = if total > 0 {
                ((watched as f64 / total as f64) * 100.0).round() as i64
            } else {
                0
            };
            item["watchedEpisodes"] = json!(watched);
            item["totalEpisodes"] = json!(total);
            item["percentage"] = json!(percentage);

            item
        }
    })).await;

    let mut resp = Response::builder()
        .status(200)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(&items_with_details).unwrap()))
        .unwrap();
    add_cors(&mut resp);
    Ok(resp)
}

async fn handle_add_to_library(req: Request) -> Result<Response<Body>, AppError> {
    let user_id = extract_user_id(&req)?;
    let path = req.uri().path();
    let raw_id = extract_series_id(path)?;

    if raw_id.is_empty() {
        return Err(AppError::Internal("Invalid series ID format".into()));
    }

    let series_id = normalize_series_id(raw_id);

    let table = get_table_name()?;
    let client = get_client().await;

    let existing = get_library_item(&client, &table, &user_id, &series_id).await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    if let Some(item) = existing {
        let body = json!({
            "id": item.id,
            "seriesId": item.series_id,
            "addedAt": item.added_at,
        });

        let mut resp = Response::builder()
            .status(200)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        add_cors(&mut resp);
        return Ok(resp);
    }

    let new_item = LibraryItem {
        id: id::generate("lib"),
        user_id: user_id.clone(),
        series_id,
        added_at: chrono::Utc::now().to_rfc3339(),
    };

    add_to_library(&client, &table, &new_item).await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    let body = json!({
        "id": new_item.id,
        "seriesId": new_item.series_id,
        "addedAt": new_item.added_at,
    });

    let mut resp = Response::builder()
        .status(201)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    add_cors(&mut resp);
    Ok(resp)
}

async fn handle_remove_from_library(req: Request) -> Result<Response<Body>, AppError> {
    let user_id = extract_user_id(&req)?;
    let path = req.uri().path();
    let raw_id = extract_series_id(path)?;

    if raw_id.is_empty() {
        return Err(AppError::Internal("Invalid series ID format".into()));
    }

    let series_id = normalize_series_id(raw_id);

    let table = get_table_name()?;
    let client = get_client().await;

    let existing = get_library_item(&client, &table, &user_id, &series_id).await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    if existing.is_none() {
        return Err(AppError::NotInLibrary);
    }

    remove_from_library(&client, &table, &user_id, &series_id).await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    let mut resp = Response::builder()
        .status(204)
        .body(Body::from(""))
        .unwrap();
    add_cors(&mut resp);
    Ok(resp)
}
