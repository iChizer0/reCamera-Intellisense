use rmcp::{ErrorData as McpError, model::*};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::devices::client::DeviceClient;

const GPIO_OUTPUT_STATE: &str = "push-pull";
const GPIO_INPUT_STATE: &str = "floating";
const GPIO_DEBOUNCE_MS_DEFAULT: i64 = 100;
const GPIO_OUTPUT_STATES: &[&str] = &["push-pull", "open-drain", "open-source"];
const GPIO_INPUT_STATES: &[&str] = &["floating", "pull-up", "pull-down"];

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

fn parse_pin_info(data: &Value) -> Value {
    let info = data.get("info").unwrap_or(data);
    let empty_obj = json!({});
    let settings = data.get("settings").unwrap_or(&empty_obj);

    json!({
        "info": {
            "name": info.get("name").and_then(|v| v.as_str()).unwrap_or(""),
            "chip": info.get("chip").and_then(|v| v.as_str()).unwrap_or(""),
            "line": info.get("line").and_then(|v| v.as_i64()).unwrap_or(0),
            "capabilities": info.get("capabilities").unwrap_or(&json!([])),
        },
        "settings": {
            "state": settings.get("state").and_then(|v| v.as_str()).unwrap_or("disabled"),
            "edge": settings.get("edge").and_then(|v| v.as_str()).unwrap_or("none"),
            "debounce_ms": settings.get("debounce_ms").and_then(|v| v.as_i64()).unwrap_or(0),
        }
    })
}

pub async fn list_gpios(client: &Arc<DeviceClient>) -> Result<CallToolResult, McpError> {
    let data = client
        .get_json("/api/v1/gpios", &[])
        .await
        .map_err(|e| err(e))?;

    let pins_obj = data
        .as_object()
        .ok_or_else(|| err("Invalid response: expected a JSON object keyed by pin ID"))?;

    let mut pins: Vec<Value> = pins_obj
        .iter()
        .filter_map(|(id, pin_data)| {
            let pin_id: i64 = id.parse().ok()?;
            let mut info = parse_pin_info(pin_data);
            info["pin_id"] = json!(pin_id);
            Some(info)
        })
        .collect();

    pins.sort_by_key(|p| p.get("pin_id").and_then(|v| v.as_i64()).unwrap_or(0));

    Ok(json_text_result(&json!(pins)))
}

pub async fn get_gpio_info(
    client: &Arc<DeviceClient>,
    pin_id: i64,
) -> Result<CallToolResult, McpError> {
    let data = client
        .get_json(&format!("/api/v1/gpio/{}", pin_id), &[])
        .await
        .map_err(|e| err(e))?;

    if !data.is_object() {
        return Err(err("Invalid response format"));
    }

    let mut info = parse_pin_info(&data);
    info["pin_id"] = json!(pin_id);

    Ok(json_text_result(&info))
}

/// Get pin settings from the device.
async fn get_pin_settings(client: &Arc<DeviceClient>, pin_id: i64) -> Result<Value, McpError> {
    client
        .get_json(&format!("/api/v1/gpio/{}/settings", pin_id), &[])
        .await
        .map_err(|e| err(e))
}

/// Ensure pin is configured as output.
async fn ensure_output(client: &Arc<DeviceClient>, pin_id: i64) -> Result<(), McpError> {
    let settings = get_pin_settings(client, pin_id).await?;
    let current_state = settings
        .get("state")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if GPIO_OUTPUT_STATES.contains(&current_state) {
        return Ok(());
    }

    client
        .post_json(
            &format!("/api/v1/gpio/{}/settings", pin_id),
            &[],
            Some(&json!({"state": GPIO_OUTPUT_STATE})),
        )
        .await
        .map_err(|e| err(e))?;

    Ok(())
}

/// Ensure pin is configured as input with the given debounce.
async fn ensure_input(
    client: &Arc<DeviceClient>,
    pin_id: i64,
    debounce_ms: Option<i64>,
) -> Result<(), McpError> {
    let settings = get_pin_settings(client, pin_id).await?;
    let current_state = settings
        .get("state")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let current_debounce = settings
        .get("debounce_ms")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    let state_ok = GPIO_INPUT_STATES.contains(&current_state);
    let debounce_ok = debounce_ms.is_none() || current_debounce == debounce_ms.unwrap();

    if state_ok && debounce_ok {
        return Ok(());
    }

    let mut payload = json!({});
    if !state_ok {
        payload["state"] = json!(GPIO_INPUT_STATE);
    }
    if !debounce_ok {
        payload["debounce_ms"] = json!(debounce_ms.unwrap());
    }

    client
        .post_json(
            &format!("/api/v1/gpio/{}/settings", pin_id),
            &[],
            Some(&payload),
        )
        .await
        .map_err(|e| err(e))?;

    Ok(())
}

pub async fn set_gpio_value(
    client: &Arc<DeviceClient>,
    pin_id: i64,
    value: i64,
) -> Result<CallToolResult, McpError> {
    if value != 0 && value != 1 {
        return Err(err("GPIO value must be 0 or 1"));
    }

    ensure_output(client, pin_id).await?;

    client
        .post_text(
            &format!("/api/v1/gpio/{}/value", pin_id),
            &value.to_string(),
        )
        .await
        .map_err(|e| err(e))?;

    Ok(text_result(&format!(
        "GPIO pin {} set to {}",
        pin_id, value
    )))
}

pub async fn get_gpio_value(
    client: &Arc<DeviceClient>,
    pin_id: i64,
    debounce_ms: Option<i64>,
) -> Result<CallToolResult, McpError> {
    let debounce_ms = debounce_ms.unwrap_or(GPIO_DEBOUNCE_MS_DEFAULT);

    ensure_input(client, pin_id, Some(debounce_ms)).await?;

    let data = client
        .get_json(&format!("/api/v1/gpio/{}/value", pin_id), &[])
        .await
        .map_err(|e| err(e))?;

    let value = data.as_i64().unwrap_or(0);

    Ok(text_result(&format!(
        "GPIO pin {} value: {}",
        pin_id, value
    )))
}
