//! High-level record-file browsing via the Record HTTP relay + nginx autoindex.
//! Maintains a per-process relay-UUID cache so the agent does not have to juggle
//! the open/close lifecycle explicitly.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::Mutex;

use crate::api::{relay as api_relay, storage as api_storage};
use crate::api_client::ApiClient;
use crate::types::{DeviceRecord, DirEntry, DirListing, StorageSlot};

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

pub const LIST_RECORDS_DEFAULT_LIMIT: usize = 100;
pub const LIST_RECORDS_MAX_LIMIT: usize = 500;

pub async fn list_records(
    cache: &RelayCache,
    client: &ApiClient,
    device: &DeviceRecord,
    dev_path: Option<&str>,
    rel_path: &str,
    limit: Option<usize>,
    offset: Option<usize>,
) -> Result<DirListing> {
    let slot = api_relay::resolve_slot(client, device, dev_path).await?;
    let uuid = ensure_relay(cache, client, device, &slot).await?;
    let path = rel_path.trim_matches('/').to_string();
    let entries = api_relay::list_dir(client, device, &uuid, &path).await?;
    Ok(paginate_entries(entries, limit, offset))
}

/// Deterministically sort `entries` (directories first, then by name) and apply
/// `offset` / `limit`, returning a `DirListing` with pagination metadata.
pub fn paginate_entries(
    mut entries: Vec<DirEntry>,
    limit: Option<usize>,
    offset: Option<usize>,
) -> DirListing {
    entries.sort_by(|a, b| match (a.is_dir, b.is_dir) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.cmp(&b.name),
    });
    let total = entries.len();
    let offset = offset.unwrap_or(0).min(total);
    let limit = limit
        .unwrap_or(LIST_RECORDS_DEFAULT_LIMIT)
        .clamp(1, LIST_RECORDS_MAX_LIMIT);
    let end = (offset + limit).min(total);
    let slice: Vec<DirEntry> = entries[offset..end].to_vec();
    let has_more = end < total;
    DirListing {
        entries: slice,
        offset,
        limit,
        total,
        has_more,
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, is_dir: bool) -> DirEntry {
        DirEntry {
            name: name.to_string(),
            is_dir,
            size: None,
            mtime: None,
        }
    }

    #[test]
    fn paginate_entries_sorts_dirs_first_then_name() {
        let entries = vec![
            entry("b.mp4", false),
            entry("zebra", true),
            entry("a.mp4", false),
            entry("alpha", true),
        ];
        let listing = paginate_entries(entries, None, None);
        let names: Vec<&str> = listing.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "zebra", "a.mp4", "b.mp4"]);
        assert_eq!(listing.total, 4);
        assert_eq!(listing.offset, 0);
        assert!(!listing.has_more);
    }

    #[test]
    fn paginate_entries_applies_offset_and_limit() {
        let entries: Vec<DirEntry> = (0..10).map(|i| entry(&format!("f{i:02}"), false)).collect();
        let listing = paginate_entries(entries.clone(), Some(3), Some(2));
        let names: Vec<&str> = listing.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["f02", "f03", "f04"]);
        assert_eq!(listing.total, 10);
        assert_eq!(listing.offset, 2);
        assert_eq!(listing.limit, 3);
        assert!(listing.has_more);
    }

    #[test]
    fn paginate_entries_clamps_limit_and_offset() {
        let entries: Vec<DirEntry> = (0..5).map(|i| entry(&format!("f{i}"), false)).collect();
        // offset beyond total is clamped; limit above MAX is clamped.
        let listing = paginate_entries(entries.clone(), Some(9999), Some(100));
        assert_eq!(listing.offset, 5);
        assert_eq!(listing.entries.len(), 0);
        assert_eq!(listing.limit, LIST_RECORDS_MAX_LIMIT);
        assert!(!listing.has_more);

        // limit of 0 is clamped to 1.
        let listing = paginate_entries(entries, Some(0), Some(0));
        assert_eq!(listing.limit, 1);
        assert_eq!(listing.entries.len(), 1);
        assert!(listing.has_more);
    }
}
