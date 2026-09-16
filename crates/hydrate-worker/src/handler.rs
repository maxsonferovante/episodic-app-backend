//! SQS consumer that fully hydrates a series' metadata.
//!
//! Reports per-message failures via `batchItemFailures` so one bad series does
//! not force the whole batch to be reprocessed (Lambda `ReportBatchItemFailures`).

use lambda_runtime::LambdaEvent;
use serde::{Deserialize, Serialize};
use shared::db;

#[derive(Debug, Deserialize)]
pub struct SqsEvent {
    #[serde(rename = "Records", default)]
    pub records: Vec<SqsRecord>,
}

#[derive(Debug, Deserialize)]
pub struct SqsRecord {
    #[serde(rename = "messageId")]
    pub message_id: String,
    pub body: String,
}

#[derive(Debug, Deserialize)]
struct HydrateMessage {
    #[serde(rename = "seriesId")]
    series_id: Option<String>,
    #[serde(rename = "tmdbId")]
    tmdb_id: Option<i64>,
    /// Operator backfills set this to bypass the fresh-hydration short-circuit.
    #[serde(default)]
    force: bool,
}

#[derive(Debug, Serialize)]
pub struct BatchItemFailure {
    #[serde(rename = "itemIdentifier")]
    pub item_identifier: String,
}

#[derive(Debug, Serialize)]
pub struct SqsBatchResponse {
    #[serde(rename = "batchItemFailures")]
    pub batch_item_failures: Vec<BatchItemFailure>,
}

pub async fn handle_sqs_event(
    event: LambdaEvent<SqsEvent>,
) -> Result<SqsBatchResponse, lambda_runtime::Error> {
    let table =
        std::env::var("DYNAMODB_TABLE_NAME").unwrap_or_else(|_| "EpisodicEpisodes".to_string());
    let client = db::get_client().await;

    let mut batch_item_failures = Vec::new();

    for record in event.payload.records {
        if let Err(e) = process_record(&client, &table, &record).await {
            tracing::error!("Hydrate failed for message {}: {}", record.message_id, e);
            batch_item_failures.push(BatchItemFailure {
                item_identifier: record.message_id,
            });
        }
    }

    Ok(SqsBatchResponse {
        batch_item_failures,
    })
}

async fn process_record(
    client: &aws_sdk_dynamodb::Client,
    table: &str,
    record: &SqsRecord,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let message: HydrateMessage =
        serde_json::from_str(&record.body).map_err(|e| format!("invalid hydrate message: {}", e))?;

    let tmdb_id = match (message.tmdb_id, message.series_id.as_deref()) {
        (Some(id), _) if id > 0 => id,
        (_, Some(series_id)) => parse_tmdb_id(series_id)
            .ok_or_else(|| format!("invalid series id: {}", series_id))?,
        _ => return Err("hydrate message missing seriesId/tmdbId".into()),
    };

    let outcome = shared::hydrate::hydrate_series(client, table, tmdb_id, message.force).await?;
    tracing::info!(
        "Hydrated {} (status={}, finished={}, seasons={}, episodes={}, skipped={}, forced={})",
        tmdb_id,
        outcome.status,
        outcome.finished,
        outcome.seasons_hydrated,
        outcome.episodes_written,
        outcome.skipped,
        message.force
    );

    Ok(())
}

fn parse_tmdb_id(value: &str) -> Option<i64> {
    value.strip_prefix("ser_").unwrap_or(value).parse::<i64>().ok()
}
