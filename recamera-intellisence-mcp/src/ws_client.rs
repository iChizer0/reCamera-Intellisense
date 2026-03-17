use futures_util::StreamExt;
use tokio::time::{sleep, Duration};
use tokio_tungstenite::connect_async;
use tracing::{error, info, warn};

use crate::event_store::SharedEventStore;
use crate::models::IncomingEvent;

/// Run the WebSocket client loop with automatic reconnection.
pub async fn run_ws_client(
    ws_url: String,
    store: SharedEventStore,
    shutdown: tokio::sync::watch::Receiver<bool>,
) {
    let mut reconnect_delay = Duration::from_secs(1);
    let max_delay = Duration::from_secs(30);

    loop {
        if *shutdown.borrow() {
            info!("WebSocket client shutting down");
            return;
        }

        info!(url = %ws_url, "Connecting to WebSocket");

        match connect_async(&ws_url).await {
            Ok((ws_stream, _response)) => {
                info!("WebSocket connected");
                reconnect_delay = Duration::from_secs(1);

                let (_write, mut read) = ws_stream.split();

                loop {
                    tokio::select! {
                        msg = read.next() => {
                            match msg {
                                Some(Ok(message)) => {
                                    if message.is_text() || message.is_binary() {
                                        let data = message.into_data();
                                        match serde_json::from_slice::<IncomingEvent>(&data) {
                                            Ok(event) => {
                                                let mut store = store.lock().await;
                                                match event {
                                                    IncomingEvent::Rule(rule) => {
                                                        store.handle_rule_event(rule);
                                                    }
                                                    IncomingEvent::File(file) => {
                                                        store.handle_file_event(file);
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                warn!("Failed to parse WebSocket message: {e}");
                                            }
                                        }
                                    }
                                }
                                Some(Err(e)) => {
                                    error!("WebSocket error: {e}");
                                    break;
                                }
                                None => {
                                    warn!("WebSocket stream ended");
                                    break;
                                }
                            }
                        }
                        _ = shutdown_wait(&shutdown) => {
                            info!("WebSocket client shutting down");
                            return;
                        }
                    }
                }
            }
            Err(e) => {
                error!("WebSocket connection failed: {e}");
            }
        }

        if *shutdown.borrow() {
            return;
        }

        warn!(delay_secs = reconnect_delay.as_secs(), "Reconnecting after delay");
        sleep(reconnect_delay).await;
        reconnect_delay = (reconnect_delay * 2).min(max_delay);
    }
}

async fn shutdown_wait(shutdown: &tokio::sync::watch::Receiver<bool>) {
    let mut rx = shutdown.clone();
    while !*rx.borrow_and_update() {
        if rx.changed().await.is_err() {
            return;
        }
    }
}
