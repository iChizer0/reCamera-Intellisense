use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs;
use tokio::net::UnixListener;
use tracing::{error, info};

use crate::event_store::SharedEventStore;

type BoxBody = http_body_util::combinators::BoxBody<Bytes, std::io::Error>;

fn json_response(status: StatusCode, body: &str) -> Response<BoxBody> {
    use http_body_util::BodyExt;
    Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .body(
            Full::new(Bytes::from(body.to_string()))
                .map_err(|never| match never {})
                .boxed(),
        )
        .unwrap()
}

fn empty_response(status: StatusCode) -> Response<BoxBody> {
    use http_body_util::BodyExt;
    Response::builder()
        .status(status)
        .body(
            Full::new(Bytes::new())
                .map_err(|never| match never {})
                .boxed(),
        )
        .unwrap()
}

fn empty_error_response(status: StatusCode, msg: &str) -> Response<BoxBody> {
    use http_body_util::BodyExt;
    // Sanitize header value to visible ASCII (0x20..=0x7E) to avoid invalid header panics
    let safe_msg: String = msg
        .chars()
        .map(|c| if (' '..='~').contains(&c) { c } else { '?' })
        .collect();
    Response::builder()
        .status(status)
        .header("X-Error", safe_msg)
        .body(
            Full::new(Bytes::new())
                .map_err(|never| match never {})
                .boxed(),
        )
        .unwrap()
}

