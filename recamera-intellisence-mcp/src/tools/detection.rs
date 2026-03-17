use rmcp::{ErrorData as McpError, model::*};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::devices::client::DeviceClient;

const STORAGE_DEV_PATH_DEFAULT: &str = "/dev/mmcblk0p8";

fn err(msg: impl Into<String>) -> McpError {
    McpError::internal_error(msg.into(), None)
}

fn json_text_result(value: &Value) -> CallToolResult {
    CallToolResult::success(vec![Content::text(
        serde_json::to_string_pretty(value).unwrap(),
    )])
}

fn text_result(text: &str) -> CallToolResult {
    CallToolResult::success(vec![Content::text(text.to_string())])
}

/// Parse a raw model entry from the /model/list API into a clean model object.
fn parse_model(index: usize, data: &Value) -> Value {
    let name = data
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let labels: Vec<String> = data
        .get("modelInfo")
        .and_then(|v| v.get("classes"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(|l| l.as_str().unwrap_or("").to_string())
                .collect()
        })
        .unwrap_or_default();

    json!({
        "id": index,
        "name": name,
        "labels": labels,
    })
}

pub async fn get_detection_models(
    client: &Arc<DeviceClient>,
) -> Result<CallToolResult, McpError> {
    let data = client
        .get_json("/cgi-bin/entry.cgi/model/list", &[])
        .await
        .map_err(|e| err(e))?;

    let models: Vec<Value> = match data.as_array() {
        Some(arr) => arr
            .iter()
            .enumerate()
            .map(|(i, m)| parse_model(i, m))
            .collect(),
        None => return Err(err("Invalid response format: expected a list of models")),
    };

    Ok(json_text_result(&json!(models)))
}

pub async fn get_detection_model(
    client: &Arc<DeviceClient>,
) -> Result<CallToolResult, McpError> {
    let data = client
        .get_json("/cgi-bin/entry.cgi/model/inference", &[])
        .await
        .map_err(|e| err(e))?;

    if !data.is_object() || data.get("iEnable").and_then(|v| v.as_i64()) == Some(0) {
        return Ok(text_result("null (detection disabled)"));
    }

    let active_name = data
        .get("sModel")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    // Get full model info to find the matching model
    let models_data = client
        .get_json("/cgi-bin/entry.cgi/model/list", &[])
        .await
        .map_err(|e| err(e))?;

    if let Some(arr) = models_data.as_array() {
        for (i, m) in arr.iter().enumerate() {
            let name = m.get("model").and_then(|v| v.as_str()).unwrap_or("");
            if name == active_name {
                return Ok(json_text_result(&parse_model(i, m)));
            }
        }
    }

    Ok(text_result(&format!(
        "Active model '{}' not found in model list",
        active_name
    )))
}

pub async fn set_detection_model(
    client: &Arc<DeviceClient>,
    model_id: Option<i64>,
    model_name: Option<String>,
) -> Result<CallToolResult, McpError> {
    if model_id.is_none() && model_name.is_none() {
        return Err(err("Provide either 'model_id' or 'model_name'"));
    }
    if model_id.is_some() && model_name.is_some() {
        return Err(err("Provide only one of 'model_id' or 'model_name'"));
    }

    // Get models list to find the target
    let models_data = client
        .get_json("/cgi-bin/entry.cgi/model/list", &[])
        .await
        .map_err(|e| err(e))?;

    let models = models_data
        .as_array()
        .ok_or_else(|| err("Invalid response: expected a list of models"))?;

    let target = if let Some(id) = model_id {
        models
            .get(id as usize)
            .ok_or_else(|| err(format!("Model ID '{}' not found on device", id)))?
    } else {
        let name = model_name.as_deref().unwrap();
        models
            .iter()
            .find(|m| m.get("model").and_then(|v| v.as_str()) == Some(name))
            .ok_or_else(|| err(format!("Model name '{}' not found on device", name)))?
    };

    let target_name = target
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let target_id = models
        .iter()
        .position(|m| m.get("model").and_then(|v| v.as_str()) == Some(target_name))
        .unwrap_or(0);

    let payload = json!({
        "iEnable": 1,
        "iFPS": 30,
        "sModel": target_name,
    });

    let result = client
        .post_json(
            "/cgi-bin/entry.cgi/model/inference",
            &[("id", &target_id.to_string())],
            Some(&payload),
        )
        .await
        .map_err(|e| err(e))?;

    if result.get("code").and_then(|c| c.as_i64()) != Some(0) {
        let msg = result
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("Unknown error");
        return Err(err(format!("Failed to set detection model: {msg}")));
    }

    Ok(text_result(&format!(
        "Detection model set to '{}' (id: {})",
        target_name, target_id
    )))
}

