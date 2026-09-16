//! Daily refresh scheduler.
//!
//! This job no longer talks to TMDB or writes metadata itself. It scans every
//! series that appears in a user's library and enqueues a hydrate message for
//! the ones that actually need a refresh: never-hydrated series, on-air series
//! whose next episode has aired, and series whose cached metadata has expired.
//! Finished, already-hydrated series are skipped forever.

use lambda_runtime::LambdaEvent;
use crate::ScheduledEvent;
use shared::db::get_client;
use aws_sdk_dynamodb::types::AttributeValue;
use aws_sdk_dynamodb::Client;
use std::collections::{HashMap, HashSet};
use chrono::Utc;

#[derive(Debug, serde::Serialize)]
struct SyncMetrics {
    series_scanned: usize,
    enqueued: usize,
    skipped: usize,
    errors: usize,
    duration_ms: u128,
}

pub async fn handle_scheduled_event(
    event: LambdaEvent<ScheduledEvent>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let start = Utc::now();
    tracing::info!("Sync scheduler triggered: {:?}", event.payload);

    let table_name = std::env::var("DYNAMODB_TABLE_NAME")
        .unwrap_or_else(|_| "EpisodicEpisodes".to_string());

    let client = get_client().await;

    let library_series = scan_library_items(&client, &table_name).await?;
    let unique_series: Vec<String> = library_series
        .into_iter()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let total = unique_series.len();
    tracing::info!("Found {} library series", total);

    let now = Utc::now();
    let today = now.format("%Y-%m-%d").to_string();
    let now_ts = now.timestamp();

    let mut enqueued = 0;
    let mut skipped = 0;
    let mut errors = 0;

    for series_id in &unique_series {
        let tmdb_id = match extract_tmdb_id(series_id) {
            Some(id) => id,
            None => {
                tracing::warn!("Skipping unparseable series id: {}", series_id);
                errors += 1;
                continue;
            }
        };

        match shared::db::get_hydration_state(&client, &table_name, tmdb_id).await {
            Ok(state) => {
                if needs_refresh(state, now_ts, &today) {
                    if let Err(e) = shared::sqs::enqueue_hydrate(series_id).await {
                        tracing::error!("Failed to enqueue hydrate for {}: {}", series_id, e);
                        errors += 1;
                    } else {
                        enqueued += 1;
                    }
                } else {
                    skipped += 1;
                }
            }
            Err(e) => {
                tracing::error!("Failed to read hydration state for {}: {}", series_id, e);
                errors += 1;
            }
        }
    }

    let duration = Utc::now().signed_duration_since(start);
    let metrics = SyncMetrics {
        series_scanned: total,
        enqueued,
        skipped,
        errors,
        duration_ms: duration.num_milliseconds() as u128,
    };

    tracing::info!("Sync scheduling completed: {:?}", metrics);
    Ok(())
}

/// A series needs a refresh when it has never been hydrated, when its next
/// episode has already aired, or when its cached metadata has expired.
fn needs_refresh(state: Option<shared::db::HydrationState>, now_ts: i64, today: &str) -> bool {
    let state = match state {
        Some(state) => state,
        None => return true,
    };

    let finished = state
        .series_status
        .as_deref()
        .map(shared::enums::series_status::is_finished)
        .unwrap_or(false);
    let complete = state.hydration_status.as_deref()
        == Some(shared::enums::hydration_status::COMPLETE);

    // Finished series never change again.
    if finished && complete {
        return false;
    }

    if let Some(next_air_date) = state.next_air_date.as_deref() {
        if next_air_date <= today {
            return true;
        }
    }

    state.expires_at <= now_ts
}

fn extract_tmdb_id(series_id: &str) -> Option<i64> {
    series_id.strip_prefix("ser_")?.parse().ok()
}

async fn scan_library_items(
    client: &Client,
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
