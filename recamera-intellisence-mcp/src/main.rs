mod devices;
mod event_store;
mod models;
mod server;
mod tools;
mod ws_client;

use clap::Parser;
use devices::client::DeviceClient;
use devices::DeviceConfig;
use event_store::{EventStore, EventStoreConfig};
use rmcp::ServiceExt;
use server::ReCameraServer;
use std::path::PathBuf;
use tokio::sync::watch;
use tokio::time::{interval, Duration};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "recamera-intellisense-mcp",
    about = "reCamera Intellisense MCP Server — middleware between reCamera HTTP API and MCP clients"
)]
struct Args {
    /// Device host address
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// Device port (default: auto based on protocol)
    #[arg(long)]
    port: Option<u16>,

    /// Device API authentication token
    #[arg(long, default_value = "")]
    token: String,

    /// Protocol: http or https
    #[arg(long, default_value = "http")]
    protocol: String,

    /// Allow insecure TLS connections (self-signed certs)
    #[arg(long, default_value_t = true)]
    tls_allow_insecure: bool,

    /// WebSocket URL for event monitoring
    #[arg(long, default_value = "ws://127.0.0.1:16383/api/v1/record/events")]
    ws_url: String,

    /// Maximum merged events capacity
    #[arg(long, default_value_t = 1000)]
    events_capacity: usize,

    /// Events time window in seconds
    #[arg(long, default_value_t = 180)]
    events_window_secs: u64,

    /// Rule event promote timeout in milliseconds
    #[arg(long, default_value_t = 1000)]
    rule_timeout_ms: u64,

    /// Allowed absolute path prefix for file access
    #[arg(long, default_value = "/mnt")]
    allowed_path_prefix: String,
}

#[tokio::main]
async fn main() {
    // Initialize tracing to stderr (MCP uses stdout for protocol messages)
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();

    let mut allowed_file_prefix = PathBuf::from(&args.allowed_path_prefix);
    if !allowed_file_prefix.is_absolute() {
        warn!(
            configured = %args.allowed_path_prefix,
            fallback = "/mnt",
            "allowed-path-prefix must be absolute; falling back to default"
        );
        allowed_file_prefix = PathBuf::from("/mnt");
    }

    let device_config = DeviceConfig {
        host: args.host,
        port: args.port,
        token: args.token,
        protocol: args.protocol,
        tls_allow_insecure: args.tls_allow_insecure,
    };

    info!(
        device_url = %device_config.base_url(),
        ws_url = %args.ws_url,
        allowed_path_prefix = %allowed_file_prefix.display(),
        "reCamera Intellisense MCP Server starting"
    );

    // Create event store for detection event monitoring
    let store_config = EventStoreConfig {
        rule_queue_capacity: 1000,
        merged_capacity: args.events_capacity,
        merged_window_secs: args.events_window_secs,
        rule_promote_timeout_ms: args.rule_timeout_ms,
    };
    let store = EventStore::new_shared(store_config);

    // Shutdown signal channel
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    // Start WebSocket client for event monitoring (background)
    let ws_store = store.clone();
    let ws_shutdown = shutdown_rx.clone();
    let ws_url = args.ws_url.clone();
    let ws_handle = tokio::spawn(async move {
        ws_client::run_ws_client(ws_url, ws_store, ws_shutdown).await;
    });

    // Start promotion ticker for expired pending rule events
    let tick_store = store.clone();
    let mut tick_shutdown = shutdown_rx.clone();
    let tick_handle = tokio::spawn(async move {
        let mut ticker = interval(Duration::from_millis(200));
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    let mut s = tick_store.lock().await;
                    s.promote_expired_rules();
                }
                _ = tick_shutdown.changed() => {
                    if *tick_shutdown.borrow() {
                        return;
                    }
                }
            }
        }
    });

    // Create device client and MCP server handler
    let device_client = DeviceClient::new(device_config);
    let server = ReCameraServer::new(device_client, store.clone(), allowed_file_prefix);

    // Run MCP server over stdio transport (blocks until stdin closes)
    let transport = rmcp::transport::io::stdio();
    let service = server
        .serve(transport)
        .await
        .inspect_err(|e| tracing::error!("serving error: {:?}", e))
        .expect("Failed to start MCP server");

    service.waiting().await.expect("MCP server error");

    // Signal all background tasks to shut down
    let _ = shutdown_tx.send(true);
    let _ = ws_handle.await;
    let _ = tick_handle.await;

    info!("MCP server stopped");
}
