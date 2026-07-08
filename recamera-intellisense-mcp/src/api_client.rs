use std::net::Ipv6Addr;
use std::time::Duration;

use anyhow::{bail, Result};
use reqwest::Client;
use serde_json::Value;

use crate::types::{DetectedDevice, DeviceRecord};

const CONNECTION_TIMEOUT: Duration = Duration::from_secs(10);
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

pub struct ApiClient {
    secure_client: Client,
    insecure_client: Client,
}

impl ApiClient {
    pub fn new() -> Self {
        let secure_client = Client::builder()
            .timeout(CONNECTION_TIMEOUT)
            .build()
            .expect("Failed to build secure HTTP client");
        let insecure_client = Client::builder()
            .timeout(CONNECTION_TIMEOUT)
            .danger_accept_invalid_certs(true)
            .build()
            .expect("Failed to build insecure HTTP client");
        Self {
            secure_client,
            insecure_client,
        }
    }

    fn with_auth(req: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
        if token.is_empty() {
            req
        } else {
            req.header("Authorization", token)
        }
    }

    fn client_for(&self, device: &DeviceRecord) -> &Client {
        if device.protocol == "https" && device.allow_unsecured {
            &self.insecure_client
        } else {
            &self.secure_client
        }
    }

    fn format_host(host: &str) -> String {
        if host.parse::<Ipv6Addr>().is_ok() {
            format!("[{host}]")
        } else {
            host.to_string()
        }
    }

    pub fn api_url(device: &DeviceRecord, endpoint: &str) -> String {
        let host = Self::format_host(&device.host);
        match device.port {
            Some(port) => format!("{}://{}:{}{}", device.protocol, host, port, endpoint),
            None => format!("{}://{}{}", device.protocol, host, endpoint),
        }
    }

    pub async fn get_json(
        &self,
        device: &DeviceRecord,
        endpoint: &str,
        params: Option<&[(&str, &str)]>,
    ) -> Result<Value> {
        let url = Self::api_url(device, endpoint);
        let client = self.client_for(device);
        let mut req = Self::with_auth(client.get(&url), &device.token);
        if let Some(params) = params {
            req = req.query(params);
        }
        let resp = req.send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("HTTP {status}: {body}");
        }
        Ok(resp.json().await?)
    }

    pub async fn post_json(
        &self,
        device: &DeviceRecord,
        endpoint: &str,
        params: Option<&[(&str, &str)]>,
        payload: Option<&Value>,
    ) -> Result<Value> {
        let url = Self::api_url(device, endpoint);
        let client = self.client_for(device);
        let mut req = Self::with_auth(client.post(&url), &device.token);
        if let Some(params) = params {
            req = req.query(params);
        }
        if let Some(payload) = payload {
            req = req.json(payload);
        }
        let resp = req.send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("HTTP {status}: {body}");
        }
        Ok(resp.json().await?)
    }

    pub async fn post_text(&self, device: &DeviceRecord, endpoint: &str, body: &str) -> Result<()> {
        let url = Self::api_url(device, endpoint);
        let client = self.client_for(device);
        let resp = Self::with_auth(client.post(&url), &device.token)
            .header("Content-Type", "text/plain")
            .body(body.to_string())
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let resp_body = resp.text().await.unwrap_or_default();
            bail!("HTTP {status}: {resp_body}");
        }
        Ok(())
    }

    pub async fn get_bytes(
        &self,
        device: &DeviceRecord,
        endpoint: &str,
        params: Option<&[(&str, &str)]>,
    ) -> Result<Vec<u8>> {
        let url = Self::api_url(device, endpoint);
        let client = self.client_for(device);
        let mut req = Self::with_auth(client.get(&url), &device.token);
        if let Some(params) = params {
            req = req.query(params);
        }
        let resp = req.send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let err_msg = resp
                .headers()
                .get("x-error")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());
            let body = resp.text().await.unwrap_or_default();
            let detail = err_msg.filter(|s| !s.is_empty()).unwrap_or(body);
            bail!("Failed to fetch file: HTTP {status}: {detail}");
        }
        Ok(resp.bytes().await?.to_vec())
    }

    pub async fn delete(
        &self,
        device: &DeviceRecord,
        endpoint: &str,
        params: Option<&[(&str, &str)]>,
    ) -> Result<()> {
        let url = Self::api_url(device, endpoint);
        let client = self.client_for(device);
        let mut req = Self::with_auth(client.delete(&url), &device.token);
        if let Some(params) = params {
            req = req.query(params);
        }
        let resp = req.send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let err_msg = resp
                .headers()
                .get("x-error")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());
            let body = resp.text().await.unwrap_or_default();
            let detail = err_msg.filter(|s| !s.is_empty()).unwrap_or(body);
            bail!("HTTP {status}: {detail}");
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn probe_generate_204(
        &self,
        host: &str,
        port: u16,
        protocol: &str,
        allow_unsecured: bool,
        token: &str,
        auth_error_is_reachable: bool,
        timeout: Duration,
    ) -> Result<()> {
        let host = Self::format_host(host);
        let url = format!("{protocol}://{host}:{port}/api/v1/recamera-generate-204");
        let client = if protocol == "https" && allow_unsecured {
            &self.insecure_client
        } else {
            &self.secure_client
        };
        let resp = Self::with_auth(client.get(&url).timeout(timeout), token)
            .send()
            .await?;
        let status = resp.status().as_u16();
        if status == 401 || status == 403 {
            if auth_error_is_reachable {
                return Ok(());
            }
            bail!("Authentication failed (HTTP {status}). Verify the token.");
        }
        if !resp.status().is_success() {
            bail!("Unexpected response (HTTP {status}). Verify host and device service.");
        }
        Ok(())
    }

    pub async fn test_connection(
        &self,
        host: &str,
        token: &str,
        protocol: &str,
        allow_unsecured: bool,
        port: Option<u16>,
    ) -> Result<()> {
        let port = port.unwrap_or_else(|| if protocol == "https" { 443 } else { 80 });
        self.probe_generate_204(
            host,
            port,
            protocol,
            allow_unsecured,
            token,
            false,
            CONNECTION_TIMEOUT,
        )
        .await?;
        Ok(())
    }

    pub async fn detect_device(
        &self,
        host: &str,
        port: Option<u16>,
        token: &str,
    ) -> Result<Option<DetectedDevice>> {
        let probes = [
            ("https", port.unwrap_or(443), false),
            ("https", port.unwrap_or(443), true),
            ("http", port.unwrap_or(80), false),
        ];
        for (protocol, probe_port, allow_unsecured) in probes {
            if (self
                .probe_generate_204(
                    host,
                    probe_port,
                    protocol,
                    allow_unsecured,
                    token,
                    true,
                    PROBE_TIMEOUT,
                )
                .await)
                .is_ok()
            {
                return Ok(Some(DetectedDevice {
                    host: host.to_string(),
                    port: probe_port,
                    protocol: protocol.to_string(),
                    allow_unsecured,
                }));
            }
        }
        Ok(None)
    }
}
