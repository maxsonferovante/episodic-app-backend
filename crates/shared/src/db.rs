pub use aws_sdk_dynamodb::Client;
use aws_sdk_dynamodb::types::{AttributeValue, PutRequest, WriteRequest};
use crate::models::user::User;
use crate::models::library::LibraryItem;
use crate::models::progress::{WatchProgress, WatchEvent, WatchStatus, NextEpisode};
use crate::models::series::WatchProviders;
use std::collections::HashMap;

const CACHE_TTL_MONTH: i64 = 30 * 24 * 3600;
const CACHE_TTL_PROVIDERS: i64 = 7 * 24 * 3600;
const CACHE_TTL_SEARCH: i64 = 3600;

/// Lazy catalog reads (series / seasons) are cached for a month while airing.
pub const CACHE_TTL_LAZY_ONGOING: i64 = CACHE_TTL_MONTH;

/// Finished series never change, so their metadata is frozen effectively forever.
pub const CACHE_TTL_FINISHED: i64 = 3650 * 24 * 3600;
/// On-air series are refreshed weekly by the hydrate worker.
pub const CACHE_TTL_HYDRATED_ONGOING: i64 = 7 * 24 * 3600;

/// Cache TTL for a series based on its TMDB status: `ongoing` while it is still
/// airing, the far-future finished TTL once it has ended.
pub fn ttl_for_series(status: &str, ongoing: i64) -> i64 {
    if crate::enums::series_status::is_finished(status) {
        CACHE_TTL_FINISHED
    } else {
        ongoing
    }
}

/// Deterministic canonical id for a series, shared across every response.
pub fn series_id(tmdb_id: i64) -> String {
    format!("ser_{}", tmdb_id)
}

/// Deterministic canonical id for a season.
pub fn season_id(tmdb_id: i64, season_number: i32) -> String {
    format!("sea_{}_{}", tmdb_id, season_number)
}

pub async fn get_client() -> Client {
    if let Ok(endpoint) = std::env::var("AWS_ENDPOINT_URL") {
        let config = aws_config::from_env()
            .endpoint_url(endpoint)
            .region(aws_config::Region::new(
                std::env::var("AWS_REGION").unwrap_or_else(|_| "sa-east-1".to_string())
            ))
            .load()
            .await;
        return Client::new(&config);
    }

    let config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
    Client::new(&config)
}

fn get_str<'a>(item: &'a HashMap<String, AttributeValue>, key: &str) -> &'a str {
    item.get(key).and_then(|v| v.as_s().ok()).map(|s| s.as_str()).unwrap_or("")
}

fn is_cache_expired(item: &HashMap<String, AttributeValue>) -> bool {
    let expires_at = item.get("expiresAt")
        .and_then(|v| v.as_n().ok())
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0);
    
    chrono::Utc::now().timestamp() > expires_at
}

