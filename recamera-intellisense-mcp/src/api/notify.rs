//! Result-push (notify) configuration (`/notify/cfg`), shared by built-in
//! vision and AcousticsLab results. Secrets are redacted on read: the device
//! returns cleartext passwords/tokens, which must not enter agent contexts.

use anyhow::{bail, Result};
use serde_json::Value;

use crate::api::expect_ok;
use crate::api_client::ApiClient;
use crate::types::{DeviceRecord, HttpChannelPatch, MqttChannelPatch, NotifyConfig,
                   NotifyHttp, NotifyMqtt, NotifySetResult, NotifyTemplates,
                   NotifyTemplatesPatch, NotifyUart, SetNotifyConfigParams};

const PATH_CFG: &str = "/cgi-bin/entry.cgi/notify/cfg";

fn text(v: &Value, key: &str) -> String {
    v.get(key).and_then(|x| x.as_str()).unwrap_or("").to_string()
}

fn redact(v: &Value, key: &str) -> String {
    match v.get(key).and_then(|x| x.as_str()) {
        Some(s) if !s.is_empty() => "***".to_string(),
        _ => String::new(),
    }
}

fn mode_name(mode: i64) -> String {
    match mode {
        0 => "off",
        1 => "mqtt",
        2 => "http",
        3 => "uart",
        other => return format!("unknown({other})"),
    }
    .to_string()
}

