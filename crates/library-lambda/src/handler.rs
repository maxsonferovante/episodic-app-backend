use lambda_http::{Body, Request, Response};
use shared::auth::extract_user_id;
use shared::db::{get_client, get_library_item, add_to_library, remove_from_library, list_library};
use shared::error::AppError;
use shared::id;
use shared::models::library::LibraryItem;
use serde_json::json;

fn error_response(err: AppError) -> Result<Response<Body>, Box<dyn std::error::Error + Send + Sync>> {
    let status = err.status_code();
    let body = serde_json::to_string(&err)?;
    Ok(Response::builder()
        .status(status.as_u16())
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap())
}

fn get_table_name() -> Result<String, AppError> {
    std::env::var("DYNAMODB_TABLE_NAME")
        .map_err(|_| AppError::Internal("DYNAMODB_TABLE_NAME not set".into()))
}

fn extract_series_id(path: &str) -> Result<&str, AppError> {
    path.strip_prefix("/api/v1/library/")
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::Internal("Invalid path".into()))
}

fn is_valid_series_id(id: &str) -> bool {
    id.starts_with("ser_") && id.len() > 4
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
        Err(e) => error_response(e),
    }
}

async fn handle_list_library(req: Request) -> Result<Response<Body>, AppError> {
    let user_id = extract_user_id(&req)?;
    let table = get_table_name()?;
    let client = get_client().await;

    let response = list_library(&client, &table, &user_id).await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    Ok(Response::builder()
        .status(200)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(&response).unwrap()))
        .unwrap())
}

async fn handle_add_to_library(req: Request) -> Result<Response<Body>, AppError> {
    let user_id = extract_user_id(&req)?;
    let path = req.uri().path();
    let series_id = extract_series_id(path)?;

    if !is_valid_series_id(series_id) {
        return Err(AppError::Internal("Invalid series ID format".into()));
    }

    let table = get_table_name()?;
    let client = get_client().await;

    let existing = get_library_item(&client, &table, &user_id, series_id).await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    if let Some(item) = existing {
        let body = json!({
            "id": item.id,
            "seriesId": item.series_id,
            "addedAt": item.added_at,
        });

        return Ok(Response::builder()
            .status(200)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap());
    }

    let new_item = LibraryItem {
        id: id::generate("lib"),
        user_id: user_id.clone(),
        series_id: series_id.to_string(),
        added_at: chrono::Utc::now().to_rfc3339(),
    };

    add_to_library(&client, &table, &new_item).await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    let body = json!({
        "id": new_item.id,
        "seriesId": new_item.series_id,
        "addedAt": new_item.added_at,
    });

    Ok(Response::builder()
        .status(201)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap())
}

async fn handle_remove_from_library(req: Request) -> Result<Response<Body>, AppError> {
    let user_id = extract_user_id(&req)?;
    let path = req.uri().path();
    let series_id = extract_series_id(path)?;

    if !is_valid_series_id(series_id) {
        return Err(AppError::Internal("Invalid series ID format".into()));
    }

    let table = get_table_name()?;
    let client = get_client().await;

    let existing = get_library_item(&client, &table, &user_id, series_id).await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    if existing.is_none() {
        return Err(AppError::NotInLibrary);
    }

    remove_from_library(&client, &table, &user_id, series_id).await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    Ok(Response::builder()
        .status(204)
        .body(Body::from(""))
        .unwrap())
}
