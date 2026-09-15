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

/// Parse a series identifier that may be either a bare TMDB id (`1399`) or the
/// internal id (`ser_1399`).
fn parse_tmdb_id(value: &str) -> Option<i64> {
    value.strip_prefix("ser_").unwrap_or(value).parse::<i64>().ok()
}

fn extract_series_id(path: &str, prefix: &str) -> Option<i64> {
    parse_tmdb_id(path.strip_prefix(prefix)?.split('/').next()?)
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

    let table = std::env::var("DYNAMODB_TABLE_NAME").unwrap_or_else(|_| "episodic".to_string());
    let client = db::get_client().await;

    if let Ok(Some((cached_items, cached_total_pages))) = db::get_cached_search(&client, &table, &q, page).await {
        let body = json!({
            "items": cached_items,
            "pagination": { "page": page, "totalPages": cached_total_pages }
        });
        let mut resp = Response::builder()
            .status(200)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        add_cors(&mut resp);
        return Ok(resp);
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

    let _ = db::cache_search_results(&client, &table, &q, page, &items, search_resp.total_pages).await;

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

    // 1. Resolve the series itself (cache first, then TMDB).
    let (series, providers) = match db::get_cached_series(&client, &table, tmdb_id).await {
        Ok(Some(cached)) => {
            let providers = db::get_cached_providers(&client, &table, tmdb_id).await.unwrap_or(None);
            (cached, providers)
        }
        _ => {
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

            if let Some(ref prov) = providers {
                let _ = db::cache_providers(&client, &table, tmdb_id, prov).await;
            }

            (series, providers)
        }
    };

    // 2. Ensure seasons are cached. `/tv/{id}` details don't include them, so a
    //    first visit would otherwise render an empty season list.
    let mut seasons = db::get_cached_seasons(&client, &table, tmdb_id).await.unwrap_or_default();
    if seasons.is_empty() {
        if let Ok(tmdb_seasons) = crate::tmdb::get_tv_seasons(tmdb_id).await {
            seasons = tmdb_seasons.into_iter().map(|s| Season {
                id: season_id(tmdb_id, s.season_number),
                series_id: format!("ser_{}", tmdb_id),
                tmdb_id: Some(s.id),
                season_number: s.season_number,
                name: s.name,
                overview: s.overview,
                poster_path: s.poster_path,
                air_date: s.air_date,
                episode_count: s.episode_count,
            }).collect();
            let _ = db::cache_seasons(&client, &table, tmdb_id, &seasons).await;
        }
    }

    let body = build_series_response(&series, providers, &seasons);
    let mut resp = Response::builder()
        .status(200)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    add_cors(&mut resp);
    Ok(resp)
}

/// Deterministic season id shared by every catalog response.
fn season_id(tmdb_id: i64, season_number: i32) -> String {
    format!("sea_{}_{}", tmdb_id, season_number)
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
        id: season_id(tmdb_id, s.season_number),
        series_id: format!("ser_{}", tmdb_id),
        tmdb_id: Some(s.id),
        season_number: s.season_number,
        name: s.name,
        overview: s.overview,
        poster_path: s.poster_path,
        air_date: s.air_date,
        episode_count: s.episode_count,
    }).collect();

    let _ = db::cache_seasons(&client, &table, tmdb_id, &seasons).await;

    let body = json!({ "items": seasons });
    let mut resp = Response::builder()
        .status(200)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    add_cors(&mut resp);
    Ok(resp)
}

