use futures_util::stream::SplitStream;
use futures_util::StreamExt;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::time::{sleep, Duration};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{client_async, connect_async, WebSocketStream};
use tracing::{error, info, warn};

use crate::event_store::SharedEventStore;
use crate::models::IncomingEvent;

/// WebSocket connection target.
pub enum WsTarget {
    /// Connect via TCP using a ws:// or wss:// URL.
    Tcp(String),
    /// Connect via Unix domain socket with the given socket path and URI path.
    Unix {
        socket_path: String,
        uri_path: String,
    },
}

/// Run the WebSocket client loop with automatic reconnection.
/// This function never returns under normal operation.
pub async fn run_ws_client(
    target: WsTarget,
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

        match &target {
            WsTarget::Tcp(url) => {
                info!(url = %url, "Connecting to WebSocket (TCP)");
                match connect_async(url).await {
                    Ok((ws_stream, _response)) => {
                        info!("WebSocket connected (TCP)");
                        reconnect_delay = Duration::from_secs(1);
                        let (_write, read) = ws_stream.split();
                        if read_loop(read, &store, &shutdown).await {
                            return; // shutdown requested
                        }
                    }
                    Err(e) => {
                        error!("WebSocket connection failed: {e}");
                    }
                }
            }
            WsTarget::Unix {
                socket_path,
                uri_path,
            } => {
                info!(path = %socket_path, uri = %uri_path, "Connecting to WebSocket (Unix)");
                match connect_unix(socket_path, uri_path).await {
                    Ok(ws_stream) => {
                        info!("WebSocket connected (Unix)");
                        reconnect_delay = Duration::from_secs(1);
                        let (_write, read) = ws_stream.split();
                        if read_loop(read, &store, &shutdown).await {
                            return; // shutdown requested
                        }
                    }
                    Err(e) => {
                        error!("WebSocket connection failed: {e}");
                    }
                }
            }
        }

        if *shutdown.borrow() {
            return;
        }

        warn!(
            delay_secs = reconnect_delay.as_secs(),
            "Reconnecting after delay"
        );
        sleep(reconnect_delay).await;
        reconnect_delay = (reconnect_delay * 2).min(max_delay);
    }
}

async fn connect_unix(
    socket_path: &str,
    uri_path: &str,
) -> Result<WebSocketStream<tokio::net::UnixStream>, tokio_tungstenite::tungstenite::Error> {
    let stream = tokio::net::UnixStream::connect(socket_path)
        .await
        .map_err(tokio_tungstenite::tungstenite::Error::Io)?;
    let request = format!("ws://localhost{uri_path}")
        .into_client_request()
        .map_err(|e| {
            tokio_tungstenite::tungstenite::Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                e,
            ))
        })?;
    let (ws_stream, _response) = client_async(request, stream).await?;
    Ok(ws_stream)
}

/// Process incoming WebSocket messages. Returns `true` if shutdown was requested.
async fn read_loop<S>(
    mut read: SplitStream<WebSocketStream<S>>,
    store: &SharedEventStore,
    shutdown: &tokio::sync::watch::Receiver<bool>,
) -> bool
where
    S: AsyncRead + AsyncWrite + Unpin,
{
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
                        return false;
                    }
                    None => {
                        warn!("WebSocket stream ended");
                        return false;
                    }
                }
            }
            _ = shutdown_wait(shutdown) => {
                info!("WebSocket client shutting down");
                return true;
            }
        }
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
