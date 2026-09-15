use lambda_runtime::LambdaEvent;
use crate::ScheduledEvent;
use shared::db::get_client;
use aws_sdk_dynamodb::types::{AttributeValue, PutRequest, WriteRequest};
use std::collections::HashMap;
use chrono::Utc;

const TMDB_BASE_URL: &str = "https://api.themoviedb.org/3";

#[derive(Debug, serde::Deserialize)]
struct TmdbSeriesResponse {
    id: i64,
    name: String,
    overview: String,
    poster_path: Option<String>,
    backdrop_path: Option<String>,
    first_air_date: Option<String>,
    last_air_date: Option<String>,
    status: String,
    number_of_seasons: i32,
    number_of_episodes: i32,
    seasons: Vec<TmdbSeason>,
}

#[derive(Debug, serde::Deserialize)]
struct TmdbSeason {
    id: i64,
    season_number: i32,
    name: String,
    overview: Option<String>,
    poster_path: Option<String>,
    air_date: Option<String>,
    episode_count: i32,
}

#[derive(Debug, serde::Deserialize)]
struct TmdbSeasonDetail {
    id: i64,
    season_number: i32,
    name: String,
    episodes: Vec<TmdbEpisode>,
}

#[derive(Debug, serde::Deserialize)]
struct TmdbEpisode {
    id: i64,
    episode_number: i32,
    name: String,
    overview: Option<String>,
    still_path: Option<String>,
    air_date: Option<String>,
    runtime: Option<i32>,
    vote_average: Option<f64>,
}

#[derive(Debug, serde::Deserialize)]
struct TmdbWatchProvidersResponse {
    results: HashMap<String, TmdbCountryProviders>,
}

#[derive(Debug, serde::Deserialize)]
struct TmdbCountryProviders {
    flatrate: Option<Vec<TmdbProvider>>,
    rent: Option<Vec<TmdbProvider>>,
    buy: Option<Vec<TmdbProvider>>,
}

#[derive(Debug, serde::Deserialize)]
struct TmdbProvider {
    provider_id: i32,
    provider_name: String,
}

#[derive(Debug, serde::Serialize)]
struct SyncMetrics {
    series_scanned: usize,
    series_synced: usize,
    episodes_written: usize,
    errors: usize,
    duration_ms: u128,
}

pub async fn handle_scheduled_event(
    event: LambdaEvent<ScheduledEvent>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let start = Utc::now();
    tracing::info!("Sync job triggered: {:?}", event.payload);

    let table_name = std::env::var("DYNAMODB_TABLE_NAME")
        .unwrap_or_else(|_| "EpisodicEpisodes".to_string());
    let tmdb_api_key = std::env::var("TMDB_API_KEY")
        .map_err(|_| "TMDB_API_KEY not set")?;

    let client = get_client().await;
    let http_client = reqwest::Client::new();

    let series_ids = scan_library_items(&client, &table_name).await?;
    tracing::info!("Found {} library items", series_ids.len());

    let unique_series: Vec<String> = series_ids.into_iter().collect::<std::collections::HashSet<_>>().into_iter().collect();
    let total = unique_series.len();
    let mut synced = 0;
    let mut episodes_written = 0;
    let mut errors = 0;

    for (i, series_id) in unique_series.iter().enumerate() {
        tracing::info!("Syncing series {}/{}: {}", i + 1, total, series_id);
        match sync_series(&http_client, &client, &table_name, &tmdb_api_key, series_id).await {
            Ok(ep_count) => {
                synced += 1;
                episodes_written += ep_count;
            }
            Err(e) => {
                tracing::error!("Failed to sync series {}: {}", series_id, e);
                errors += 1;
            }
        }
    }

    let duration = Utc::now().signed_duration_since(start);
    let metrics = SyncMetrics {
        series_scanned: total,
        series_synced: synced,
        episodes_written,
        errors,
        duration_ms: duration.num_milliseconds() as u128,
    };

    tracing::info!("Sync completed: {:?}", metrics);
    Ok(())
}

async fn scan_library_items(
    client: &aws_sdk_dynamodb::Client,
    table_name: &str,
) -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
    let mut series_ids = Vec::new();
    let mut last_key: Option<HashMap<String, AttributeValue>> = None;

    loop {
        let mut req = client
            .scan()
            .table_name(table_name)
            .filter_expression("begins_with(PK, :pk_prefix) AND begins_with(SK, :sk_prefix)")
            .expression_attribute_values(":pk_prefix", AttributeValue::S("USR#".to_string()))
            .expression_attribute_values(":sk_prefix", AttributeValue::S("LIB#".to_string()));

        if let Some(ref key) = last_key {
            req = req.set_exclusive_start_key(Some(key.clone()));
        }

        let result = req.send().await?;

        for item in result.items() {
            if let Some(sk) = item.get("SK").and_then(|v| v.as_s().ok()) {
                if let Some(series_id) = sk.strip_prefix("LIB#") {
                    series_ids.push(series_id.to_string());
                }
            }
        }

        last_key = result.last_evaluated_key().cloned();
        if last_key.is_none() {
            break;
        }
    }

    Ok(series_ids)
}

