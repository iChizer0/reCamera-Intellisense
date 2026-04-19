//! High-level storage orchestration. HTTP details live in `api::storage`.

use anyhow::{bail, Result};
use std::time::{Duration, Instant};

use crate::api::storage as api_storage;
use crate::api_client::ApiClient;
use crate::types::DeviceRecord;

// MARK: Constants

const DEFAULT_INTERNAL_DEV_PATH: &str = "/dev/mmcblk0p8";
const DEFAULT_READY_TIMEOUT: Duration = Duration::from_secs(3);
const POLL_INTERVAL: Duration = Duration::from_millis(500);

// MARK: Public entry point

/// Make sure at least one storage slot is enabled and configured with quota rotation,
/// so capture/rule writes do not fail on a full disk. Prefers the built-in eMMC slot
/// when nothing is enabled yet.
pub async fn ensure_storage(client: &ApiClient, device: &DeviceRecord) -> Result<()> {
    let slots = api_storage::get_status(client, device).await?;

    if !slots.iter().any(|s| s.enabled) {
        let default = slots
            .iter()
            .find(|s| s.dev_path == DEFAULT_INTERNAL_DEV_PATH)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "default storage '{}' not found; call set_storage_slot to pick one",
                    DEFAULT_INTERNAL_DEV_PATH
                )
            })?
            .clone();
        api_storage::set_selection(client, device, &default.dev_path, "").await?;
        wait_until_ready(client, device, &default.dev_path, DEFAULT_READY_TIMEOUT).await?;
        api_storage::control_config(client, device, &default.dev_path, -1, true).await?;
        return Ok(());
    }

    for slot in slots.iter().filter(|s| s.enabled && !s.quota_rotate) {
        api_storage::control_config(client, device, &slot.dev_path, slot.quota_limit_bytes, true)
            .await?;
    }
    Ok(())
}

// MARK: Helpers

async fn wait_until_ready(
    client: &ApiClient,
    device: &DeviceRecord,
    dev_path: &str,
    timeout: Duration,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let slots = api_storage::get_status(client, device).await?;
        if let Some(s) = slots.iter().find(|s| s.dev_path == dev_path) {
            if s.enabled && s.is_configured() {
                return Ok(());
            }
        }
        if Instant::now() >= deadline {
            bail!(
                "timeout waiting for slot '{}' to become enabled + configured",
                dev_path
            );
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
