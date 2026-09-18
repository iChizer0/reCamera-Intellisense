//! Whole-device configuration backup (`/config/export`): the device stages a
//! tarball and returns a relay URL; we download it to a LOCAL file.
//! Read-only with respect to the device.

use anyhow::{bail, Result};

use crate::api_client::ApiClient;
use crate::types::{ConfigBackup, DeviceRecord};

const PATH_EXPORT: &str = "/cgi-bin/entry.cgi/config/export";

/// Download the full device configuration tarball to the local `output` path.
pub async fn export_device_config(
    client: &ApiClient,
    device: &DeviceRecord,
    output: &str,
) -> Result<ConfigBackup> {
    if output.trim().is_empty() {
        bail!("output (local file path) is required");
    }
    if std::path::Path::new(output).exists() {
        bail!("output already exists (refusing to overwrite): {output}");
    }
    let meta = client.get_json(device, PATH_EXPORT, None).await?;
    let url = meta.get("url").and_then(|v| v.as_str()).unwrap_or("");
    if url.is_empty() {
        bail!("config export failed: unexpected payload {meta}");
    }
    let body = client.get_bytes(device, url, None).await?;
    std::fs::write(output, &body)?;
    Ok(ConfigBackup {
        output: output.to_string(),
        size_bytes: body.len() as u64,
    })
}