async fn sync_series(
    http: &reqwest::Client,
    client: &aws_sdk_dynamodb::Client,
    table_name: &str,
    api_key: &str,
    series_id: &str,
) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    let tmdb_id = extract_tmdb_id(series_id);
    let details = fetch_series_details(http, api_key, tmdb_id).await?;
    let providers = fetch_watch_providers(http, api_key, tmdb_id).await;

    write_series_meta(client, table_name, series_id, &details).await?;

    let mut total_episodes = 0;
    for season in &details.seasons {
        if season.season_number == 0 {
            continue;
        }
        let season_detail = fetch_season_detail(http, api_key, tmdb_id, season.season_number).await?;
        let ep_count = season_detail.episodes.len();
        write_season(client, table_name, series_id, season).await?;
        write_episodes(client, table_name, series_id, &season_detail).await?;
        total_episodes += ep_count;
    }

    if let Some(prov) = providers {
        write_providers(client, table_name, series_id, &prov).await?;
    }

    Ok(total_episodes)
}

fn extract_tmdb_id(series_id: &str) -> i64 {
    series_id
        .split('_')
        .last()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

async fn fetch_series_details(
    http: &reqwest::Client,
    api_key: &str,
    tmdb_id: i64,
) -> Result<TmdbSeriesResponse, Box<dyn std::error::Error + Send + Sync>> {
    let url = format!("{}/tv/{}?api_key={}", TMDB_BASE_URL, tmdb_id, api_key);
    let resp = http.get(&url).send().await?.error_for_status()?.json().await?;
    Ok(resp)
}

async fn fetch_watch_providers(
    http: &reqwest::Client,
    api_key: &str,
    tmdb_id: i64,
) -> Option<TmdbWatchProvidersResponse> {
    let url = format!("{}/tv/{}/watch/providers?api_key={}", TMDB_BASE_URL, tmdb_id, api_key);
    http.get(&url).send().await.ok()?.error_for_status().ok()?.json().await.ok()
}

async fn fetch_season_detail(
    http: &reqwest::Client,
    api_key: &str,
    tmdb_id: i64,
    season_number: i32,
) -> Result<TmdbSeasonDetail, Box<dyn std::error::Error + Send + Sync>> {
    let url = format!(
        "{}/tv/{}/season/{}?api_key={}",
        TMDB_BASE_URL, tmdb_id, season_number, api_key
    );
    let resp = http.get(&url).send().await?.error_for_status()?.json().await?;
    Ok(resp)
}

async fn write_series_meta(
    client: &aws_sdk_dynamodb::Client,
    table_name: &str,
    series_id: &str,
    details: &TmdbSeriesResponse,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut item = HashMap::new();
    item.insert("PK".to_string(), AttributeValue::S(format!("SER#{}", series_id)));
    item.insert("SK".to_string(), AttributeValue::S("META".to_string()));
    item.insert("tmdbId".to_string(), AttributeValue::N(details.id.to_string()));
    item.insert("name".to_string(), AttributeValue::S(details.name.clone()));
    item.insert("overview".to_string(), AttributeValue::S(details.overview.clone()));
    item.insert("status".to_string(), AttributeValue::S(details.status.clone()));
    item.insert("numberOfSeasons".to_string(), AttributeValue::N(details.number_of_seasons.to_string()));
    item.insert("numberOfEpisodes".to_string(), AttributeValue::N(details.number_of_episodes.to_string()));
    item.insert("updatedAt".to_string(), AttributeValue::S(Utc::now().to_rfc3339()));

    if let Some(ref p) = details.poster_path {
        item.insert("posterPath".to_string(), AttributeValue::S(p.clone()));
    }
    if let Some(ref b) = details.backdrop_path {
        item.insert("backdropPath".to_string(), AttributeValue::S(b.clone()));
    }
    if let Some(ref d) = details.first_air_date {
        item.insert("firstAirDate".to_string(), AttributeValue::S(d.clone()));
    }
    if let Some(ref d) = details.last_air_date {
        item.insert("lastAirDate".to_string(), AttributeValue::S(d.clone()));
    }

    client
        .put_item()
        .table_name(table_name)
        .set_item(Some(item))
        .send()
        .await?;

    Ok(())
}

async fn write_season(
    client: &aws_sdk_dynamodb::Client,
    table_name: &str,
    series_id: &str,
    season: &TmdbSeason,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut item = HashMap::new();
    item.insert("PK".to_string(), AttributeValue::S(format!("SER#{}", series_id)));
    item.insert("SK".to_string(), AttributeValue::S(format!("SN#{:02}", season.season_number)));
    item.insert("tmdbId".to_string(), AttributeValue::N(season.id.to_string()));
    item.insert("seasonNumber".to_string(), AttributeValue::N(season.season_number.to_string()));
    item.insert("name".to_string(), AttributeValue::S(season.name.clone()));
    item.insert("episodeCount".to_string(), AttributeValue::N(season.episode_count.to_string()));

    if let Some(ref o) = season.overview {
        item.insert("overview".to_string(), AttributeValue::S(o.clone()));
    }
    if let Some(ref p) = season.poster_path {
        item.insert("posterPath".to_string(), AttributeValue::S(p.clone()));
    }
    if let Some(ref d) = season.air_date {
        item.insert("airDate".to_string(), AttributeValue::S(d.clone()));
    }

    client
        .put_item()
        .table_name(table_name)
        .set_item(Some(item))
        .send()
        .await?;

    Ok(())
}

async fn write_episodes(
    client: &aws_sdk_dynamodb::Client,
    table_name: &str,
    series_id: &str,
    season: &TmdbSeasonDetail,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut requests = Vec::new();

    for ep in &season.episodes {
        let mut item = HashMap::new();
        item.insert("PK".to_string(), AttributeValue::S(format!("SER#{}", series_id)));
        item.insert("SK".to_string(), AttributeValue::S(format!(
            "EP#{:02}#{:02}",
            season.season_number, ep.episode_number
        )));
        item.insert("tmdbId".to_string(), AttributeValue::N(ep.id.to_string()));
        item.insert("seasonNumber".to_string(), AttributeValue::N(season.season_number.to_string()));
        item.insert("episodeNumber".to_string(), AttributeValue::N(ep.episode_number.to_string()));
        item.insert("name".to_string(), AttributeValue::S(ep.name.clone()));

        // Stable episode id + GSI1 index so the progress lambda can resolve it.
        let episode_id = format!("epi_{}", ep.id);
        item.insert("id".to_string(), AttributeValue::S(episode_id.clone()));
        item.insert("GSI1PK".to_string(), AttributeValue::S(episode_id.clone()));
        item.insert("GSI1SK".to_string(), AttributeValue::S(format!("EPI#{}", episode_id)));
        item.insert("seriesId".to_string(), AttributeValue::S(series_id.to_string()));
        item.insert("seasonId".to_string(), AttributeValue::S(format!("sea_{}", season.season_number)));

        if let Some(ref o) = ep.overview {
            item.insert("overview".to_string(), AttributeValue::S(o.clone()));
        }
        if let Some(ref s) = ep.still_path {
            item.insert("stillPath".to_string(), AttributeValue::S(s.clone()));
        }
        if let Some(ref d) = ep.air_date {
            item.insert("airDate".to_string(), AttributeValue::S(d.clone()));
        }
        if let Some(r) = ep.runtime {
            item.insert("runtime".to_string(), AttributeValue::N(r.to_string()));
        }
        if let Some(v) = ep.vote_average {
            item.insert("voteAverage".to_string(), AttributeValue::N(v.to_string()));
        }

        requests.push(WriteRequest::builder()
            .put_request(PutRequest::builder().set_item(Some(item)).build()?)
            .build());
    }

    for chunk in requests.chunks(25) {
        let mut request_items = HashMap::new();
        request_items.insert(table_name.to_string(), chunk.to_vec());
        client
            .batch_write_item()
            .set_request_items(Some(request_items))
            .send()
            .await?;
    }

    Ok(())
}

async fn write_providers(
    client: &aws_sdk_dynamodb::Client,
    table_name: &str,
    series_id: &str,
    providers: &TmdbWatchProvidersResponse,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if let Some(country) = providers.results.get("US") {
        let mut item = HashMap::new();
        item.insert("PK".to_string(), AttributeValue::S(format!("SER#{}", series_id)));
        item.insert("SK".to_string(), AttributeValue::S("PROVIDERS".to_string()));
        item.insert("country".to_string(), AttributeValue::S("US".to_string()));

        let mut provider_names = Vec::new();
        if let Some(ref flatrate) = country.flatrate {
            for p in flatrate {
                provider_names.push(format!("{}:stream", p.provider_name));
            }
        }
        if let Some(ref rent) = country.rent {
            for p in rent {
                provider_names.push(format!("{}:rent", p.provider_name));
            }
        }
        if let Some(ref buy) = country.buy {
            for p in buy {
                provider_names.push(format!("{}:buy", p.provider_name));
            }
        }

        item.insert("providers".to_string(), AttributeValue::Ss(provider_names));
        item.insert("updatedAt".to_string(), AttributeValue::S(Utc::now().to_rfc3339()));

        client
            .put_item()
            .table_name(table_name)
            .set_item(Some(item))
            .send()
            .await?;
    }

    Ok(())
}
