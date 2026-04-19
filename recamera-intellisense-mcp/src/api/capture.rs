use anyhow::Result;
use serde_json::{json, Value};

use crate::api::expect_ok;
use crate::api_client::ApiClient;
use crate::types::{CaptureEvent, CaptureStatus, DeviceRecord};

// MARK: Paths + defaults

const PATH_STATUS: &str = "/cgi-bin/entry.cgi/record/capture/status";
const PATH_START: &str = "/cgi-bin/entry.cgi/record/capture/start";
const PATH_STOP: &str = "/cgi-bin/entry.cgi/record/capture/stop";

pub const FORMAT_IMAGE: &str = "JPG";
pub const OUTPUT_FALLBACK: &str = "/mnt/rc_mmcblk0p8/reCamera";
const POLL_INTERVAL_MS: u64 = 500;
const TIMEOUT_SECS: u64 = 5;
const TERMINAL_STATUSES: &[&str] = &["COMPLETED", "FAILED", "INTERRUPTED", "CANCELED"];

// MARK: Parsing

fn parse_event(data: &Value) -> CaptureEvent {
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

// MARK: Status / start / stop

pub async fn get_status(client: &ApiClient, device: &DeviceRecord) -> Result<CaptureStatus> {
    let data = client.get_json(device, PATH_STATUS, None).await?;
    let last_capture = data
        .get("dLastCapture")
        .filter(|v| v.is_object())
        .map(parse_event);
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

pub async fn start(
    client: &ApiClient,
    device: &DeviceRecord,
    output: Option<&str>,
    format: Option<&str>,
    video_length_seconds: Option<i32>,
) -> Result<CaptureEvent> {
    let mut payload = json!({
        "sOutput": output.unwrap_or(OUTPUT_FALLBACK),
        "sFormat": format.unwrap_or(FORMAT_IMAGE).to_uppercase(),
    });
    if let Some(vl) = video_length_seconds {
        payload["iVideoLengthSeconds"] = json!(vl);
    }
    let resp = client
        .post_json(device, PATH_START, None, Some(&payload))
        .await?;
    expect_ok(&resp, "start capture")?;
    let data = resp
        .get("dCapture")
        .ok_or_else(|| anyhow::anyhow!("missing dCapture in response"))?;
    Ok(parse_event(data))
}

pub async fn stop(client: &ApiClient, device: &DeviceRecord) -> Result<()> {
    let resp = client.post_json(device, PATH_STOP, None, None).await?;
    expect_ok(&resp, "stop capture")
}

// MARK: High-level: capture image + download

/// Start a JPG capture, poll to completion, and download the result via the daemon file API.
pub async fn capture_image(
    client: &ApiClient,
    device: &DeviceRecord,
    output: Option<&str>,
    timeout_seconds: Option<u64>,
) -> Result<(CaptureEvent, Vec<u8>)> {
    let output = match output {
        Some(o) => o.to_string(),
        None => default_output(client, device).await,
    };
    let capture = start(
        client,
        device,
        Some(output.as_str()),
        Some(FORMAT_IMAGE),
        None,
    )
    .await?;

    let timeout = timeout_seconds.unwrap_or(TIMEOUT_SECS);
    let poll = std::time::Duration::from_millis(POLL_INTERVAL_MS);
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(timeout);
    let mut final_event = capture.clone();
    while tokio::time::Instant::now() < deadline {
        tokio::time::sleep(poll).await;
        let status = get_status(client, device).await?;
        if let Some(last) = status.last_capture {
            if last.id == capture.id && TERMINAL_STATUSES.contains(&last.status.as_str()) {
                final_event = last;
                break;
            }
        }
    }
    if final_event.status != "COMPLETED" {
        anyhow::bail!(
            "capture did not complete (status: '{}')",
            final_event.status
        );
    }
    let remote = format!("{}/{}", final_event.output_directory, final_event.file_name);
    let bytes = crate::api::daemon::fetch_file(client, device, &remote).await?;
    Ok((final_event, bytes))
}

async fn default_output(client: &ApiClient, device: &DeviceRecord) -> String {
    match crate::api::storage::get_status(client, device).await {
        Ok(slots) => slots
            .into_iter()
            .find(|s| s.enabled && !s.mount_path.is_empty())
            .and_then(|s| {
                std::path::Path::new(&s.mount_path)
                    .join(&s.data_dir)
                    .to_str()
                    .map(|p| p.to_string())
            })
            .unwrap_or_else(|| OUTPUT_FALLBACK.to_string()),
        Err(_) => OUTPUT_FALLBACK.to_string(),
    }
}
