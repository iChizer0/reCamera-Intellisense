use crate::api_client::ApiClient;
use crate::storage;
use crate::types::*;
use anyhow::{bail, Result};
use serde_json::{json, Value};

fn unix_ms_to_iso8601(unix_ms: u64) -> String {
    let secs = unix_ms / 1000;
    let millis = unix_ms % 1000;
    let days = secs / 86400;
    let remaining = secs % 86400;
    let hours = remaining / 3600;
    let minutes = (remaining % 3600) / 60;
    let seconds = remaining % 60;
    // Calculate year/month/day from days since epoch (1970-01-01)
    let (year, month, day) = days_to_ymd(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        year, month, day, hours, minutes, seconds, millis
    )
}

fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    // Algorithm from http://howardhinnant.github.io/date_algorithms.html
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

pub async fn get_detection_models_info(
    client: &ApiClient,
    device: &DeviceRecord,
) -> Result<Vec<DetectionModel>> {
    let raw = client
        .get_json(device, "/cgi-bin/entry.cgi/model/list", None)
        .await?;
    let models = raw
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("Expected array of models"))?;
    let mut result = Vec::new();
    for (i, model) in models.iter().enumerate() {
        let name = model
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let labels: Vec<String> = model
            .get("modelInfo")
            .and_then(|v| v.get("classes"))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|l| l.as_str().unwrap_or("").to_string())
                    .collect()
            })
            .unwrap_or_default();
        result.push(DetectionModel {
            id: i as i32,
            name,
            labels,
        });
    }
    Ok(result)
}

pub async fn get_detection_model(
    client: &ApiClient,
    device: &DeviceRecord,
) -> Result<Option<DetectionModel>> {
    let info = client
        .get_json(device, "/cgi-bin/entry.cgi/model/inference", None)
        .await?;
    if !info.is_object() || info.get("iEnable").and_then(|v| v.as_i64()).unwrap_or(0) == 0 {
        return Ok(None);
    }
    let active_name = info
        .get("sModel")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let models = get_detection_models_info(client, device).await?;
    Ok(models.into_iter().find(|m| m.name == active_name))
}

pub async fn set_detection_model(
    client: &ApiClient,
    device: &DeviceRecord,
    model_id: Option<i32>,
    model_name: Option<&str>,
) -> Result<()> {
    if model_id.is_none() == model_name.is_none() {
        bail!("Provide exactly one of 'model_id' or 'model_name'.");
    }
    let models = get_detection_models_info(client, device).await?;
    let target = if let Some(id) = model_id {
        models
            .iter()
            .find(|m| m.id == id)
            .ok_or_else(|| anyhow::anyhow!("Model ID '{id}' not found on device."))?
    } else {
        let name = model_name.unwrap();
        models
            .iter()
            .find(|m| m.name == name)
            .ok_or_else(|| anyhow::anyhow!("Model name '{name}' not found on device."))?
    };
    let params = [("id", target.id.to_string())];
    let params_ref: Vec<(&str, &str)> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let payload = json!({
        "iEnable": 1,
        "iFPS": 30,
        "sModel": target.name,
    });
    let result = client
        .post_json(
            device,
            "/cgi-bin/entry.cgi/model/inference",
            Some(&params_ref),
            Some(&payload),
        )
        .await?;
    if result.get("code").and_then(|v| v.as_i64()).unwrap_or(-1) != 0 {
        let msg = result
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown error");
        bail!("Failed to set detection model: {msg}");
    }
    Ok(())
}