fn get_i32(item: &HashMap<String, AttributeValue>, key: &str) -> i32 {
    item.get(key)
        .and_then(|v| v.as_n().ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

pub async fn get_user_by_email(client: &Client, table: &str, email: &str) -> Result<Option<User>, aws_sdk_dynamodb::Error> {
    let result = client
        .query()
        .table_name(table)
        .index_name("GSI1")
        .key_condition_expression("GSI1PK = :pk")
        .expression_attribute_values(":pk", AttributeValue::S(format!("EMAIL#{}", email)))
        .send()
        .await?;

    let items = result.items();
    if items.is_empty() {
        return Ok(None);
    }

    let item = &items[0];
    let pk = get_str(item, "PK");
    let id = pk.strip_prefix("USR#").unwrap_or(pk).to_string();

    Ok(Some(User {
        id,
        email: get_str(item, "email").to_string(),
        name: get_str(item, "name").to_string(),
        avatar_url: item.get("avatarUrl").and_then(|v| v.as_s().ok()).map(|s| s.to_string()),
        provider: get_str(item, "provider").to_string(),
        created_at: get_str(item, "createdAt").to_string(),
        updated_at: get_str(item, "updatedAt").to_string(),
    }))
}

pub async fn get_user_by_id(client: &Client, table: &str, user_id: &str) -> Result<Option<User>, aws_sdk_dynamodb::Error> {
    let result = client
        .get_item()
        .table_name(table)
        .key("PK", AttributeValue::S(format!("USR#{}", user_id)))
        .key("SK", AttributeValue::S("PROFILE".to_string()))
        .send()
        .await?;

    let item = match result.item() {
        Some(item) => item,
        None => return Ok(None),
    };

    Ok(Some(User {
        id: user_id.to_string(),
        email: get_str(item, "email").to_string(),
        name: get_str(item, "name").to_string(),
        avatar_url: item.get("avatarUrl").and_then(|v| v.as_s().ok()).map(|s| s.to_string()),
        provider: get_str(item, "provider").to_string(),
        created_at: get_str(item, "createdAt").to_string(),
        updated_at: get_str(item, "updatedAt").to_string(),
    }))
}

pub async fn create_user(client: &Client, table: &str, user: &User) -> Result<(), aws_sdk_dynamodb::Error> {
    let mut item = std::collections::HashMap::new();
    item.insert("PK".to_string(), AttributeValue::S(format!("USR#{}", user.id)));
    item.insert("SK".to_string(), AttributeValue::S("PROFILE".to_string()));
    item.insert("GSI1PK".to_string(), AttributeValue::S(format!("EMAIL#{}", user.email)));
    item.insert("GSI1SK".to_string(), AttributeValue::S(user.email.clone()));
    item.insert("email".to_string(), AttributeValue::S(user.email.clone()));
    item.insert("name".to_string(), AttributeValue::S(user.name.clone()));
    item.insert("provider".to_string(), AttributeValue::S(user.provider.clone()));
    item.insert("createdAt".to_string(), AttributeValue::S(user.created_at.clone()));
    item.insert("updatedAt".to_string(), AttributeValue::S(user.updated_at.clone()));

    if let Some(ref avatar_url) = user.avatar_url {
        item.insert("avatarUrl".to_string(), AttributeValue::S(avatar_url.clone()));
    }

    client
        .put_item()
        .table_name(table)
        .set_item(Some(item))
        .send()
        .await?;

    Ok(())
}

pub async fn get_episode_by_id(
    client: &Client,
    table: &str,
    episode_id: &str,
) -> Result<Option<(String, i32, i32)>, aws_sdk_dynamodb::Error> {
    let result = client
        .query()
        .table_name(table)
        .index_name("GSI1")
        .key_condition_expression("GSI1PK = :pk")
        .expression_attribute_values(":pk", AttributeValue::S(episode_id.to_string()))
        .limit(1)
        .send()
        .await?;

    let items = result.items();
    if items.is_empty() {
        return Ok(None);
    }

    let item = &items[0];
    let pk = get_str(item, "PK");
    let series_id = pk.strip_prefix("SER#").unwrap_or(pk).to_string();
    let season_number = get_i32(item, "seasonNumber");
    let episode_number = get_i32(item, "episodeNumber");

    Ok(Some((series_id, season_number, episode_number)))
}

/// Direct read of an episode row (PK = SER#<series>, SK = EP#<ss>#<ee>).
async fn get_episode_item(
    client: &Client,
    table: &str,
    series_id: &str,
    season_number: i32,
    episode_number: i32,
) -> Result<Option<HashMap<String, AttributeValue>>, aws_sdk_dynamodb::Error> {
    let result = client
        .get_item()
        .table_name(table)
        .key("PK", AttributeValue::S(format!("SER#{}", series_id)))
        .key("SK", AttributeValue::S(format!("EP#{:02}#{:02}", season_number, episode_number)))
        .send()
        .await?;
    Ok(result.item().cloned())
}

/// Episode numbers for a season that have already aired (airDate <= today), so
/// bulk "mark season watched" never touches unaired episodes.
pub async fn list_season_episode_numbers(
    client: &Client,
    table: &str,
    series_id: &str,
    season_number: i32,
) -> Result<Vec<i32>, aws_sdk_dynamodb::Error> {
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();

    let result = client
        .query()
        .table_name(table)
        .key_condition_expression("PK = :pk AND begins_with(SK, :sk_prefix)")
        .expression_attribute_values(":pk", AttributeValue::S(format!("SER#{}", series_id)))
        .expression_attribute_values(":sk_prefix", AttributeValue::S(format!("EP#{:02}#", season_number)))
        .send()
        .await?;

    let mut episodes: Vec<i32> = result
        .items()
        .iter()
        .filter(|item| match get_opt_str(item, "airDate") {
            Some(air_date) => air_date <= today.as_str(),
            None => false,
        })
        .map(|item| get_i32(item, "episodeNumber"))
        .filter(|n| *n > 0)
        .collect();
    episodes.sort_unstable();
    Ok(episodes)
}

/// Series name + poster, preferring the synced series meta and falling back to
/// the catalog cache keyed by TMDB id.
/// Series display metadata.
pub struct SeriesMeta {
    pub name: String,
    pub poster_path: Option<String>,
    pub first_air_date: Option<String>,
    pub status: Option<String>,
}

/// Series display metadata, read from the canonical `SER#<series_id>` row.
pub async fn get_series_meta(
    client: &Client,
    table: &str,
    series_id: &str,
) -> Result<Option<SeriesMeta>, aws_sdk_dynamodb::Error> {
    if series_id.is_empty() {
        return Ok(None);
    }

    let meta = client
        .get_item()
        .table_name(table)
        .key("PK", AttributeValue::S(format!("SER#{}", series_id)))
        .key("SK", AttributeValue::S("META".to_string()))
        .send()
        .await?;

    if let Some(item) = meta.item() {
        let name = get_str(item, "name").to_string();
        if !name.is_empty() {
            return Ok(Some(SeriesMeta {
                name,
                poster_path: get_opt_str(item, "posterPath").map(str::to_string),
                first_air_date: get_opt_str(item, "firstAirDate").map(str::to_string),
                status: get_opt_str(item, "status").map(str::to_string),
            }));
        }
    }

    Ok(None)
}

/// Name + poster convenience wrapper.
async fn get_series_ref(
    client: &Client,
    table: &str,
    series_id: &str,
) -> Result<(String, Option<String>), aws_sdk_dynamodb::Error> {
    Ok(match get_series_meta(client, table, series_id).await? {
        Some(meta) => (meta.name, meta.poster_path),
        None => (String::new(), None),
    })
}

pub async fn get_episode_progress(
    client: &Client,
    table: &str,
    user_id: &str,
    series_id: &str,
    season_number: i32,
    episode_number: i32,
) -> Result<Option<WatchProgress>, aws_sdk_dynamodb::Error> {
    let sk = format!("PROG#{}#{:02}#{:02}", series_id, season_number, episode_number);

    let result = client
        .get_item()
        .table_name(table)
        .key("PK", AttributeValue::S(format!("USR#{}", user_id)))
        .key("SK", AttributeValue::S(sk))
        .send()
        .await?;

    let item = match result.item() {
        Some(item) => item,
        None => return Ok(None),
    };

    let status = WatchStatus::from_db(get_str(item, "status"));

    Ok(Some(WatchProgress {
        user_id: user_id.to_string(),
        series_id: series_id.to_string(),
        season_number,
        episode_number,
        status,
        watched_at: item.get("watchedAt").and_then(|v| v.as_s().ok()).map(|s| s.to_string()),
    }))
}

pub async fn mark_episode(
    client: &Client,
    table: &str,
    user_id: &str,
    series_id: &str,
    season_number: i32,
    episode_number: i32,
) -> Result<WatchProgress, aws_sdk_dynamodb::Error> {
    let now = chrono::Utc::now().to_rfc3339();
    let sk = format!("PROG#{}#{:02}#{:02}", series_id, season_number, episode_number);

    let mut item = HashMap::new();
    item.insert("PK".to_string(), AttributeValue::S(format!("USR#{}", user_id)));
    item.insert("SK".to_string(), AttributeValue::S(sk));
    item.insert("seriesId".to_string(), AttributeValue::S(series_id.to_string()));
    item.insert("seasonNumber".to_string(), AttributeValue::N(season_number.to_string()));
    item.insert("episodeNumber".to_string(), AttributeValue::N(episode_number.to_string()));
    item.insert("status".to_string(), AttributeValue::S(WatchStatus::Watched.as_str().to_string()));
    item.insert("watchedAt".to_string(), AttributeValue::S(now.clone()));

    client
        .put_item()
        .table_name(table)
        .set_item(Some(item))
        .send()
        .await?;

    Ok(WatchProgress {
        user_id: user_id.to_string(),
        series_id: series_id.to_string(),
        season_number,
        episode_number,
        status: WatchStatus::Watched,
        watched_at: Some(now),
    })
}

pub async fn unmark_episode(
    client: &Client,
    table: &str,
    user_id: &str,
    series_id: &str,
    season_number: i32,
    episode_number: i32,
) -> Result<WatchProgress, aws_sdk_dynamodb::Error> {
    let sk = format!("PROG#{}#{:02}#{:02}", series_id, season_number, episode_number);

    let mut item = HashMap::new();
    item.insert("PK".to_string(), AttributeValue::S(format!("USR#{}", user_id)));
    item.insert("SK".to_string(), AttributeValue::S(sk));
    item.insert("seriesId".to_string(), AttributeValue::S(series_id.to_string()));
    item.insert("seasonNumber".to_string(), AttributeValue::N(season_number.to_string()));
    item.insert("episodeNumber".to_string(), AttributeValue::N(episode_number.to_string()));
    item.insert("status".to_string(), AttributeValue::S(WatchStatus::Unwatched.as_str().to_string()));

    client
        .put_item()
        .table_name(table)
        .set_item(Some(item))
        .send()
        .await?;

    Ok(WatchProgress {
        user_id: user_id.to_string(),
        series_id: series_id.to_string(),
        season_number,
        episode_number,
        status: WatchStatus::Unwatched,
        watched_at: None,
    })
}

pub async fn create_watch_event(
    client: &Client,
    table: &str,
    user_id: &str,
    episode_id: &str,
    event_type: &str,
) -> Result<WatchEvent, aws_sdk_dynamodb::Error> {
    let now = chrono::Utc::now();
    let timestamp = now.timestamp_millis();
    let sk = format!("EVT#{}#{}", timestamp, episode_id);

    let mut item = HashMap::new();
    item.insert("PK".to_string(), AttributeValue::S(format!("USR#{}", user_id)));
    item.insert("SK".to_string(), AttributeValue::S(sk));
    item.insert("episodeId".to_string(), AttributeValue::S(episode_id.to_string()));
    item.insert("eventType".to_string(), AttributeValue::S(event_type.to_string()));
    item.insert("occurredAt".to_string(), AttributeValue::S(now.to_rfc3339()));

    // Enrich the event so history reads don't need extra lookups.
    if let Ok(Some((series_id, season_number, episode_number))) =
        get_episode_by_id(client, table, episode_id).await
    {
        let mut episode_name = String::new();
        if let Ok(Some(ep)) = get_episode_item(client, table, &series_id, season_number, episode_number).await {
            episode_name = get_str(&ep, "name").to_string();
        }
        let (series_name, poster_path) = get_series_ref(client, table, &series_id)
            .await
            .unwrap_or((String::new(), None));

        item.insert("seriesId".to_string(), AttributeValue::S(series_id));
        item.insert("seasonNumber".to_string(), AttributeValue::N(season_number.to_string()));
        item.insert("episodeNumber".to_string(), AttributeValue::N(episode_number.to_string()));
        if !episode_name.is_empty() {
            item.insert("episodeName".to_string(), AttributeValue::S(episode_name));
        }
        if !series_name.is_empty() {
            item.insert("seriesName".to_string(), AttributeValue::S(series_name));
        }
        if let Some(p) = poster_path {
            item.insert("posterPath".to_string(), AttributeValue::S(p));
        }
    }

    client
        .put_item()
        .table_name(table)
        .set_item(Some(item))
        .send()
        .await?;

    Ok(WatchEvent {
        user_id: user_id.to_string(),
        episode_id: episode_id.to_string(),
        event_type: event_type.to_string(),
        occurred_at: now.to_rfc3339(),
    })
}

pub async fn count_watched_in_series(
    client: &Client,
    table: &str,
    user_id: &str,
    series_id: &str,
) -> Result<i32, aws_sdk_dynamodb::Error> {
    let prefix = format!("PROG#{}#", series_id);

    let result = client
        .query()
        .table_name(table)
        .key_condition_expression("PK = :pk AND begins_with(SK, :sk_prefix)")
        .expression_attribute_values(":pk", AttributeValue::S(format!("USR#{}", user_id)))
        .expression_attribute_values(":sk_prefix", AttributeValue::S(prefix))
        .send()
        .await?;

    let count = result.items()
        .iter()
        .filter(|item| get_str(item, "status") == WatchStatus::Watched.as_str())
        .count() as i32;

    Ok(count)
}

pub async fn count_watched_in_season(
    client: &Client,
    table: &str,
    user_id: &str,
    series_id: &str,
    season_number: i32,
) -> Result<i32, aws_sdk_dynamodb::Error> {
    let prefix = format!("PROG#{}#{:02}#", series_id, season_number);

    let result = client
        .query()
        .table_name(table)
        .key_condition_expression("PK = :pk AND begins_with(SK, :sk_prefix)")
        .expression_attribute_values(":pk", AttributeValue::S(format!("USR#{}", user_id)))
        .expression_attribute_values(":sk_prefix", AttributeValue::S(prefix))
        .send()
        .await?;

    let count = result.items()
        .iter()
        .filter(|item| get_str(item, "status") == WatchStatus::Watched.as_str())
        .count() as i32;

    Ok(count)
}

pub async fn get_season_episode_count(
    client: &Client,
    table: &str,
    series_id: &str,
    season_number: i32,
) -> Result<i32, aws_sdk_dynamodb::Error> {
    let sk = format!("SN#{:02}", season_number);

    let result = client
        .get_item()
        .table_name(table)
        .key("PK", AttributeValue::S(format!("SER#{}", series_id)))
        .key("SK", AttributeValue::S(sk))
        .send()
        .await?;

    let count = result.item()
        .map(|item| get_i32(item, "episodeCount"))
        .unwrap_or(0);

    Ok(count)
}

pub async fn get_series_total_episodes(
    client: &Client,
    table: &str,
    series_id: &str,
) -> Result<i32, aws_sdk_dynamodb::Error> {
    let result = client
        .get_item()
        .table_name(table)
        .key("PK", AttributeValue::S(format!("SER#{}", series_id)))
        .key("SK", AttributeValue::S("META".to_string()))
        .send()
        .await?;

    let count = result.item()
        .map(|item| get_i32(item, "numberOfEpisodes"))
        .unwrap_or(0);

    Ok(count)
}

/// Aired episodes (airDate <= today) for a series: total and per season.
pub async fn get_aired_counts(
    client: &Client,
    table: &str,
    series_id: &str,
) -> Result<(i32, HashMap<i32, i32>), aws_sdk_dynamodb::Error> {
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();

    let result = client
        .query()
        .table_name(table)
        .key_condition_expression("PK = :pk AND begins_with(SK, :sk_prefix)")
        .expression_attribute_values(":pk", AttributeValue::S(format!("SER#{}", series_id)))
        .expression_attribute_values(":sk_prefix", AttributeValue::S("EP#".to_string()))
        .send()
        .await?;

    let mut by_season: HashMap<i32, i32> = HashMap::new();
    let mut total = 0;
    for item in result.items() {
        if let Some(air_date) = get_opt_str(item, "airDate") {
            if air_date <= today.as_str() {
                let season = get_i32(item, "seasonNumber");
                *by_season.entry(season).or_insert(0) += 1;
                total += 1;
            }
        }
    }

    Ok((total, by_season))
}

pub async fn get_next_unwatched_episode(
    client: &Client,
    table: &str,
    user_id: &str,
    series_id: &str,
) -> Result<Option<NextEpisode>, aws_sdk_dynamodb::Error> {
    let result = client
        .query()
        .table_name(table)
        .key_condition_expression("PK = :pk AND begins_with(SK, :sk_prefix)")
        .expression_attribute_values(":pk", AttributeValue::S(format!("SER#{}", series_id)))
        .expression_attribute_values(":sk_prefix", AttributeValue::S("EP#".to_string()))
        .send()
        .await?;

    let mut episodes: Vec<(i32, i32, String)> = result.items()
        .iter()
        .filter_map(|item| {
            let season = get_i32(item, "seasonNumber");
            let episode = get_i32(item, "episodeNumber");
            // Specials (season 0) are eligible: they count towards progress and
            // sort first, so they can be the next unwatched episode.
            if episode > 0 {
                let pk = get_str(item, "PK");
                let sid = pk.strip_prefix("SER#").unwrap_or(pk).to_string();
                Some((season, episode, sid))
            } else {
                None
            }
        })
        .collect();

    episodes.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));

    for (season, episode, sid) in &episodes {
        let progress = get_episode_progress(
            client,
            table,
            user_id,
            sid,
            *season,
            *episode,
        ).await?;

        let is_watched = progress
            .as_ref()
            .map(|p| p.status == WatchStatus::Watched)
            .unwrap_or(false);

        if !is_watched {
            let episode_id = format!("{}#S{:02}#E{:02}", sid, season, episode);
            return Ok(Some(NextEpisode {
                episode_id,
                series_id: sid.clone(),
                season_number: *season,
                episode_number: *episode,
            }));
        }
    }

    Ok(None)
}

