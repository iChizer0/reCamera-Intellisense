use anyhow::{bail, Result};
use serde_json::json;

use crate::api_client::ApiClient;
use crate::types::{DeviceRecord, StorageInfo};

const STORAGE_DEV_PATH_DEFAULT: &str = "/dev/mmcblk0p8";

pub async fn get_storage_list(
    client: &ApiClient,
    device: &DeviceRecord,
) -> Result<Vec<StorageInfo>> {
    let status = client
        .get_json(device, "/cgi-bin/entry.cgi/record/storage/status", None)
        .await?;
    let data_dir = status
        .get("sDataDirName")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let storages = status
        .get("lSlots")
        .and_then(|v| v.as_array())
        .map(|slots| {
            slots
                .iter()
                .map(|s| StorageInfo {
                    dev_path: s
                        .get("sDevPath")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    uuid: s
                        .get("sUUID")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    // 1: ERROR, 2: NOT_FORMATTED_OR_FORMAT_UNSUPPORTED, 3: FORMATTING, 4: NOT_MOUNTED, 5: MOUNTED, 6: CONFIGURED, 7: INDEXING, 8: READY
                    is_configured: s.get("eState").and_then(|v| v.as_i64()).unwrap_or(0) >= 6,
                    is_enabled: s.get("bEnabled").and_then(|v| v.as_bool()).unwrap_or(false),
                    is_internal: s
                        .get("bInternal")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                    quota_rotate: s
                        .get("bQuotaRotate")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                    quota_limit_bytes: s.get("iQuotaLimitBytes").and_then(|v| v.as_i64()),
                    mount_path: s
                        .get("sMountPath")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    data_dir: data_dir.clone().unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(storages)
}

pub async fn set_storage_default(
    client: &ApiClient,
    device: &DeviceRecord,
    storage_info: &StorageInfo,
) -> Result<()> {
    // If the storage is not internal, select it by UUID, otherwise select by dev path
    let payload = if storage_info.is_internal {
        json!({
            "dSelectSlotToEnable": {"sByDevPath": storage_info.dev_path, "sByUUID": ""}
        })
    } else {
        json!({
            "dSelectSlotToEnable": {"sByDevPath": "", "sByUUID": storage_info.uuid}
        })
    };
    client
        .post_json(
            device,
            "/cgi-bin/entry.cgi/record/storage/config",
            None,
            Some(&payload),
        )
        .await?;
    Ok(())
}

pub async fn set_storage_quota(
    client: &ApiClient,
    device: &DeviceRecord,
    storage_info: &StorageInfo,
    quota_limit_bytes: i64,
    quota_rotate: bool,
    timeout_seconds: Option<u64>,
) -> Result<()> {
    // Wait until the storage is enabled and configured before setting quota, otherwise the API will return error
    let start_time = std::time::Instant::now();
    loop {
        let storages = get_storage_list(client, device).await?;
        if let Some(s) = storages
            .iter()
            .find(|s| s.dev_path == storage_info.dev_path)
        {
            if s.is_enabled && s.is_configured {
                break;
            }
        }
        if let Some(timeout) = timeout_seconds {
            if start_time.elapsed().as_secs() >= timeout {
                bail!("Timeout waiting for storage to be enabled and configured");
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    let payload = json!({
        "sAction": "CONFIG",
        "sSlotDevPath": storage_info.dev_path,
        "dSlotConfig": {
            "iQuotaLimitBytes": quota_limit_bytes,
            "bQuotaRotate": quota_rotate,
        },
    });
    client
        .post_json(
            device,
            "/cgi-bin/entry.cgi/record/storage/control",
            None,
            Some(&payload),
        )
        .await?;
    Ok(())
}

pub async fn ensure_storage(client: &ApiClient, device: &DeviceRecord) -> Result<()> {
    let storages = get_storage_list(client, device).await?;
    // Check if there's already an enabled storage, if not, enable the default storage
    if !storages.iter().any(|s| s.is_enabled) {
        let default_storage = storages
            .iter()
            .find(|s| s.dev_path == STORAGE_DEV_PATH_DEFAULT)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Default storage with dev path '{}' not found",
                    STORAGE_DEV_PATH_DEFAULT
                )
            })?;
        set_storage_default(client, device, default_storage).await?;
        set_storage_quota(client, device, default_storage, -1, true, Some(3)).await?;
    } else {
        // If there's already an enabled storage, enable quota rotate if it's not enabled
        for storage in storages.iter().filter(|s| s.is_enabled) {
            if !storage.quota_rotate {
                set_storage_quota(
                    client,
                    device,
                    storage,
                    storage.quota_limit_bytes.unwrap_or(-1),
                    true,
                    Some(3),
                )
                .await?;
            }
        }
    }
    Ok(())
}

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
