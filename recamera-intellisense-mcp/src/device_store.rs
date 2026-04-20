use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use tokio::fs;

use crate::types::{DeviceEntry, DeviceRecord};

/// Unix file mode for the credential store: owner read/write only.
#[cfg(unix)]
const CRED_STORE_MODE: u32 = 0o600;

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
        // Best-effort: tighten permissions on any pre-existing credential store
        // created by older versions that did not enforce 0600.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if fs::try_exists(&file_path).await.unwrap_or(false) {
                let perms = std::fs::Permissions::from_mode(CRED_STORE_MODE);
                let _ = fs::set_permissions(&file_path, perms).await;
            }
        }
        Ok(Self { devices, file_path })
    }

    async fn save(&self) -> Result<()> {
        let data = serde_json::to_string_pretty(&self.devices)?;
        let dir = self
            .file_path
            .parent()
            .context("credential store has no parent directory")?;
        // Atomic replace: write to a tempfile in the same directory, fsync-able via
        // rename. Permissions are tightened on the temp file *before* it becomes
        // visible under the final path, so there is no window during which the
        // credentials are world-readable.
        let tmp_path = {
            let mut p = self.file_path.clone();
            let mut name = p.file_name().map(|n| n.to_os_string()).unwrap_or_default();
            name.push(".tmp");
            p.set_file_name(name);
            p
        };
        fs::create_dir_all(dir).await?;
        fs::write(&tmp_path, data.as_bytes()).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(CRED_STORE_MODE);
            if let Err(e) = fs::set_permissions(&tmp_path, perms).await {
                let _ = fs::remove_file(&tmp_path).await;
                return Err(e).context("set 0600 on credential store tempfile");
            }
        }
        if let Err(e) = fs::rename(&tmp_path, &self.file_path).await {
            let _ = fs::remove_file(&tmp_path).await;
            return Err(e).context("atomic rename of credential store");
        }
        // Best-effort: re-apply 0600 to the final path in case a pre-existing file
        // was replaced and inherited different modes on some filesystems.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(CRED_STORE_MODE);
            let _ = fs::set_permissions(&self.file_path, perms).await;
        }
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