pub async fn get_cached_series(client: &Client, table: &str, tmdb_id: i64) -> Result<Option<crate::models::series::Series>, aws_sdk_dynamodb::Error> {
    let result = client
        .get_item()
        .table_name(table)
        .key("PK", AttributeValue::S(format!("SER#{}", series_id(tmdb_id))))
        .key("SK", AttributeValue::S("META".to_string()))
        .send()
        .await?;

    let item = match result.item() {
        Some(item) => item,
        None => return Ok(None),
    };

    if is_cache_expired(&item) {
        return Ok(None);
    }

    Ok(Some(crate::models::series::Series {
        id: get_str(item, "id").to_string(),
        tmdb_id,
        imdb_id: item.get("imdbId").and_then(|v| v.as_s().ok()).map(|s| s.to_string()),
        name: get_str(item, "name").to_string(),
        original_name: get_str(item, "originalName").to_string(),
        overview: get_str(item, "overview").to_string(),
        poster_path: item.get("posterPath").and_then(|v| v.as_s().ok()).map(|s| s.to_string()),
        backdrop_path: item.get("backdropPath").and_then(|v| v.as_s().ok()).map(|s| s.to_string()),
        first_air_date: item.get("firstAirDate").and_then(|v| v.as_s().ok()).map(|s| s.to_string()),
        last_air_date: item.get("lastAirDate").and_then(|v| v.as_s().ok()).map(|s| s.to_string()),
        status: get_str(item, "status").to_string(),
        number_of_seasons: get_i32(item, "numberOfSeasons"),
        number_of_episodes: get_i32(item, "numberOfEpisodes"),
        created_at: get_str(item, "createdAt").to_string(),
        updated_at: get_str(item, "updatedAt").to_string(),
    }))
}

