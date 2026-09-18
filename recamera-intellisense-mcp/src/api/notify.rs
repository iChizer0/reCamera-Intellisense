//! Result-push (notify) configuration (`/notify/cfg`), shared by built-in
//! vision and AcousticsLab results. Secrets are redacted on read: the device
//! returns cleartext passwords/tokens, which must not enter agent contexts.

use anyhow::Result;
use serde_json::Value;

use crate::api_client::ApiClient;
use crate::types::{DeviceRecord, NotifyConfig, NotifyHttp, NotifyMqtt, NotifyTemplates, NotifyUart};

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
