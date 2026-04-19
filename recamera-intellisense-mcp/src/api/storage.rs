use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::api::expect_ok;
use crate::api_client::ApiClient;
use crate::types::{
    normalize_storage_action, DeviceRecord, RelayStatus, StorageSlot, StorageTaskHistory,
};

// MARK: Paths

const PATH_STATUS: &str = "/cgi-bin/entry.cgi/record/storage/status";
const PATH_CONFIG: &str = "/cgi-bin/entry.cgi/record/storage/config";
const PATH_CONTROL: &str = "/cgi-bin/entry.cgi/record/storage/control";

// MARK: Slot status + selection

pub async fn get_status(client: &ApiClient, device: &DeviceRecord) -> Result<Vec<StorageSlot>> {
    let data = client.get_json(device, PATH_STATUS, None).await?;
    let data_dir = data
        .get("sDataDirName")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Ok(data
        .get("lSlots")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().map(|s| parse_slot(s, &data_dir)).collect())
        .unwrap_or_default())
}

fn parse_slot(s: &Value, data_dir: &str) -> StorageSlot {
    let get_str = |k: &str| s.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let get_bool = |k: &str| s.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
    let get_i64 = |k: &str| s.get(k).and_then(|v| v.as_i64()).unwrap_or(0);
    let opt_str = |k: &str| s.get(k).and_then(|v| v.as_str()).map(String::from);
    StorageSlot {
        dev_path: get_str("sDevPath"),
        mount_path: get_str("sMountPath"),
        removable: get_bool("bRemovable"),
        internal: get_bool("bInternal"),
        label: opt_str("sLabel"),
        uuid: opt_str("sUUID"),
        fs_type: opt_str("sType"),
        selected: get_bool("bSelected"),
        enabled: get_bool("bEnabled"),
        syncing: get_bool("bSyncing"),
        writing: get_bool("bWriting"),
        rotating: get_bool("bRotating"),
        state_code: get_i64("eState"),
        state: get_str("sState"),
        size_bytes: get_i64("iStatsSizeBytes"),
        free_bytes: get_i64("iStatsFreeBytes"),
        quota_min_recommend_bytes: get_i64("iQuotaMinimumRecommendBytes"),
        quota_preserved_bytes: get_i64("iQuotaPreservedBytes"),
        quota_used_bytes: s.get("iQuotaUsedBytes").and_then(|v| v.as_i64()),
        quota_limit_bytes: get_i64("iQuotaLimitBytes"),
        quota_rotate: get_bool("bQuotaRotate"),
        data_dir: data_dir.to_string(),
    }
}

/// Select the slot to enable. Pass both selectors empty to disable all slots.
pub async fn set_selection(
    client: &ApiClient,
    device: &DeviceRecord,
    by_dev_path: &str,
    by_uuid: &str,
) -> Result<()> {
    let select = if by_dev_path.is_empty() && by_uuid.is_empty() {
        Value::Null
    } else {
        json!({ "sByDevPath": by_dev_path, "sByUUID": by_uuid })
    };
    let payload = json!({ "dSelectSlotToEnable": select });
    let resp = client
        .post_json(device, PATH_CONFIG, None, Some(&payload))
        .await?;
    expect_ok(&resp, "set storage selection")
}

// MARK: Sync control (CONFIG / RELAY / RELAY_STATUS / UNRELAY + sync variants)

pub async fn control_config(
    client: &ApiClient,
    device: &DeviceRecord,
    dev_path: &str,
    quota_limit_bytes: i64,
    quota_rotate: bool,
) -> Result<()> {
    let payload = json!({
        "sTaskType": "SYNC",
        "sAction": "CONFIG",
        "sSlotDevPath": dev_path,
        "dSlotConfig": {
            "iQuotaLimitBytes": quota_limit_bytes,
            "bQuotaRotate": quota_rotate,
        },
    });
    let resp = client
        .post_json(device, PATH_CONTROL, None, Some(&payload))
        .await?;
    expect_ok(&resp, "configure storage quota")
}

pub async fn control_relay_open(
    client: &ApiClient,
    device: &DeviceRecord,
    dev_path: &str,
) -> Result<RelayStatus> {
    let resp = relay_call(client, device, "RELAY", dev_path).await?;
    extract_relay_status(&resp)
}

pub async fn control_relay_status(
    client: &ApiClient,
    device: &DeviceRecord,
    dev_path: &str,
) -> Result<RelayStatus> {
    let resp = relay_call(client, device, "RELAY_STATUS", dev_path).await?;
    extract_relay_status(&resp)
}

