pub use aws_sdk_dynamodb::Client;
use aws_sdk_dynamodb::types::AttributeValue;
use crate::models::user::User;
use crate::models::library::LibraryItem;
use crate::models::progress::{WatchProgress, WatchEvent, WatchStatus, NextEpisode};
use std::collections::HashMap;

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

    let status = match get_str(item, "status") {
        "WATCHED" => WatchStatus::Watched,
        "UPCOMING" => WatchStatus::Upcoming,
        _ => WatchStatus::Unwatched,
    };

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
    item.insert("status".to_string(), AttributeValue::S("WATCHED".to_string()));
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
    item.insert("status".to_string(), AttributeValue::S("UNWATCHED".to_string()));

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
        .filter(|item| get_str(item, "status") == "WATCHED")
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
        .filter(|item| get_str(item, "status") == "WATCHED")
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
            if season > 0 && episode > 0 {
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
        .key("PK", AttributeValue::S(format!("SERIES#{}", tmdb_id)))
        .key("SK", AttributeValue::S("META".to_string()))
        .send()
        .await?;

    let item = match result.item() {
        Some(item) => item,
        None => return Ok(None),
    };

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

pub async fn cache_series(client: &Client, table: &str, series: &crate::models::series::Series) -> Result<(), aws_sdk_dynamodb::Error> {
    let expires_at = chrono::Utc::now().timestamp() + 7 * 24 * 3600;
    let mut item = std::collections::HashMap::new();
    item.insert("PK".to_string(), AttributeValue::S(format!("SERIES#{}", series.tmdb_id)));
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

pub async fn get_cached_seasons(client: &Client, table: &str, tmdb_id: i64) -> Result<Vec<crate::models::season::Season>, aws_sdk_dynamodb::Error> {
    let result = client
        .query()
        .table_name(table)
        .key_condition_expression("PK = :pk AND begins_with(SK, :sk_prefix)")
        .expression_attribute_values(":pk", AttributeValue::S(format!("SERIES#{}", tmdb_id)))
        .expression_attribute_values(":sk_prefix", AttributeValue::S("SEASON#".to_string()))
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

pub async fn get_cached_episodes(client: &Client, table: &str, series_tmdb_id: i64, season_number: i32) -> Result<Vec<crate::models::episode::Episode>, aws_sdk_dynamodb::Error> {
    let result = client
        .query()
        .table_name(table)
        .key_condition_expression("PK = :pk AND begins_with(SK, :sk_prefix)")
        .expression_attribute_values(":pk", AttributeValue::S(format!("SERIES#{}", series_tmdb_id)))
        .expression_attribute_values(":sk_prefix", AttributeValue::S(format!("EPISODE#{}#", season_number)))
        .send()
        .await?;

    let episodes = result.items().iter().filter_map(|item| {
        let episode_number = item.get("episodeNumber").and_then(|v| v.as_n().ok())?.parse::<i32>().ok()?;
        Some(crate::models::episode::Episode {
            id: get_str(item, "id").to_string(),
            series_id: get_str(item, "seriesId").to_string(),
            season_id: get_str(item, "seasonId").to_string(),
            tmdb_id: item.get("tmdbId").and_then(|v| v.as_n().ok()).and_then(|n| n.parse::<i64>().ok()),
            episode_number,
            name: get_str(item, "name").to_string(),
            overview: item.get("overview").and_then(|v| v.as_s().ok()).map(|s| s.to_string()),
            still_path: item.get("stillPath").and_then(|v| v.as_s().ok()).map(|s| s.to_string()),
            air_date: item.get("airDate").and_then(|v| v.as_s().ok()).map(|s| s.to_string()),
            runtime: item.get("runtime").and_then(|v| v.as_n().ok()).and_then(|n| n.parse::<i32>().ok()),
            vote_average: item.get("voteAverage").and_then(|v| v.as_n().ok()).and_then(|n| n.parse::<f64>().ok()),
        })
    }).collect();

    Ok(episodes)
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
        .expression_attribute_values(":in_progress", AttributeValue::S("IN_PROGRESS".to_string()))
        .limit(10)
        .send()
        .await?;

    let items = result.items();
    let mut continue_watching = Vec::with_capacity(items.len());

    for item in items {
        let series_id = get_str(item, "seriesId").to_string();
        let series_name = get_str(item, "seriesName").to_string();
        let poster_path = get_opt_str(item, "posterPath").map(|s| s.to_string());
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
    let fourteen_days = (chrono::Utc::now() + chrono::Duration::days(14)).format("%Y-%m-%d").to_string();

    let mut upcoming = Vec::new();

    for lib_item in lib_result.items() {
        let series_id = get_str(lib_item, "seriesId").to_string();
        let series_name = get_str(lib_item, "seriesName").to_string();
        let poster_path = get_opt_str(lib_item, "posterPath").map(|s| s.to_string());

        let ep_result = client
            .query()
            .table_name(table)
            .key_condition_expression("PK = :pk AND begins_with(SK, :sk_prefix)")
            .expression_attribute_values(":pk", AttributeValue::S(format!("SER#{}", series_id)))
            .expression_attribute_values(":sk_prefix", AttributeValue::S("EP#".to_string()))
            .send()
            .await?;

        for ep_item in ep_result.items() {
            let air_date = match get_opt_str(ep_item, "airDate") {
                Some(d) => d.to_string(),
                None => continue,
            };

            if air_date.as_str() >= today.as_str() && air_date.as_str() <= fourteen_days.as_str() {
                let episode_id = get_str(ep_item, "id").to_string();
                let season_number = get_i32(ep_item, "seasonNumber");
                let episode_number = get_i32(ep_item, "episodeNumber");
                let episode_name = get_str(ep_item, "name").to_string();

                upcoming.push(UpcomingItem {
                    series: SeriesRef {
                        id: series_id.clone(),
                        name: series_name.clone(),
                        poster_path: poster_path.clone(),
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
    }

    upcoming.sort_by(|a, b| a.air_date.cmp(&b.air_date));
    Ok(upcoming)
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
        let episode_id = get_str(item, "episodeId").to_string();
        let series_id = get_str(item, "seriesId").to_string();
        let series_name = get_str(item, "seriesName").to_string();
        let episode_name = get_str(item, "episodeName").to_string();
        let season_number = get_i32(item, "seasonNumber");
        let episode_number = get_i32(item, "episodeNumber");
        let watched_at = get_str(item, "occurredAt").to_string();
        let poster_path = get_opt_str(item, "posterPath").map(|s| s.to_string());

        history.push(HistoryItem {
            episode: EpisodeRef {
                id: episode_id,
                season_number,
                episode_number,
                name: episode_name,
            },
            series: SeriesRef {
                id: series_id,
                name: series_name,
                poster_path,
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
        let episode_id = get_str(item, "episodeId").to_string();
        let series_id = get_str(item, "seriesId").to_string();
        let series_name = get_str(item, "seriesName").to_string();
        let episode_name = get_str(item, "episodeName").to_string();
        let season_number = get_i32(item, "seasonNumber");
        let episode_number = get_i32(item, "episodeNumber");
        let watched_at = get_str(item, "occurredAt").to_string();
        let poster_path = get_opt_str(item, "posterPath").map(|s| s.to_string());
        let sk = get_str(item, "SK").to_string();

        last_sk = Some(sk);

        history.push(HistoryItem {
            episode: EpisodeRef {
                id: episode_id,
                season_number,
                episode_number,
                name: episode_name,
            },
            series: SeriesRef {
                id: series_id,
                name: series_name,
                poster_path,
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
