use lambda_http::{Body, Request, Response};
use shared::db;
use shared::error::{AppError, app_error_response as error_response, add_cors};
use shared::id;
use shared::models::series::{CatalogSeries, Series, WatchProviders, Provider};
use shared::models::season::Season;
use shared::models::episode::Episode;
use serde_json::json;

fn parse_query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let mut parts = pair.splitn(2, '=');
        let k = parts.next()?;
        let v = parts.next().unwrap_or("");
        if k == key {
            Some(v.replace('+', " "))
        } else {
            None
        }
    })
}

fn extract_series_id(path: &str, prefix: &str) -> Option<i64> {
    path.strip_prefix(prefix)?
        .split('/')
        .next()?
        .parse::<i64>()
        .ok()
}

async fn handle_search(req: Request) -> Result<Response<Body>, Box<dyn std::error::Error + Send + Sync>> {
    let query = req.uri().query().unwrap_or("");
    let q = parse_query_param(query, "q").unwrap_or_default();
    let page: i32 = parse_query_param(query, "page")
        .and_then(|p| p.parse().ok())
        .unwrap_or(1);

    if q.is_empty() {
        return Ok(error_response(AppError::Internal("Missing required parameter: q".into())));
    }

    let search_resp = match crate::tmdb::search_tv(&q, page).await {
        Ok(r) => r,
        Err(e) => return Ok(error_response(AppError::Internal(format!("TMDB search failed: {}", e)))),
    };

    let items: Vec<CatalogSeries> = search_resp.results.into_iter().map(|r| CatalogSeries {
        id: r.id,
        name: r.name,
        poster_path: r.poster_path,
        first_air_date: r.first_air_date,
    }).collect();

    let body = json!({
        "items": items,
        "pagination": { "page": page, "totalPages": search_resp.total_pages }
    });

    let mut resp = Response::builder()
        .status(200)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    add_cors(&mut resp);
    Ok(resp)
}

async fn handle_series_details(path: &str) -> Result<Response<Body>, Box<dyn std::error::Error + Send + Sync>> {
    let tmdb_id = extract_series_id(path, "/api/v1/series/")
        .ok_or_else(|| AppError::Internal("Invalid series ID".into()))?;

    let table = std::env::var("DYNAMODB_TABLE_NAME").unwrap_or_else(|_| "episodic".to_string());
    let client = db::get_client().await;

    if let Ok(Some(cached)) = db::get_cached_series(&client, &table, tmdb_id).await {
        let providers = extract_providers_from_cache(&client, &table, tmdb_id).await;
        let body = build_series_response(&cached, providers);
        let mut resp = Response::builder()
            .status(200)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        add_cors(&mut resp);
        return Ok(resp);
    }

    let details = match crate::tmdb::get_tv_details(tmdb_id).await {
        Ok(d) => d,
        Err(e) => return Ok(error_response(AppError::Internal(format!("TMDB details failed: {}", e)))),
    };

    let now = chrono::Utc::now().to_rfc3339();
    let series = Series {
        id: id::generate("ser"),
        tmdb_id: details.id,
        imdb_id: details.imdb_id,
        name: details.name,
        original_name: details.original_name,
        overview: details.overview.unwrap_or_default(),
        poster_path: details.poster_path,
        backdrop_path: details.backdrop_path,
        first_air_date: details.first_air_date,
        last_air_date: details.last_air_date,
        status: details.status.unwrap_or_default(),
        number_of_seasons: details.number_of_seasons.unwrap_or(0),
        number_of_episodes: details.number_of_episodes.unwrap_or(0),
        created_at: now.clone(),
        updated_at: now,
    };

    let _ = db::cache_series(&client, &table, &series).await;

    let providers = details.watch_providers.and_then(|wp| wp.results).and_then(|r| {
        let country = detect_country();
        r.get(&country).map(|cp| WatchProviders {
            flatrate: cp.flatrate.as_ref().map(|v| v.iter().map(|p| Provider {
                provider_id: p.provider_id,
                provider_name: p.provider_name.clone(),
                logo_path: p.logo_path.clone(),
            }).collect()),
            rent: cp.rent.as_ref().map(|v| v.iter().map(|p| Provider {
                provider_id: p.provider_id,
                provider_name: p.provider_name.clone(),
                logo_path: p.logo_path.clone(),
            }).collect()),
            buy: cp.buy.as_ref().map(|v| v.iter().map(|p| Provider {
                provider_id: p.provider_id,
                provider_name: p.provider_name.clone(),
                logo_path: p.logo_path.clone(),
            }).collect()),
            free: cp.free.as_ref().map(|v| v.iter().map(|p| Provider {
                provider_id: p.provider_id,
                provider_name: p.provider_name.clone(),
                logo_path: p.logo_path.clone(),
            }).collect()),
        })
    });

    let body = build_series_response(&series, providers);
    let mut resp = Response::builder()
        .status(200)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    add_cors(&mut resp);
    Ok(resp)
}