pub async fn get_detection_schedule(
    client: &Arc<DeviceClient>,
) -> Result<CallToolResult, McpError> {
    let data = client
        .get_json(
            "/cgi-bin/entry.cgi/record/record/rule/schedule-rule-config",
            &[],
        )
        .await
        .map_err(|e| err(e))?;

    if !data.is_object() || data.get("bEnable").and_then(|v| v.as_bool()) != Some(true) {
        return Ok(text_result(
            "null (schedule disabled - detection active all the time)",
        ));
    }

    let weekdays: Vec<Value> = data
        .get("lActiveWeekdays")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(|rng| {
                    json!([
                        rng.get("sStart").and_then(|v| v.as_str()).unwrap_or(""),
                        rng.get("sEnd").and_then(|v| v.as_str()).unwrap_or(""),
                    ])
                })
                .collect()
        })
        .unwrap_or_default();

    if weekdays.is_empty() {
        return Ok(text_result("null (no schedule ranges configured)"));
    }

    Ok(json_text_result(&json!({
        "active_weekdays": weekdays,
    })))
}

pub async fn set_detection_schedule(
    client: &Arc<DeviceClient>,
    schedule: Option<Value>,
) -> Result<CallToolResult, McpError> {
    let is_null = schedule.is_none() || schedule.as_ref() == Some(&Value::Null);

    let mut active_weekdays = Vec::new();
    if !is_null {
        if let Some(sched) = schedule.as_ref().and_then(|s| s.as_object()) {
            if let Some(weekdays) = sched.get("active_weekdays").and_then(|w| w.as_array()) {
                for pair in weekdays {
                    let arr = pair
                        .as_array()
                        .ok_or_else(|| err("Each weekday entry must be [start, end]"))?;
                    if arr.len() != 2 {
                        return Err(err("Each weekday entry must be [start, end]"));
                    }
                    let start = arr[0]
                        .as_str()
                        .ok_or_else(|| err("Schedule start time must be a string"))?;
                    let end = arr[1]
                        .as_str()
                        .ok_or_else(|| err("Schedule end time must be a string"))?;
                    active_weekdays.push(json!({"sStart": start, "sEnd": end}));
                }
            }
        }
    }

    let payload = json!({
        "bEnable": !is_null,
        "lActiveWeekdays": active_weekdays,
    });

    let result = client
        .post_json(
            "/cgi-bin/entry.cgi/record/record/rule/schedule-rule-config",
            &[],
            Some(&payload),
        )
        .await
        .map_err(|e| err(e))?;

    if result.get("code").and_then(|c| c.as_i64()) != Some(0) {
        let msg = result
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("Unknown error");
        return Err(err(format!("Failed to set detection schedule: {msg}")));
    }

    if is_null {
        Ok(text_result(
            "Detection schedule disabled (active all the time)",
        ))
    } else {
        Ok(text_result("Detection schedule updated successfully"))
    }
}

/// Check if record image is enabled on the device.
async fn check_record_image(client: &Arc<DeviceClient>) -> bool {
    let data = match client
        .get_json("/cgi-bin/entry.cgi/record/rule/config", &[])
        .await
    {
        Ok(d) => d,
        Err(_) => return false,
    };

    if !data.is_object() || data.get("bRuleEnabled").and_then(|v| v.as_bool()) != Some(true) {
        return false;
    }

    data.get("dWriterConfig")
        .and_then(|w| w.get("sFormat"))
        .and_then(|f| f.as_str())
        .map(|f| f.eq_ignore_ascii_case("JPG"))
        .unwrap_or(false)
}

/// Enable record image on the device.
async fn enable_record_image(client: &Arc<DeviceClient>) -> Result<(), McpError> {
    let payload = json!({
        "bRuleEnabled": true,
        "dWriterConfig": {
            "iIntervalMs": 0,
            "sFormat": "JPG",
        },
    });

    let result = client
        .post_json(
            "/cgi-bin/entry.cgi/record/rule/config",
            &[],
            Some(&payload),
        )
        .await
        .map_err(|e| err(e))?;

    if result.get("code").and_then(|c| c.as_i64()) != Some(0) {
        let msg = result
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("Unknown error");
        return Err(err(format!("Failed to enable record image: {msg}")));
    }

    Ok(())
}

/// Check if storage is enabled on the device.
async fn check_storage_enabled(client: &Arc<DeviceClient>) -> bool {
    let data = match client
        .get_json("/cgi-bin/entry.cgi/record/storage/status", &[])
        .await
    {
        Ok(d) => d,
        Err(_) => return false,
    };

    if !data.is_object() {
        return false;
    }

    data.get("lSlots")
        .and_then(|v| v.as_array())
        .map(|slots| {
            slots.iter().any(|slot| {
                slot.get("bEnabled").and_then(|v| v.as_bool()) == Some(true)
                    && slot.get("bQuotaRotate").and_then(|v| v.as_bool()) == Some(true)
            })
        })
        .unwrap_or(false)
}

