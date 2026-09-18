use anyhow::Result;

use crate::api_client::ApiClient;
use crate::types::{AcousticModel, DeviceRecord};

const PATH_ACTIVE: &str = "/extension/acousticslab/api/v1/active";
const PATH_WORKSPACES: &str = "/extension/acousticslab/api/v1/workspaces";

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

/// All trained heads across workspaces; the active one is marked. Empty when
/// the AcousticsLab app is stopped.
pub async fn list_acoustic_models(
    client: &ApiClient,
    device: &DeviceRecord,
) -> Result<Vec<crate::types::AcousticHead>> {
    let ws_data = match client.get_json(device, PATH_WORKSPACES, None).await {
        Ok(v) => v,
        Err(_) => return Ok(vec![]), // AcousticsLab stopped / unreachable
    };
    let active_id = get_active_model(client, device)
        .await
        .ok()
        .flatten()
        .map(|m| m.runtime_head_id)
        .unwrap_or_default();
    let mut out = Vec::new();
    let workspaces = ws_data
        .get("workspaces")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    for ws in workspaces {
        let ws_id = ws.get("id").and_then(|v| v.as_str()).unwrap_or("");
        if ws_id.is_empty() {
            continue;
        }
        let heads_data = match client
            .get_json(device, &format!("{PATH_WORKSPACES}/{ws_id}/heads"), None)
            .await
        {
            Ok(v) => v,
            Err(_) => continue,
        };
        for head in heads_data
            .get("heads")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default()
        {
            let head_id = head.get("head_id").and_then(|v| v.as_str()).unwrap_or("");
            if head_id.is_empty() {
                continue;
            }
            out.push(crate::types::AcousticHead {
                workspace_id: ws_id.to_string(),
                workspace_name: ws
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                head_id: head_id.to_string(),
                n_classes: head.get("n_classes").and_then(|v| v.as_i64()),
                created_at: head
                    .get("created_at")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                status: head
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                active: head_id == active_id,
            });
        }
    }
    Ok(out)
}