async fn handle_seasons_list(path: &str) -> Result<Response<Body>, Box<dyn std::error::Error + Send + Sync>> {
    let tmdb_id = extract_series_id(path, "/api/v1/series/")
        .ok_or_else(|| AppError::Internal("Invalid series ID".into()))?;

    let table = std::env::var("DYNAMODB_TABLE_NAME").unwrap_or_else(|_| "episodic".to_string());
    let client = db::get_client().await;

    if let Ok(cached) = db::get_cached_seasons(&client, &table, tmdb_id).await {
        if !cached.is_empty() {
            let body = json!({ "items": cached });
            let mut resp = Response::builder()
                .status(200)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap();
            add_cors(&mut resp);
            return Ok(resp);
        }
    }

    let tmdb_seasons = match crate::tmdb::get_tv_seasons(tmdb_id).await {
        Ok(s) => s,
        Err(e) => return Ok(error_response(AppError::Internal(format!("TMDB seasons failed: {}", e)))),
    };

    let seasons: Vec<Season> = tmdb_seasons.into_iter().map(|s| Season {
        id: id::generate("sea"),
        series_id: format!("ser_{}", tmdb_id),
        tmdb_id: Some(s.id),
        season_number: s.season_number,
        name: s.name,
        overview: s.overview,
        poster_path: s.poster_path,
        air_date: s.air_date,
        episode_count: s.episode_count,
    }).collect();

    let body = json!({ "items": seasons });
    let mut resp = Response::builder()
        .status(200)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    add_cors(&mut resp);
    Ok(resp)
}

async fn handle_season_detail(path: &str) -> Result<Response<Body>, Box<dyn std::error::Error + Send + Sync>> {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() < 7 {
        return Ok(error_response(AppError::Internal("Invalid path".into())));
    }

    let tmdb_id: i64 = parts[4].parse().map_err(|_| AppError::Internal("Invalid series ID".into()))?;
    let season_number: i32 = parts[6].parse().map_err(|_| AppError::Internal("Invalid season number".into()))?;

    let table = std::env::var("DYNAMODB_TABLE_NAME").unwrap_or_else(|_| "episodic".to_string());
    let client = db::get_client().await;

    if let Ok(cached) = db::get_cached_episodes(&client, &table, tmdb_id, season_number).await {
        if !cached.is_empty() {
            let body = json!({
                "seasonNumber": season_number,
                "episodes": cached,
            });
            let mut resp = Response::builder()
                .status(200)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap();
            add_cors(&mut resp);
            return Ok(resp);
        }
    }

    let detail = match crate::tmdb::get_tv_season_detail(tmdb_id, season_number).await {
        Ok(d) => d,
        Err(e) => return Ok(error_response(AppError::Internal(format!("TMDB season detail failed: {}", e)))),
    };

    let season_id = id::generate("sea");
    let series_id = format!("ser_{}", tmdb_id);

    let episodes: Vec<Episode> = detail.episodes.into_iter().map(|e| Episode {
        id: id::generate("epi"),
        series_id: series_id.clone(),
        season_id: season_id.clone(),
        tmdb_id: Some(e.id),
        episode_number: e.episode_number,
        name: e.name,
        overview: e.overview,
        still_path: e.still_path,
        air_date: e.air_date,
        runtime: e.runtime,
        vote_average: e.vote_average,
    }).collect();

    let body = json!({
        "seasonNumber": season_number,
        "episodes": episodes,
    });

    let mut resp = Response::builder()
        .status(200)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    add_cors(&mut resp);
    Ok(resp)
}

fn detect_country() -> String {
    std::env::var("TMDB_COUNTRY").unwrap_or_else(|_| "US".to_string())
}

fn build_series_response(series: &Series, providers: Option<WatchProviders>) -> serde_json::Value {
    let mut resp = json!({
        "id": series.id,
        "tmdbId": series.tmdb_id,
        "name": series.name,
        "originalName": series.original_name,
        "overview": series.overview,
        "posterPath": series.poster_path,
        "backdropPath": series.backdrop_path,
        "firstAirDate": series.first_air_date,
        "lastAirDate": series.last_air_date,
        "status": series.status,
        "numberOfSeasons": series.number_of_seasons,
        "numberOfEpisodes": series.number_of_episodes,
    });

    if let Some(prov) = providers {
        if let Ok(v) = serde_json::to_value(&prov) {
            resp["watchProviders"] = v;
        }
    }

    resp
}

async fn extract_providers_from_cache(_client: &aws_sdk_dynamodb::Client, _table: &str, _tmdb_id: i64) -> Option<WatchProviders> {
    None
}

pub async fn handle_request(req: Request) -> Result<Response<Body>, Box<dyn std::error::Error + Send + Sync>> {
    let method = req.method().clone();
    let path = req.uri().path().to_string();

    if method != "GET" {
        return Ok(error_response(AppError::Internal("Method not allowed".into())));
    }

    if path.contains("/api/v1/series/search") {
        return handle_search(req).await;
    }

    if path.starts_with("/api/v1/series/") && path.ends_with("/seasons") {
        return handle_seasons_list(&path).await;
    }

    if path.matches('/').count() == 6 && path.contains("/seasons/") {
        return handle_season_detail(&path).await;
    }

    if path.starts_with("/api/v1/series/") && path.matches('/').count() == 4 {
        return handle_series_details(&path).await;
    }

    Ok(error_response(AppError::Internal("Not found".into())))
}
