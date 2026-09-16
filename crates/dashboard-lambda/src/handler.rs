use lambda_http::{Body, Request, Response};
use shared::auth::extract_user_id;
use shared::db::{get_client, get_continue_watching, get_upcoming, get_recent_history, get_history_page, get_calendar};
use shared::error::{AppError, app_error_response, add_cors};
use serde_json::json;

fn get_table_name() -> Result<String, AppError> {
    std::env::var("DYNAMODB_TABLE_NAME")
        .map_err(|_| AppError::Internal("DYNAMODB_TABLE_NAME not set".into()))
}

pub async fn handle_request(req: Request) -> Result<Response<Body>, Box<dyn std::error::Error + Send + Sync>> {
    let method = req.method().as_str();
    let path = req.uri().path();

    let result = match method {
        "GET" if path.ends_with("/dashboard") => handle_dashboard(req).await,
        "GET" if path.ends_with("/history") => handle_history(req).await,
        "GET" if path.ends_with("/calendar") => handle_calendar(req).await,
        _ => Err(AppError::Internal("Not found".into())),
    };

    match result {
        Ok(resp) => Ok(resp),
        Err(e) => Ok(app_error_response(e)),
    }
}

async fn handle_dashboard(req: Request) -> Result<Response<Body>, AppError> {
    let user_id = extract_user_id(&req)?;
    let table = get_table_name()?;
    let client = get_client().await;

    let (continue_watching, upcoming, recent_history) = tokio::try_join!(
        get_continue_watching(&client, &table, &user_id),
        get_upcoming(&client, &table, &user_id),
        get_recent_history(&client, &table, &user_id),
    ).map_err(|e| AppError::Internal(e.to_string()))?;

    let response = shared::models::dashboard::DashboardResponse {
        continue_watching,
        upcoming,
        recent_history,
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

async fn handle_history(req: Request) -> Result<Response<Body>, AppError> {
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

    let response = get_history_page(&client, &table, &user_id, cursor, limit).await
        .map_err(|e| AppError::Internal(e.to_string()))?;

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

async fn handle_calendar(req: Request) -> Result<Response<Body>, AppError> {
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
    let from_param = params.get("from").map(|s| s.as_str()).unwrap_or("");
    let to_param = params.get("to").map(|s| s.as_str()).unwrap_or("");

    // Accept either an explicit range (`from`/`to`, what the web client sends)
    // or the convenience `month=YYYY-MM`.
    let (from, to) = if !from_param.is_empty() && !to_param.is_empty() {
        (from_param.to_string(), to_param.to_string())
    } else if month.len() == 7 {
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
        return Err(AppError::Internal(
            "provide from/to (YYYY-MM-DD) or month (YYYY-MM)".into(),
        ));
    };

    let calendar_days = get_calendar(&client, &table, &user_id, &from, &to).await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    let response = shared::models::dashboard::CalendarResponse {
        items: calendar_days,
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
