use std::time::Duration;

use anyhow::{bail, Result};
use reqwest::Client;
use serde_json::Value;

use crate::types::DeviceRecord;

const CONNECTION_TIMEOUT: Duration = Duration::from_secs(10);

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

    fn client_for(&self, device: &DeviceRecord) -> &Client {
        if device.protocol == "https" && device.allow_unsecured {
            &self.insecure_client
        } else {
            &self.secure_client
        }
    }

    pub fn api_url(device: &DeviceRecord, endpoint: &str) -> String {
        match device.port {
            Some(port) => format!("{}://{}:{}{}", device.protocol, device.host, port, endpoint),
            None => format!("{}://{}{}", device.protocol, device.host, endpoint),
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
        let mut req = client.get(&url).header("Authorization", &device.token);
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
        let mut req = client.post(&url).header("Authorization", &device.token);
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
        let resp = client
            .post(&url)
            .header("Authorization", &device.token)
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
        let mut req = client.get(&url).header("Authorization", &device.token);
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
            let detail = err_msg
                .filter(|s| !s.is_empty())
                .unwrap_or(body);
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
        let mut req = client.delete(&url).header("Authorization", &device.token);
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
            let detail = err_msg
                .filter(|s| !s.is_empty())
                .unwrap_or(body);
            bail!("HTTP {status}: {detail}");
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
        let url = match port {
            Some(p) => format!("{protocol}://{host}:{p}/api/v1/recamera-generate-204"),
            None => format!("{protocol}://{host}/api/v1/recamera-generate-204"),
        };
        let client = if protocol == "https" && allow_unsecured {
            &self.insecure_client
        } else {
            &self.secure_client
        };
        let resp = client
            .get(&url)
            .header("Authorization", token)
            .send()
            .await?;
        let status = resp.status().as_u16();
        if status == 401 || status == 403 {
            bail!("Authentication failed (HTTP {status}). Verify the token.");
        }
        if !resp.status().is_success() {
            bail!("Unexpected response (HTTP {status}). Verify host and device service.");
        }
        Ok(())
    }

    pub async fn detect_local(socket_path: &str) -> bool {
        tokio::net::UnixStream::connect(socket_path)
            .await
            .is_ok()
    }
}