pub async fn control_relay_close(
    client: &ApiClient,
    device: &DeviceRecord,
    dev_path: &str,
) -> Result<()> {
    let resp = relay_call(client, device, "UNRELAY", dev_path).await?;
    expect_ok(&resp, "close relay")
}

async fn relay_call(
    client: &ApiClient,
    device: &DeviceRecord,
    action: &str,
    dev_path: &str,
) -> Result<Value> {
    let payload = json!({
        "sTaskType": "SYNC",
        "sAction": action,
        "sSlotDevPath": dev_path,
    });
    let resp = client
        .post_json(device, PATH_CONTROL, None, Some(&payload))
        .await?;
    expect_ok(&resp, &format!("storage control {action}"))?;
    Ok(resp)
}

fn extract_relay_status(resp: &Value) -> Result<RelayStatus> {
    let r = resp
        .get("dRelayStatus")
        .context("missing dRelayStatus in response")?;
    Ok(RelayStatus {
        uuid: r
            .get("sRelayDirectory")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        timeout: r.get("iRelayTimeout").and_then(|v| v.as_i64()).unwrap_or(0),
        timeout_remain: r
            .get("iRelayTimeoutRemain")
            .and_then(|v| v.as_i64())
            .unwrap_or(0),
    })
}

// MARK: Long-running actions (FORMAT / FREE_UP / EJECT / REMOVE)

pub async fn control_sync(
    client: &ApiClient,
    device: &DeviceRecord,
    action: &str,
    dev_path: &str,
    files: &[String],
) -> Result<Value> {
    let action = normalize_storage_action(action)
        .with_context(|| format!("unknown storage action '{action}'"))?;
    let mut payload = json!({
        "sTaskType": "SYNC",
        "sAction": action,
        "sSlotDevPath": dev_path,
    });
    if action == "REMOVE_FILES_OR_DIRECTORIES" {
        if files.is_empty() {
            bail!("REMOVE_FILES_OR_DIRECTORIES requires a non-empty 'files' list");
        }
        payload["lFilesOrDirectoriesToRemove"] = json!(files);
    }
    let resp = client
        .post_json(device, PATH_CONTROL, None, Some(&payload))
        .await?;
    expect_ok(&resp, &format!("storage {action} (sync)"))?;
    Ok(resp)
}

pub async fn control_async_submit(
    client: &ApiClient,
    device: &DeviceRecord,
    action: &str,
    dev_path: &str,
    files: &[String],
) -> Result<StorageTaskHistory> {
    let action = normalize_storage_action(action)
        .with_context(|| format!("unknown storage action '{action}'"))?;
    let mut payload = json!({
        "sTaskType": "ASYNC_SUBMIT",
        "sAction": action,
        "sSlotDevPath": dev_path,
    });
    if action == "REMOVE_FILES_OR_DIRECTORIES" {
        if files.is_empty() {
            bail!("REMOVE_FILES_OR_DIRECTORIES requires a non-empty 'files' list");
        }
        payload["lFilesOrDirectoriesToRemove"] = json!(files);
    }
    let resp = client
        .post_json(device, PATH_CONTROL, None, Some(&payload))
        .await?;
    expect_ok(&resp, &format!("submit async {action}"))?;
    Ok(resp)
}

pub async fn control_async_status(
    client: &ApiClient,
    device: &DeviceRecord,
    action: &str,
    dev_path: &str,
) -> Result<StorageTaskHistory> {
    let action = normalize_storage_action(action)
        .with_context(|| format!("unknown storage action '{action}'"))?;
    let payload = json!({
        "sTaskType": "ASYNC_STATUS",
        "sAction": action,
        "sSlotDevPath": dev_path,
    });
    let resp = client
        .post_json(device, PATH_CONTROL, None, Some(&payload))
        .await?;
    expect_ok(&resp, &format!("query async {action} status"))?;
    Ok(resp)
}

pub async fn control_async_cancel(
    client: &ApiClient,
    device: &DeviceRecord,
    action: &str,
    dev_path: &str,
) -> Result<StorageTaskHistory> {
    let action = normalize_storage_action(action)
        .with_context(|| format!("unknown storage action '{action}'"))?;
    let payload = json!({
        "sTaskType": "ASYNC_CANCEL",
        "sAction": action,
        "sSlotDevPath": dev_path,
    });
    let resp = client
        .post_json(device, PATH_CONTROL, None, Some(&payload))
        .await?;
    expect_ok(&resp, &format!("cancel async {action}"))?;
    Ok(resp)
}
