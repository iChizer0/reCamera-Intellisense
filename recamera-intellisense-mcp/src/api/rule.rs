use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::api::expect_ok;
use crate::api_client::ApiClient;
use crate::types::{
    AvailableGpio, AvailableTty, DetectionRule, DeviceRecord, GpioTrigger, RecordTrigger,
    RuleConfig, RuleEvent, RuleEventOwner, RuleInfo, ScheduleRange, TtyTrigger, WriterConfig,
};

// MARK: Paths

const PATH_CONFIG: &str = "/cgi-bin/entry.cgi/record/rule/config";
const PATH_INFO: &str = "/cgi-bin/entry.cgi/record/rule/info";
const PATH_SCHEDULE: &str = "/cgi-bin/entry.cgi/record/rule/schedule-rule-config";
const PATH_RECORD_RULE: &str = "/cgi-bin/entry.cgi/record/rule/record-rule-config";
const PATH_HTTP_ACTIVATE: &str = "/cgi-bin/entry.cgi/record/rule/http-rule-activate";

// MARK: Global config (enable + writer)

pub async fn get_config(client: &ApiClient, device: &DeviceRecord) -> Result<RuleConfig> {
    let data = client.get_json(device, PATH_CONFIG, None).await?;
    parse_rule_config(&data)
}

pub async fn set_config(
    client: &ApiClient,
    device: &DeviceRecord,
    config: &RuleConfig,
) -> Result<()> {
    let payload = json!({
        "bRuleEnabled": config.rule_enabled,
        "dWriterConfig": {
            "sFormat": config.writer.format.to_uppercase(),
            "iIntervalMs": config.writer.interval_ms,
        },
    });
    let resp = client
        .post_json(device, PATH_CONFIG, None, Some(&payload))
        .await?;
    expect_ok(&resp, "set rule config")
}

