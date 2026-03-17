pub mod client;

use serde::{Deserialize, Serialize};

/// Device connection configuration for a reCamera device.
///
/// This struct is designed to be hardware-generation agnostic. Different reCamera
/// generations may use different ports or protocols, but the connection parameters
/// remain consistent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceConfig {
    /// Device host address (IP or hostname)
    pub host: String,
    /// Custom port (None = default: 80 for HTTP, 443 for HTTPS)
    pub port: Option<u16>,
    /// API authentication token
    pub token: String,
    /// Protocol: "http" or "https"
    pub protocol: String,
    /// Whether to accept self-signed TLS certificates
    pub tls_allow_insecure: bool,
}

impl DeviceConfig {
    pub fn base_url(&self) -> String {
        match self.port {
            Some(port) => format!("{}://{}:{}", self.protocol, self.host, port),
            None => format!("{}://{}", self.protocol, self.host),
        }
    }
}
