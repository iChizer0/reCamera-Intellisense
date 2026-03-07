use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs;
use tokio::net::{TcpListener, UnixListener};
use tracing::{error, info};

use crate::event_store::SharedEventStore;

const MAX_FILE_SIZE: u64 = 100 * 1024 * 1024; // 100 MB

type BoxBody = Full<Bytes>;

fn json_response(status: StatusCode, body: &str) -> Response<BoxBody> {
    Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .body(Full::new(Bytes::from(body.to_string())))
        .unwrap()
}

fn no_content_response() -> Response<BoxBody> {
    Response::builder()
        .status(StatusCode::NO_CONTENT)
        .body(Full::new(Bytes::new()))
        .unwrap()
}

fn parse_query(uri: &hyper::Uri) -> HashMap<String, String> {
    uri.query()
        .map(|q| {
            url::form_urlencoded::parse(q.as_bytes())
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

async fn handle_request(
    req: Request<Incoming>,
    store: SharedEventStore,
    allowed_file_prefix: Arc<PathBuf>,
) -> Result<Response<BoxBody>, hyper::Error> {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let query = parse_query(req.uri());

    let response = match (method, path.as_str()) {
        (Method::GET, "/api/v1/generate-204") | (Method::HEAD, "/api/v1/generate-204") => {
            no_content_response()
        }
        (Method::GET, "/api/v1/events") => handle_get_events(&query, &store).await,
        (Method::GET, "/api/v1/events/size") => handle_get_events_size(&query, &store).await,
        (Method::POST, "/api/v1/events/clear") => handle_clear_events(&store).await,
        (Method::GET, "/api/v1/file") => {
            handle_get_file(&query, allowed_file_prefix.as_path()).await
        }
        _ => json_response(StatusCode::NOT_FOUND, r#"{"error":"Not found"}"#),
    };

    Ok(response)
}

async fn handle_get_events(
    query: &HashMap<String, String>,
    store: &SharedEventStore,
) -> Response<BoxBody> {
    let start = query.get("start").and_then(|s| s.parse::<u64>().ok());
    let end = query.get("end").and_then(|s| s.parse::<u64>().ok());

    let events = {
        let store = store.lock().await;
        store.query_events(start, end)
    };

    let json = serde_json::to_string(&events).unwrap_or_else(|_| "[]".to_string());
    json_response(StatusCode::OK, &json)
}

async fn handle_get_events_size(
    query: &HashMap<String, String>,
    store: &SharedEventStore,
) -> Response<BoxBody> {
    let start = query.get("start").and_then(|s| s.parse::<u64>().ok());
    let end = query.get("end").and_then(|s| s.parse::<u64>().ok());

    let size = {
        let store = store.lock().await;
        store.query_events_size(start, end)
    };

    json_response(StatusCode::OK, &format!(r#"{{"size":{size}}}"#))
}

async fn handle_clear_events(store: &SharedEventStore) -> Response<BoxBody> {
    let mut store = store.lock().await;
    store.clear_merged();
    json_response(StatusCode::OK, r#"{"status":"ok"}"#)
}

async fn handle_get_file(
    query: &HashMap<String, String>,
    allowed_file_prefix: &Path,
) -> Response<BoxBody> {
    let path_str = match query.get("path") {
        Some(p) => p,
        None => {
            return json_response(
                StatusCode::BAD_REQUEST,
                r#"{"error":"Missing 'path' query parameter"}"#,
            );
        }
    };

    let path = Path::new(path_str);

    // Security: only allow absolute paths under configured prefix.
    if !path.is_absolute() {
        return json_response(
            StatusCode::BAD_REQUEST,
            r#"{"error":"Path must be absolute"}"#,
        );
    }

    let canonical_requested = match fs::canonicalize(path).await {
        Ok(p) => p,
        Err(e) => {
            return json_response(
                StatusCode::NOT_FOUND,
                &format!(r#"{{"error":"File not found: {e}"}}"#),
            );
        }
    };

    let canonical_prefix = fs::canonicalize(allowed_file_prefix)
        .await
        .unwrap_or_else(|_| allowed_file_prefix.to_path_buf());

    if !canonical_requested.starts_with(&canonical_prefix) {
        return json_response(
            StatusCode::FORBIDDEN,
            &format!(
                r#"{{"error":"Path is outside allowed prefix: {}"}}"#,
                canonical_prefix.display()
            ),
        );
    }

    // Check file metadata
    let metadata = match fs::metadata(&canonical_requested).await {
        Ok(m) => m,
        Err(e) => {
            return json_response(
                StatusCode::NOT_FOUND,
                &format!(r#"{{"error":"File not found: {e}"}}"#),
            );
        }
    };

    if !metadata.is_file() {
        return json_response(StatusCode::BAD_REQUEST, r#"{"error":"Path is not a file"}"#);
    }

    if metadata.len() > MAX_FILE_SIZE {
        return json_response(
            StatusCode::BAD_REQUEST,
            &format!(
                r#"{{"error":"File size ({}) exceeds maximum allowed size ({})"}}"#,
                metadata.len(),
                MAX_FILE_SIZE
            ),
        );
    }

    // Read file contents
    match fs::read(&canonical_requested).await {
        Ok(data) => {
            // Guess content type from extension
            let content_type = guess_content_type(path_str);
            Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", content_type)
                .header("Content-Length", data.len().to_string())
                .body(Full::new(Bytes::from(data)))
                .unwrap()
        }
        Err(e) => json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!(r#"{{"error":"Failed to read file: {e}"}}"#),
        ),
    }
}

fn guess_content_type(path: &str) -> &'static str {
    let lower = path.to_lowercase();
    if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg"
    } else if lower.ends_with(".png") {
        "image/png"
    } else if lower.ends_with(".gif") {
        "image/gif"
    } else if lower.ends_with(".mp4") {
        "video/mp4"
    } else if lower.ends_with(".json") {
        "application/json"
    } else if lower.ends_with(".txt") {
        "text/plain"
    } else {
        "application/octet-stream"
    }
}

/// Start the HTTP server on a TCP socket
pub async fn run_tcp_server(
    addr: SocketAddr,
    store: SharedEventStore,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
    allowed_file_prefix: PathBuf,
) {
    let listener = match TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            error!("Failed to bind TCP listener on {addr}: {e}");
            return;
        }
    };
    info!(prefix = %allowed_file_prefix.display(), "HTTP server listening on {addr}");

    let allowed_file_prefix = Arc::new(allowed_file_prefix);

    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok((stream, _remote)) => {
                        let store = store.clone();
                        let allowed_file_prefix = allowed_file_prefix.clone();
                        tokio::spawn(async move {
                            let io = TokioIo::new(stream);
                            let svc = service_fn(move |req| {
                                let store = store.clone();
                                let allowed_file_prefix = allowed_file_prefix.clone();
                                handle_request(req, store, allowed_file_prefix)
                            });
                            if let Err(e) = http1::Builder::new().serve_connection(io, svc).await {
                                error!("HTTP connection error: {e}");
                            }
                        });
                    }
                    Err(e) => {
                        error!("TCP accept error: {e}");
                    }
                }
            }
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    info!("TCP HTTP server shutting down");
                    return;
                }
            }
        }
    }
}

