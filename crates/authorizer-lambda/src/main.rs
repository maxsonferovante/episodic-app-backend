use lambda_runtime::{run, service_fn};

mod handler;

#[tokio::main]
async fn main() -> Result<(), lambda_runtime::Error> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    run(service_fn(handler::handle_request)).await
}
