use anyhow::{bail, Result};
use serde_json::{json, Value};

use crate::api_client::ApiClient;
use crate::types::*;

const CAPTURE_OUTPUT_DEFAULT: &str = "/mnt/rc_mmcblk0p8/reCamera";
const CAPTURE_FORMAT_IMAGE: &str = "JPG";
const CAPTURE_POLL_INTERVAL_MS: u64 = 500;
const CAPTURE_TIMEOUT_SECS: u64 = 5;

fn parse_capture_event(data: &Value) -> CaptureEvent {
    CaptureEvent {
        id: data
            .get("sID")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        output_directory: data
            .get("sOutputDirectory")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        format: data
            .get("sFormat")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        video_length_seconds: data
            .get("iVideoLengthSeconds")
            .and_then(|v| v.as_i64())
            .map(|v| v as i32),
        status: data
            .get("sStatus")
            .and_then(|v| v.as_str())
            .unwrap_or("UNKNOWN")
            .to_string(),
        timestamp_unix_ms: data.get("iTimestamp").and_then(|v| v.as_u64()).unwrap_or(0),
        file_name: data
            .get("sFileName")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
    }
}

pub async fn get_capture_status(
    client: &ApiClient,
    device: &DeviceRecord,
) -> Result<CaptureStatus> {
    let data = client
        .get_json(device, "/cgi-bin/entry.cgi/record/capture/status", None)
        .await?;
    let last_capture = data
        .get("dLastCapture")
        .filter(|v| v.is_object())
        .map(parse_capture_event);
    Ok(CaptureStatus {
        last_capture,
        ready_to_start_new: data
            .get("bReadyToStartNew")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        stop_requested: data
            .get("bStopRequested")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
    })
}

pub async fn start_capture(
    client: &ApiClient,
    device: &DeviceRecord,
    output: Option<&str>,
    format: Option<&str>,
    video_length_seconds: Option<i32>,
) -> Result<CaptureEvent> {
    let mut payload = json!({
        "sOutput": output.unwrap_or(CAPTURE_OUTPUT_DEFAULT),
        "sFormat": format.unwrap_or(CAPTURE_FORMAT_IMAGE).to_uppercase(),
    });
    if let Some(vl) = video_length_seconds {
        payload["iVideoLengthSeconds"] = json!(vl);
    }
    let result = client
        .post_json(
            device,
            "/cgi-bin/entry.cgi/record/capture/start",
            None,
            Some(&payload),
        )
        .await?;
    if result.get("code").and_then(|v| v.as_i64()).unwrap_or(-1) != 0 {
        let msg = result
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown error");
        bail!("Failed to start capture: {msg}");
    }
    let capture_data = result
        .get("dCapture")
        .ok_or_else(|| anyhow::anyhow!("Missing capture event in response"))?;
    Ok(parse_capture_event(capture_data))
}

pub async fn stop_capture(client: &ApiClient, device: &DeviceRecord) -> Result<()> {
    let result = client
        .post_json(device, "/cgi-bin/entry.cgi/record/capture/stop", None, None)
        .await?;
    if result.get("code").and_then(|v| v.as_i64()).unwrap_or(-1) != 0 {
        let msg = result
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown error");
        bail!("Failed to stop capture: {msg}");
    }
    Ok(())
}

const TERMINAL_STATUSES: &[&str] = &["COMPLETED", "FAILED", "INTERRUPTED", "CANCELED"];

/// Capture an image, poll for completion, and download it.
/// Returns (CaptureEvent, image_bytes).
pub async fn capture_image(
    client: &ApiClient,
    device: &DeviceRecord,
    output: Option<&str>,
    timeout_seconds: Option<u64>,
) -> Result<(CaptureEvent, Vec<u8>)> {
    // If no output is specified, get the default enabled storage and use its path as output
    let output = if let Some(output) = output {
        output.to_string()
    } else {
        let storages = crate::storage::get_storage_list(client, device).await?;
        storages
            .iter()
            .find(|s| s.is_enabled)
            .filter(|s| !s.mount_path.is_empty())
            .and_then(|s| {
                let full_path = std::path::Path::new(&s.mount_path).join(&s.data_dir);
                full_path.to_str().map(|p| p.to_string())
            })
            .unwrap_or_else(|| CAPTURE_OUTPUT_DEFAULT.to_string())
    };
    let capture = start_capture(
        client,
        device,
        Some(output.as_str()),
        Some(CAPTURE_FORMAT_IMAGE),
        None,
    )
    .await?;
    let timeout = timeout_seconds.unwrap_or(CAPTURE_TIMEOUT_SECS);
    let interval = std::time::Duration::from_millis(CAPTURE_POLL_INTERVAL_MS);
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(timeout);
    let mut final_capture = capture.clone();
    while tokio::time::Instant::now() < deadline {
        let status = get_capture_status(client, device).await?;
        if let Some(last) = &status.last_capture {
            tokio::time::sleep(interval).await;
            if last.id == capture.id && TERMINAL_STATUSES.contains(&last.status.as_str()) {
                final_capture = last.clone();
                break;
            }
        }
    }
    if final_capture.status != "COMPLETED" {
        bail!(
            "Capture did not complete successfully (status: '{}')",
            final_capture.status
        );
    }
    let remote_path = format!(
        "{}/{}",
        final_capture.output_directory, final_capture.file_name
    );
    let image_data = crate::storage::fetch_file(client, device, &remote_path).await?;
    Ok((final_capture, image_data))
}
