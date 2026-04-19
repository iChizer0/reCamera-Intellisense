use anyhow::{bail, Result};
use serde_json::{json, Value};

use crate::api_client::ApiClient;
use crate::types::{DeviceRecord, PinDescriptor, PinInfo, PinSettings};

// MARK: Constants

const BASE: &str = "/api/v1";
const OUTPUT_STATE: &str = "push-pull";
const INPUT_STATE: &str = "floating";
const OUTPUT_STATES: &[&str] = &["push-pull", "open-drain", "open-source"];
const INPUT_STATES: &[&str] = &["floating", "pull-up", "pull-down"];
const DEBOUNCE_MS_DEFAULT: i32 = 100;

// MARK: Parsing

fn parse_info(data: &Value) -> PinInfo {
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
        capabilities: data
            .get("capabilities")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|c| c.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
    }
}

fn parse_settings(data: &Value) -> PinSettings {
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

fn parse_descriptor(pin_id: i32, data: &Value) -> PinDescriptor {
    PinDescriptor {
        pin_id,
        info: data
            .get("info")
            .filter(|v| v.is_object())
            .map(parse_info)
            .unwrap_or(PinInfo {
                name: String::new(),
                chip: String::new(),
                line: 0,
                capabilities: vec![],
            }),
        settings: data
            .get("settings")
            .filter(|v| v.is_object())
            .map(parse_settings)
            .unwrap_or(PinSettings {
                state: "disabled".into(),
                edge: "none".into(),
                debounce_ms: 0,
            }),
    }
}

// MARK: Listing

pub async fn list(client: &ApiClient, device: &DeviceRecord) -> Result<Vec<PinDescriptor>> {
    let data = client
        .get_json(device, &format!("{BASE}/gpios"), None)
        .await?;
    let obj = data
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("expected object keyed by pin id"))?;
    let mut pins: Vec<PinDescriptor> = obj
        .iter()
        .filter_map(|(k, v)| {
            let pin_id = k.parse::<i32>().ok()?;
            Some(parse_descriptor(pin_id, v))
        })
        .collect();
    pins.sort_by_key(|p| p.pin_id);
    Ok(pins)
}

pub async fn info(client: &ApiClient, device: &DeviceRecord, pin_id: i32) -> Result<PinDescriptor> {
    let data = client
        .get_json(device, &format!("{BASE}/gpio/{pin_id}"), None)
        .await?;
    Ok(parse_descriptor(pin_id, &data))
}

async fn settings(client: &ApiClient, device: &DeviceRecord, pin_id: i32) -> Result<PinSettings> {
    let data = client
        .get_json(device, &format!("{BASE}/gpio/{pin_id}/settings"), None)
        .await?;
    Ok(parse_settings(&data))
}

// MARK: Ensure mode + set/get value

async fn ensure_output(client: &ApiClient, device: &DeviceRecord, pin_id: i32) -> Result<()> {
    let s = settings(client, device, pin_id).await?;
    if OUTPUT_STATES.contains(&s.state.as_str()) {
        return Ok(());
    }
    let payload = json!({"state": OUTPUT_STATE});
    client
        .post_json(
            device,
            &format!("{BASE}/gpio/{pin_id}/settings"),
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
    let s = settings(client, device, pin_id).await?;
    let state_ok = INPUT_STATES.contains(&s.state.as_str());
    let debounce_ok = debounce_ms.is_none_or(|d| s.debounce_ms == d);
    if state_ok && debounce_ok {
        return Ok(());
    }
    let mut payload = serde_json::Map::new();
    if !state_ok {
        payload.insert("state".into(), json!(INPUT_STATE));
    }
    if let Some(d) = debounce_ms.filter(|_| !debounce_ok) {
        payload.insert("debounce_ms".into(), json!(d));
    }
    client
        .post_json(
            device,
            &format!("{BASE}/gpio/{pin_id}/settings"),
            None,
            Some(&Value::Object(payload)),
        )
        .await?;
    Ok(())
}

pub async fn set_value(
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
            &format!("{BASE}/gpio/{pin_id}/value"),
            &value.to_string(),
        )
        .await?;
    Ok(value)
}

pub async fn get_value(
    client: &ApiClient,
    device: &DeviceRecord,
    pin_id: i32,
    debounce_ms: Option<i32>,
) -> Result<i32> {
    ensure_input(
        client,
        device,
        pin_id,
        debounce_ms.or(Some(DEBOUNCE_MS_DEFAULT)),
    )
    .await?;
    let data = client
        .get_json(device, &format!("{BASE}/gpio/{pin_id}/value"), None)
        .await?;
    Ok(data
        .as_i64()
        .ok_or_else(|| anyhow::anyhow!("expected integer GPIO value"))? as i32)
}
