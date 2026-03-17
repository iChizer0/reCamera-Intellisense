use reqwest::Client;
use serde_json::Value;
use std::time::Duration;

use crate::devices::DeviceConfig;

/// HTTP client for communicating with a reCamera device API.
///
/// Wraps reqwest::Client with device-specific configuration (base URL, auth token,
/// TLS settings). Designed to be extensible for future hardware generations by
/// supporting configurable endpoints.
pub struct DeviceClient {
    config: DeviceConfig,
    http: Client,
}

impl DeviceClient {
    pub fn new(config: DeviceConfig) -> Self {
        let mut builder = Client::builder().timeout(Duration::from_secs(30));
        if config.tls_allow_insecure {
            builder = builder.danger_accept_invalid_certs(true);
        }
        let http = builder.build().expect("Failed to create HTTP client");
        Self { config, http }
    }

    fn full_url(&self, path: &str) -> String {
        format!("{}{}", self.config.base_url(), path)
    }

    fn auth_header(&self) -> &str {
        &self.config.token
    }

    /// Send a GET request and parse the response as JSON.
    pub async fn get_json(
        &self,
        path: &str,
        params: &[(&str, &str)],
    ) -> Result<Value, String> {
        let url = self.full_url(path);
        let resp = self
            .http
            .get(&url)
            .header("Authorization", self.auth_header())
            .query(params)
            .send()
            .await
            .map_err(|e| format!("HTTP request failed: {e}"))?;

        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| format!("Failed to read response: {e}"))?;

        if !status.is_success() {
            return Err(format!("HTTP {}: {}", status.as_u16(), body));
        }

        serde_json::from_str(&body).map_err(|e| format!("Invalid JSON response: {e}"))
    }

    /// Send a POST request with optional JSON payload and parse the response as JSON.
    pub async fn post_json(
        &self,
        path: &str,
        params: &[(&str, &str)],
        payload: Option<&Value>,
    ) -> Result<Value, String> {
        let url = self.full_url(path);
        let mut req = self
            .http
            .post(&url)
            .header("Authorization", self.auth_header())
            .query(params);

        if let Some(body) = payload {
            req = req.json(body);
        }

        let resp = req
            .send()
            .await
            .map_err(|e| format!("HTTP request failed: {e}"))?;

        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| format!("Failed to read response: {e}"))?;

        if !status.is_success() {
            return Err(format!("HTTP {}: {}", status.as_u16(), body));
        }

        serde_json::from_str(&body).map_err(|e| format!("Invalid JSON response: {e}"))
    }

    /// Send a POST request with a plain text body.
    pub async fn post_text(&self, path: &str, body: &str) -> Result<String, String> {
        let url = self.full_url(path);
        let resp = self
            .http
            .post(&url)
            .header("Authorization", self.auth_header())
            .header("Content-Type", "text/plain")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| format!("HTTP request failed: {e}"))?;

        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| format!("Failed to read response: {e}"))?;

        if !status.is_success() {
            return Err(format!("HTTP {}: {}", status.as_u16(), text));
        }

        Ok(text)
    }

    /// Send a GET request and return raw bytes with content type.
    pub async fn get_bytes(
        &self,
        path: &str,
        params: &[(&str, &str)],
    ) -> Result<(Vec<u8>, String), String> {
        let url = self.full_url(path);
        let resp = self
            .http
            .get(&url)
            .header("Authorization", self.auth_header())
            .query(params)
            .send()
            .await
            .map_err(|e| format!("HTTP request failed: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("HTTP {}: {}", status.as_u16(), body));
        }

        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_string();

        let bytes = resp
            .bytes()
            .await
            .map_err(|e| format!("Failed to read response bytes: {e}"))?;

        Ok((bytes.to_vec(), content_type))
    }
}
