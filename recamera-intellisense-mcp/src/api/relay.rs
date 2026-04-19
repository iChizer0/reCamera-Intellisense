use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::api::storage as api_storage;
use crate::api_client::ApiClient;
use crate::types::{DeviceRecord, DirEntry, RelayStatus, StorageSlot};

// MARK: Relay lifecycle (delegates to storage control API)
//
// These thin wrappers are retained as internal helpers even though the relay tools
// are no longer exposed to MCP clients — `records` opens/refreshes a relay lazily
// via `ensure_relay_uuid` and never calls these directly. Kept for future use and
// for any in-process caller that needs explicit lifecycle control.

#[allow(dead_code)]
pub async fn open(
    client: &ApiClient,
    device: &DeviceRecord,
    dev_path: &str,
) -> Result<RelayStatus> {
    api_storage::control_relay_open(client, device, dev_path).await
}

#[allow(dead_code)]
pub async fn status(
    client: &ApiClient,
    device: &DeviceRecord,
    dev_path: &str,
) -> Result<RelayStatus> {
    api_storage::control_relay_status(client, device, dev_path).await
}

#[allow(dead_code)]
pub async fn close(client: &ApiClient, device: &DeviceRecord, dev_path: &str) -> Result<()> {
    api_storage::control_relay_close(client, device, dev_path).await
}

// MARK: Relay URL helpers

pub fn build_url(device: &DeviceRecord, uuid: &str, rel_path: &str) -> String {
    let rel = rel_path.trim_start_matches('/');
    let endpoint = if rel.is_empty() {
        format!("/storage/relay/{uuid}/")
    } else {
        format!("/storage/relay/{uuid}/{rel}")
    };
    ApiClient::api_url(device, &endpoint)
}

// MARK: Fetch bytes via relay

pub async fn fetch(
    client: &ApiClient,
    device: &DeviceRecord,
    uuid: &str,
    rel_path: &str,
) -> Result<Vec<u8>> {
    let endpoint = relay_endpoint(uuid, rel_path);
    client.get_bytes(device, &endpoint, None).await
}

// MARK: Directory listing (nginx autoindex JSON)

pub async fn list_dir(
    client: &ApiClient,
    device: &DeviceRecord,
    uuid: &str,
    rel_path: &str,
) -> Result<Vec<DirEntry>> {
    // Ensure trailing slash so nginx serves the autoindex instead of a file.
    let rel_path = rel_path.trim_start_matches('/');
    let endpoint = if rel_path.is_empty() {
        format!("/storage/relay/{uuid}/")
    } else {
        format!("/storage/relay/{uuid}/{}/", rel_path.trim_end_matches('/'))
    };
    let bytes = client.get_bytes(device, &endpoint, None).await?;
    parse_autoindex(&bytes).context("parse directory listing")
}

fn relay_endpoint(uuid: &str, rel_path: &str) -> String {
    let rel = rel_path.trim_start_matches('/');
    if rel.is_empty() {
        format!("/storage/relay/{uuid}/")
    } else {
        format!("/storage/relay/{uuid}/{rel}")
    }
}

/// Parse nginx `autoindex_format json` output. Returns a typed listing.
/// Expected entries look like:
/// `{"name":"...", "type":"directory|file", "mtime":"...", "size":123}`.
fn parse_autoindex(bytes: &[u8]) -> Result<Vec<DirEntry>> {
    let v: Value = serde_json::from_slice(bytes).context("directory listing is not JSON")?;
    // Some devices route all relay requests through the cgi handler and do not
    // expose directory listings — they instead return `{"code":..,"message":..}`.
    if let Some(obj) = v.as_object() {
        if obj.contains_key("code") && !obj.contains_key("name") {
            let code = obj.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
            let msg = obj.get("message").and_then(|v| v.as_str()).unwrap_or("");
            bail!("directory listing not supported by device (code={code}): {msg}");
        }
    }
    let arr = v.as_array().context("expected JSON array")?;
    Ok(arr
        .iter()
        .filter_map(|e| {
            let name = e.get("name").and_then(|v| v.as_str())?.to_string();
            let ty = e.get("type").and_then(|v| v.as_str()).unwrap_or("file");
            Some(DirEntry {
                name,
                is_dir: ty.eq_ignore_ascii_case("directory"),
                size: e.get("size").and_then(|v| v.as_u64()),
                mtime: e.get("mtime").and_then(|v| v.as_str()).map(String::from),
            })
        })
        .collect())
}

// MARK: Slot resolution helper

/// Resolve the target slot for relay ops: the caller-provided dev_path if given,
/// else the currently enabled slot. Errors if none enabled and none provided.
pub async fn resolve_slot(
    client: &ApiClient,
    device: &DeviceRecord,
    dev_path: Option<&str>,
) -> Result<StorageSlot> {
    let slots = api_storage::get_status(client, device).await?;
    if let Some(p) = dev_path.filter(|p| !p.is_empty()) {
        return slots
            .into_iter()
            .find(|s| s.dev_path == p)
            .with_context(|| format!("slot with dev_path '{p}' not found"));
    }
    let enabled: Vec<_> = slots.into_iter().filter(|s| s.enabled).collect();
    match enabled.len() {
        0 => bail!("no storage slot is enabled; call set_storage_slot first"),
        1 => Ok(enabled.into_iter().next().unwrap()),
        _ => bail!("multiple slots enabled; specify dev_path explicitly"),
    }
}
