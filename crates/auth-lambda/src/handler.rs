use lambda_http::{Body, Request, Response};
use shared::error::AppError;
use shared::models::user::{GoogleLoginRequest, RefreshRequest};
use shared::auth::{encode_access_token, encode_refresh_token, verify_refresh_token};
use shared::db::{get_client, get_user_by_email, create_user, get_user_by_id};
use shared::id;
use serde_json::json;

use crate::google::validate_google_token;

fn get_jwt_secret() -> Result<Vec<u8>, AppError> {
    std::env::var("JWT_SECRET")
        .map(|s| s.into_bytes())
        .map_err(|_| AppError::Internal("JWT_SECRET not set".into()))
}

fn get_table_name() -> Result<String, AppError> {
    std::env::var("DYNAMODB_TABLE_NAME")
        .map_err(|_| AppError::Internal("DYNAMODB_TABLE_NAME not set".into()))
}

fn error_response(status: u16, code: &str, message: &str) -> Result<Response<Body>, Box<dyn std::error::Error + Send + Sync>> {
    Ok(Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::from(json!({ "error": { "code": code, "message": message } }).to_string()))
        .unwrap())
}

pub async fn handle_request(req: Request) -> Result<Response<Body>, Box<dyn std::error::Error + Send + Sync>> {
    let method = req.method();
    let path = req.uri().path();

    match (method.as_str(), path) {
        ("POST", "/api/v1/auth/google") => handle_google_login(req).await,
        ("POST", "/api/v1/auth/refresh") => handle_refresh(req).await,
        _ => Err(AppError::Internal("Not found".into()).into()),
    }
}

async fn handle_google_login(req: Request) -> Result<Response<Body>, Box<dyn std::error::Error + Send + Sync>> {
    let body = req.body();
    let body_str = match body {
        Body::Text(s) => s.clone(),
        Body::Binary(b) => String::from_utf8_lossy(b).to_string(),
        _ => return error_response(400, "INVALID_REQUEST", "Request body is required"),
    };

    let login_req: GoogleLoginRequest = match serde_json::from_str(&body_str) {
        Ok(r) => r,
        Err(_) => return error_response(400, "INVALID_REQUEST", "Invalid request body"),
    };

    let google_user = match validate_google_token(&login_req.id_token).await {
        Ok(u) => u,
        Err(e) => return error_response(401, "INVALID_GOOGLE_TOKEN", &e.to_string()),
    };

    let table = get_table_name()?;
    let client = get_client().await;

    let user = match get_user_by_email(&client, &table, &google_user.email).await? {
        Some(existing) => existing,
        None => {
            let now = chrono::Utc::now().to_rfc3339();
            let new_user = shared::models::user::User {
                id: id::generate("usr"),
                email: google_user.email,
                name: google_user.name,
                avatar_url: google_user.picture,
                provider: "google".to_string(),
                created_at: now.clone(),
                updated_at: now,
            };
            create_user(&client, &table, &new_user).await?;
            new_user
        }
    };

    let secret = get_jwt_secret()?;
    let access_token = encode_access_token(&user.id, &user.email, &secret)?;
    let refresh_token = encode_refresh_token(&user.id, &user.email, &secret)?;

    Ok(Response::builder()
        .status(200)
        .header("content-type", "application/json")
        .body(Body::from(json!({
            "user": {
                "id": user.id,
                "name": user.name,
                "email": user.email,
                "avatarUrl": user.avatar_url,
            },
            "accessToken": access_token,
            "refreshToken": refresh_token,
        }).to_string()))
        .unwrap())
}

async fn handle_refresh(req: Request) -> Result<Response<Body>, Box<dyn std::error::Error + Send + Sync>> {
    let body = req.body();
    let body_str = match body {
        Body::Text(s) => s.clone(),
        Body::Binary(b) => String::from_utf8_lossy(b).to_string(),
        _ => return error_response(400, "INVALID_REQUEST", "Request body is required"),
    };

    let refresh_req: RefreshRequest = match serde_json::from_str(&body_str) {
        Ok(r) => r,
        Err(_) => return error_response(400, "INVALID_REQUEST", "Invalid request body"),
    };

    let secret = get_jwt_secret()?;
    let claims = match verify_refresh_token(&refresh_req.refresh_token, &secret) {
        Ok(c) => c,
        Err(_) => return error_response(401, "INVALID_REFRESH_TOKEN", "Invalid or expired refresh token"),
    };

    let table = get_table_name()?;
    let client = get_client().await;

    let user = match get_user_by_id(&client, &table, &claims.sub).await? {
        Some(u) => u,
        None => return error_response(401, "USER_NOT_FOUND", "User not found"),
    };

    let access_token = encode_access_token(&user.id, &user.email, &secret)?;

    Ok(Response::builder()
        .status(200)
        .header("content-type", "application/json")
        .body(Body::from(json!({ "accessToken": access_token }).to_string()))
        .unwrap())
}