pub async fn cache_series(
    client: &Client,
    table: &str,
    series: &crate::models::series::Series,
    expires_at: i64,
) -> Result<(), aws_sdk_dynamodb::Error> {
    let mut item = std::collections::HashMap::new();
    item.insert("PK".to_string(), AttributeValue::S(format!("SER#{}", series_id(series.tmdb_id))));
    item.insert("SK".to_string(), AttributeValue::S("META".to_string()));
    item.insert("id".to_string(), AttributeValue::S(series.id.clone()));
    item.insert("name".to_string(), AttributeValue::S(series.name.clone()));
    item.insert("originalName".to_string(), AttributeValue::S(series.original_name.clone()));
    item.insert("overview".to_string(), AttributeValue::S(series.overview.clone()));
    item.insert("status".to_string(), AttributeValue::S(series.status.clone()));
    item.insert("numberOfSeasons".to_string(), AttributeValue::N(series.number_of_seasons.to_string()));
    item.insert("numberOfEpisodes".to_string(), AttributeValue::N(series.number_of_episodes.to_string()));
    item.insert("createdAt".to_string(), AttributeValue::S(series.created_at.clone()));
    item.insert("updatedAt".to_string(), AttributeValue::S(series.updated_at.clone()));
    item.insert("expiresAt".to_string(), AttributeValue::N(expires_at.to_string()));

    if let Some(ref v) = series.imdb_id { item.insert("imdbId".to_string(), AttributeValue::S(v.clone())); }
    if let Some(ref v) = series.poster_path { item.insert("posterPath".to_string(), AttributeValue::S(v.clone())); }
    if let Some(ref v) = series.backdrop_path { item.insert("backdropPath".to_string(), AttributeValue::S(v.clone())); }
    if let Some(ref v) = series.first_air_date { item.insert("firstAirDate".to_string(), AttributeValue::S(v.clone())); }
    if let Some(ref v) = series.last_air_date { item.insert("lastAirDate".to_string(), AttributeValue::S(v.clone())); }

    client
        .put_item()
        .table_name(table)
        .set_item(Some(item))
        .send()
        .await?;

    Ok(())
}

/// Hydration state tracked on the canonical series meta row.
pub struct HydrationState {
    pub hydration_status: Option<String>,
    pub series_status: Option<String>,
    pub next_air_date: Option<String>,
    pub expires_at: i64,
}

/// Read the hydrate bookkeeping for a series, if its meta row exists.
pub async fn get_hydration_state(
    client: &Client,
    table: &str,
    tmdb_id: i64,
) -> Result<Option<HydrationState>, aws_sdk_dynamodb::Error> {
    let result = client
        .get_item()
        .table_name(table)
        .key("PK", AttributeValue::S(format!("SER#{}", series_id(tmdb_id))))
        .key("SK", AttributeValue::S("META".to_string()))
        .send()
        .await?;

    let item = match result.item() {
        Some(item) => item,
        None => return Ok(None),
    };

    Ok(Some(HydrationState {
        hydration_status: get_opt_str(item, "hydrationStatus").map(str::to_string),
        series_status: get_opt_str(item, "status").map(str::to_string),
        next_air_date: get_opt_str(item, "nextAirDate").map(str::to_string),
        expires_at: item
            .get("expiresAt")
            .and_then(|v| v.as_n().ok())
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0),
    }))
}

/// Acquire the in-flight hydration lock. Returns `false` when another worker
/// holds it (or held it more recently than `stale_before`), so concurrent
/// deliveries of the same series don't stampede TMDB.
pub async fn try_acquire_hydration_lock(
    client: &Client,
    table: &str,
    tmdb_id: i64,
    stale_before: &str,
) -> Result<bool, aws_sdk_dynamodb::Error> {
    let result = client
        .update_item()
        .table_name(table)
        .key("PK", AttributeValue::S(format!("SER#{}", series_id(tmdb_id))))
        .key("SK", AttributeValue::S("META".to_string()))
        .update_expression("SET hydrationStatus = :h, hydratedAt = :now")
        .condition_expression(
            "attribute_not_exists(hydrationStatus) OR hydrationStatus <> :h \
             OR attribute_not_exists(hydratedAt) OR hydratedAt < :stale",
        )
        .expression_attribute_values(
            ":h",
            AttributeValue::S(crate::enums::hydration_status::HYDRATING.to_string()),
        )
        .expression_attribute_values(":now", AttributeValue::S(chrono::Utc::now().to_rfc3339()))
        .expression_attribute_values(":stale", AttributeValue::S(stale_before.to_string()))
        .send()
        .await;

    match result {
        Ok(_) => Ok(true),
        Err(e) => {
            let is_conditional = e
                .as_service_error()
                .map(|service_error| service_error.is_conditional_check_failed_exception())
                .unwrap_or(false);
            if is_conditional {
                Ok(false)
            } else {
                Err(e.into())
            }
        }
    }
}

/// Release or reset the hydration lock (used when a hydrate run fails).
pub async fn set_hydration_status(
    client: &Client,
    table: &str,
    tmdb_id: i64,
    status: &str,
) -> Result<(), aws_sdk_dynamodb::Error> {
    client
        .update_item()
        .table_name(table)
        .key("PK", AttributeValue::S(format!("SER#{}", series_id(tmdb_id))))
        .key("SK", AttributeValue::S("META".to_string()))
        .update_expression("SET hydrationStatus = :h, hydratedAt = :now")
        .expression_attribute_values(":h", AttributeValue::S(status.to_string()))
        .expression_attribute_values(":now", AttributeValue::S(chrono::Utc::now().to_rfc3339()))
        .send()
        .await?;
    Ok(())
}

/// Stamp a hydrated series as complete, recording when it was fetched, when the
/// next episode airs (so the scheduler knows when to refresh), and its TTL.
pub async fn mark_hydration_complete(
    client: &Client,
    table: &str,
    tmdb_id: i64,
    hydrated_at: &str,
    next_air_date: Option<&str>,
    expires_at: i64,
) -> Result<(), aws_sdk_dynamodb::Error> {
    let mut builder = client
        .update_item()
        .table_name(table)
        .key("PK", AttributeValue::S(format!("SER#{}", series_id(tmdb_id))))
        .key("SK", AttributeValue::S("META".to_string()))
        .update_expression(
            "SET hydrationStatus = :h, hydratedAt = :now, expiresAt = :exp",
        )
        .expression_attribute_values(
            ":h",
            AttributeValue::S(crate::enums::hydration_status::COMPLETE.to_string()),
        )
        .expression_attribute_values(":now", AttributeValue::S(hydrated_at.to_string()))
        .expression_attribute_values(":exp", AttributeValue::N(expires_at.to_string()));

    if let Some(air_date) = next_air_date {
        builder = builder
            .update_expression(
                "SET hydrationStatus = :h, hydratedAt = :now, expiresAt = :exp, nextAirDate = :next",
            )
            .expression_attribute_values(":next", AttributeValue::S(air_date.to_string()));
    }

    builder.send().await?;
    Ok(())
}

