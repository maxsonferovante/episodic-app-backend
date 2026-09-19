pub use aws_sdk_dynamodb::Client;
use aws_sdk_dynamodb::types::{AttributeValue, KeysAndAttributes, PutRequest, WriteRequest};
use crate::models::user::User;
use crate::models::library::LibraryItem;
use crate::models::progress::{WatchProgress, WatchEvent, WatchStatus, NextEpisode};
use crate::models::series::WatchProviders;
use std::collections::HashMap;

const CACHE_TTL_MONTH: i64 = 30 * 24 * 3600;
const CACHE_TTL_PROVIDERS: i64 = 7 * 24 * 3600;

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

/// Series name + poster, preferring the synced series meta and falling back to
/// the catalog cache keyed by TMDB id.
/// Series display metadata.
pub struct SeriesMeta {
    pub name: String,
    pub poster_path: Option<String>,
    pub first_air_date: Option<String>,
    pub status: Option<String>,
    /// TMDB's total episode count, used as the denominator on library cards.
    pub total_episodes: i32,
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
                total_episodes: get_i32(item, "numberOfEpisodes"),
            }));
        }
    }

    Ok(None)
}

/// Series display metadata for many series in one `BatchGetItem` (100 keys per
/// batch), so `/library` no longer issues one read per series.
pub async fn get_series_meta_bulk(
    client: &Client,
    table: &str,
    series_ids: &[String],
) -> Result<HashMap<String, SeriesMeta>, aws_sdk_dynamodb::Error> {
    let mut metas = HashMap::new();
    if series_ids.is_empty() {
        return Ok(metas);
    }

    for chunk in series_ids.chunks(100) {
        let mut keys = Vec::with_capacity(chunk.len());
        for series_id in chunk {
            let mut key = HashMap::new();
            key.insert("PK".to_string(), AttributeValue::S(format!("SER#{}", series_id)));
            key.insert("SK".to_string(), AttributeValue::S("META".to_string()));
            keys.push(key);
        }

        let mut request_items = HashMap::new();
        request_items.insert(
            table.to_string(),
            KeysAndAttributes::builder().set_keys(Some(keys)).build()?,
        );

        let result = client
            .batch_get_item()
            .set_request_items(Some(request_items))
            .send()
            .await?;

        let responses = result.responses();
        for item in responses.and_then(|r| r.get(table)).into_iter().flatten() {
            let name = get_str(item, "name").to_string();
            if name.is_empty() {
                continue;
            }
            let series_id = get_str(item, "PK")
                .strip_prefix("SER#")
                .unwrap_or_default()
                .to_string();
            metas.insert(
                series_id,
                SeriesMeta {
                    name,
                    poster_path: get_opt_str(item, "posterPath").map(str::to_string),
                    first_air_date: get_opt_str(item, "firstAirDate").map(str::to_string),
                    status: get_opt_str(item, "status").map(str::to_string),
                    total_episodes: get_i32(item, "numberOfEpisodes"),
                },
            );
        }
    }

    Ok(metas)
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

/// A user's progress row for one `(season, episode)`, as stored on
/// `PROG#<series>#<ss>#<ee>`.
#[derive(Debug, Clone)]
pub struct ProgressEntry {
    pub status: String,
    pub watched_at: Option<String>,
}

impl ProgressEntry {
    pub fn is_watched(&self) -> bool {
        self.status == WatchStatus::Watched.as_str()
    }
}

/// Every progress row the user has for one series, keyed by
/// `(season_number, episode_number)`.
///
/// One Query replaces the fan-out that used to be issued while scanning for
/// the next unwatched episode (one `GetItem` per episode) and the two
/// overlapping `count_watched_*` Queries.
pub async fn get_series_progress_map(
    client: &Client,
    table: &str,
    user_id: &str,
    series_id: &str,
) -> Result<HashMap<(i32, i32), ProgressEntry>, aws_sdk_dynamodb::Error> {
    let prefix = format!("PROG#{}#", series_id);

    let result = client
        .query()
        .table_name(table)
        .key_condition_expression("PK = :pk AND begins_with(SK, :sk_prefix)")
        .expression_attribute_values(":pk", AttributeValue::S(format!("USR#{}", user_id)))
        .expression_attribute_values(":sk_prefix", AttributeValue::S(prefix))
        .send()
        .await?;

    let mut map = HashMap::with_capacity(result.items().len());
    for item in result.items() {
        map.insert(
            (get_i32(item, "seasonNumber"), get_i32(item, "episodeNumber")),
            ProgressEntry {
                status: get_str(item, "status").to_string(),
                watched_at: get_opt_str(item, "watchedAt").map(str::to_string),
            },
        );
    }

    Ok(map)
}

/// Watched-episode count per series for one user, from a single paginated
/// Query over every `PROG#` row they own.
///
/// Replaces the per-series `count_watched_in_series` fan-out used by
/// `/library`.
pub async fn get_user_progress_counts(
    client: &Client,
    table: &str,
    user_id: &str,
) -> Result<HashMap<String, i32>, aws_sdk_dynamodb::Error> {
    let mut counts: HashMap<String, i32> = HashMap::new();
    let mut last_key: Option<HashMap<String, AttributeValue>> = None;

    loop {
        let mut request = client
            .query()
            .table_name(table)
            .key_condition_expression("PK = :pk AND begins_with(SK, :sk_prefix)")
            .expression_attribute_values(":pk", AttributeValue::S(format!("USR#{}", user_id)))
            .expression_attribute_values(":sk_prefix", AttributeValue::S("PROG#".to_string()));

        if let Some(key) = last_key.take() {
            request = request.set_exclusive_start_key(Some(key));
        }

        let result = request.send().await?;

        for item in result.items() {
            if get_str(item, "status") != WatchStatus::Watched.as_str() {
                continue;
            }
            let series_id = get_str(item, "seriesId");
            if series_id.is_empty() {
                continue;
            }
            *counts.entry(series_id.to_string()).or_insert(0) += 1;
        }

        match result.last_evaluated_key() {
            Some(key) => last_key = Some(key.clone()),
            None => break,
        }
    }

    Ok(counts)
}

/// One episode coordinate from a series partition.
#[derive(Debug, Clone)]
pub struct EpisodeRow {
    pub season_number: i32,
    pub episode_number: i32,
    pub name: String,
    pub air_date: Option<String>,
}

/// The whole `SER#<series_id>` partition — `META`, every `SN#` and every
/// `EP#` — read in a single paginated Query.
///
/// Collapsing these into one round trip removes the per-request fan-out into
/// separate season, aired-count, total-count and episode reads.
pub struct SeriesPartition {
    pub name: String,
    pub poster_path: Option<String>,
    pub number_of_episodes: i32,
    pub seasons: Vec<crate::models::season::Season>,
    pub episodes: Vec<EpisodeRow>,
}

impl SeriesPartition {
    /// Sum of every season's episode count (specials included), falling back
    /// to the aired episode count and then to the stored series total — the
    /// same precedence `series_episode_total` used.
    pub fn total_episodes(&self) -> i32 {
        let season_sum: i32 = self.seasons.iter().map(|s| s.episode_count).sum();
        if season_sum > 0 {
            return season_sum;
        }
        let aired = self.aired_episode_count(None);
        if aired > 0 {
            return aired;
        }
        self.number_of_episodes
    }

    /// Number of episodes that have already aired, optionally scoped to one
    /// season.
    pub fn aired_episode_count(&self, season_number: Option<i32>) -> i32 {
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        self.episodes
            .iter()
            .filter(|e| season_number.is_none_or(|s| e.season_number == s))
            .filter(|e| e.episode_number > 0)
            .filter(|e| match e.air_date.as_deref() {
                Some(date) => date <= today.as_str(),
                None => false,
            })
            .count() as i32
    }

    /// Episode numbers that have already aired for a season, ascending.
    pub fn aired_episode_numbers(&self, season_number: i32) -> Vec<i32> {
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let mut numbers: Vec<i32> = self
            .episodes
            .iter()
            .filter(|e| e.season_number == season_number && e.episode_number > 0)
            .filter(|e| match e.air_date.as_deref() {
                Some(date) => date <= today.as_str(),
                None => false,
            })
            .map(|e| e.episode_number)
            .collect();
        numbers.sort_unstable();
        numbers
    }

    pub fn season_episode_count(&self, season_number: i32) -> i32 {
        self.seasons
            .iter()
            .find(|s| s.season_number == season_number)
            .map(|s| s.episode_count)
            .unwrap_or(0)
    }

    pub fn episode_name(&self, season_number: i32, episode_number: i32) -> Option<&str> {
        self.episodes
            .iter()
            .find(|e| e.season_number == season_number && e.episode_number == episode_number)
            .map(|e| e.name.as_str())
    }
}

pub async fn get_series_partition(
    client: &Client,
    table: &str,
    series_id: &str,
) -> Result<SeriesPartition, aws_sdk_dynamodb::Error> {
    let mut partition = SeriesPartition {
        name: String::new(),
        poster_path: None,
        number_of_episodes: 0,
        seasons: Vec::new(),
        episodes: Vec::new(),
    };

    let mut last_key: Option<HashMap<String, AttributeValue>> = None;
    loop {
        let mut request = client
            .query()
            .table_name(table)
            .key_condition_expression("PK = :pk")
            .expression_attribute_values(":pk", AttributeValue::S(format!("SER#{}", series_id)));

        if let Some(key) = last_key.take() {
            request = request.set_exclusive_start_key(Some(key));
        }

        let result = request.send().await?;

        for item in result.items() {
            let sk = get_str(item, "SK");
            if sk == "META" {
                partition.name = get_str(item, "name").to_string();
                partition.poster_path = get_opt_str(item, "posterPath").map(str::to_string);
                partition.number_of_episodes = get_i32(item, "numberOfEpisodes");
            } else if sk.starts_with("SN#") {
                partition.seasons.push(crate::models::season::Season {
                    id: get_str(item, "id").to_string(),
                    series_id: get_str(item, "seriesId").to_string(),
                    tmdb_id: item
                        .get("tmdbId")
                        .and_then(|v| v.as_n().ok())
                        .and_then(|n| n.parse::<i64>().ok()),
                    season_number: get_i32(item, "seasonNumber"),
                    name: get_str(item, "name").to_string(),
                    overview: get_opt_str(item, "overview").map(str::to_string),
                    poster_path: get_opt_str(item, "posterPath").map(str::to_string),
                    air_date: get_opt_str(item, "airDate").map(str::to_string),
                    episode_count: get_i32(item, "episodeCount"),
                });
            } else if sk.starts_with("EP#") {
                partition.episodes.push(EpisodeRow {
                    season_number: get_i32(item, "seasonNumber"),
                    episode_number: get_i32(item, "episodeNumber"),
                    name: get_str(item, "name").to_string(),
                    air_date: get_opt_str(item, "airDate").map(str::to_string),
                });
            }
        }

        match result.last_evaluated_key() {
            Some(key) => last_key = Some(key.clone()),
            None => break,
        }
    }

    Ok(partition)
}

/// First episode (specials first, then season/episode order) that the user has
/// not marked watched. Pure — derived from an already-fetched partition and
/// progress map, so it costs no extra reads.
pub fn next_unwatched_episode(
    series_id: &str,
    partition: &SeriesPartition,
    progress: &HashMap<(i32, i32), ProgressEntry>,
) -> Option<NextEpisode> {
    let mut episodes: Vec<&EpisodeRow> = partition
        .episodes
        .iter()
        .filter(|e| e.episode_number > 0)
        .collect();
    episodes.sort_by(|a, b| {
        a.season_number
            .cmp(&b.season_number)
            .then(a.episode_number.cmp(&b.episode_number))
    });

    for episode in episodes {
        let is_watched = progress
            .get(&(episode.season_number, episode.episode_number))
            .map(ProgressEntry::is_watched)
            .unwrap_or(false);
        if !is_watched {
            return Some(NextEpisode {
                episode_id: format!(
                    "{}#S{:02}#E{:02}",
                    series_id, episode.season_number, episode.episode_number
                ),
                series_id: series_id.to_string(),
                season_number: episode.season_number,
                episode_number: episode.episode_number,
            });
        }
    }

    None
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

/// Metadata the caller has already resolved, so writing a watch event needs
/// no extra reads. Built by the progress handler from the series partition.
pub struct WatchEventSeed<'a> {
    pub series_id: &'a str,
    pub season_number: i32,
    pub episode_number: i32,
    pub episode_name: Option<&'a str>,
    pub series_name: Option<&'a str>,
    pub poster_path: Option<&'a str>,
}

pub async fn create_watch_event(
    client: &Client,
    table: &str,
    user_id: &str,
    episode_id: &str,
    event_type: &str,
    seed: &WatchEventSeed<'_>,
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
    item.insert("seriesId".to_string(), AttributeValue::S(seed.series_id.to_string()));
    item.insert(
        "seasonNumber".to_string(),
        AttributeValue::N(seed.season_number.to_string()),
    );
    item.insert(
        "episodeNumber".to_string(),
        AttributeValue::N(seed.episode_number.to_string()),
    );
    if let Some(name) = seed.episode_name.filter(|n| !n.is_empty()) {
        item.insert("episodeName".to_string(), AttributeValue::S(name.to_string()));
    }
    if let Some(name) = seed.series_name.filter(|n| !n.is_empty()) {
        item.insert("seriesName".to_string(), AttributeValue::S(name.to_string()));
    }
    if let Some(poster) = seed.poster_path {
        item.insert("posterPath".to_string(), AttributeValue::S(poster.to_string()));
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

/// Mark (or unmark) many episodes of one season in a single `BatchWriteItem`
/// per 25 items, instead of one `PutItem` per episode.
pub async fn mark_episodes_bulk(
    client: &Client,
    table: &str,
    user_id: &str,
    series_id: &str,
    season_number: i32,
    episode_numbers: &[i32],
    watched: bool,
) -> Result<(), aws_sdk_dynamodb::Error> {
    if episode_numbers.is_empty() {
        return Ok(());
    }

    let now = chrono::Utc::now().to_rfc3339();
    let mut requests = Vec::with_capacity(episode_numbers.len());

    for &episode_number in episode_numbers {
        let mut item = HashMap::new();
        item.insert("PK".to_string(), AttributeValue::S(format!("USR#{}", user_id)));
        item.insert(
            "SK".to_string(),
            AttributeValue::S(format!(
                "PROG#{}#{:02}#{:02}",
                series_id, season_number, episode_number
            )),
        );
        item.insert("seriesId".to_string(), AttributeValue::S(series_id.to_string()));
        item.insert(
            "seasonNumber".to_string(),
            AttributeValue::N(season_number.to_string()),
        );
        item.insert(
            "episodeNumber".to_string(),
            AttributeValue::N(episode_number.to_string()),
        );
        item.insert(
            "status".to_string(),
            AttributeValue::S(if watched {
                WatchStatus::Watched.as_str().to_string()
            } else {
                WatchStatus::Unwatched.as_str().to_string()
            }),
        );
        if watched {
            item.insert("watchedAt".to_string(), AttributeValue::S(now.clone()));
        }

        requests.push(
            WriteRequest::builder()
                .put_request(PutRequest::builder().set_item(Some(item)).build()?)
                .build(),
        );
    }

    for chunk in requests.chunks(25) {
        let mut pending: Vec<WriteRequest> = chunk.to_vec();

        // BatchWriteItem can return unprocessed items under throttling; retry
        // them a bounded number of times so a mark is never silently dropped.
        for attempt in 0..5 {
            let mut request_items = HashMap::new();
            request_items.insert(table.to_string(), pending.clone());
            let result = client
                .batch_write_item()
                .set_request_items(Some(request_items))
                .send()
                .await?;

            pending = result
                .unprocessed_items()
                .and_then(|items| items.get(table))
                .cloned()
                .unwrap_or_default();

            if pending.is_empty() {
                break;
            }
            if attempt < 4 {
                tokio::time::sleep(std::time::Duration::from_millis(50 * (attempt + 1))).await;
            }
        }

        if !pending.is_empty() {
            tracing::warn!(
                "{} progress writes still unprocessed after retries for {} S{:02}",
                pending.len(),
                series_id,
                season_number
            );
        }
    }

    Ok(())
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

/// Aired episodes per season for a series.
///
/// Prefers the `airedCount` rollup that hydrate/season-detail write onto each
/// `SN#` row — one small Query, no episode scan. Falls back to the legacy
/// per-episode scan when the rollup is absent (series cached before the rollup
/// existed), so the response never changes just because of the migration.
pub async fn get_aired_counts(
    client: &Client,
    table: &str,
    series_id: &str,
) -> Result<(i32, HashMap<i32, i32>), aws_sdk_dynamodb::Error> {
    let result = client
        .query()
        .table_name(table)
        .key_condition_expression("PK = :pk AND begins_with(SK, :sk_prefix)")
        .expression_attribute_values(":pk", AttributeValue::S(format!("SER#{}", series_id)))
        .expression_attribute_values(":sk_prefix", AttributeValue::S("SN#".to_string()))
        .send()
        .await?;

    let items = result.items();

    if items.iter().any(|item| item.contains_key("airedCount")) {
        let mut by_season: HashMap<i32, i32> = HashMap::new();
        for item in items {
            let season = get_i32(item, "seasonNumber");
            let aired = item
                .get("airedCount")
                .and_then(|v| v.as_n().ok())
                .and_then(|n| n.parse::<i32>().ok())
                .unwrap_or_else(|| get_i32(item, "episodeCount"));
            by_season.insert(season, aired);
        }
        let total = by_season.values().sum();
        return Ok((total, by_season));
    }

    // Legacy path: count aired episode rows.
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();

    let episodes = client
        .query()
        .table_name(table)
        .key_condition_expression("PK = :pk AND begins_with(SK, :sk_prefix)")
        .expression_attribute_values(":pk", AttributeValue::S(format!("SER#{}", series_id)))
        .expression_attribute_values(":sk_prefix", AttributeValue::S("EP#".to_string()))
        .send()
        .await?;

    let mut by_season: HashMap<i32, i32> = HashMap::new();
    let mut total = 0;
    for item in episodes.items() {
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

    if is_cache_expired(item) {
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

    if is_cache_expired(item) {
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
    aired_count: i32,
) -> Result<(), aws_sdk_dynamodb::Error> {
    client
        .update_item()
        .table_name(table)
        .key("PK", AttributeValue::S(format!("SER#{}", series_id)))
        .key("SK", AttributeValue::S(format!("SN#{:02}", season_number)))
        .update_expression("SET seasonNumber = :sn, episodeCount = :ec, airedCount = :ac")
        .expression_attribute_values(":sn", AttributeValue::N(season_number.to_string()))
        .expression_attribute_values(":ec", AttributeValue::N(episode_count.to_string()))
        .expression_attribute_values(":ac", AttributeValue::N(aired_count.to_string()))
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

/// English weekday name ("Monday".."Sunday") for a `YYYY-MM-DD` date.
/// Falls back to "" when the date doesn't parse.
pub fn weekday_name(date: &str) -> String {
    chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .map(|d| d.format("%A").to_string())
        .unwrap_or_default()
}

/// One releases row: (air_date, series_id, series_name, poster_path,
/// season_number, episode_number, episode_id, episode_name).
type ReleaseRow = (
    String,
    String,
    String,
    Option<String>,
    i32,
    i32,
    String,
    String,
);

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
            let weekday = weekday_name(&air_date);
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
                weekday,
            });
        }
    }

    upcoming.sort_by(|a, b| a.air_date.cmp(&b.air_date));
    Ok(upcoming)
}

/// Cap for a single releases query so one call can't balloon the payload.
pub const MAX_RELEASE_ITEMS: usize = 300;

/// Every episode from the user's library series airing in `[from, to)`.
/// Powers the releases views (week / month / 3 months / specific month).
/// Only episodes already in cache are visible — series never hydrated or
/// browsed contribute nothing. Episodes without an air date are skipped.
pub async fn get_releases(
    client: &Client,
    table: &str,
    user_id: &str,
    from: &str,
    to: &str,
) -> Result<Vec<UpcomingItem>, aws_sdk_dynamodb::Error> {
    let lib_result = client
        .query()
        .table_name(table)
        .key_condition_expression("PK = :pk AND begins_with(SK, :sk_prefix)")
        .expression_attribute_values(":pk", AttributeValue::S(format!("USR#{}", user_id)))
        .expression_attribute_values(":sk_prefix", AttributeValue::S("LIB#".to_string()))
        .send()
        .await?;

    // (air_date, series_id, series_name, poster, season, episode, id, name).
    let mut items: Vec<ReleaseRow> = Vec::new();

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

        for ep_item in ep_result.items() {
            let air_date = match get_opt_str(ep_item, "airDate") {
                Some(d) => d.to_string(),
                None => continue,
            };
            if air_date.as_str() < from || air_date.as_str() >= to {
                continue;
            }
            items.push((
                air_date,
                series_id.clone(),
                series_name.clone(),
                poster_path.clone(),
                get_i32(ep_item, "seasonNumber"),
                get_i32(ep_item, "episodeNumber"),
                get_str(ep_item, "id").to_string(),
                get_str(ep_item, "name").to_string(),
            ));
        }
    }

    items.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then(a.1.cmp(&b.1))
            .then(a.4.cmp(&b.4))
            .then(a.5.cmp(&b.5))
    });
    items.truncate(MAX_RELEASE_ITEMS);

    Ok(items
        .into_iter()
        .map(
            |(air_date, series_id, series_name, poster_path, season_number, episode_number, episode_id, episode_name)| {
                let weekday = weekday_name(&air_date);
                UpcomingItem {
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
                    weekday,
                }
            },
        )
        .collect())
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
    event_type: String,
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
        event_type: get_str(item, "eventType").to_string(),
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
            event_type: meta.event_type,
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
    // Keyset pagination: SKs are `EVT#<millis>#<id>` (time-ordered) and the
    // read is descending, so "next page" is simply `SK < cursor`. This avoids
    // ExclusiveStartKey entirely. Non-EVT# rows (PROFILE/LIB#/PROG#) all sort
    // above any EVT# cursor, so they can never leak into a page.
    let mut query = client
        .query()
        .table_name(table)
        .expression_attribute_values(":pk", AttributeValue::S(format!("USR#{}", user_id)))
        .scan_index_forward(false)
        .limit(limit + 1);

    if let Some(cursor_val) = cursor {
        query = query
            .key_condition_expression("PK = :pk AND SK < :cursor")
            .expression_attribute_values(
                ":cursor",
                AttributeValue::S(cursor_val.to_string()),
            );
    } else {
        query = query
            .key_condition_expression("PK = :pk AND begins_with(SK, :sk_prefix)")
            .expression_attribute_values(":sk_prefix", AttributeValue::S("EVT#".to_string()));
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
        // Defensive: only EVT# rows belong on this feed. The cursor always
        // advances past the raw item so a stray row can never loop or repeat.
        last_sk = Some(sk.clone());
        if !sk.starts_with("EVT#") {
            continue;
        }
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
            event_type: meta.event_type,
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
