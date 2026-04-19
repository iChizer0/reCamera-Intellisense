use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::api::expect_ok;
use crate::api_client::ApiClient;
use crate::types::{DetectionModel, DeviceRecord};

// MARK: Model inference CGI

pub async fn list_models(client: &ApiClient, device: &DeviceRecord) -> Result<Vec<DetectionModel>> {
    let raw = client
        .get_json(device, "/cgi-bin/entry.cgi/model/list", None)
        .await?;
    let arr = raw
        .as_array()
        .context("Expected array of models from /model/list")?;
    Ok(arr
        .iter()
        .enumerate()
        .map(|(i, m)| DetectionModel {
            id: i as i32,
            name: m
                .get("model")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            labels: m
                .get("modelInfo")
                .and_then(|v| v.get("classes"))
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|c| c.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default(),
        })
        .collect())
}

/// Currently active model (iEnable != 0), or None.
pub async fn get_active_model(
    client: &ApiClient,
    device: &DeviceRecord,
) -> Result<Option<DetectionModel>> {
    let info = client
        .get_json(device, "/cgi-bin/entry.cgi/model/inference", None)
        .await?;
    if !info.is_object() || info.get("iEnable").and_then(|v| v.as_i64()).unwrap_or(0) == 0 {
        return Ok(None);
    }
    let name = info
        .get("sModel")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let models = list_models(client, device).await?;
    Ok(models.into_iter().find(|m| m.name == name))
}

pub async fn set_active_model(
    client: &ApiClient,
    device: &DeviceRecord,
    model: &DetectionModel,
) -> Result<()> {
    let id = model.id.to_string();
    let params = [("id", id.as_str())];
    let payload = json!({
        "iEnable": 1,
        "iFPS": 30,
        "sModel": model.name,
    });
    let resp: Value = client
        .post_json(
            device,
            "/cgi-bin/entry.cgi/model/inference",
            Some(&params),
            Some(&payload),
        )
        .await?;
    expect_ok(&resp, "set model inference")
}