fn parse_rule_config(data: &Value) -> Result<RuleConfig> {
    let rule_enabled = data
        .get("bRuleEnabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let writer = data
        .get("dWriterConfig")
        .context("missing dWriterConfig in rule config")?;
    Ok(RuleConfig {
        rule_enabled,
        writer: WriterConfig {
            format: writer
                .get("sFormat")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            interval_ms: writer
                .get("iIntervalMs")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
        },
    })
}

// MARK: Rule info

pub async fn get_info(client: &ApiClient, device: &DeviceRecord) -> Result<RuleInfo> {
    let d = client.get_json(device, PATH_INFO, None).await?;
    Ok(RuleInfo {
        ready_for_new_event: d
            .get("bReadyForNewEvent")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        last_event: d.get("dLastRuleEvent").and_then(parse_event),
        last_event_owner: d.get("dLastRuleEventOwner").and_then(parse_event_owner),
        available_gpios: d
            .get("dAvailableGPIOs")
            .and_then(|v| v.as_object())
            .map(|o| {
                o.iter()
                    .map(|(k, v)| (k.clone(), parse_avail_gpio(v)))
                    .collect()
            })
            .unwrap_or_default(),
        available_ttys: d
            .get("dAvailableTTYs")
            .and_then(|v| v.as_object())
            .map(|o| {
                o.iter()
                    .map(|(k, v)| (k.clone(), parse_avail_tty(v)))
                    .collect()
            })
            .unwrap_or_default(),
        media_paused: d
            .get("bMediaPaused")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        video_clip_length_seconds: d
            .get("bVideoClipLengthSeconds")
            .and_then(|v| v.as_i64())
            .unwrap_or(0),
    })
}

fn parse_event(v: &Value) -> Option<RuleEvent> {
    let status = v.get("sStatus").and_then(|v| v.as_str())?.to_string();
    let timestamp_unix_ms = v.get("iTimestamp").and_then(|v| v.as_u64())?;
    Some(RuleEvent {
        status,
        timestamp_unix_ms,
    })
}

fn parse_event_owner(v: &Value) -> Option<RuleEventOwner> {
    let rule_type = v.get("sRuleType").and_then(|v| v.as_str())?.to_string();
    let rule_id = v.get("sRuleID").and_then(|v| v.as_str())?.to_string();
    let timestamp_unix_ms = v.get("iTimestamp").and_then(|v| v.as_u64())?;
    Some(RuleEventOwner {
        rule_type,
        rule_id,
        timestamp_unix_ms,
    })
}

fn parse_avail_gpio(v: &Value) -> AvailableGpio {
    AvailableGpio {
        num: v.get("iNum").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
        state: v.get("sState").and_then(|v| v.as_str()).map(String::from),
        capabilities: v
            .get("lCapabilities")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|c| c.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
        level: v.get("sLevel").and_then(|v| v.as_str()).map(String::from),
    }
}

fn parse_avail_tty(v: &Value) -> AvailableTty {
    AvailableTty {
        socket_path: v
            .get("sSocketPath")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        buffer_size: v.get("iBufferSize").and_then(|v| v.as_i64()).unwrap_or(0),
    }
}

// MARK: Schedule rule

/// Read schedule. Returns None if schedule is disabled on the device.
pub async fn get_schedule(
    client: &ApiClient,
    device: &DeviceRecord,
) -> Result<Option<Vec<ScheduleRange>>> {
    let data = client.get_json(device, PATH_SCHEDULE, None).await?;
    if !data.is_object()
        || !data
            .get("bEnabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    {
        return Ok(None);
    }
    let ranges = data
        .get("lActiveWeekdays")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|r| {
                    let start = r.get("sStart")?.as_str()?.to_string();
                    let end = r.get("sEnd")?.as_str()?.to_string();
                    Some(ScheduleRange { start, end })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if ranges.is_empty() {
        Ok(None)
    } else {
        Ok(Some(ranges))
    }
}

/// Set schedule. Pass `None` or an empty slice to disable (rule active all the time).
pub async fn set_schedule(
    client: &ApiClient,
    device: &DeviceRecord,
    schedule: Option<&[ScheduleRange]>,
) -> Result<()> {
    let ranges = schedule.unwrap_or(&[]);
    let enabled = !ranges.is_empty();
    let weekdays: Vec<Value> = ranges
        .iter()
        .map(|r| json!({"sStart": r.start, "sEnd": r.end}))
        .collect();
    let payload = json!({
        "bEnabled": enabled,
        "lActiveWeekdays": weekdays,
    });
    let resp = client
        .post_json(device, PATH_SCHEDULE, None, Some(&payload))
        .await?;
    expect_ok(&resp, "set schedule rule")
}

// MARK: Record-rule trigger (tagged by sCurrentSelected)

pub async fn get_trigger(client: &ApiClient, device: &DeviceRecord) -> Result<RecordTrigger> {
    let data = client.get_json(device, PATH_RECORD_RULE, None).await?;
    parse_trigger(&data)
}

pub async fn set_trigger(
    client: &ApiClient,
    device: &DeviceRecord,
    trigger: &RecordTrigger,
) -> Result<()> {
    let payload = trigger_to_json(trigger)?;
    let resp = client
        .post_json(device, PATH_RECORD_RULE, None, Some(&payload))
        .await?;
    expect_ok(&resp, "set record trigger")
}

pub async fn activate_http_trigger(client: &ApiClient, device: &DeviceRecord) -> Result<()> {
    let resp = client
        .post_json(device, PATH_HTTP_ACTIVATE, None, None)
        .await?;
    expect_ok(&resp, "activate HTTP trigger")
}

// MARK: Trigger JSON <-> enum

pub fn parse_trigger(data: &Value) -> Result<RecordTrigger> {
    let kind = data
        .get("sCurrentSelected")
        .and_then(|v| v.as_str())
        .context("missing sCurrentSelected")?;
    Ok(match kind {
        "INFERENCE_SET" => RecordTrigger::InferenceSet {
            rules: data
                .get("lInferenceSet")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().map(parse_detection_rule).collect())
                .unwrap_or_default(),
        },
        "TIMER" => RecordTrigger::Timer {
            interval_seconds: data
                .get("dTimer")
                .and_then(|v| v.get("iIntervalSeconds"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
        },
        "GPIO" => {
            let d = data.get("dGPIO").context("missing dGPIO")?;
            RecordTrigger::Gpio(GpioTrigger {
                name: d.get("sName").and_then(|v| v.as_str()).map(String::from),
                num: d.get("iNum").and_then(|v| v.as_i64()).map(|n| n as i32),
                state: d
                    .get("sState")
                    .and_then(|v| v.as_str())
                    .unwrap_or("FLOATING")
                    .to_string(),
                signal: d
                    .get("sSignal")
                    .and_then(|v| v.as_str())
                    .unwrap_or("RISING")
                    .to_string(),
                debounce_ms: d
                    .get("iDebounceDurationMs")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0),
            })
        }
        "TTY" => {
            let d = data.get("dTTY").context("missing dTTY")?;
            RecordTrigger::Tty(TtyTrigger {
                name: d
                    .get("sName")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                command: d
                    .get("sCommand")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            })
        }
        "HTTP" => RecordTrigger::Http,
        "ALWAYS_ON" => RecordTrigger::AlwaysOn,
        other => bail!("unknown trigger kind '{other}'"),
    })
}

pub fn trigger_to_json(trigger: &RecordTrigger) -> Result<Value> {
    Ok(match trigger {
        RecordTrigger::InferenceSet { rules } => {
            let inference_set: Vec<Value> = rules.iter().map(detection_rule_to_json).collect();
            json!({
                "sCurrentSelected": "INFERENCE_SET",
                "lInferenceSet": inference_set,
            })
        }
        RecordTrigger::Timer { interval_seconds } => json!({
            "sCurrentSelected": "TIMER",
            "dTimer": { "iIntervalSeconds": interval_seconds },
        }),
        RecordTrigger::Gpio(g) => {
            if g.name.is_none() && g.num.is_none() {
                bail!("GPIO trigger requires either 'name' or 'num'");
            }
            let mut d = serde_json::Map::new();
            if let Some(name) = &g.name {
                d.insert("sName".into(), json!(name));
            }
            if let Some(num) = g.num {
                d.insert("iNum".into(), json!(num));
            }
            d.insert("sState".into(), json!(g.state));
            d.insert("sSignal".into(), json!(g.signal));
            d.insert("iDebounceDurationMs".into(), json!(g.debounce_ms));
            json!({
                "sCurrentSelected": "GPIO",
                "dGPIO": Value::Object(d),
            })
        }
        RecordTrigger::Tty(t) => {
            if t.name.trim().is_empty() || t.command.trim().is_empty() {
                bail!("TTY trigger requires non-empty 'name' and 'command'");
            }
            json!({
                "sCurrentSelected": "TTY",
                "dTTY": { "sName": t.name, "sCommand": t.command },
            })
        }
        RecordTrigger::Http => json!({ "sCurrentSelected": "HTTP" }),
        RecordTrigger::AlwaysOn => json!({ "sCurrentSelected": "ALWAYS_ON" }),
    })
}

fn parse_detection_rule(v: &Value) -> DetectionRule {
    let confidence = v
        .get("lConfidenceFilter")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_f64()).collect())
        .unwrap_or_else(|| vec![0.0, 1.0]);
    let labels: Vec<String> = v
        .get("lClassFilter")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    let regions = v.get("lRegionFilter").and_then(|v| v.as_array()).map(|rs| {
        rs.iter()
            .filter_map(|r| {
                r.get("lPolygon").and_then(|v| v.as_array()).map(|pts| {
                    pts.iter()
                        .filter_map(|pt| {
                            pt.as_array()
                                .map(|coords| coords.iter().filter_map(|c| c.as_f64()).collect())
                        })
                        .collect()
                })
            })
            .collect()
    });
    DetectionRule {
        name: v
            .get("sID")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        debounce_times: v
            .get("iDebounceTimes")
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as i32,
        confidence_range_filter: confidence,
        label_filter: labels,
        region_filter: regions,
    }
}

