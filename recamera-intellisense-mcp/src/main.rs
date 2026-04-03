mod api_client;
mod capture;
mod detection;
mod device_store;
mod gpio;
mod server;
mod storage;
mod types;

use anyhow::Result;
use rmcp::{transport::stdio, ServiceExt};
use tracing_subscriber::EnvFilter;

use api_client::ApiClient;
use device_store::DeviceStore;
use server::ReCameraServer;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    tracing::info!("Starting reCamera Intellisense MCP server");

    let store = DeviceStore::new().await?;
    let client = ApiClient::new();
    let server = ReCameraServer::new(store, client);

    let service = server.serve(stdio()).await.inspect_err(|e| {
        tracing::error!("MCP server error: {:?}", e);
    })?;

    service.waiting().await?;

    tracing::info!("reCamera Intellisense MCP server stopped");
    Ok(())
}
