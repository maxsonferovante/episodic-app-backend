//! Best-effort enqueue helpers for the hydrate worker.

use aws_sdk_sqs::Client;
use serde::Serialize;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct HydrateMessage<'a> {
    series_id: &'a str,
    force: bool,
}

async fn client() -> Client {
    if let Ok(endpoint) = std::env::var("AWS_ENDPOINT_URL") {
        let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .endpoint_url(endpoint)
            .region(aws_config::Region::new(
                std::env::var("AWS_REGION").unwrap_or_else(|_| "sa-east-1".to_string()),
            ))
            .load()
            .await;
        return Client::new(&config);
    }

    let config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
    Client::new(&config)
}

/// Enqueue a series for full hydration. A missing queue URL (e.g. local runs)
/// is treated as a no-op so the add-to-library path never fails because of it.
pub async fn enqueue_hydrate(
    series_id: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    send_hydrate(series_id, false).await
}

/// Enqueue a *forced* hydration, bypassing the "already fresh" short-circuit.
/// Used by operator backfills (e.g. re-hydrating to pick up specials). Still a
/// no-op when the queue URL is unset.
pub async fn enqueue_hydrate_forced(
    series_id: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    send_hydrate(series_id, true).await
}

async fn send_hydrate(
    series_id: &str,
    force: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let queue_url = match std::env::var("HYDRATE_QUEUE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            tracing::warn!(
                "HYDRATE_QUEUE_URL not set; skipping hydrate enqueue for {}",
                series_id
            );
            return Ok(());
        }
    };

    let body = serde_json::to_string(&HydrateMessage { series_id, force })?;
    client()
        .await
        .send_message()
        .queue_url(queue_url)
        .message_body(body)
        .send()
        .await?;

    Ok(())
}