fn detection_rule_to_json(rule: &DetectionRule) -> Value {
    // Empty or missing region_filter => full-frame polygon.
    let full_region: Vec<Vec<f64>> = vec![
        vec![0.0, 0.0],
        vec![1.0, 0.0],
        vec![1.0, 1.0],
        vec![0.0, 1.0],
    ];
    let regions = rule
        .region_filter
        .as_ref()
        .filter(|r| !r.is_empty())
        .cloned()
        .unwrap_or_else(|| vec![full_region]);
    json!({
        "sID": rule.name,
        "iDebounceTimes": rule.debounce_times,
        "lConfidenceFilter": rule.confidence_range_filter,
        "lClassFilter": rule.label_filter,
        "lRegionFilter": regions.iter().map(|poly| json!({"lPolygon": poly})).collect::<Vec<_>>(),
    })
}

// MARK: Tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_roundtrip_inference() {
        let t = RecordTrigger::InferenceSet {
            rules: vec![DetectionRule {
                name: "person".into(),
                debounce_times: 3,
                confidence_range_filter: vec![0.5, 1.0],
                label_filter: vec!["person".into()],
                region_filter: None,
            }],
        };
        let j = trigger_to_json(&t).unwrap();
        let back = parse_trigger(&j).unwrap();
        match back {
            RecordTrigger::InferenceSet { rules } => {
                assert_eq!(rules.len(), 1);
                assert_eq!(rules[0].name, "person");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn trigger_roundtrip_timer() {
        let t = RecordTrigger::Timer {
            interval_seconds: 60,
        };
        let j = trigger_to_json(&t).unwrap();
        assert_eq!(j["sCurrentSelected"], "TIMER");
        let back = parse_trigger(&j).unwrap();
        matches!(
            back,
            RecordTrigger::Timer {
                interval_seconds: 60
            }
        );
    }

    #[test]
    fn trigger_roundtrip_gpio() {
        let t = RecordTrigger::Gpio(GpioTrigger {
            name: Some("GPIO_01".into()),
            num: None,
            state: "FLOATING".into(),
            signal: "RISING".into(),
            debounce_ms: 100,
        });
        let j = trigger_to_json(&t).unwrap();
        assert_eq!(j["sCurrentSelected"], "GPIO");
        assert_eq!(j["dGPIO"]["sName"], "GPIO_01");
        let back = parse_trigger(&j).unwrap();
        match back {
            RecordTrigger::Gpio(g) => {
                assert_eq!(g.name.as_deref(), Some("GPIO_01"));
                assert_eq!(g.state, "FLOATING");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn trigger_roundtrip_http_always() {
        let j = trigger_to_json(&RecordTrigger::Http).unwrap();
        assert_eq!(j["sCurrentSelected"], "HTTP");
        let j = trigger_to_json(&RecordTrigger::AlwaysOn).unwrap();
        assert_eq!(j["sCurrentSelected"], "ALWAYS_ON");
    }

    #[test]
    fn gpio_trigger_requires_name_or_num() {
        let t = RecordTrigger::Gpio(GpioTrigger {
            name: None,
            num: None,
            state: "FLOATING".into(),
            signal: "RISING".into(),
            debounce_ms: 0,
        });
        assert!(trigger_to_json(&t).is_err());
    }
}