pub async fn cache_providers(
    client: &Client,
    table: &str,
    tmdb_id: i64,
    providers: &WatchProviders,
) -> Result<(), aws_sdk_dynamodb::Error> {
    let expires_at = chrono::Utc::now().timestamp() + CACHE_TTL_PROVIDERS;
    let mut item = HashMap::new();
    item.insert("PK".to_string(), AttributeValue::S(format!("SER#{}", series_id(tmdb_id))));
    item.insert("SK".to_string(), AttributeValue::S("PROVIDERS".to_string()));
    item.insert("expiresAt".to_string(), AttributeValue::N(expires_at.to_string()));
    item.insert("updatedAt".to_string(), AttributeValue::S(chrono::Utc::now().to_rfc3339()));

    if let Some(ref flatrate) = providers.flatrate {
        let names: Vec<String> = flatrate.iter().map(|p| p.provider_name.clone()).collect();
        item.insert("flatrate".to_string(), AttributeValue::Ss(names));
    }
    if let Some(ref rent) = providers.rent {
        let names: Vec<String> = rent.iter().map(|p| p.provider_name.clone()).collect();
        item.insert("rent".to_string(), AttributeValue::Ss(names));
    }
    if let Some(ref buy) = providers.buy {
        let names: Vec<String> = buy.iter().map(|p| p.provider_name.clone()).collect();
        item.insert("buy".to_string(), AttributeValue::Ss(names));
    }
    if let Some(ref free) = providers.free {
        let names: Vec<String> = free.iter().map(|p| p.provider_name.clone()).collect();
        item.insert("free".to_string(), AttributeValue::Ss(names));
    }

    client
        .put_item()
        .table_name(table)
        .set_item(Some(item))
        .send()
        .await?;

    Ok(())
}

pub async fn get_cached_providers(
    client: &Client,
    table: &str,
    tmdb_id: i64,
) -> Result<Option<WatchProviders>, aws_sdk_dynamodb::Error> {
    let result = client
        .get_item()
        .table_name(table)
        .key("PK", AttributeValue::S(format!("SER#{}", series_id(tmdb_id))))
        .key("SK", AttributeValue::S("PROVIDERS".to_string()))
        .send()
        .await?;

    let item = match result.item() {
        Some(item) => item,
        None => return Ok(None),
    };

    if is_cache_expired(&item) {
        return Ok(None);
    }

    let flatrate = item.get("flatrate").and_then(|v| v.as_ss().ok()).map(|names| {
        names.iter().map(|name| crate::models::series::Provider {
            provider_id: 0,
            provider_name: name.clone(),
            logo_path: None,
        }).collect()
    });

    let rent = item.get("rent").and_then(|v| v.as_ss().ok()).map(|names| {
        names.iter().map(|name| crate::models::series::Provider {
            provider_id: 0,
            provider_name: name.clone(),
            logo_path: None,
        }).collect()
    });

    let buy = item.get("buy").and_then(|v| v.as_ss().ok()).map(|names| {
        names.iter().map(|name| crate::models::series::Provider {
            provider_id: 0,
            provider_name: name.clone(),
            logo_path: None,
        }).collect()
    });

    let free = item.get("free").and_then(|v| v.as_ss().ok()).map(|names| {
        names.iter().map(|name| crate::models::series::Provider {
            provider_id: 0,
            provider_name: name.clone(),
            logo_path: None,
        }).collect()
    });

    Ok(Some(WatchProviders {
        flatrate,
        rent,
        buy,
        free,
    }))
}

pub async fn cache_search_results(
    client: &Client,
    table: &str,
    query: &str,
    page: i32,
    results: &[crate::models::series::CatalogSeries],
    total_pages: i32,
) -> Result<(), aws_sdk_dynamodb::Error> {
    let expires_at = chrono::Utc::now().timestamp() + CACHE_TTL_SEARCH;
    let cache_key = format!("SEARCH#{}#{}", query.to_lowercase(), page);
    
    let serialized = serde_json::to_string(&serde_json::json!({
        "results": results,
        "total_pages": total_pages,
    })).unwrap_or_default();

    let mut item = HashMap::new();
    item.insert("PK".to_string(), AttributeValue::S(cache_key));
    item.insert("SK".to_string(), AttributeValue::S("RESULTS".to_string()));
    item.insert("expiresAt".to_string(), AttributeValue::N(expires_at.to_string()));
    item.insert("data".to_string(), AttributeValue::S(serialized));

    client
        .put_item()
        .table_name(table)
        .set_item(Some(item))
        .send()
        .await?;

    Ok(())
}

pub async fn get_cached_search(
    client: &Client,
    table: &str,
    query: &str,
    page: i32,
) -> Result<Option<(Vec<crate::models::series::CatalogSeries>, i32)>, aws_sdk_dynamodb::Error> {
    let cache_key = format!("SEARCH#{}#{}", query.to_lowercase(), page);
    
    let result = client
        .get_item()
        .table_name(table)
        .key("PK", AttributeValue::S(cache_key))
        .key("SK", AttributeValue::S("RESULTS".to_string()))
        .send()
        .await?;

    let item = match result.item() {
        Some(item) => item,
        None => return Ok(None),
    };

    if is_cache_expired(&item) {
        return Ok(None);
    }

    let data = match item.get("data").and_then(|v| v.as_s().ok()) {
        Some(d) => d,
        None => return Ok(None),
    };

    let parsed: serde_json::Value = match serde_json::from_str(data) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };

    let results: Vec<crate::models::series::CatalogSeries> = match serde_json::from_value(
        parsed.get("results").cloned().unwrap_or(serde_json::Value::Array(vec![]))
    ) {
        Ok(r) => r,
        Err(_) => return Ok(None),
    };

    let total_pages = parsed.get("total_pages").and_then(|v| v.as_i64()).unwrap_or(1) as i32;

    Ok(Some((results, total_pages)))
}

pub async fn get_cached_seasons(client: &Client, table: &str, tmdb_id: i64) -> Result<Vec<crate::models::season::Season>, aws_sdk_dynamodb::Error> {
    let result = client
        .query()
        .table_name(table)
        .key_condition_expression("PK = :pk AND begins_with(SK, :sk_prefix)")
        .expression_attribute_values(":pk", AttributeValue::S(format!("SER#{}", series_id(tmdb_id))))
        .expression_attribute_values(":sk_prefix", AttributeValue::S("SN#".to_string()))
        .send()
        .await?;

    let seasons = result.items().iter().filter_map(|item| {
        let season_number = item.get("seasonNumber").and_then(|v| v.as_n().ok())?.parse::<i32>().ok()?;
        Some(crate::models::season::Season {
            id: get_str(item, "id").to_string(),
            series_id: get_str(item, "seriesId").to_string(),
            tmdb_id: item.get("tmdbId").and_then(|v| v.as_n().ok()).and_then(|n| n.parse::<i64>().ok()),
            season_number,
            name: get_str(item, "name").to_string(),
            overview: item.get("overview").and_then(|v| v.as_s().ok()).map(|s| s.to_string()),
            poster_path: item.get("posterPath").and_then(|v| v.as_s().ok()).map(|s| s.to_string()),
            air_date: item.get("airDate").and_then(|v| v.as_s().ok()).map(|s| s.to_string()),
            episode_count: get_i32(item, "episodeCount"),
        })
    }).collect();

    Ok(seasons)
}

pub async fn cache_seasons(
    client: &Client,
    table: &str,
    tmdb_id: i64,
    seasons: &[crate::models::season::Season],
    expires_at: i64,
) -> Result<(), aws_sdk_dynamodb::Error> {
    for season in seasons {
        let mut item = HashMap::new();
        item.insert("PK".to_string(), AttributeValue::S(format!("SER#{}", series_id(tmdb_id))));
        item.insert("SK".to_string(), AttributeValue::S(format!("SN#{:02}", season.season_number)));
        item.insert("id".to_string(), AttributeValue::S(season.id.clone()));
        item.insert("seriesId".to_string(), AttributeValue::S(season.series_id.clone()));
        if let Some(tid) = season.tmdb_id {
            item.insert("tmdbId".to_string(), AttributeValue::N(tid.to_string()));
        }
        item.insert("seasonNumber".to_string(), AttributeValue::N(season.season_number.to_string()));
        item.insert("name".to_string(), AttributeValue::S(season.name.clone()));
        if let Some(ref overview) = season.overview {
            item.insert("overview".to_string(), AttributeValue::S(overview.clone()));
        }
        if let Some(ref poster_path) = season.poster_path {
            item.insert("posterPath".to_string(), AttributeValue::S(poster_path.clone()));
        }
        if let Some(ref air_date) = season.air_date {
            item.insert("airDate".to_string(), AttributeValue::S(air_date.clone()));
        }
        item.insert("episodeCount".to_string(), AttributeValue::N(season.episode_count.to_string()));
        item.insert("expiresAt".to_string(), AttributeValue::N(expires_at.to_string()));

        client
            .put_item()
            .table_name(table)
            .set_item(Some(item))
            .send()
            .await?;
    }

    Ok(())
}