async fn handle_season_detail(
    path: &str,
    user_id: Option<String>,
) -> Result<Response<Body>, Box<dyn std::error::Error + Send + Sync>> {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() < 7 {
        return Ok(error_response(AppError::Internal("Invalid path".into())));
    }

    let tmdb_id: i64 = parse_tmdb_id(parts[4]).ok_or_else(|| AppError::Internal("Invalid series ID".into()))?;
    let season_number: i32 = parts[6].parse().map_err(|_| AppError::Internal("Invalid season number".into()))?;

    let table = std::env::var("DYNAMODB_TABLE_NAME").unwrap_or_else(|_| "episodic".to_string());
    let client = db::get_client().await;

    let series_id = format!("ser_{}", tmdb_id);
    let season_id = format!("sea_{}_{}", tmdb_id, season_number);

    // Resolve episodes from cache, falling back to TMDB.
    let mut episodes = match db::get_cached_episodes(&client, &table, tmdb_id, season_number).await {
        Ok(cached) if !cached.is_empty() => cached,
        _ => {
            let detail = match crate::tmdb::get_tv_season_detail(tmdb_id, season_number).await {
                Ok(d) => d,
                Err(e) => return Ok(error_response(AppError::Internal(format!("TMDB season detail failed: {}", e)))),
            };

            detail.episodes.into_iter().map(|e| Episode {
                id: format!("epi_{}", e.id),
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
                status: None,
            }).collect()
        }
    };

    // Persist (upsert) so episode ids stay resolvable on GSI1 for the progress
    // lambda, and so season episode counts exist for percentage maths.
    let _ = db::upsert_episodes(&client, &table, &series_id, &season_id, season_number, &episodes).await;
    let _ = db::upsert_season_meta(&client, &table, &series_id, season_number, episodes.len() as i32).await;

    // Merge the caller's watch status when authenticated.
    if let Some(user_id) = user_id {
        if let Ok(watched) = db::get_watched_set(&client, &table, &user_id, &series_id).await {
            for ep in episodes.iter_mut() {
                if watched.contains(&(season_number, ep.episode_number)) {
                    ep.status = Some(shared::enums::watch_status::WATCHED.to_string());
                }
            }
        }
    }

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

fn build_series_response(series: &Series, providers: Option<WatchProviders>, seasons: &[Season]) -> serde_json::Value {
    let seasons_json: Vec<serde_json::Value> = seasons.iter().map(|s| {
        json!({
            "id": s.id,
            "seriesId": s.series_id,
            "tmdbId": s.tmdb_id,
            "seasonNumber": s.season_number,
            "name": s.name,
            "overview": s.overview,
            "posterPath": s.poster_path,
            "airDate": s.air_date,
            "episodeCount": s.episode_count,
        })
    }).collect();

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
        "seasons": seasons_json,
    });

    if let Some(prov) = providers {
        let flatrate = prov.flatrate.as_ref().map(|v| v.iter().map(|p| json!({
            "providerId": p.provider_id,
            "providerName": p.provider_name,
            "logoPath": p.logo_path,
        })).collect::<Vec<_>>());
        let rent = prov.rent.as_ref().map(|v| v.iter().map(|p| json!({
            "providerId": p.provider_id,
            "providerName": p.provider_name,
            "logoPath": p.logo_path,
        })).collect::<Vec<_>>());
        let buy = prov.buy.as_ref().map(|v| v.iter().map(|p| json!({
            "providerId": p.provider_id,
            "providerName": p.provider_name,
            "logoPath": p.logo_path,
        })).collect::<Vec<_>>());
        let free = prov.free.as_ref().map(|v| v.iter().map(|p| json!({
            "providerId": p.provider_id,
            "providerName": p.provider_name,
            "logoPath": p.logo_path,
        })).collect::<Vec<_>>());

        let mut providers_arr = Vec::new();
        if let Some(items) = flatrate { providers_arr.extend(items); }
        if let Some(items) = rent { providers_arr.extend(items); }
        if let Some(items) = buy { providers_arr.extend(items); }
        if let Some(items) = free { providers_arr.extend(items); }

        resp["providers"] = json!(providers_arr);
    }

    resp
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
        let user_id = shared::auth::extract_user_id(&req).ok();
        return handle_season_detail(&path, user_id).await;
    }

    if path.starts_with("/api/v1/series/") && path.matches('/').count() == 4 {
        return handle_series_details(&path).await;
    }

    Ok(error_response(AppError::Internal("Not found".into())))
}
