use anyhow::{bail, Result};
use serde_json::{json, Value};

use crate::api_client::ApiClient;
use crate::types::*;

const GPIO_API_BASE: &str = "/api/v1";
const GPIO_OUTPUT_STATE: &str = "push-pull";
const GPIO_INPUT_STATE: &str = "floating";
const GPIO_OUTPUT_STATES: &[&str] = &["push-pull", "open-drain", "open-source"];
const GPIO_INPUT_STATES: &[&str] = &["floating", "pull-up", "pull-down"];
const GPIO_DEBOUNCE_MS_DEFAULT: i32 = 100;

fn parse_pin_info(data: &Value) -> PinInfo {
    let capabilities = data
        .get("capabilities")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|c| c.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    PinInfo {
        name: data
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        chip: data
            .get("chip")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        line: data.get("line").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
        capabilities,
    }
}

fn parse_pin_settings(data: &Value) -> PinSettings {
    PinSettings {
        state: data
            .get("state")
            .and_then(|v| v.as_str())
            .unwrap_or("disabled")
            .to_string(),
        edge: data
            .get("edge")
            .and_then(|v| v.as_str())
            .unwrap_or("none")
            .to_string(),
        debounce_ms: data
            .get("debounce_ms")
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as i32,
    }
}

fn parse_pin_descriptor(pin_id: i32, data: &Value) -> PinDescriptor {
    let info = data
        .get("info")
        .filter(|v| v.is_object())
        .map(parse_pin_info)
        .unwrap_or(PinInfo {
            name: String::new(),
            chip: String::new(),
            line: 0,
            capabilities: vec![],
        });
    let settings = data
        .get("settings")
        .filter(|v| v.is_object())
        .map(parse_pin_settings)
        .unwrap_or(PinSettings {
            state: "disabled".to_string(),
            edge: "none".to_string(),
            debounce_ms: 0,
        });
    PinDescriptor {
        pin_id,
        info,
        settings,
    }
}

pub async fn list_gpios(client: &ApiClient, device: &DeviceRecord) -> Result<Vec<PinDescriptor>> {
    let data = client
        .get_json(device, &format!("{GPIO_API_BASE}/gpios"), None)
        .await?;
    let obj = data
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Expected JSON object keyed by pin ID"))?;
    let mut pins: Vec<PinDescriptor> = obj
        .iter()
        .filter_map(|(key, val)| {
            let pin_id = key.parse::<i32>().ok()?;
            Some(parse_pin_descriptor(pin_id, val))
        })
        .collect();
    pins.sort_by_key(|p| p.pin_id);
    Ok(pins)
}

pub async fn get_gpio_info(
    client: &ApiClient,
    device: &DeviceRecord,
    pin_id: i32,
) -> Result<PinDescriptor> {
    let data = client
        .get_json(device, &format!("{GPIO_API_BASE}/gpio/{pin_id}"), None)
        .await?;
    Ok(parse_pin_descriptor(pin_id, &data))
}

async fn get_pin_settings(
    client: &ApiClient,
    device: &DeviceRecord,
    pin_id: i32,
) -> Result<PinSettings> {
    let data = client
        .get_json(
            device,
            &format!("{GPIO_API_BASE}/gpio/{pin_id}/settings"),
            None,
        )
        .await?;
    Ok(parse_pin_settings(&data))
}

async fn ensure_output(client: &ApiClient, device: &DeviceRecord, pin_id: i32) -> Result<()> {
    let settings = get_pin_settings(client, device, pin_id).await?;
    if GPIO_OUTPUT_STATES.contains(&settings.state.as_str()) {
        return Ok(());
    }
    let payload = json!({"state": GPIO_OUTPUT_STATE});
    client
        .post_json(
            device,
            &format!("{GPIO_API_BASE}/gpio/{pin_id}/settings"),
            None,
            Some(&payload),
        )
        .await?;
    Ok(())
}

async fn ensure_input(
    client: &ApiClient,
    device: &DeviceRecord,
    pin_id: i32,
    debounce_ms: Option<i32>,
) -> Result<()> {
    let settings = get_pin_settings(client, device, pin_id).await?;
    let state_ok = GPIO_INPUT_STATES.contains(&settings.state.as_str());
    let debounce_ok = debounce_ms.is_none() || settings.debounce_ms == debounce_ms.unwrap();
    if state_ok && debounce_ok {
        return Ok(());
    }
    let mut payload = serde_json::Map::new();
    if !state_ok {
        payload.insert("state".to_string(), json!(GPIO_INPUT_STATE));
    }
    if !debounce_ok {
        payload.insert("debounce_ms".to_string(), json!(debounce_ms.unwrap()));
    }
    client
        .post_json(
            device,
            &format!("{GPIO_API_BASE}/gpio/{pin_id}/settings"),
            None,
            Some(&Value::Object(payload)),
        )
        .await?;
    Ok(())
}

pub async fn set_gpio_value(
    client: &ApiClient,
    device: &DeviceRecord,
    pin_id: i32,
    value: i32,
) -> Result<i32> {
    if value != 0 && value != 1 {
        bail!("GPIO value must be 0 or 1, got {value}");
    }
    ensure_output(client, device, pin_id).await?;
    client
        .post_text(
            device,
            &format!("{GPIO_API_BASE}/gpio/{pin_id}/value"),
            &value.to_string(),
        )
        .await?;
    Ok(value)
}

pub async fn get_gpio_value(
    client: &ApiClient,
    device: &DeviceRecord,
    pin_id: i32,
    debounce_ms: Option<i32>,
) -> Result<i32> {
    let debounce = debounce_ms.or(Some(GPIO_DEBOUNCE_MS_DEFAULT));
    ensure_input(client, device, pin_id, debounce).await?;
    let data = client
        .get_json(
            device,
            &format!("{GPIO_API_BASE}/gpio/{pin_id}/value"),
            None,
        )
        .await?;
    let value = data
        .as_i64()
        .ok_or_else(|| anyhow::anyhow!("Expected integer value from GPIO read"))?;
    Ok(value as i32)
}
