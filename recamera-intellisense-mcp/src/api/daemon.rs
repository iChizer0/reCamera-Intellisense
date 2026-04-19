//! Daemon-side endpoints served by `recamera-intellisense-daemon` over the
//! device's HTTP surface. Distinct from the Record HTTP API: the daemon
//! exposes the intellisense event store and an absolute-path file fetch under
//! a pre-configured allowed prefix, intended for quick access to capture
//! outputs and detection-event snapshots.

use anyhow::{Context, Result};

use crate::api_client::ApiClient;
use crate::types::{DetectionEvent, DeviceRecord};

// MARK: Event store

pub async fn get_events(
    client: &ApiClient,
    device: &DeviceRecord,
    start_unix_ms: Option<i64>,
    end_unix_ms: Option<i64>,
) -> Result<Vec<DetectionEvent>> {
    let mut params: Vec<(&str, String)> = Vec::new();
    if let Some(s) = start_unix_ms {
        params.push(("start", s.to_string()));
    }
    if let Some(e) = end_unix_ms {
        params.push(("end", e.to_string()));
    }
    let params_ref: Vec<(&str, &str)> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let params_opt = if params_ref.is_empty() {
        None
    } else {
        Some(params_ref.as_slice())
    };
    let raw = client
        .get_json(device, "/api/v1/intellisense/events", params_opt)
        .await?;
    let items = raw.as_array().context("expected array of events")?;

    let mut events = Vec::with_capacity(items.len());
    for item in items {
        let Some(ts_ms) = item.get("timestamp").and_then(|v| v.as_u64()) else {
            continue;
        };
        let event_id = item.get("id").and_then(|v| v.as_str()).unwrap_or("").trim();
        let rule_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let rule_name = if !event_id.is_empty() {
            event_id.to_string()
        } else {
            rule_type.to_string()
        };
        let snapshot_path = item
            .get("file_event")
            .and_then(|fe| fe.get("path"))
            .and_then(|v| v.as_str())
            .map(String::from);
        events.push(DetectionEvent {
            timestamp: crate::util::unix_ms_to_iso8601(ts_ms),
            timestamp_unix_ms: ts_ms,
            rule_name,
            snapshot_path,
        });
    }
    Ok(events)
}

pub async fn clear_events(client: &ApiClient, device: &DeviceRecord) -> Result<()> {
    let resp = client
        .post_json(device, "/api/v1/intellisense/events/clear", None, None)
        .await?;
    let status = resp.get("status").and_then(|v| v.as_str()).unwrap_or("");
    anyhow::ensure!(
        status.eq_ignore_ascii_case("ok"),
        "clear events failed: {}",
        resp.get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error")
    );
    Ok(())
}

// MARK: File fetch / delete (daemon allowed-prefix)

pub async fn fetch_file(
    client: &ApiClient,
    device: &DeviceRecord,
    remote_path: &str,
) -> Result<Vec<u8>> {
    client
        .get_bytes(device, "/api/v1/file", Some(&[("path", remote_path)]))
        .await
}

pub async fn delete_file(
    client: &ApiClient,
    device: &DeviceRecord,
    remote_path: &str,
) -> Result<()> {
    client
        .delete(device, "/api/v1/file", Some(&[("path", remote_path)]))
        .await
}
