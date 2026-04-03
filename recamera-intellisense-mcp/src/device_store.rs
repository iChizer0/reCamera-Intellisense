use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use tokio::fs;

use crate::types::{DeviceEntry, DeviceRecord};

pub struct DeviceStore {
    devices: HashMap<String, DeviceEntry>,
    file_path: PathBuf,
}

impl DeviceStore {
    pub async fn new() -> Result<Self> {
        let dir = dirs::home_dir()
            .context("Cannot determine home directory")?
            .join(".recamera");
        fs::create_dir_all(&dir).await?;
        let file_path = dir.join("devices.json");
        let devices = match fs::read_to_string(&file_path).await {
            Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
            Err(_) => HashMap::new(),
        };
        Ok(Self { devices, file_path })
    }

    async fn save(&self) -> Result<()> {
        let data = serde_json::to_string_pretty(&self.devices)?;
        fs::write(&self.file_path, data).await?;
        Ok(())
    }

    pub fn get_device(&self, name: &str) -> Option<DeviceRecord> {
        self.devices.get(name).map(|e| DeviceRecord {
            name: name.to_string(),
            host: e.host.clone(),
            token: e.token.clone(),
            protocol: e.protocol.clone(),
            allow_unsecured: e.allow_unsecured,
            port: e.port,
        })
    }

    pub fn list_devices(&self) -> Vec<DeviceRecord> {
        let mut devices: Vec<_> = self
            .devices
            .iter()
            .map(|(name, e)| DeviceRecord {
                name: name.clone(),
                host: e.host.clone(),
                token: e.token.clone(),
                protocol: e.protocol.clone(),
                allow_unsecured: e.allow_unsecured,
                port: e.port,
            })
            .collect();
        devices.sort_by(|a, b| a.name.cmp(&b.name));
        devices
    }

    pub async fn add_device(
        &mut self,
        name: &str,
        host: &str,
        token: &str,
        protocol: &str,
        allow_unsecured: bool,
        port: Option<u16>,
    ) -> Result<()> {
        if self.devices.contains_key(name) {
            bail!("Device '{name}' already exists. Use update_device to modify it, or remove_device first.");
        }
        self.devices.insert(
            name.to_string(),
            DeviceEntry {
                host: host.to_string(),
                token: token.to_string(),
                protocol: protocol.to_string(),
                allow_unsecured,
                port,
            },
        );
        self.save().await?;
        Ok(())
    }

    /// Replace all fields of an existing device atomically (used after connection testing).
    pub async fn replace_device(
        &mut self,
        name: &str,
        host: &str,
        token: &str,
        protocol: &str,
        allow_unsecured: bool,
        port: Option<u16>,
    ) -> Result<()> {
        let entry = self
            .devices
            .get_mut(name)
            .context(format!("Device '{name}' was removed during update."))?;
        entry.host = host.to_string();
        entry.token = token.to_string();
        entry.protocol = protocol.to_string();
        entry.allow_unsecured = allow_unsecured;
        entry.port = port;
        self.save().await?;
        Ok(())
    }

    pub async fn remove_device(&mut self, name: &str) -> bool {
        let removed = self.devices.remove(name).is_some();
        if removed {
            let _ = self.save().await;
        }
        removed
    }

    pub fn resolve_device(&self, device_name: &str) -> Result<DeviceRecord> {
        self.get_device(device_name).context(format!(
            "Device '{device_name}' not found. Use add_device to register it first."
        ))
    }
}
