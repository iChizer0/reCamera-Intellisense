use base64::Engine;
use rmcp::{ErrorData as McpError, model::*};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;

use crate::devices::client::DeviceClient;

const CAPTURE_OUTPUT_DEFAULT: &str = "/mnt/rc_mmcblk0p8/reCamera";
const CAPTURE_POLL_INTERVAL_MS: u64 = 500;
const CAPTURE_POLL_TIMEOUT_MS: u64 = 5000;

fn err(msg: impl Into<String>) -> McpError {
    McpError::internal_error(msg.into(), None)
}

fn parse_capture_event(data: &Value) -> Value {
    json!({
        "id": data.get("sID").and_then(|v| v.as_str()).unwrap_or(""),
        "output_directory": data.get("sOutputDirectory").and_then(|v| v.as_str()).unwrap_or(""),
        "format": data.get("sFormat").and_then(|v| v.as_str()).unwrap_or(""),
        "video_length_seconds": data.get("iVideoLengthSeconds"),
        "status": data.get("sStatus").and_then(|v| v.as_str()).unwrap_or("UNKNOWN"),
        "timestamp_unix_ms": data.get("iTimestamp").and_then(|v| v.as_u64()).unwrap_or(0),
        "file_name": data.get("sFileName").and_then(|v| v.as_str()).unwrap_or(""),
    })
}

fn parse_capture_status(data: &Value) -> Result<Value, McpError> {
    if !data.is_object() {
        return Err(err("Invalid response format: expected an object"));
    }

    let last_capture = data
        .get("dLastCapture")
        .filter(|v| v.is_object())
        .map(|v| parse_capture_event(v));

    Ok(json!({
        "last_capture": last_capture,
        "ready_to_start_new": data.get("bReadyToStartNew").and_then(|v| v.as_bool()).unwrap_or(false),
        "stop_requested": data.get("bStopRequested").and_then(|v| v.as_bool()).unwrap_or(false),
    }))
}

fn json_text_result(value: &Value) -> CallToolResult {
    CallToolResult::success(vec![Content::text(
        serde_json::to_string_pretty(value).unwrap(),
    )])
}

fn text_result(text: &str) -> CallToolResult {
    CallToolResult::success(vec![Content::text(text.to_string())])
}

pub async fn get_capture_status(client: &Arc<DeviceClient>) -> Result<CallToolResult, McpError> {
    let data = client
        .get_json("/cgi-bin/entry.cgi/record/capture/status", &[])
        .await
        .map_err(|e| err(e))?;

    let result = parse_capture_status(&data)?;
    Ok(json_text_result(&result))
}

pub async fn start_capture(
    client: &Arc<DeviceClient>,
    format: Option<String>,
    output: Option<String>,
    video_length_seconds: Option<i64>,
) -> Result<CallToolResult, McpError> {
    let format = format.as_deref().unwrap_or("JPG");
    let output = output.as_deref().unwrap_or(CAPTURE_OUTPUT_DEFAULT);

    let mut payload = json!({
        "sOutput": output,
        "sFormat": format.to_uppercase(),
    });

    if let Some(length) = video_length_seconds {
        payload["iVideoLengthSeconds"] = json!(length);
    }

    let data = client
        .post_json(
            "/cgi-bin/entry.cgi/record/capture/start",
            &[],
            Some(&payload),
        )
        .await
        .map_err(|e| err(e))?;

    if data.get("code").and_then(|c| c.as_i64()) != Some(0) {
        let msg = data
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("Unknown error");
        return Err(err(format!("Failed to start capture: {msg}")));
    }

    let capture = data
        .get("dCapture")
        .ok_or_else(|| err("Missing capture event in response"))?;
    let result = parse_capture_event(capture);
    Ok(json_text_result(&result))
}

pub async fn stop_capture(client: &Arc<DeviceClient>) -> Result<CallToolResult, McpError> {
    let data = client
        .post_json("/cgi-bin/entry.cgi/record/capture/stop", &[], None)
        .await
        .map_err(|e| err(e))?;

    if data.get("code").and_then(|c| c.as_i64()) != Some(0) {
        let msg = data
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("Unknown error");
        return Err(err(format!("Failed to stop capture: {msg}")));
    }

    Ok(text_result("Capture stopped successfully"))
}

pub async fn capture_image(
    client: &Arc<DeviceClient>,
    allowed_file_prefix: &Path,
    output: Option<String>,
) -> Result<CallToolResult, McpError> {
    let output = output.as_deref().unwrap_or(CAPTURE_OUTPUT_DEFAULT);

    // Start image capture
    let payload = json!({
        "sOutput": output,
        "sFormat": "JPG",
    });

    let data = client
        .post_json(
            "/cgi-bin/entry.cgi/record/capture/start",
            &[],
            Some(&payload),
        )
        .await
        .map_err(|e| err(e))?;

    if data.get("code").and_then(|c| c.as_i64()) != Some(0) {
        let msg = data
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("Unknown error");
        return Err(err(format!("Failed to start capture: {msg}")));
    }

    let capture = data
        .get("dCapture")
        .ok_or_else(|| err("Missing capture event in response"))?;
    let capture_id = capture
        .get("sID")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // Poll for completion
    let mut elapsed = 0u64;
    let mut final_capture = parse_capture_event(capture);

    while elapsed < CAPTURE_POLL_TIMEOUT_MS {
        tokio::time::sleep(tokio::time::Duration::from_millis(CAPTURE_POLL_INTERVAL_MS)).await;
        elapsed += CAPTURE_POLL_INTERVAL_MS;

        let status_data = client
            .get_json("/cgi-bin/entry.cgi/record/capture/status", &[])
            .await
            .map_err(|e| err(e))?;

        if let Some(last) = status_data.get("dLastCapture").filter(|v| v.is_object()) {
            let last_id = last.get("sID").and_then(|v| v.as_str()).unwrap_or("");
            if last_id == capture_id {
                final_capture = parse_capture_event(last);
                let status = last.get("sStatus").and_then(|v| v.as_str()).unwrap_or("");
                if matches!(
                    status,
                    "COMPLETED" | "FAILED" | "INTERRUPTED" | "CANCELED"
                ) {
                    break;
                }
            }
        }
    }

    let status = final_capture
        .get("status")
        .and_then(|s| s.as_str())
        .unwrap_or("UNKNOWN");
    if status != "COMPLETED" {
        return Err(err(format!(
            "Capture did not complete successfully (status: '{status}')"
        )));
    }

    // Download the captured image from the device filesystem
    let output_dir = final_capture
        .get("output_directory")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let file_name = final_capture
        .get("file_name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let remote_path = format!("{}/{}", output_dir, file_name);

    let image_data = read_device_file(client, allowed_file_prefix, &remote_path).await?;
    let base64_data = base64::engine::general_purpose::STANDARD.encode(&image_data);

    let mime_type = if file_name.to_lowercase().ends_with(".png") {
        "image/png"
    } else {
        "image/jpeg"
    };

    Ok(CallToolResult::success(vec![
        Content::text(serde_json::to_string_pretty(&final_capture).unwrap()),
        Content::image(base64_data, mime_type),
    ]))
}

/// Read a file from the device. Tries local filesystem first (for on-device use),
/// falls back to the device HTTP API.
pub async fn read_device_file(
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

    // Check if the response is an error JSON instead of file content
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