/// Enable default storage on the device.
async fn enable_default_storage(client: &Arc<DeviceClient>) -> Result<(), McpError> {
    // Enable storage slot
    let payload = json!({
        "dSelectSlotToEnable": {"sByDevPath": STORAGE_DEV_PATH_DEFAULT, "sByUUID": ""}
    });

    let result = client
        .post_json(
            "/cgi-bin/entry.cgi/record/storage/config",
            &[],
            Some(&payload),
        )
        .await
        .map_err(|e| err(e))?;

    if result.get("code").and_then(|c| c.as_i64()) != Some(0) {
        let msg = result
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("Unknown error");
        return Err(err(format!("Failed to enable default storage: {msg}")));
    }

    // Configure storage quota
    let payload = json!({
        "sAction": "CONFIG",
        "sSlotDevPath": STORAGE_DEV_PATH_DEFAULT,
        "dSlotConfig": {
            "iQuotaLimitBytes": -1,
            "bQuotaRotate": true,
        },
    });

    let result = client
        .post_json(
            "/cgi-bin/entry.cgi/record/storage/control",
            &[],
            Some(&payload),
        )
        .await
        .map_err(|e| err(e))?;

    if result.get("code").and_then(|c| c.as_i64()) != Some(0) {
        let msg = result
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("Unknown error");
        return Err(err(format!("Failed to configure default storage: {msg}")));
    }

    Ok(())
}

pub async fn get_detection_rules(
    client: &Arc<DeviceClient>,
) -> Result<CallToolResult, McpError> {
    // Check prerequisites
    if !check_record_image(client).await {
        return Ok(json_text_result(&json!([])));
    }
    if !check_storage_enabled(client).await {
        return Ok(json_text_result(&json!([])));
    }

    let data = client
        .get_json(
            "/cgi-bin/entry.cgi/record/rule/record-rule-config",
            &[],
        )
        .await
        .map_err(|e| err(e))?;

    if !data.is_object()
        || data
            .get("sCurrentSelected")
            .and_then(|v| v.as_str())
            != Some("INFERENCE_SET")
    {
        return Ok(json_text_result(&json!([])));
    }

    let rules: Vec<Value> = data
        .get("lInferenceSet")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(|rule| {
                    json!({
                        "name": rule.get("sID").and_then(|v| v.as_str()).unwrap_or(""),
                        "debounce_times": rule.get("iDebounceTimes").and_then(|v| v.as_i64()).unwrap_or(0),
                        "confidence_range_filter": rule.get("lConfidenceFilter").unwrap_or(&json!([0.0, 1.0])),
                        "label_filter": rule.get("lClassFilter").unwrap_or(&json!([])),
                        "region_filter": rule.get("lRegionFilter").and_then(|v| v.as_array()).map(|regions| {
                            regions.iter().map(|r| r.get("lPolygon").cloned().unwrap_or(json!([]))).collect::<Vec<Value>>()
                        }).unwrap_or_default(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(json_text_result(&json!(rules)))
}

const FULL_REGION_POLYGON: &str = "[[0.0,0.0],[1.0,0.0],[1.0,1.0],[0.0,1.0]]";

pub async fn set_detection_rules(
    client: &Arc<DeviceClient>,
    rules: Vec<Value>,
) -> Result<CallToolResult, McpError> {
    // Ensure prerequisites are met
    if !check_record_image(client).await {
        enable_record_image(client).await?;
    }
    if !check_storage_enabled(client).await {
        enable_default_storage(client).await?;
    }

    let default_region: Value = serde_json::from_str(FULL_REGION_POLYGON).unwrap();

    let inference_set: Vec<Value> = rules
        .iter()
        .map(|rule| {
            let name = rule
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("default");
            let debounce = rule
                .get("debounce_times")
                .and_then(|v| v.as_i64())
                .unwrap_or(3);
            let confidence = rule
                .get("confidence_range_filter")
                .cloned()
                .unwrap_or(json!([0.25, 1.0]));
            let labels = rule
                .get("label_filter")
                .cloned()
                .unwrap_or(json!([]));
            let regions = rule.get("region_filter");
            let region_filter = if regions.is_none() || regions == Some(&Value::Null) {
                vec![json!({"lPolygon": &default_region})]
            } else {
                regions
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .map(|polygon| json!({"lPolygon": polygon}))
                            .collect()
                    })
                    .unwrap_or_else(|| vec![json!({"lPolygon": &default_region})])
            };

            json!({
                "sID": name,
                "iDebounceTimes": debounce,
                "lConfidenceFilter": confidence,
                "lClassFilter": labels,
                "lRegionFilter": region_filter,
            })
        })
        .collect();

    let payload = json!({
        "sCurrentSelected": "INFERENCE_SET",
        "lInferenceSet": inference_set,
    });

    let result = client
        .post_json(
            "/cgi-bin/entry.cgi/record/rule/record-rule-config",
            &[],
            Some(&payload),
        )
        .await
        .map_err(|e| err(e))?;

    if result.get("code").and_then(|c| c.as_i64()) != Some(0) {
        let msg = result
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("Unknown error");
        return Err(err(format!("Failed to set detection rules: {msg}")));
    }

    Ok(text_result(&format!(
        "Detection rules updated ({} rules set)",
        inference_set.len()
    )))
}
