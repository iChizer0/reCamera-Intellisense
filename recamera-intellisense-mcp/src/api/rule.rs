use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::api::expect_ok;
use crate::api_client::ApiClient;
use crate::types::{
    AvailableGpio, AvailableTty, DetectionRule, DeviceRecord, GpioTrigger, GpioTriggerSignal,
    GpioTriggerState, RecordTrigger, RuleConfig, RuleEvent, RuleEventOwner, RuleInfo,
    ScheduleRange, TtyTrigger, WriterConfig,
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
    // Fetch the current config first so we can preserve sibling sub-objects
    // that belong to the *other* trigger kinds. Some clients (and the Web
    // Console) remember their last-known TIMER / GPIO / TTY / INFERENCE_SET
    // settings and expect them to survive a kind switch. A GET failure is
    // non-fatal — we fall back to a minimal payload so setting a trigger
    // still works on a freshly provisioned device.
    let current = client.get_json(device, PATH_RECORD_RULE, None).await.ok();
    let payload = merge_trigger_payload(current.as_ref(), trigger)?;
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
            let state_str = d
                .get("sState")
                .and_then(|v| v.as_str())
                .unwrap_or("FLOATING");
            let state = match state_str {
                "DISABLED" => GpioTriggerState::Disabled,
                "PULL_UP" => GpioTriggerState::PullUp,
                "PULL_DOWN" => GpioTriggerState::PullDown,
                _ => GpioTriggerState::Floating,
            };
            let signal_str = d
                .get("sSignal")
                .and_then(|v| v.as_str())
                .unwrap_or("RISING");
            let signal = match signal_str {
                "HIGH" => GpioTriggerSignal::High,
                "LOW" => GpioTriggerSignal::Low,
                "FALLING" => GpioTriggerSignal::Falling,
                _ => GpioTriggerSignal::Rising,
            };
            RecordTrigger::Gpio(GpioTrigger {
                name: d.get("sName").and_then(|v| v.as_str()).map(String::from),
                num: d.get("iNum").and_then(|v| v.as_i64()).map(|n| n as i32),
                state,
                signal,
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

/// Keys on `/record-rule-config` that are owned by **one specific** trigger
/// kind. When switching kinds, we copy forward every such key that the device
/// already has so other clients' remembered settings survive.
const TRIGGER_SIBLING_KEYS: &[&str] = &["lInferenceSet", "dTimer", "dGPIO", "dTTY"];

/// Return the `(sCurrentSelected, [(key, value), ...])` patch that fully
/// describes a trigger. Patch entries overwrite any pre-existing value for
/// the same key in the current config.
fn trigger_patch(trigger: &RecordTrigger) -> Result<(&'static str, Vec<(&'static str, Value)>)> {
    Ok(match trigger {
        RecordTrigger::InferenceSet { rules } => {
            let inference_set: Vec<Value> = rules.iter().map(detection_rule_to_json).collect();
            (
                "INFERENCE_SET",
                vec![("lInferenceSet", Value::Array(inference_set))],
            )
        }
        RecordTrigger::Timer { interval_seconds } => (
            "TIMER",
            vec![("dTimer", json!({ "iIntervalSeconds": interval_seconds }))],
        ),
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
            d.insert("sState".into(), json!(g.state.as_str()));
            d.insert("sSignal".into(), json!(g.signal.as_str()));
            d.insert("iDebounceDurationMs".into(), json!(g.debounce_ms));
            ("GPIO", vec![("dGPIO", Value::Object(d))])
        }
        RecordTrigger::Tty(t) => {
            if t.name.trim().is_empty() || t.command.trim().is_empty() {
                bail!("TTY trigger requires non-empty 'name' and 'command'");
            }
            (
                "TTY",
                vec![("dTTY", json!({ "sName": t.name, "sCommand": t.command }))],
            )
        }
        RecordTrigger::Http => ("HTTP", vec![]),
        RecordTrigger::AlwaysOn => ("ALWAYS_ON", vec![]),
    })
}

/// Build the POST payload by starting from a copy of the sibling sub-objects
/// in `current` (if any) and layering the patch for the requested kind on
/// top. This is the merge semantics used by [`set_trigger`].
pub fn merge_trigger_payload(current: Option<&Value>, trigger: &RecordTrigger) -> Result<Value> {
    let (kind, patch) = trigger_patch(trigger)?;
    let mut out = serde_json::Map::new();

    // Carry forward the allowlisted sibling sub-objects so they are not lost
    // when the device persists the new config. Only known keys are copied to
    // avoid round-tripping server-only metadata.
    if let Some(obj) = current.and_then(|v| v.as_object()) {
        for key in TRIGGER_SIBLING_KEYS {
            if let Some(v) = obj.get(*key) {
                out.insert((*key).to_string(), v.clone());
            }
        }
    }

    out.insert("sCurrentSelected".into(), Value::String(kind.to_string()));
    for (k, v) in patch {
        // Patch values overwrite carried-forward siblings for the selected kind.
        out.insert(k.to_string(), v);
    }
    Ok(Value::Object(out))
}

/// Backwards-compatible shape: equivalent to [`merge_trigger_payload`] with
/// no prior config (i.e. a full replace). Retained for tests that exercise
/// the round-trip against [`parse_trigger`].
#[cfg(test)]
pub fn trigger_to_json(trigger: &RecordTrigger) -> Result<Value> {
    merge_trigger_payload(None, trigger)
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
            state: GpioTriggerState::Floating,
            signal: GpioTriggerSignal::Rising,
            debounce_ms: 100,
        });
        let j = trigger_to_json(&t).unwrap();
        assert_eq!(j["sCurrentSelected"], "GPIO");
        assert_eq!(j["dGPIO"]["sName"], "GPIO_01");
        assert_eq!(j["dGPIO"]["sState"], "FLOATING");
        assert_eq!(j["dGPIO"]["sSignal"], "RISING");
        let back = parse_trigger(&j).unwrap();
        match back {
            RecordTrigger::Gpio(g) => {
                assert_eq!(g.name.as_deref(), Some("GPIO_01"));
                assert_eq!(g.state, GpioTriggerState::Floating);
                assert_eq!(g.signal, GpioTriggerSignal::Rising);
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
            state: GpioTriggerState::Floating,
            signal: GpioTriggerSignal::Rising,
            debounce_ms: 0,
        });
        assert!(trigger_to_json(&t).is_err());
    }

    #[test]
    fn gpio_trigger_parse_tolerates_unknown_strings() {
        // Device response with stale/unknown state should still parse (default to Floating/Rising).
        let j = json!({
            "sCurrentSelected": "GPIO",
            "dGPIO": {
                "sName": "GPIO_03",
                "sState": "BOGUS",
                "sSignal": "???",
                "iDebounceDurationMs": 50
            }
        });
        let back = parse_trigger(&j).unwrap();
        match back {
            RecordTrigger::Gpio(g) => {
                assert_eq!(g.state, GpioTriggerState::Floating);
                assert_eq!(g.signal, GpioTriggerSignal::Rising);
            }
            _ => panic!("wrong variant"),
        }
    }

    /// Simulates a device config that already remembers every trigger kind;
    /// switching to TIMER must preserve `dGPIO`, `dTTY`, and `lInferenceSet`.
    fn rich_current() -> Value {
        json!({
            "sCurrentSelected": "GPIO",
            "lInferenceSet": [{
                "sID": "legacy-rule",
                "iDebounceTimes": 5,
                "lConfidenceFilter": [0.4, 1.0],
                "lClassFilter": ["person"],
                "lRegionFilter": []
            }],
            "dTimer": { "iIntervalSeconds": 120 },
            "dGPIO": {
                "sName": "GPIO_07",
                "sState": "PULL_UP",
                "sSignal": "FALLING",
                "iDebounceDurationMs": 50
            },
            "dTTY": { "sName": "tty0", "sCommand": "SHOOT" },
            "bSomeServerOnlyField": true
        })
    }

    #[test]
    fn merge_preserves_other_kinds_when_switching_to_timer() {
        let current = rich_current();
        let payload = merge_trigger_payload(
            Some(&current),
            &RecordTrigger::Timer {
                interval_seconds: 30,
            },
        )
        .unwrap();
        assert_eq!(payload["sCurrentSelected"], "TIMER");
        // The selected sub-object is overwritten with the new interval.
        assert_eq!(payload["dTimer"]["iIntervalSeconds"], 30);
        // Other trigger kinds' sub-objects are preserved verbatim.
        assert_eq!(payload["dGPIO"]["sName"], "GPIO_07");
        assert_eq!(payload["dGPIO"]["sState"], "PULL_UP");
        assert_eq!(payload["dTTY"]["sCommand"], "SHOOT");
        assert_eq!(payload["lInferenceSet"][0]["sID"], "legacy-rule");
        // Server-only metadata is NOT round-tripped.
        assert!(payload.get("bSomeServerOnlyField").is_none());
    }

    #[test]
    fn merge_overwrites_only_selected_kind() {
        let current = rich_current();
        // Switching to GPIO replaces dGPIO but keeps dTTY / dTimer / lInferenceSet.
        let payload = merge_trigger_payload(
            Some(&current),
            &RecordTrigger::Gpio(GpioTrigger {
                name: None,
                num: Some(3),
                state: GpioTriggerState::PullDown,
                signal: GpioTriggerSignal::Rising,
                debounce_ms: 10,
            }),
        )
        .unwrap();
        assert_eq!(payload["sCurrentSelected"], "GPIO");
        assert_eq!(payload["dGPIO"]["iNum"], 3);
        assert_eq!(payload["dGPIO"]["sState"], "PULL_DOWN");
        // Old dGPIO.sName must be gone since the patch fully replaces dGPIO.
        assert!(payload["dGPIO"].get("sName").is_none());
        // Siblings survive.
        assert_eq!(payload["dTimer"]["iIntervalSeconds"], 120);
        assert_eq!(payload["dTTY"]["sName"], "tty0");
        assert_eq!(payload["lInferenceSet"][0]["sID"], "legacy-rule");
    }

    #[test]
    fn merge_http_and_always_on_only_flip_tag() {
        let current = rich_current();
        let http = merge_trigger_payload(Some(&current), &RecordTrigger::Http).unwrap();
        assert_eq!(http["sCurrentSelected"], "HTTP");
        assert_eq!(http["dGPIO"]["sName"], "GPIO_07");
        assert_eq!(http["dTimer"]["iIntervalSeconds"], 120);

        let always = merge_trigger_payload(Some(&current), &RecordTrigger::AlwaysOn).unwrap();
        assert_eq!(always["sCurrentSelected"], "ALWAYS_ON");
        assert_eq!(always["dTTY"]["sCommand"], "SHOOT");
    }

    #[test]
    fn merge_without_current_matches_full_replace() {
        // No prior state → payload contains only the selected kind's keys.
        let payload = merge_trigger_payload(
            None,
            &RecordTrigger::Timer {
                interval_seconds: 15,
            },
        )
        .unwrap();
        let obj = payload.as_object().unwrap();
        assert_eq!(obj.len(), 2);
        assert_eq!(payload["sCurrentSelected"], "TIMER");
        assert_eq!(payload["dTimer"]["iIntervalSeconds"], 15);
    }
}
