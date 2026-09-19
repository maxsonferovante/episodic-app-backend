use lambda_http::{Body, Request, Response};
use shared::auth::extract_user_id;
use shared::db::{
    add_to_library, get_client, get_library_item, get_series_meta_bulk, get_user_progress_counts,
    list_library, remove_from_library,
};
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

    let series_ids: Vec<String> = items.iter().map(|item| item.series_id.clone()).collect();

    // The whole page costs two reads: one Query for the user's watched counts
    // and one BatchGetItem for the series metadata. Previously each series
    // issued its own count + metadata + season reads.
    let (watched_counts, metas) = tokio::try_join!(
        get_user_progress_counts(&client, &table, &user_id),
        get_series_meta_bulk(&client, &table, &series_ids),
    )
    .map_err(|e| AppError::Internal(e.to_string()))?;

    let body: Vec<_> = items
        .iter()
        .map(|item| {
            let meta = metas.get(&item.series_id);

            // Stored snapshot first, then the canonical series meta.
            let name = item.name.clone().or_else(|| meta.map(|m| m.name.clone()));
            let poster_path = item
                .poster_path
                .clone()
                .or_else(|| meta.and_then(|m| m.poster_path.clone()));
            let first_air_date = item
                .first_air_date
                .clone()
                .or_else(|| meta.and_then(|m| m.first_air_date.clone()));
            let status = meta.and_then(|m| m.status.clone());

            let watched = watched_counts.get(&item.series_id).copied().unwrap_or(0);
            let total = meta.map(|m| m.total_episodes).unwrap_or(0);
            let percentage =
                shared::models::progress::completion_percentage(watched, total);

            json!({
                "id": item.id,
                "seriesId": item.series_id,
                "tmdbId": extract_tmdb_id(&item.series_id).unwrap_or(0),
                "addedAt": item.added_at,
                "name": name,
                "posterPath": poster_path,
                "firstAirDate": first_air_date,
                "status": status,
                "watchedEpisodes": watched,
                "totalEpisodes": total,
                "percentage": percentage,
            })
        })
        .collect();

    let mut resp = Response::builder()
        .status(200)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(&body).unwrap()))
        .unwrap();
    add_cors(&mut resp);
    Ok(resp)
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddLibraryPayload {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    poster_path: Option<String>,
    #[serde(default)]
    first_air_date: Option<String>,
}

async fn handle_add_to_library(req: Request) -> Result<Response<Body>, AppError> {
    let user_id = extract_user_id(&req)?;
    let path = req.uri().path();
    let raw_id = extract_series_id(path)?;

    if raw_id.is_empty() {
        return Err(AppError::Internal("Invalid series ID format".into()));
    }

    let series_id = normalize_series_id(raw_id);

    // Optional metadata snapshot sent by the client (search / detail screen),
    // so the library renders without waiting for the daily sync job.
    let payload: Option<AddLibraryPayload> = serde_json::from_slice(req.body().as_ref()).ok();

    let table = get_table_name()?;
    let client = get_client().await;

    let existing = get_library_item(&client, &table, &user_id, &series_id).await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    let mut item = match existing {
        Some(item) => item,
        None => LibraryItem {
            id: id::generate("lib"),
            user_id: user_id.clone(),
            series_id: series_id.clone(),
            added_at: chrono::Utc::now().to_rfc3339(),
            name: None,
            poster_path: None,
            first_air_date: None,
        },
    };

    if let Some(p) = payload {
        if p.name.is_some() {
            item.name = p.name;
        }
        if p.poster_path.is_some() {
            item.poster_path = p.poster_path;
        }
        if p.first_air_date.is_some() {
            item.first_air_date = p.first_air_date;
        }
    }

    add_to_library(&client, &table, &item).await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    // Kick off full metadata hydration in the background (SQS). Best-effort:
    // a queue hiccup must never fail the add.
    if let Err(e) = shared::sqs::enqueue_hydrate(&item.series_id).await {
        tracing::warn!("Failed to enqueue hydrate for {}: {}", item.series_id, e);
    }

    let body = json!({
        "id": item.id,
        "seriesId": item.series_id,
        "addedAt": item.added_at,
        "name": item.name,
        "posterPath": item.poster_path,
        "firstAirDate": item.first_air_date,
    });

    let mut resp = Response::builder()
        .status(200)
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