pub async fn get_detection_schedule(
    client: &ApiClient,
    device: &DeviceRecord,
) -> Result<Option<Vec<ScheduleRange>>> {
    let data = client
        .get_json(
            device,
            "/cgi-bin/entry.cgi/record/record/rule/schedule-rule-config",
            None,
        )
        .await?;
    if !data.is_object()
        || !data
            .get("bEnable")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    {
        return Ok(None);
    }
    let weekdays = data
        .get("lActiveWeekdays")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|rng| {
                    let start = rng.get("sStart")?.as_str()?.to_string();
                    let end = rng.get("sEnd")?.as_str()?.to_string();
                    Some(ScheduleRange { start, end })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if weekdays.is_empty() {
        return Ok(None);
    }
    Ok(Some(weekdays))
}

pub async fn set_detection_schedule(
    client: &ApiClient,
    device: &DeviceRecord,
    schedule: Option<&[ScheduleRange]>,
) -> Result<()> {
    let weekdays: Vec<Value> = schedule
        .map(|ranges| {
            ranges
                .iter()
                .map(|r| json!({"sStart": r.start, "sEnd": r.end}))
                .collect()
        })
        .unwrap_or_default();
    let payload = json!({
        "bEnable": schedule.is_some(),
        "lActiveWeekdays": weekdays,
    });
    let result = client
        .post_json(
            device,
            "/cgi-bin/entry.cgi/record/record/rule/schedule-rule-config",
            None,
            Some(&payload),
        )
        .await?;
    if result.get("code").and_then(|v| v.as_i64()).unwrap_or(-1) != 0 {
        let msg = result
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown error");
        bail!("Failed to set detection schedule: {msg}");
    }
    Ok(())
}

fn check_record_image(data: &Value) -> bool {
    if !data.is_object() {
        return false;
    }
    if !data
        .get("bRuleEnabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return false;
    }
    data.get("dWriterConfig")
        .and_then(|w| w.get("sFormat"))
        .and_then(|v| v.as_str())
        .map(|s| s.eq_ignore_ascii_case("JPG"))
        .unwrap_or(false)
}

async fn ensure_record_image(client: &ApiClient, device: &DeviceRecord) -> Result<()> {
    let data = client
        .get_json(device, "/cgi-bin/entry.cgi/record/rule/config", None)
        .await?;
    if check_record_image(&data) {
        return Ok(());
    }
    let payload = json!({
        "bRuleEnabled": true,
        "dWriterConfig": {
            "iIntervalMs": 0,
            "sFormat": "JPG",
        },
    });
    let result = client
        .post_json(
            device,
            "/cgi-bin/entry.cgi/record/rule/config",
            None,
            Some(&payload),
        )
        .await?;
    if result.get("code").and_then(|v| v.as_i64()).unwrap_or(-1) != 0 {
        bail!("Failed to enable record image");
    }
    Ok(())
}

pub async fn get_detection_rules(
    client: &ApiClient,
    device: &DeviceRecord,
) -> Result<Vec<DetectionRule>> {
    // Check prerequisites
    let record_cfg = client
        .get_json(device, "/cgi-bin/entry.cgi/record/rule/config", None)
        .await
        .ok();
    if record_cfg
        .as_ref()
        .map(|d| !check_record_image(d))
        .unwrap_or(true)
    {
        return Ok(vec![]); // No record image means no detection events, so rules are irrelevant
    }
    let rules_data = client
        .get_json(
            device,
            "/cgi-bin/entry.cgi/record/rule/record-rule-config",
            None,
        )
        .await?;
    if rules_data.get("sCurrentSelected").and_then(|v| v.as_str()) != Some("INFERENCE_SET") {
        return Ok(vec![]); // If the current selected rule set is not "INFERENCE_SET", we consider there are no valid detection rules configured
    }
    let rules = rules_data
        .get("lInferenceSet")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(|rule| {
                    let name = rule
                        .get("sID")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let debounce_times = rule
                        .get("iDebounceTimes")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0) as i32;
                    let confidence = rule
                        .get("lConfidenceFilter")
                        .and_then(|v| v.as_array())
                        .map(|a| a.iter().filter_map(|x| x.as_f64()).collect())
                        .unwrap_or_else(|| vec![0.0, 1.0]);
                    let labels: Vec<String> = rule
                        .get("lClassFilter")
                        .and_then(|v| v.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str())
                                .map(|s| s.to_string())
                                .collect()
                        })
                        .unwrap_or_default();
                    let regions =
                        rule.get("lRegionFilter")
                            .and_then(|v| v.as_array())
                            .map(|regions| {
                                regions
                                    .iter()
                                    .filter_map(|r| {
                                        r.get("lPolygon").and_then(|v| v.as_array()).map(|pts| {
                                            pts.iter()
                                                .filter_map(|pt| {
                                                    pt.as_array().map(|coords| {
                                                        coords
                                                            .iter()
                                                            .filter_map(|c| c.as_f64())
                                                            .collect()
                                                    })
                                                })
                                                .collect()
                                        })
                                    })
                                    .collect()
                            });
                    DetectionRule {
                        name,
                        debounce_times,
                        confidence_range_filter: confidence,
                        label_filter: labels,
                        region_filter: regions,
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(rules)
}

pub async fn set_detection_rules(
    client: &ApiClient,
    device: &DeviceRecord,
    rules: &[DetectionRule],
) -> Result<()> {
    ensure_record_image(client, device).await?;
    storage::ensure_storage(client, device).await?;

    let full_region: Vec<Vec<f64>> = vec![
        vec![0.0, 0.0],
        vec![1.0, 0.0],
        vec![1.0, 1.0],
        vec![0.0, 1.0],
    ];
    let inference_set: Vec<Value> = rules
        .iter()
        .map(|rule| {
            let regions = rule
                .region_filter
                .as_ref()
                .map(|r| {
                    if r.is_empty() {
                        vec![full_region.clone()]
                    } else {
                        r.clone()
                    }
                })
                .unwrap_or_else(|| vec![full_region.clone()]);
            json!({
                "sID": rule.name,
                "iDebounceTimes": rule.debounce_times,
                "lConfidenceFilter": rule.confidence_range_filter,
                "lClassFilter": rule.label_filter,
                "lRegionFilter": regions.iter().map(|poly| json!({"lPolygon": poly})).collect::<Vec<_>>(),
            })
        })
        .collect();
    let payload = json!({
        "sCurrentSelected": "INFERENCE_SET",
        "lInferenceSet": inference_set,
    });
    let result = client
        .post_json(
            device,
            "/cgi-bin/entry.cgi/record/rule/record-rule-config",
            None,
            Some(&payload),
        )
        .await?;
    if result.get("code").and_then(|v| v.as_i64()).unwrap_or(-1) != 0 {
        let msg = result
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown error");
        bail!("Failed to set detection rules: {msg}");
    }
    Ok(())
}

pub async fn get_detection_events(
    client: &ApiClient,
    device: &DeviceRecord,
    start_unix_ms: Option<i64>,
    end_unix_ms: Option<i64>,
) -> Result<Vec<DetectionEvent>> {
    let mut params = Vec::new();
    let start_str;
    let end_str;
    if let Some(start) = start_unix_ms {
        start_str = start.to_string();
        params.push(("start", start_str.as_str()));
    }
    if let Some(end) = end_unix_ms {
        end_str = end.to_string();
        params.push(("end", end_str.as_str()));
    }
    let params_opt = if params.is_empty() {
        None
    } else {
        Some(params.as_slice())
    };
    let raw = client
        .get_json(device, "/api/v1/intellisense/events", params_opt)
        .await?;
    let items = raw
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("Expected array of events"))?;
    let mut events = Vec::new();
    for item in items {
        let ts_ms = match item.get("timestamp").and_then(|v| v.as_u64()) {
            Some(ts) => ts,
            None => continue,
        };
        let event_id = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let rule_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let rule_name = if !event_id.trim().is_empty() {
            event_id.trim().to_string()
        } else {
            rule_type.to_string()
        };
        let snapshot_path = item
            .get("file_event")
            .and_then(|fe| fe.get("path"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.to_string());
        events.push(DetectionEvent {
            timestamp: unix_ms_to_iso8601(ts_ms),
            timestamp_unix_ms: ts_ms,
            rule_name,
            snapshot_path,
        });
    }
    Ok(events)
}

pub async fn clear_detection_events(client: &ApiClient, device: &DeviceRecord) -> Result<()> {
    let result = client
        .post_json(device, "/api/v1/intellisense/events/clear", None, None)
        .await?;
    let status = result.get("status").and_then(|v| v.as_str()).unwrap_or("");
    if !status.eq_ignore_ascii_case("ok") {
        bail!(
            "Failed to clear events: {}",
            result
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown error")
        );
    }
    Ok(())
}
