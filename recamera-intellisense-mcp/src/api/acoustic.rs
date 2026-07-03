use anyhow::Result;

use crate::api_client::ApiClient;
use crate::types::{AcousticModel, DeviceRecord};

const PATH_ACTIVE: &str = "/extension/acousticslab/api/v1/active";

pub async fn get_active_model(
    client: &ApiClient,
    device: &DeviceRecord,
) -> Result<Option<AcousticModel>> {
    let data = client.get_json(device, PATH_ACTIVE, None).await?;
    if !data.is_object() {
        return Ok(None);
    }
    let runtime_head_id = data
        .get("runtime_head_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let labels: Vec<String> = data
        .get("labels")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|c| c.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    if runtime_head_id.is_empty() && labels.is_empty() {
        return Ok(None);
    }
    Ok(Some(AcousticModel {
        runtime_head_id,
        labels,
        n_classes: data.get("n_classes").and_then(|v| v.as_i64()),
        sha256: data
            .get("sha256")
            .and_then(|v| v.as_str())
            .map(String::from),
        activated_at: data
            .get("activated_at")
            .and_then(|v| v.as_str())
            .map(String::from),
    }))
}