pub async fn get_cached_episodes(client: &Client, table: &str, series_tmdb_id: i64, season_number: i32) -> Result<Vec<crate::models::episode::Episode>, aws_sdk_dynamodb::Error> {
    let series_id = format!("ser_{}", series_tmdb_id);

    let result = client
        .query()
        .table_name(table)
        .key_condition_expression("PK = :pk AND begins_with(SK, :sk_prefix)")
        .expression_attribute_values(":pk", AttributeValue::S(format!("SER#{}", series_id)))
        .expression_attribute_values(":sk_prefix", AttributeValue::S(format!("EP#{:02}#", season_number)))
        .send()
        .await?;

    let episodes = result.items().iter().filter_map(|item| {
        let episode_number = item.get("episodeNumber").and_then(|v| v.as_n().ok())?.parse::<i32>().ok()?;
        let tmdb_id = item.get("tmdbId").and_then(|v| v.as_n().ok()).and_then(|n| n.parse::<i64>().ok());
        let stored_id = get_str(item, "id");
        // Episodes written by older sync runs have no `id`; derive the stable
        // one from the TMDB episode id so ids never drift.
        let id = if stored_id.is_empty() {
            tmdb_id.map(|t| format!("epi_{}", t)).unwrap_or_default()
        } else {
            stored_id.to_string()
        };
        Some(crate::models::episode::Episode {
            id,
            series_id: get_str(item, "seriesId").to_string(),
            season_id: get_str(item, "seasonId").to_string(),
            tmdb_id,
            episode_number,
            name: get_str(item, "name").to_string(),
            overview: item.get("overview").and_then(|v| v.as_s().ok()).map(|s| s.to_string()),
            still_path: item.get("stillPath").and_then(|v| v.as_s().ok()).map(|s| s.to_string()),
            air_date: item.get("airDate").and_then(|v| v.as_s().ok()).map(|s| s.to_string()),
            runtime: item.get("runtime").and_then(|v| v.as_n().ok()).and_then(|n| n.parse::<i32>().ok()),
            vote_average: item.get("voteAverage").and_then(|v| v.as_n().ok()).and_then(|n| n.parse::<f64>().ok()),
            status: None,
        })
    }).collect();

    Ok(episodes)
}

/// Persist episodes under the canonical `SER#<id>` / `EP#<ss>#<ee>` schema and
/// index each one by its stable id on GSI1 so the progress lambda can resolve
/// an episode id back to (series, season, episode).
pub async fn upsert_episodes(
    client: &Client,
    table: &str,
    series_id: &str,
    season_id: &str,
    season_number: i32,
    episodes: &[crate::models::episode::Episode],
) -> Result<(), aws_sdk_dynamodb::Error> {
    let mut requests = Vec::new();

    for ep in episodes {
        let mut item = HashMap::new();
        item.insert("PK".to_string(), AttributeValue::S(format!("SER#{}", series_id)));
        item.insert("SK".to_string(), AttributeValue::S(format!("EP#{:02}#{:02}", season_number, ep.episode_number)));
        item.insert("GSI1PK".to_string(), AttributeValue::S(ep.id.clone()));
        item.insert("GSI1SK".to_string(), AttributeValue::S(format!("EPI#{}", ep.id)));
        item.insert("id".to_string(), AttributeValue::S(ep.id.clone()));
        item.insert("seriesId".to_string(), AttributeValue::S(series_id.to_string()));
        item.insert("seasonId".to_string(), AttributeValue::S(season_id.to_string()));
        item.insert("seasonNumber".to_string(), AttributeValue::N(season_number.to_string()));
        item.insert("episodeNumber".to_string(), AttributeValue::N(ep.episode_number.to_string()));
        item.insert("name".to_string(), AttributeValue::S(ep.name.clone()));

        if let Some(tid) = ep.tmdb_id {
            item.insert("tmdbId".to_string(), AttributeValue::N(tid.to_string()));
        }
        if let Some(ref v) = ep.overview { item.insert("overview".to_string(), AttributeValue::S(v.clone())); }
        if let Some(ref v) = ep.still_path { item.insert("stillPath".to_string(), AttributeValue::S(v.clone())); }
        if let Some(ref v) = ep.air_date { item.insert("airDate".to_string(), AttributeValue::S(v.clone())); }
        if let Some(v) = ep.runtime { item.insert("runtime".to_string(), AttributeValue::N(v.to_string())); }
        if let Some(v) = ep.vote_average { item.insert("voteAverage".to_string(), AttributeValue::N(v.to_string())); }

        requests.push(WriteRequest::builder()
            .put_request(PutRequest::builder().set_item(Some(item)).build()?)
            .build());
    }

    for chunk in requests.chunks(25) {
        let mut request_items = HashMap::new();
        request_items.insert(table.to_string(), chunk.to_vec());
        client
            .batch_write_item()
            .set_request_items(Some(request_items))
            .send()
            .await?;
    }

    Ok(())
}

/// Set the episode count for a season without clobbering the richer season
/// metadata written by the sync job.
pub async fn upsert_season_meta(
    client: &Client,
    table: &str,
    series_id: &str,
    season_number: i32,
    episode_count: i32,
) -> Result<(), aws_sdk_dynamodb::Error> {
    client
        .update_item()
        .table_name(table)
        .key("PK", AttributeValue::S(format!("SER#{}", series_id)))
        .key("SK", AttributeValue::S(format!("SN#{:02}", season_number)))
        .update_expression("SET seasonNumber = :sn, episodeCount = :ec")
        .expression_attribute_values(":sn", AttributeValue::N(season_number.to_string()))
        .expression_attribute_values(":ec", AttributeValue::N(episode_count.to_string()))
        .send()
        .await?;

    Ok(())
}

/// All (season, episode) pairs the user has marked as watched for a series.
pub async fn get_watched_set(
    client: &Client,
    table: &str,
    user_id: &str,
    series_id: &str,
) -> Result<std::collections::HashSet<(i32, i32)>, aws_sdk_dynamodb::Error> {
    let prefix = format!("PROG#{}#", series_id);

    let result = client
        .query()
        .table_name(table)
        .key_condition_expression("PK = :pk AND begins_with(SK, :sk_prefix)")
        .expression_attribute_values(":pk", AttributeValue::S(format!("USR#{}", user_id)))
        .expression_attribute_values(":sk_prefix", AttributeValue::S(prefix))
        .send()
        .await?;

    let mut watched = std::collections::HashSet::new();
    for item in result.items() {
        if get_str(item, "status") != WatchStatus::Watched.as_str() {
            continue;
        }
        watched.insert((get_i32(item, "seasonNumber"), get_i32(item, "episodeNumber")));
    }

    Ok(watched)
}

use crate::models::dashboard::*;

pub async fn get_library_item(
    client: &Client,
    table: &str,
    user_id: &str,
    series_id: &str,
) -> Result<Option<LibraryItem>, aws_sdk_dynamodb::Error> {
    let result = client
        .get_item()
        .table_name(table)
        .key("PK", AttributeValue::S(format!("USR#{}", user_id)))
        .key("SK", AttributeValue::S(format!("LIB#{}", series_id)))
        .send()
        .await?;

    let item = match result.item() {
        Some(item) => item,
        None => return Ok(None),
    };

    Ok(Some(LibraryItem {
        id: get_str(item, "id").to_string(),
        user_id: user_id.to_string(),
        series_id: get_str(item, "seriesId").to_string(),
        added_at: get_str(item, "addedAt").to_string(),
        name: get_opt_str(item, "name").map(str::to_string),
        poster_path: get_opt_str(item, "posterPath").map(str::to_string),
        first_air_date: get_opt_str(item, "firstAirDate").map(str::to_string),
    }))
}