fn parse_query(uri: &hyper::Uri) -> HashMap<String, String> {
    uri.query()
        .map(|q| {
            form_urlencoded::parse(q.as_bytes())
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
        (Method::GET, "/api/v1/recamera-generate-204")
        | (Method::HEAD, "/api/v1/recamera-generate-204") => empty_response(StatusCode::NO_CONTENT),
        (Method::GET, "/api/v1/intellisense/events") => handle_get_events(&query, &store).await,
        (Method::GET, "/api/v1/intellisense/events/size") => {
            handle_get_events_size(&query, &store).await
        }
        (Method::POST, "/api/v1/intellisense/events/clear") => handle_clear_events(&store).await,
        (Method::GET, "/api/v1/file") => {
            handle_get_file(&req, &query, allowed_file_prefix.as_path()).await
        }
        (Method::DELETE, "/api/v1/file") => {
            handle_delete_file(&query, allowed_file_prefix.as_path()).await
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
    req: &Request<Incoming>,
    query: &HashMap<String, String>,
    allowed_file_prefix: &Path,
) -> Response<BoxBody> {
    let path_str = match query.get("path") {
        Some(p) => p,
        None => {
            return empty_error_response(StatusCode::BAD_REQUEST, "Missing 'path' query parameter")
        }
    };

    let path = Path::new(path_str);

    // Security: only allow absolute paths under configured prefix.
    if !path.is_absolute() {
        return empty_error_response(StatusCode::BAD_REQUEST, "Path must be absolute");
    }

    let canonical_requested = match fs::canonicalize(path).await {
        Ok(p) => p,
        Err(e) => {
            return empty_error_response(StatusCode::NOT_FOUND, &format!("File not found: {e}"))
        }
    };

    let canonical_prefix = fs::canonicalize(allowed_file_prefix)
        .await
        .unwrap_or_else(|_| allowed_file_prefix.to_path_buf());

    if !canonical_requested.starts_with(&canonical_prefix) {
        return empty_error_response(StatusCode::FORBIDDEN, "Path is outside allowed prefix");
    }

    // Check file metadata
    let metadata = match fs::metadata(&canonical_requested).await {
        Ok(m) => m,
        Err(e) => {
            return empty_error_response(StatusCode::NOT_FOUND, &format!("File not found: {e}"))
        }
    };

    if !metadata.is_file() {
        return empty_error_response(StatusCode::BAD_REQUEST, "Path is not a file");
    }

    // Read file contents using streaming
    match fs::File::open(&canonical_requested).await {
        Ok(mut file) => {
            // Check for Range header to support resuming download
            let mut start_byte = 0;
            let file_size = metadata.len();
            let mut end_byte = file_size.saturating_sub(1);
            let mut status = StatusCode::OK;

            if let Some(range_header) = req.headers().get(hyper::header::RANGE) {
                if let Ok(range_str) = range_header.to_str() {
                    if let Some(range) = range_str.strip_prefix("bytes=") {
                        let parts: Vec<&str> = range.split('-').collect();
                        if let Some(start_str) = parts.first() {
                            if let Ok(start) = start_str.parse::<u64>() {
                                start_byte = start;
                                status = StatusCode::PARTIAL_CONTENT;
                            }
                        }
                        if parts.len() > 1 {
                            if let Ok(end) = parts[1].parse::<u64>() {
                                end_byte = std::cmp::min(end, end_byte);
                            }
                        }
                    }
                }
            }

            if start_byte > file_size {
                return empty_error_response(StatusCode::RANGE_NOT_SATISFIABLE, "Invalid range");
            }

            if start_byte > 0 {
                use std::io::SeekFrom;
                use tokio::io::AsyncSeekExt;
                if let Err(e) = file.seek(SeekFrom::Start(start_byte)).await {
                    return empty_error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        &format!("Failed to seek file: {e}"),
                    );
                }
            }

            let max_read = (end_byte - start_byte).saturating_add(1);
            use tokio::io::AsyncReadExt;
            let stream = tokio_util::io::ReaderStream::new(file.take(max_read));
            use futures_util::stream::StreamExt;
            use http_body_util::{BodyExt, StreamBody};
            use hyper::body::Frame;

            let stream_body = StreamBody::new(
                stream.map(|result: Result<bytes::Bytes, std::io::Error>| result.map(Frame::data)),
            );
            let content_type = guess_content_type(path_str);
            let mut builder = Response::builder()
                .status(status)
                .header("Content-Type", content_type)
                .header("Accept-Ranges", "bytes")
                .header("Content-Length", max_read.to_string());

            if status == StatusCode::PARTIAL_CONTENT {
                builder = builder.header(
                    "Content-Range",
                    format!("bytes {}-{}/{}", start_byte, end_byte, file_size),
                );
            }

            builder.body(BodyExt::boxed(stream_body)).unwrap()
        }
        Err(e) => empty_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Failed to open file: {e}"),
        ),
    }
}

async fn handle_delete_file(
    query: &HashMap<String, String>,
    allowed_file_prefix: &Path,
) -> Response<BoxBody> {
    let path_str = match query.get("path") {
        Some(p) => p,
        None => {
            return empty_error_response(StatusCode::BAD_REQUEST, "Missing 'path' query parameter")
        }
    };

    let path = Path::new(path_str);

    if !path.is_absolute() {
        return empty_error_response(StatusCode::BAD_REQUEST, "Path must be absolute");
    }

    let canonical_requested = match fs::canonicalize(path).await {
        Ok(p) => p,
        Err(e) => {
            return empty_error_response(StatusCode::NOT_FOUND, &format!("File not found: {e}"))
        }
    };

    let canonical_prefix = fs::canonicalize(allowed_file_prefix)
        .await
        .unwrap_or_else(|_| allowed_file_prefix.to_path_buf());

    if !canonical_requested.starts_with(&canonical_prefix) {
        return empty_error_response(StatusCode::FORBIDDEN, "Path is outside allowed prefix");
    }

    let metadata = match fs::metadata(&canonical_requested).await {
        Ok(m) => m,
        Err(e) => {
            return empty_error_response(StatusCode::NOT_FOUND, &format!("File not found: {e}"))
        }
    };

    if !metadata.is_file() {
        return empty_error_response(StatusCode::BAD_REQUEST, "Path is not a file");
    }

    match fs::remove_file(&canonical_requested).await {
        Ok(()) => empty_response(StatusCode::NO_CONTENT),
        Err(e) => empty_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Failed to delete file: {e}"),
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
