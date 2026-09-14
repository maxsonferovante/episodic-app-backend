use lambda_http::{Body, Request, Response};
use shared::auth::extract_user_id;
use shared::db::{get_client, get_continue_watching, get_upcoming, get_recent_history, get_history_page, get_calendar};
use shared::error::AppError;
use serde_json::json;

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
        ("GET", "/api/v1/dashboard") => handle_dashboard(req).await,
        ("GET", "/api/v1/history") => handle_history(req).await,
        ("GET", "/api/v1/calendar") => handle_calendar(req).await,
        _ => Err(AppError::Internal("Not found".into()).into()),
    }
}

async fn handle_dashboard(req: Request) -> Result<Response<Body>, Box<dyn std::error::Error + Send + Sync>> {
    let user_id = extract_user_id(&req)?;
    let table = get_table_name()?;
    let client = get_client().await;

    let (continue_watching, upcoming, recent_history) = tokio::try_join!(
        get_continue_watching(&client, &table, &user_id),
        get_upcoming(&client, &table, &user_id),
        get_recent_history(&client, &table, &user_id),
    )?;

    let response = shared::models::dashboard::DashboardResponse {
        continue_watching,
        upcoming,
        recent_history,
    };

    Ok(Response::builder()
        .status(200)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(&response)?))
        .unwrap())
}

async fn handle_history(req: Request) -> Result<Response<Body>, Box<dyn std::error::Error + Send + Sync>> {
    let user_id = extract_user_id(&req)?;
    let table = get_table_name()?;
    let client = get_client().await;

    let query_str = req.uri().query().unwrap_or("");
    let params: std::collections::HashMap<String, String> = query_str
        .split('&')
        .filter_map(|pair| {
            let mut parts = pair.splitn(2, '=');
            Some((parts.next()?.to_string(), parts.next().unwrap_or("").to_string()))
        })
        .collect();

    let cursor = params.get("cursor").map(|s| s.as_str());
    let limit = params.get("limit")
        .and_then(|s| s.parse::<i32>().ok())
        .unwrap_or(20)
        .min(100);

    let response = get_history_page(&client, &table, &user_id, cursor, limit).await?;

    Ok(Response::builder()
        .status(200)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(&response)?))
        .unwrap())
}

async fn handle_calendar(req: Request) -> Result<Response<Body>, Box<dyn std::error::Error + Send + Sync>> {
    let user_id = extract_user_id(&req)?;
    let table = get_table_name()?;
    let client = get_client().await;

    let query_str = req.uri().query().unwrap_or("");
    let params: std::collections::HashMap<String, String> = query_str
        .split('&')
        .filter_map(|pair| {
            let mut parts = pair.splitn(2, '=');
            Some((parts.next()?.to_string(), parts.next().unwrap_or("").to_string()))
        })
        .collect();

    let month = params.get("month").map(|s| s.as_str()).unwrap_or("");
    let (from, to) = if month.len() == 7 {
        let year_month = month;
        let from = format!("{}-01", year_month);
        let next_month = {
            let parts: Vec<&str> = year_month.split('-').collect();
            let y: i32 = parts[0].parse().unwrap_or(2026);
            let m: i32 = parts[1].parse().unwrap_or(1);
            if m == 12 {
                format!("{}-01", y + 1)
            } else {
                format!("{}-{:02}", y, m + 1)
            }
        };
        (from, next_month)
    } else {
        return error_response(400, "INVALID_PARAMS", "month parameter required (YYYY-MM)");
    };

    let calendar_days = get_calendar(&client, &table, &user_id, &from, &to).await?;

    let response = shared::models::dashboard::CalendarResponse {
        items: calendar_days,
    };

    Ok(Response::builder()
        .status(200)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(&response)?))
        .unwrap())
}
