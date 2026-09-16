use lambda_runtime::{run, service_fn};
use serde::Deserialize;

mod handler;

#[derive(Debug, Deserialize)]
pub struct ScheduledEvent {
    pub source: Option<String>,
    #[serde(rename = "detail-type")]
    pub detail_type: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    run(service_fn(handler::handle_scheduled_event)).await?;
    Ok(())
}