pub async fn add_to_library(
    client: &Client,
    table: &str,
    item: &LibraryItem,
) -> Result<(), aws_sdk_dynamodb::Error> {
    let mut db_item = HashMap::new();
    db_item.insert("PK".to_string(), AttributeValue::S(format!("USR#{}", item.user_id)));
    db_item.insert("SK".to_string(), AttributeValue::S(format!("LIB#{}", item.series_id)));
    db_item.insert("id".to_string(), AttributeValue::S(item.id.clone()));
    db_item.insert("seriesId".to_string(), AttributeValue::S(item.series_id.clone()));
    db_item.insert("addedAt".to_string(), AttributeValue::S(item.added_at.clone()));
    db_item.insert("GSI1PK".to_string(), AttributeValue::S(format!("SERIES#{}", item.series_id)));

    // Snapshot of the series metadata at add time.
    if let Some(ref name) = item.name {
        db_item.insert("name".to_string(), AttributeValue::S(name.clone()));
    }
    if let Some(ref poster) = item.poster_path {
        db_item.insert("posterPath".to_string(), AttributeValue::S(poster.clone()));
    }
    if let Some(ref first_air_date) = item.first_air_date {
        db_item.insert("firstAirDate".to_string(), AttributeValue::S(first_air_date.clone()));
    }

    client
        .put_item()
        .table_name(table)
        .set_item(Some(db_item))
        .send()
        .await?;

    Ok(())
}

pub async fn remove_from_library(
    client: &Client,
    table: &str,
    user_id: &str,
    series_id: &str,
) -> Result<(), aws_sdk_dynamodb::Error> {
    client
        .delete_item()
        .table_name(table)
        .key("PK", AttributeValue::S(format!("USR#{}", user_id)))
        .key("SK", AttributeValue::S(format!("LIB#{}", series_id)))
        .send()
        .await?;

    Ok(())
}

pub async fn list_library(
    client: &Client,
    table: &str,
    user_id: &str,
) -> Result<Vec<LibraryItem>, aws_sdk_dynamodb::Error> {
    let result = client
        .query()
        .table_name(table)
        .key_condition_expression("PK = :pk AND begins_with(SK, :sk_prefix)")
        .expression_attribute_values(":pk", AttributeValue::S(format!("USR#{}", user_id)))
        .expression_attribute_values(":sk_prefix", AttributeValue::S("LIB#".to_string()))
        .send()
        .await?;

    let items = result.items().iter().map(|item| {
        LibraryItem {
            id: get_str(item, "id").to_string(),
            user_id: user_id.to_string(),
            series_id: get_str(item, "seriesId").to_string(),
            added_at: get_str(item, "addedAt").to_string(),
            name: get_opt_str(item, "name").map(str::to_string),
            poster_path: get_opt_str(item, "posterPath").map(str::to_string),
            first_air_date: get_opt_str(item, "firstAirDate").map(str::to_string),
        }
    }).collect();

    Ok(items)
}

fn get_num(item: &HashMap<String, AttributeValue>, key: &str) -> f64 {
    item.get(key)
        .and_then(|v| v.as_n().ok())
        .and_then(|n| n.parse::<f64>().ok())
        .unwrap_or(0.0)
}

fn get_opt_str<'a>(item: &'a HashMap<String, AttributeValue>, key: &str) -> Option<&'a str> {
    item.get(key).and_then(|v| v.as_s().ok()).map(|s| s.as_str())
}

pub async fn get_continue_watching(client: &Client, table: &str, user_id: &str) -> Result<Vec<ContinueWatchingItem>, aws_sdk_dynamodb::Error> {
    let result = client
        .query()
        .table_name(table)
        .key_condition_expression("PK = :pk AND begins_with(SK, :sk_prefix)")
        .expression_attribute_values(":pk", AttributeValue::S(format!("USR#{}", user_id)))
        .expression_attribute_values(":sk_prefix", AttributeValue::S("LIB#".to_string()))
        .filter_expression("#status = :in_progress")
        .expression_attribute_names("#status", "status")
        .expression_attribute_values(":in_progress", AttributeValue::S(crate::enums::library_status::IN_PROGRESS.to_string()))
        .limit(10)
        .send()
        .await?;

    let items = result.items();
    let mut continue_watching = Vec::with_capacity(items.len());

    for item in items {
        let series_id = get_str(item, "seriesId").to_string();
        let mut series_name = get_str(item, "seriesName").to_string();
        let mut poster_path = get_opt_str(item, "posterPath").map(|s| s.to_string());
        if series_name.is_empty() || poster_path.is_none() {
            let (name, poster) = get_series_ref(client, table, &series_id).await?;
            if series_name.is_empty() {
                series_name = name;
            }
            if poster_path.is_none() {
                poster_path = poster;
            }
        }
        let percentage = get_num(item, "percentage");

        let next_episode_id = get_str(item, "nextEpisodeId").to_string();
        let next_season = get_i32(item, "nextSeasonNumber");
        let next_episode = get_i32(item, "nextEpisodeNumber");
        let next_episode_name = get_str(item, "nextEpisodeName").to_string();

        if next_episode_id.is_empty() {
            continue;
        }

        continue_watching.push(ContinueWatchingItem {
            series: SeriesRef {
                id: series_id,
                name: series_name,
                poster_path,
            },
            next_episode: EpisodeRef {
                id: next_episode_id,
                season_number: next_season,
                episode_number: next_episode,
                name: next_episode_name,
            },
            progress: ProgressInfo { percentage },
        });
    }

    Ok(continue_watching)
}

pub async fn get_upcoming(client: &Client, table: &str, user_id: &str) -> Result<Vec<UpcomingItem>, aws_sdk_dynamodb::Error> {
    let lib_result = client
        .query()
        .table_name(table)
        .key_condition_expression("PK = :pk AND begins_with(SK, :sk_prefix)")
        .expression_attribute_values(":pk", AttributeValue::S(format!("USR#{}", user_id)))
        .expression_attribute_values(":sk_prefix", AttributeValue::S("LIB#".to_string()))
        .send()
        .await?;

    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();

    let mut upcoming = Vec::new();

    for lib_item in lib_result.items() {
        let series_id = get_str(lib_item, "seriesId").to_string();
        let mut series_name = get_str(lib_item, "seriesName").to_string();
        let mut poster_path = get_opt_str(lib_item, "posterPath").map(|s| s.to_string());
        if series_name.is_empty() || poster_path.is_none() {
            let (name, poster) = get_series_ref(client, table, &series_id).await?;
            if series_name.is_empty() {
                series_name = name;
            }
            if poster_path.is_none() {
                poster_path = poster;
            }
        }

        let ep_result = client
            .query()
            .table_name(table)
            .key_condition_expression("PK = :pk AND begins_with(SK, :sk_prefix)")
            .expression_attribute_values(":pk", AttributeValue::S(format!("SER#{}", series_id)))
            .expression_attribute_values(":sk_prefix", AttributeValue::S("EP#".to_string()))
            .send()
            .await?;

        // Only the next (earliest) upcoming episode per series.
        let mut next: Option<(String, i32, i32, String, String)> = None;
        for ep_item in ep_result.items() {
            let air_date = match get_opt_str(ep_item, "airDate") {
                Some(d) => d.to_string(),
                None => continue,
            };
            if air_date.as_str() < today.as_str() {
                continue;
            }
            let is_earlier = match &next {
                None => true,
                Some((current, ..)) => air_date.as_str() < current.as_str(),
            };
            if is_earlier {
                next = Some((
                    air_date,
                    get_i32(ep_item, "seasonNumber"),
                    get_i32(ep_item, "episodeNumber"),
                    get_str(ep_item, "id").to_string(),
                    get_str(ep_item, "name").to_string(),
                ));
            }
        }

        if let Some((air_date, season_number, episode_number, episode_id, episode_name)) = next {
            upcoming.push(UpcomingItem {
                series: SeriesRef {
                    id: series_id,
                    name: series_name,
                    poster_path,
                },
                episode: EpisodeRef {
                    id: episode_id,
                    season_number,
                    episode_number,
                    name: episode_name,
                },
                air_date,
            });
        }
    }

    upcoming.sort_by(|a, b| a.air_date.cmp(&b.air_date));
    Ok(upcoming)
}