/// Start the HTTP server on a Unix domain socket
pub async fn run_unix_server(
    path: String,
    permission: u32,
    store: SharedEventStore,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
    allowed_file_prefix: PathBuf,
) {
    // Remove existing socket file if it exists
    let _ = std::fs::remove_file(&path);

    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            error!("Failed to bind Unix socket at {path}: {e}");
            return;
        }
    };

    // Set permissions
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(permission))
        {
            error!("Failed to set socket permissions: {e}");
        }
    }

    info!(prefix = %allowed_file_prefix.display(), "HTTP server listening on unix:{path}");

    let allowed_file_prefix = Arc::new(allowed_file_prefix);

    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok((stream, _)) => {
                        let store = store.clone();
                        let allowed_file_prefix = allowed_file_prefix.clone();
                        tokio::spawn(async move {
                            let io = TokioIo::new(stream);
                            let svc = service_fn(move |req| {
                                let store = store.clone();
                                let allowed_file_prefix = allowed_file_prefix.clone();
                                handle_request(req, store, allowed_file_prefix)
                            });
                            if let Err(e) = http1::Builder::new().serve_connection(io, svc).await {
                                error!("HTTP connection error (unix): {e}");
                            }
                        });
                    }
                    Err(e) => {
                        error!("Unix accept error: {e}");
                    }
                }
            }
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    info!("Unix HTTP server shutting down");
                    // Clean up socket file
                    let _ = std::fs::remove_file(&path);
                    return;
                }
            }
        }
    }
}
