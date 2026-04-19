//! High-level record-file browsing via the Record HTTP relay + nginx autoindex.
//! Maintains a per-process relay-UUID cache so the agent does not have to juggle
//! the open/close lifecycle explicitly.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::Mutex;

use crate::api::{relay as api_relay, storage as api_storage};
use crate::api_client::ApiClient;
use crate::types::{DeviceRecord, DirEntry, StorageSlot};

// MARK: Relay UUID cache

#[derive(Default, Clone)]
pub struct RelayCache(Arc<Mutex<HashMap<(String, String), String>>>);

impl RelayCache {
    pub fn new() -> Self {
        Self::default()
    }

    async fn get(&self, device: &str, dev_path: &str) -> Option<String> {
        self.0
            .lock()
            .await
            .get(&(device.to_string(), dev_path.to_string()))
            .cloned()
    }

    async fn set(&self, device: &str, dev_path: &str, uuid: String) {
        self.0
            .lock()
            .await
            .insert((device.to_string(), dev_path.to_string()), uuid);
    }

    async fn drop_entry(&self, device: &str, dev_path: &str) {
        self.0
            .lock()
            .await
            .remove(&(device.to_string(), dev_path.to_string()));
    }
}

// MARK: Ensure a live relay for the target slot

async fn ensure_relay(
    cache: &RelayCache,
    client: &ApiClient,
    device: &DeviceRecord,
    slot: &StorageSlot,
) -> Result<String> {
    if let Some(uuid) = cache.get(&device.name, &slot.dev_path).await {
        match api_storage::control_relay_status(client, device, &slot.dev_path).await {
            Ok(status) if !status.uuid.is_empty() && status.timeout_remain > 0 => {
                return Ok(uuid);
            }
            _ => {
                cache.drop_entry(&device.name, &slot.dev_path).await;
            }
        }
    }
    let status = api_storage::control_relay_open(client, device, &slot.dev_path).await?;
    cache
        .set(&device.name, &slot.dev_path, status.uuid.clone())
        .await;
    Ok(status.uuid)
}

// MARK: Public API

pub async fn list_records(
    cache: &RelayCache,
    client: &ApiClient,
    device: &DeviceRecord,
    dev_path: Option<&str>,
    rel_path: &str,
) -> Result<Vec<DirEntry>> {
    let slot = api_relay::resolve_slot(client, device, dev_path).await?;
    let uuid = ensure_relay(cache, client, device, &slot).await?;
    let path = rel_path.trim_matches('/').to_string();
    api_relay::list_dir(client, device, &uuid, &path).await
}

pub async fn fetch_record(
    cache: &RelayCache,
    client: &ApiClient,
    device: &DeviceRecord,
    dev_path: Option<&str>,
    rel_path: &str,
) -> Result<(Vec<u8>, String)> {
    let slot = api_relay::resolve_slot(client, device, dev_path).await?;
    let uuid = ensure_relay(cache, client, device, &slot).await?;
    let path = rel_path.trim_matches('/').to_string();
    let bytes = api_relay::fetch(client, device, &uuid, &path).await?;
    let url = api_relay::build_url(device, &uuid, &path);
    Ok((bytes, url))
}

/// Build the direct relay URL without downloading bytes.
pub async fn fetch_record_url(
    cache: &RelayCache,
    client: &ApiClient,
    device: &DeviceRecord,
    dev_path: Option<&str>,
    rel_path: &str,
) -> Result<String> {
    let slot = api_relay::resolve_slot(client, device, dev_path).await?;
    let uuid = ensure_relay(cache, client, device, &slot).await?;
    let path = rel_path.trim_matches('/').to_string();
    Ok(api_relay::build_url(device, &uuid, &path))
}
