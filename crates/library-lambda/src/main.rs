use lambda_http::{run, service_fn};

mod handler;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    run(service_fn(handler::handle_request)).await?;
    Ok(())
}
