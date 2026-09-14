use lambda_http::{Body, Request, Response};
use shared::error::{AppError, error_response, app_error_response, add_cors};
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

pub async fn handle_request(req: Request) -> Result<Response<Body>, Box<dyn std::error::Error + Send + Sync>> {
    let method = req.method().as_str();
    let path = req.uri().path();

    let result = match method {
        "POST" if path.ends_with("/auth/google") => handle_google_login(req).await,
        "POST" if path.ends_with("/auth/refresh") => handle_refresh(req).await,
        _ => Err(AppError::Internal("Not found".into())),
    };

    match result {
        Ok(resp) => Ok(resp),
        Err(e) => Ok(app_error_response(e)),
    }
}

async fn handle_google_login(req: Request) -> Result<Response<Body>, AppError> {
    let body = req.body();
    let body_str = match body {
        Body::Text(s) => s.clone(),
        Body::Binary(b) => String::from_utf8_lossy(b).to_string(),
        _ => return Err(AppError::Internal("Request body is required".into())),
    };

    let login_req: GoogleLoginRequest = serde_json::from_str(&body_str)
        .map_err(|_| AppError::Internal("Invalid request body".into()))?;

    let google_user = validate_google_token(&login_req.id_token).await
        .map_err(|e| AppError::InvalidCredentials)?;

    let table = get_table_name()?;
    let client = get_client().await;

    let user = match get_user_by_email(&client, &table, &google_user.email).await
        .map_err(|e| AppError::Internal(e.to_string()))?
    {
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
            create_user(&client, &table, &new_user).await
                .map_err(|e| AppError::Internal(e.to_string()))?;
            new_user
        }
    };

    let secret = get_jwt_secret()?;
    let access_token = encode_access_token(&user.id, &user.email, &secret)
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let refresh_token = encode_refresh_token(&user.id, &user.email, &secret)
        .map_err(|e| AppError::Internal(e.to_string()))?;

    let body = json!({
        "user": {
            "id": user.id,
            "name": user.name,
            "email": user.email,
            "avatarUrl": user.avatar_url,
        },
        "accessToken": access_token,
        "refreshToken": refresh_token,
    });

    let mut resp = Response::builder()
        .status(200)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    add_cors(&mut resp);
    Ok(resp)
}

async fn handle_refresh(req: Request) -> Result<Response<Body>, AppError> {
    let body = req.body();
    let body_str = match body {
        Body::Text(s) => s.clone(),
        Body::Binary(b) => String::from_utf8_lossy(b).to_string(),
        _ => return Err(AppError::Internal("Request body is required".into())),
    };

    let refresh_req: RefreshRequest = serde_json::from_str(&body_str)
        .map_err(|_| AppError::Internal("Invalid request body".into()))?;

    let secret = get_jwt_secret()?;
    let claims = verify_refresh_token(&refresh_req.refresh_token, &secret)
        .map_err(|_| AppError::InvalidCredentials)?;

    let table = get_table_name()?;
    let client = get_client().await;

    let user = match get_user_by_id(&client, &table, &claims.sub).await
        .map_err(|e| AppError::Internal(e.to_string()))?
    {
        Some(u) => u,
        None => return Err(AppError::InvalidCredentials),
    };

    let access_token = encode_access_token(&user.id, &user.email, &secret)
        .map_err(|e| AppError::Internal(e.to_string()))?;

    let body = json!({ "accessToken": access_token });

    let mut resp = Response::builder()
        .status(200)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    add_cors(&mut resp);
    Ok(resp)
}
