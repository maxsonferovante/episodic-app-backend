use lambda_http::{Body, Request, Response};
use shared::auth::extract_user_id;
use shared::db::{get_client, get_releases, get_history_page, get_calendar};
use shared::error::{AppError, app_error_response, add_cors};

fn get_table_name() -> Result<String, AppError> {
    std::env::var("DYNAMODB_TABLE_NAME")
        .map_err(|_| AppError::Internal("DYNAMODB_TABLE_NAME not set".into()))
}

/// Percent-decode a query value (`%23` -> `#`, `+` -> space). API Gateway
/// forwards query values still-encoded and `req.uri().query()` does not
/// decode them — comparing an encoded cursor against stored keys silently
/// matches the wrong range (`%` sorts above `#`, so an encoded `EVT%23…`
/// cursor would return page 1 forever).
fn url_decode(s: &str) -> String {
    fn hex_val(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    }

    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push(h << 4 | l);
                i += 3;
                continue;
            }
        }
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub async fn handle_request(req: Request) -> Result<Response<Body>, Box<dyn std::error::Error + Send + Sync>> {
    let method = req.method().as_str();
    let path = req.uri().path();

    let result = match method {
        "GET" if path.ends_with("/history") => handle_history(req).await,
        "GET" if path.ends_with("/calendar") => handle_calendar(req).await,
        "GET" if path.ends_with("/releases") => handle_releases(req).await,
        _ => Err(AppError::Internal("Not found".into())),
    };

    match result {
        Ok(resp) => Ok(resp),
        Err(e) => Ok(app_error_response(e)),
    }
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

    let cursor = params.get("cursor").map(|s| url_decode(s));
    let limit = params.get("limit")
        .and_then(|s| s.parse::<i32>().ok())
        .unwrap_or(20)
        .min(100);

    // Page tokens are opaque sealed cursors — open them back into raw keys.
    // Anything else (tampered, raw, rotated key) is a 400, never page 1.
    let cursor_key = cursor
        .as_deref()
        .map(shared::cursor::open_cursor)
        .transpose()
        .map_err(|_| AppError::InvalidCursor)?;

    let mut response =
        get_history_page(&client, &table, &user_id, cursor_key.as_deref(), limit)
            .await
            .map_err(|e| AppError::Internal(e.to_string()))?;

    // Seal the outgoing cursor so raw table keys never leak to clients.
    response.next_cursor = response
        .next_cursor
        .map(|sk| shared::cursor::seal_cursor(&sk))
        .transpose()
        .map_err(|_| AppError::Internal("cursor key not configured".into()))?;

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
    let (from, to) = parse_window(query_str)?;

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

/// Release calendar: every episode from the user's library series airing in
/// `[from, to)`. Same window params as `/calendar`.
async fn handle_releases(req: Request) -> Result<Response<Body>, AppError> {
    let user_id = extract_user_id(&req)?;
    let table = get_table_name()?;
    let client = get_client().await;

    let query_str = req.uri().query().unwrap_or("");
    let (from, to) = parse_window(query_str)?;

    let items = get_releases(&client, &table, &user_id, &from, &to).await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    let response = shared::models::dashboard::ReleasesResponse { from, to, items };

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

/// Accept either an explicit range (`from`/`to`, what the web client sends)
/// or the convenience `month=YYYY-MM`. Windows longer than a year are
/// rejected so one call can't scan the whole catalog.
fn parse_window(query_str: &str) -> Result<(String, String), AppError> {
    let params: std::collections::HashMap<String, String> = query_str
        .split('&')
        .filter_map(|pair| {
            let mut parts = pair.splitn(2, '=');
            Some((parts.next()?.to_string(), parts.next().unwrap_or("").to_string()))
        })
        .collect();

    let month = params.get("month").map(|s| url_decode(s)).unwrap_or_default();
    let from_param = params.get("from").map(|s| url_decode(s)).unwrap_or_default();
    let to_param = params.get("to").map(|s| url_decode(s)).unwrap_or_default();

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

    if from.len() != 10 || to.len() != 10 || from.as_str() > to.as_str() {
        return Err(AppError::Internal("invalid window: use from<=to as YYYY-MM-DD".into()));
    }
    // Long windows are fine: `MAX_RELEASE_ITEMS` bounds the payload.

    Ok((from, to))
}