/// Resolved series/episode metadata for a watch event.
struct WatchEventMeta {
    episode_id: String,
    series_id: String,
    series_name: String,
    poster_path: Option<String>,
    episode_name: String,
    season_number: i32,
    episode_number: i32,
}

/// Resolve a watch event's metadata, filling any gaps from the episode/series
/// tables (older events were written without names or coordinates).
async fn resolve_watch_event(
    client: &Client,
    table: &str,
    item: &HashMap<String, AttributeValue>,
) -> Result<WatchEventMeta, aws_sdk_dynamodb::Error> {
    let episode_id = get_str(item, "episodeId").to_string();
    let mut series_id = get_str(item, "seriesId").to_string();
    let mut series_name = get_str(item, "seriesName").to_string();
    let mut episode_name = get_str(item, "episodeName").to_string();
    let mut poster_path = get_opt_str(item, "posterPath").map(str::to_string);
    let mut season_number = get_i32(item, "seasonNumber");
    let mut episode_number = get_i32(item, "episodeNumber");

    if series_id.is_empty() || season_number == 0 || episode_number == 0 {
        if let Some((sid, sn, en)) = get_episode_by_id(client, table, &episode_id).await? {
            series_id = sid;
            season_number = sn;
            episode_number = en;
        }
    }

    if episode_name.is_empty() && !series_id.is_empty() && season_number > 0 && episode_number > 0 {
        if let Some(ep) = get_episode_item(client, table, &series_id, season_number, episode_number).await? {
            episode_name = get_str(&ep, "name").to_string();
        }
    }

    if series_name.is_empty() || poster_path.is_none() {
        let (name, poster) = get_series_ref(client, table, &series_id).await?;
        if series_name.is_empty() {
            series_name = name;
        }
        if poster_path.is_none() {
            poster_path = poster;
        }
    }

    Ok(WatchEventMeta {
        episode_id,
        series_id,
        series_name,
        poster_path,
        episode_name,
        season_number,
        episode_number,
    })
}

pub async fn get_recent_history(client: &Client, table: &str, user_id: &str) -> Result<Vec<HistoryItem>, aws_sdk_dynamodb::Error> {
    let result = client
        .query()
        .table_name(table)
        .key_condition_expression("PK = :pk AND begins_with(SK, :sk_prefix)")
        .expression_attribute_values(":pk", AttributeValue::S(format!("USR#{}", user_id)))
        .expression_attribute_values(":sk_prefix", AttributeValue::S("EVT#".to_string()))
        .scan_index_forward(false)
        .limit(10)
        .send()
        .await?;

    let items = result.items();
    let mut history = Vec::with_capacity(items.len());

    for item in items {
        let watched_at = get_str(item, "occurredAt").to_string();
        let meta = resolve_watch_event(client, table, item).await?;

        history.push(HistoryItem {
            episode: EpisodeRef {
                id: meta.episode_id,
                season_number: meta.season_number,
                episode_number: meta.episode_number,
                name: meta.episode_name,
            },
            series: SeriesRef {
                id: meta.series_id,
                name: meta.series_name,
                poster_path: meta.poster_path,
            },
            watched_at,
        });
    }

    Ok(history)
}

pub async fn get_history_page(
    client: &Client,
    table: &str,
    user_id: &str,
    cursor: Option<&str>,
    limit: i32,
) -> Result<HistoryResponse, aws_sdk_dynamodb::Error> {
    let mut query = client
        .query()
        .table_name(table)
        .key_condition_expression("PK = :pk AND begins_with(SK, :sk_prefix)")
        .expression_attribute_values(":pk", AttributeValue::S(format!("USR#{}", user_id)))
        .expression_attribute_values(":sk_prefix", AttributeValue::S("EVT#".to_string()))
        .scan_index_forward(false)
        .limit(limit + 1);

    if let Some(cursor_val) = cursor {
        let exclusive_start_key = HashMap::from([
            ("PK".to_string(), AttributeValue::S(format!("USR#{}", user_id))),
            ("SK".to_string(), AttributeValue::S(cursor_val.to_string())),
        ]);
        query = query.set_exclusive_start_key(Some(exclusive_start_key));
    }

    let result = query.send().await?;

    let items = result.items();
    let has_more = items.len() as i32 > limit;
    let actual_items = if has_more { &items[..items.len() - 1] } else { items };

    let mut history = Vec::with_capacity(actual_items.len());
    let mut last_sk = None;

    for item in actual_items {
        let watched_at = get_str(item, "occurredAt").to_string();
        let sk = get_str(item, "SK").to_string();
        let meta = resolve_watch_event(client, table, item).await?;

        last_sk = Some(sk);

        history.push(HistoryItem {
            episode: EpisodeRef {
                id: meta.episode_id,
                season_number: meta.season_number,
                episode_number: meta.episode_number,
                name: meta.episode_name,
            },
            series: SeriesRef {
                id: meta.series_id,
                name: meta.series_name,
                poster_path: meta.poster_path,
            },
            watched_at,
        });
    }

    let next_cursor = if has_more {
        last_sk
    } else {
        None
    };

    Ok(HistoryResponse {
        items: history,
        next_cursor,
    })
}

pub async fn get_calendar(client: &Client, table: &str, user_id: &str, from: &str, to: &str) -> Result<Vec<CalendarDay>, aws_sdk_dynamodb::Error> {
    let result = client
        .query()
        .table_name(table)
        .key_condition_expression("PK = :pk AND begins_with(SK, :sk_prefix)")
        .expression_attribute_values(":pk", AttributeValue::S(format!("USR#{}", user_id)))
        .expression_attribute_values(":sk_prefix", AttributeValue::S("EVT#".to_string()))
        .scan_index_forward(false)
        .send()
        .await?;

    let items = result.items();
    let mut episodes_by_date: std::collections::BTreeMap<String, Vec<CalendarEpisode>> = std::collections::BTreeMap::new();

    for item in items {
        let occurred_at = get_str(item, "occurredAt").to_string();
        let date = occurred_at.split('T').next().unwrap_or("").to_string();

        if date.as_str() < from || date.as_str() >= to {
            continue;
        }

        let series_id = get_str(item, "seriesId").to_string();
        let series_name = get_str(item, "seriesName").to_string();
        let episode_id = get_str(item, "episodeId").to_string();
        let season_number = get_i32(item, "seasonNumber");
        let episode_number = get_i32(item, "episodeNumber");
        let episode_name = get_str(item, "episodeName").to_string();
        let poster_path = get_opt_str(item, "posterPath").map(|s| s.to_string());

        let ep = CalendarEpisode {
            series: SeriesRef {
                id: series_id,
                name: series_name,
                poster_path,
            },
            episode: EpisodeRef {
                id: episode_id,
                season_number,
                episode_number,
                name: episode_name,
            },
        };

        episodes_by_date.entry(date).or_default().push(ep);
    }

    let items: Vec<CalendarDay> = episodes_by_date
        .into_iter()
        .map(|(date, episodes)| CalendarDay { date, episodes })
        .collect();

    Ok(items)
}