pub async fn get_notify_config(client: &ApiClient, device: &DeviceRecord) -> Result<NotifyConfig> {
    let d = client.get_json(device, PATH_CFG, None).await?;
    let mqtt = d.get("dMqtt").cloned().unwrap_or(Value::Null);
    let http = d.get("dHttp").cloned().unwrap_or(Value::Null);
    // Wire quirk: MQTT spells its host field "sURL", HTTP "sUrl".
    let uart = d.get("dUart").cloned().unwrap_or(Value::Null);
    let tpl = d.get("dTemplate").cloned().unwrap_or(Value::Null);
    let mode = d.get("iMode").and_then(|v| v.as_i64()).unwrap_or(0);
    Ok(NotifyConfig {
        mode: mode as i32,
        mode_name: mode_name(mode),
        mqtt: NotifyMqtt {
            url: text(&mqtt, "sURL"),
            port: mqtt.get("iPort").and_then(|v| v.as_i64()).unwrap_or(1883) as i32,
            client_id: text(&mqtt, "sClientId"),
            username: text(&mqtt, "sUsername"),
            password: redact(&mqtt, "sPassword"),
            topic: text(&mqtt, "sTopic"),
        },
        http: NotifyHttp {
            url: text(&http, "sUrl"),
            token: redact(&http, "sToken"),
        },
        uart: NotifyUart {
            port: text(&uart, "sPort"),
            port_dev: text(&uart, "sPortDev"),
        },
        templates: NotifyTemplates {
            classification: text(&tpl, "sClassification"),
            detection: text(&tpl, "sDetection"),
            keypoint: text(&tpl, "sKeypoint"),
            segmentation: text(&tpl, "sSegmentation"),
            tracking: text(&tpl, "sTracking"),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_redacted_and_empties_stay_empty() {
        let mqtt = serde_json::json!({"sPassword": "admin", "sUsername": "u"});
        assert_eq!(redact(&mqtt, "sPassword"), "***");
        let http = serde_json::json!({"sToken": ""});
        assert_eq!(redact(&http, "sToken"), "");
        assert_eq!(text(&mqtt, "sUsername"), "u");
        assert_eq!(mode_name(1), "mqtt");
        assert_eq!(mode_name(9), "unknown(9)");
    }
}

const REDACTED: &str = "***";
const TEMPLATE_KEYS: [(&str, &str); 5] = [
    ("classification", "sClassification"),
    ("detection", "sDetection"),
    ("keypoint", "sKeypoint"),
    ("segmentation", "sSegmentation"),
    ("tracking", "sTracking"),
];

fn mqtt_empty(p: &MqttChannelPatch) -> bool {
    p.url.is_none() && p.port.is_none() && p.client_id.is_none()
        && p.username.is_none() && p.password.is_none() && p.topic.is_none()
}

fn http_empty(p: &HttpChannelPatch) -> bool {
    p.url.is_none() && p.token.is_none()
}

fn templates_empty(p: &NotifyTemplatesPatch) -> bool {
    p.classification.is_none() && p.detection.is_none() && p.keypoint.is_none()
        && p.segmentation.is_none() && p.tracking.is_none()
}

fn reject_secret_echo(value: &Option<String>, field: &str) -> Result<()> {
    if value.as_deref() == Some(REDACTED) {
        bail!(
            "{field}={REDACTED:?} is the redaction placeholder, not a secret — \
             pass the real value, \"\" to clear, or omit the field to keep the stored one"
        );
    }
    Ok(())
}

fn merge_channel(
    raw: Option<&Value>,
    changes: Vec<(&str, Value)>,
    field_map: &[(&str, &str)],
) -> Value {
    let mut merged = raw.and_then(|v| v.as_object().cloned()).unwrap_or_default();
    for (friendly, value) in changes {
        if let Some((_, wire)) = field_map.iter().find(|(f, _)| *f == friendly) {
            merged.insert((*wire).to_string(), value);
        }
    }
    Value::Object(merged)
}

/// Update the result-push config. Omitted fields keep their stored values —
/// secrets included (merged from the device's own unredacted config).
/// Applying restarts the notify service and recameraipc.
pub async fn set_notify_config(
    client: &ApiClient,
    device: &DeviceRecord,
    params: &SetNotifyConfigParams,
) -> Result<NotifySetResult> {
    // Empty patch sections count as absent — a no-op must not restart the
    // device pipeline for nothing.
    let mqtt = params.mqtt.as_ref().filter(|p| !mqtt_empty(p));
    let http = params.http.as_ref().filter(|p| !http_empty(p));
    let templates = params.templates.as_ref().filter(|p| !templates_empty(p));
    if params.mode.is_none() && mqtt.is_none() && http.is_none() && templates.is_none() {
        bail!("nothing to change: pass mode, mqtt, http, or templates");
    }
    if let Some(mode) = params.mode {
        if !(0..=3).contains(&mode) {
            bail!("mode must be 0 (off), 1 (MQTT), 2 (HTTP), or 3 (UART)");
        }
    }
    let raw = client.get_json(device, PATH_CFG, None).await?;
    let mut payload = serde_json::Map::new();
    if let Some(mode) = params.mode {
        payload.insert("iMode".into(), mode.into());
    }
    if let Some(mqtt) = mqtt {
        reject_secret_echo(&mqtt.password, "mqtt.password")?;
        if let Some(port) = mqtt.port {
            if !(1..=65535).contains(&port) {
                bail!("mqtt.port must be 1~65535");
            }
        }
        let mut changes: Vec<(&str, Value)> = Vec::new();
        if let Some(v) = &mqtt.url {
            changes.push(("url", v.clone().into()));
        }
        if let Some(v) = mqtt.port {
            changes.push(("port", v.into()));
        }
        if let Some(v) = &mqtt.client_id {
            changes.push(("client_id", v.clone().into()));
        }
        if let Some(v) = &mqtt.username {
            changes.push(("username", v.clone().into()));
        }
        if let Some(v) = &mqtt.password {
            changes.push(("password", v.clone().into()));
        }
        if let Some(v) = &mqtt.topic {
            changes.push(("topic", v.clone().into()));
        }
        const MQTT_MAP: [(&str, &str); 6] = [
            ("url", "sURL"),
            ("port", "iPort"),
            ("client_id", "sClientId"),
            ("username", "sUsername"),
            ("password", "sPassword"),
            ("topic", "sTopic"),
        ];
        payload.insert(
            "dMqtt".into(),
            merge_channel(raw.get("dMqtt"), changes, &MQTT_MAP),
        );
    }
    if let Some(http) = http {
        reject_secret_echo(&http.token, "http.token")?;
        let mut changes: Vec<(&str, Value)> = Vec::new();
        if let Some(v) = &http.url {
            changes.push(("url", v.clone().into()));
        }
        if let Some(v) = &http.token {
            changes.push(("token", v.clone().into()));
        }
        const HTTP_MAP: [(&str, &str); 2] = [("url", "sUrl"), ("token", "sToken")];
        payload.insert(
            "dHttp".into(),
            merge_channel(raw.get("dHttp"), changes, &HTTP_MAP),
        );
    }
    if let Some(templates) = templates {
        let provided: Vec<(&str, &String)> = [
            ("classification", &templates.classification),
            ("detection", &templates.detection),
            ("keypoint", &templates.keypoint),
            ("segmentation", &templates.segmentation),
            ("tracking", &templates.tracking),
        ]
        .into_iter()
        .filter_map(|(k, v)| v.as_ref().map(|s| (k, s)))
        .collect();
        let mut merged = raw
            .get("dTemplate")
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        for (friendly, value) in provided {
            if let Some((_, wire)) = TEMPLATE_KEYS.iter().find(|(f, _)| *f == friendly) {
                merged.insert((*wire).to_string(), value.clone().into());
            }
        }
        payload.insert("dTemplate".into(), Value::Object(merged));
    }
    // Mirror the server's merged-config requirements so mistakes fail here.
    let merged_mode = params
        .mode
        .unwrap_or_else(|| raw.get("iMode").and_then(|v| v.as_i64()).unwrap_or(0));
    if merged_mode == 1 {
        let url = payload
            .get("dMqtt")
            .or_else(|| raw.get("dMqtt"))
            .and_then(|m| m.get("sURL"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if url.trim().is_empty() {
            bail!("mode=1 (MQTT) requires mqtt.url");
        }
    }
    if merged_mode == 2 {
        let url = payload
            .get("dHttp")
            .or_else(|| raw.get("dHttp"))
            .and_then(|m| m.get("sUrl"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if url.trim().is_empty() {
            bail!("mode=2 (HTTP) requires http.url");
        }
    }
    let resp = client
        .post_json(device, PATH_CFG, None, Some(&Value::Object(payload)))
        .await?;
    expect_ok(&resp, "set notify config")?;
    Ok(NotifySetResult {
        changed: true,
        mode: merged_mode,
        mode_name: mode_name(merged_mode),
        note: "device restarts its notify service and recameraipc to apply (brief pipeline gap)"
            .to_string(),
    })
}
