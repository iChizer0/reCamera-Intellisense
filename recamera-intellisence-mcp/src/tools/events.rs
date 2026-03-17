use base64::Engine;
use rmcp::{ErrorData as McpError, model::*};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;

use crate::devices::client::DeviceClient;
use crate::event_store::SharedEventStore;

fn err(msg: impl Into<String>) -> McpError {
    McpError::internal_error(msg.into(), None)
}

fn json_text_result(value: &Value) -> CallToolResult {
    CallToolResult::success(vec![Content::text(
        serde_json::to_string_pretty(value).unwrap(),
    )])
}

fn text_result(text: &str) -> CallToolResult {
    CallToolResult::success(vec![Content::text(text.to_string())])
}

pub async fn get_detection_events(
    event_store: &SharedEventStore,
    start_unix_ms: Option<u64>,
    end_unix_ms: Option<u64>,
) -> Result<CallToolResult, McpError> {
    let events = {
        let store = event_store.lock().await;
        store.query_events(start_unix_ms, end_unix_ms)
    };

    let result: Vec<Value> = events
        .iter()
        .map(|e| {
            let mut event = json!({
                "timestamp_unix_ms": e.timestamp,
                "rule_type": e.rule_type,
                "rule_name": e.id.as_deref().unwrap_or(""),
                "uid": e.uid,
            });

            if let Some(ref fe) = e.file_event {
                event["snapshot_path"] = json!(fe.path);
                event["file_size"] = json!(fe.size);
                event["file_op"] = json!(fe.op);
            }

            event
        })
        .collect();

    Ok(json_text_result(&json!(result)))
}

pub async fn get_detection_events_count(
    event_store: &SharedEventStore,
    start_unix_ms: Option<u64>,
    end_unix_ms: Option<u64>,
) -> Result<CallToolResult, McpError> {
    let count = {
        let store = event_store.lock().await;
        store.query_events_size(start_unix_ms, end_unix_ms)
    };

    Ok(text_result(&format!("{}", count)))
}

pub async fn clear_detection_events(
    event_store: &SharedEventStore,
) -> Result<CallToolResult, McpError> {
    let mut store = event_store.lock().await;
    store.clear_merged();

    Ok(text_result("Detection events cleared"))
}

pub async fn fetch_detection_event_image(
    client: &Arc<DeviceClient>,
    allowed_file_prefix: &Path,
    snapshot_path: &str,
) -> Result<CallToolResult, McpError> {
    let image_data = read_device_file(client, allowed_file_prefix, snapshot_path).await?;
    let base64_data = base64::engine::general_purpose::STANDARD.encode(&image_data);

    let mime_type = guess_mime_type(snapshot_path);

    Ok(CallToolResult::success(vec![Content::image(
        base64_data,
        mime_type,
    )]))
}

/// Read a file from the device. Tries local filesystem first (for on-device use),
/// falls back to the device HTTP API.
async fn read_device_file(
    client: &Arc<DeviceClient>,
    allowed_file_prefix: &Path,
    path: &str,
) -> Result<Vec<u8>, McpError> {
    let file_path = Path::new(path);

    // Try local filesystem first (on-device mode)
    if file_path.is_absolute() {
        if let Ok(canonical) = tokio::fs::canonicalize(file_path).await {
            let prefix = tokio::fs::canonicalize(allowed_file_prefix)
                .await
                .unwrap_or_else(|_| allowed_file_prefix.to_path_buf());
            if canonical.starts_with(&prefix) {
                if let Ok(data) = tokio::fs::read(&canonical).await {
                    return Ok(data);
                }
            }
        }
    }

    // Fall back to HTTP API
    let (data, content_type) = client
        .get_bytes("/api/v1/file", &[("path", path)])
        .await
        .map_err(|e| err(e))?;

    if content_type.contains("application/json") {
        if let Ok(e) = serde_json::from_slice::<Value>(&data) {
            let msg = e
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("Unknown error");
            return Err(err(format!("Failed to fetch file: {msg}")));
        }
    }

    Ok(data)
}

fn guess_mime_type(path: &str) -> &'static str {
    let lower = path.to_lowercase();
    if lower.ends_with(".png") {
        "image/png"
    } else if lower.ends_with(".gif") {
        "image/gif"
    } else {
        "image/jpeg"
    }
}
