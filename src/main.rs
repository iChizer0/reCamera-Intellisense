mod event_store;
mod http_server;
mod models;
mod ws_client;

use clap::Parser;
use event_store::{EventStore, EventStoreConfig};
use std::net::SocketAddr;
use std::path::PathBuf;
use tokio::signal;
use tokio::sync::watch;
use tokio::time::{interval, Duration};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "monitor", about = "Monitor daemon for rule/file events")]
struct Args {
    /// WebSocket URL to connect to for events
    #[arg(long, default_value = "ws://127.0.0.1:16383/api/v1/record/events")]
    ws_url: String,

    /// HTTP listen address (host:port)
    #[arg(long, default_value = "127.0.0.1:16384")]
    http_addr: String,

    /// Unix domain socket path (empty to disable)
    #[arg(long, default_value = "")]
    unix_socket: String,

    /// Unix domain socket permission (octal)
    #[arg(long, default_value_t = 0o777)]
    unix_permission: u32,

    /// Maximum pending rule events queue size
    #[arg(long, default_value_t = 1000)]
    rule_queue_size: usize,

    /// Maximum merged results queue size
    #[arg(long, default_value_t = 1000)]
    merged_queue_size: usize,

    /// Merged results time window in seconds
    #[arg(long, default_value_t = 180)]
    merged_window_secs: u64,

    /// Timeout (ms) for unmatched rule events before promoting
    #[arg(long, default_value_t = 1000)]
    rule_timeout_ms: u64,

    /// Allowed absolute path prefix for /file endpoint
    #[arg(long, default_value = "/mnt")]
    allowed_path_prefix: String,
}

#[tokio::main]
async fn main() {
    // Initialize tracing
    tracing_subscriber::fmt()
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

    info!("Monitor daemon starting");
    info!(
        ws_url = %args.ws_url,
        http_addr = %args.http_addr,
        allowed_path_prefix = %allowed_file_prefix.display()
    );

    // Create shared event store
    let config = EventStoreConfig {
        rule_queue_capacity: args.rule_queue_size,
        merged_capacity: args.merged_queue_size,
        merged_window_secs: args.merged_window_secs,
        rule_promote_timeout_ms: args.rule_timeout_ms,
    };
    let store = EventStore::new_shared(config);

    // Shutdown signal channel
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    // Spawn WebSocket client
    let ws_store = store.clone();
    let ws_shutdown = shutdown_rx.clone();
    let ws_url = args.ws_url.clone();
    let ws_handle = tokio::spawn(async move {
        ws_client::run_ws_client(ws_url, ws_store, ws_shutdown).await;
    });

    // Spawn TCP HTTP server
    let tcp_store = store.clone();
    let tcp_shutdown = shutdown_rx.clone();
    let http_addr: SocketAddr = args.http_addr.parse().unwrap_or_else(|e| {
        warn!(
            "Invalid HTTP address '{}': {e}, using default",
            args.http_addr
        );
        "127.0.0.1:16384".parse().unwrap()
    });
    let tcp_allowed_prefix = allowed_file_prefix.clone();
    let tcp_handle = tokio::spawn(async move {
        http_server::run_tcp_server(http_addr, tcp_store, tcp_shutdown, tcp_allowed_prefix).await;
    });

    // Optionally spawn Unix socket HTTP server
    let unix_handle = if !args.unix_socket.is_empty() {
        let uds_store = store.clone();
        let uds_shutdown = shutdown_rx.clone();
        let uds_path = args.unix_socket.clone();
        let uds_perm = args.unix_permission;
        let uds_allowed_prefix = allowed_file_prefix.clone();
        Some(tokio::spawn(async move {
            http_server::run_unix_server(
                uds_path,
                uds_perm,
                uds_store,
                uds_shutdown,
                uds_allowed_prefix,
            )
            .await;
        }))
    } else {
        None
    };

    // Spawn promotion ticker: periodically promote expired pending rules
    let tick_store = store.clone();
    let mut tick_shutdown = shutdown_rx.clone();
    let tick_handle = tokio::spawn(async move {
        let mut ticker = interval(Duration::from_millis(200));
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    let mut store = tick_store.lock().await;
                    store.promote_expired_rules();
                }
                _ = tick_shutdown.changed() => {
                    if *tick_shutdown.borrow() {
                        return;
                    }
                }
            }
        }
    });

    // Wait for shutdown signal
    tokio::select! {
        _ = signal::ctrl_c() => {
            info!("Received SIGINT, shutting down...");
        }
        _ = async {
            #[cfg(unix)]
            {
                let mut sigterm = signal::unix::signal(signal::unix::SignalKind::terminate())
                    .expect("Failed to install SIGTERM handler");
                sigterm.recv().await;
            }
            #[cfg(not(unix))]
            {
                std::future::pending::<()>().await;
            }
        } => {
            info!("Received SIGTERM, shutting down...");
        }
    }

    // Signal all tasks to shut down
    let _ = shutdown_tx.send(true);

    // Wait for tasks to finish
    let _ = ws_handle.await;
    let _ = tcp_handle.await;
    if let Some(h) = unix_handle {
        let _ = h.await;
    }
    let _ = tick_handle.await;

    info!("Monitor daemon stopped");
}
