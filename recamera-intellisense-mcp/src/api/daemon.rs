//! Daemon-side endpoints served by `recamera-intellisense-daemon` over the
//! device's HTTP surface. Distinct from the Record HTTP API: the daemon
//! exposes the intellisense event store and an absolute-path file fetch under
//! a pre-configured allowed prefix, intended for quick access to capture
//! outputs and detection-event snapshots.

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::api_client::ApiClient;
use crate::types::{DetectionEvent, DetectionFileEvent, DeviceRecord};

// MARK: Event store

fn required_u64(item: &Value, key: &str) -> Result<u64> {
    item.get(key)
        .and_then(Value::as_u64)
        .with_context(|| format!("'{key}' must be an unsigned 64-bit integer"))
}

fn required_string<'a>(item: &'a Value, key: &str) -> Result<&'a str> {
    item.get(key)
        .and_then(Value::as_str)
        .with_context(|| format!("'{key}' must be a string"))
}

fn optional_dst(item: &Value) -> Result<Option<Option<bool>>> {
    match item.get("dst") {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(Value::Bool(value)) => Ok(Some(Some(*value))),
        Some(_) => bail!("'dst' must be boolean or null"),
    }
}

type TimezoneProjection = (Option<String>, Option<i64>, Option<Option<bool>>);

fn optional_timezone(item: &Value) -> Result<TimezoneProjection> {
    let tz = match item.get("tz") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) if !value.is_empty() => Some(value.clone()),
        Some(_) => bail!("'tz' must be a nonempty string when present"),
    };
    let offset = match item.get("tz_offset") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_i64()
                .context("'tz_offset' must be a signed 64-bit integer")?,
        ),
    };
    Ok((tz, offset, optional_dst(item)?))
}

fn parse_file_ids(item: &Value) -> Result<Option<Vec<u64>>> {
    let Some(value) = item.get("file_ids") else {
        return Ok(None);
    };
    let ids = value.as_array().context("'file_ids' must be an array")?;
    ids.iter()
        .map(|id| {
            id.as_u64()
                .context("every 'file_ids' entry must be an unsigned 64-bit integer")
        })
        .collect::<Result<Vec<_>>>()
        .map(Some)
}

fn parse_file_event(item: &Value) -> Result<DetectionFileEvent> {
    let ts_ms = required_u64(item, "timestamp")?;
    let event_uid = required_u64(item, "event_uid")?;
    let file_id = item
        .get("file_id")
        .map(|_| required_u64(item, "file_id"))
        .transpose()?;
    let attributes = match item.get("attributes") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) => Some(value.clone()),
        Some(_) => bail!("'attributes' must be a string or null"),
    };
    let (tz, tz_offset, dst) = optional_timezone(item)?;
    Ok(DetectionFileEvent {
        op: item
            .get("op")
            .map(|_| required_string(item, "op"))
            .transpose()?
            .unwrap_or("")
            .to_string(),
        path: required_string(item, "path")?.to_string(),
        size: required_u64(item, "size")?,
        attributes,
        event_uid,
        event_uid_str: event_uid.to_string(),
        file_id,
        file_id_str: file_id.map(|id| id.to_string()),
        timestamp: crate::util::unix_ms_to_iso8601(ts_ms),
        timestamp_unix_ms: ts_ms,
        tz,
        tz_offset,
        dst,
    })
}

fn parse_file_events(item: &Value) -> Result<Vec<DetectionFileEvent>> {
    if let Some(value) = item.get("file_events") {
        let files = value.as_array().context("'file_events' must be an array")?;
        return files
            .iter()
            .enumerate()
            .map(|(index, item)| {
                parse_file_event(item)
                    .with_context(|| format!("invalid file event at index {index}"))
            })
            .collect();
    }
    // Compatibility with older daemons, which only expose file_event.
    match item.get("file_event") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(file) => Ok(vec![
            parse_file_event(file).context("invalid legacy file_event")?
        ]),
    }
}

fn parse_detection_event(item: &Value) -> Result<DetectionEvent> {
    let ts_ms = required_u64(item, "timestamp")?;
    let uid = required_u64(item, "uid")?;
    let event_id = item.get("id").and_then(|v| v.as_str()).unwrap_or("").trim();
    let rule_type = required_string(item, "type")?;
    let rule_name = if !event_id.is_empty() {
        event_id.to_string()
    } else {
        rule_type.to_string()
    };
    let file_events = parse_file_events(item)?;
    let snapshot_path = file_events
        .first()
        .map(|file| file.path.clone())
        .filter(|path| !path.is_empty());
    let file_ids = parse_file_ids(item)?;
    let (tz, tz_offset, dst) = optional_timezone(item)?;
    Ok(DetectionEvent {
        timestamp: crate::util::unix_ms_to_iso8601(ts_ms),
        timestamp_unix_ms: ts_ms,
        rule_name,
        uid: Some(uid),
        uid_str: Some(uid.to_string()),
        file_id_strs: file_ids
            .as_ref()
            .map(|ids| ids.iter().map(|id| id.to_string()).collect()),
        file_ids,
        file_events,
        tz,
        tz_offset,
        dst,
        snapshot_path,
    })
}

pub async fn get_events(
    client: &ApiClient,
    device: &DeviceRecord,
    start_unix_ms: Option<u64>,
    end_unix_ms: Option<u64>,
) -> Result<Vec<DetectionEvent>> {
    let mut params: Vec<(&str, String)> = Vec::new();
    if let Some(s) = start_unix_ms {
        params.push(("start", s.to_string()));
    }
    if let Some(e) = end_unix_ms {
        params.push(("end", e.to_string()));
    }
    let params_ref: Vec<(&str, &str)> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let params_opt = if params_ref.is_empty() {
        None
    } else {
        Some(params_ref.as_slice())
    };
    let raw = client
        .get_json(device, "/api/v1/intellisense/events", params_opt)
        .await?;
    let items = raw
        .as_array()
        .or_else(|| raw.get("events").and_then(Value::as_array))
        .context("expected an array of events or {'events': [...]}")?;
    items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            parse_detection_event(item)
                .with_context(|| format!("invalid detection event at index {index}"))
        })
        .collect()
}

pub async fn clear_events(client: &ApiClient, device: &DeviceRecord) -> Result<()> {
    let resp = client
        .post_json(device, "/api/v1/intellisense/events/clear", None, None)
        .await?;
    let status = resp.get("status").and_then(|v| v.as_str()).unwrap_or("");
    anyhow::ensure!(
        status.eq_ignore_ascii_case("ok"),
        "clear events failed: {}",
        resp.get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error")
    );
    Ok(())
}

// MARK: File fetch / delete (daemon allowed-prefix)

pub async fn fetch_file(
    client: &ApiClient,
    device: &DeviceRecord,
    remote_path: &str,
) -> Result<Vec<u8>> {
    client
        .get_bytes(device, "/api/v1/file", Some(&[("path", remote_path)]))
        .await
}

pub async fn delete_file(
    client: &ApiClient,
    device: &DeviceRecord,
    remote_path: &str,
) -> Result<()> {
    client
        .delete(device, "/api/v1/file", Some(&[("path", remote_path)]))
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn detection_projection_retains_files_exact_ids_and_timezone_nulls() {
        let max = u64::MAX;
        let raw = json!({
            "event":"RULE", "type":"INFERENCE_SET", "id":"front-door",
            "uid":max, "timestamp":1_700_000_000_000_u64,
            "file_ids":[max - 1, max], "tz":"Asia/Shanghai",
            "tz_offset":28_800, "dst":false,
            "file_events":[{
                "op":"ADDED", "path":"/mnt/sdcard/photo.jpg", "size":max,
                "attributes":"COMPLETED", "event_uid":max, "file_id":max - 1,
                "timestamp":1_700_000_000_100_u64, "tz":"America/New_York",
                "tz_offset":-18_000, "dst":false
            }, {
                "op":"ADDED", "path":"/mnt/sdcard/video.mp4", "size":200,
                "attributes":"COMPLETED", "event_uid":0, "file_id":max,
                "timestamp":max, "tz_offset":-14_400, "dst":null
            }]
        });
        let event = parse_detection_event(&raw).unwrap();
        assert_eq!(event.uid, Some(max));
        assert_eq!(event.uid_str, Some(max.to_string()));
        assert_eq!(event.file_ids, Some(vec![max - 1, max]));
        assert_eq!(
            event.file_id_strs,
            Some(vec![(max - 1).to_string(), max.to_string()])
        );
        assert_eq!(event.timestamp_unix_ms, 1_700_000_000_000);
        assert_eq!(event.tz_offset, Some(28_800));
        assert_eq!(event.dst, Some(Some(false)));
        assert_eq!(
            event.snapshot_path.as_deref(),
            Some("/mnt/sdcard/photo.jpg")
        );
        assert_eq!(event.file_events.len(), 2);
        assert_eq!(event.file_events[0].size, max);
        assert_eq!(event.file_events[0].file_id, Some(max - 1));
        assert_eq!(event.file_events[0].event_uid, max);
        assert_eq!(
            event.file_events[0].file_id_str.as_deref(),
            Some("18446744073709551614")
        );
        assert_eq!(event.file_events[0].timestamp, "2023-11-14T22:13:20.100Z");
        assert_eq!(event.file_events[1].event_uid, 0);
        assert_eq!(event.file_events[1].file_id, Some(max));
        assert_eq!(event.file_events[1].timestamp_unix_ms, max);
        assert_eq!(
            event.file_events[1].timestamp,
            "+584556019-04-03T14:25:51.615Z"
        );
        assert_eq!(event.file_events[1].tz_offset, Some(-14_400));
        assert_eq!(event.file_events[1].dst, Some(None));

        let output = serde_json::to_value(event).unwrap();
        assert!(output["file_events"][1].get("tz").is_none());
        assert!(output["file_events"][1]["dst"].is_null());
        let roundtrip: DetectionEvent = serde_json::from_value(output.clone()).unwrap();
        assert_eq!(roundtrip.file_events[1].dst, Some(None));
        assert_eq!(
            serde_json::to_value(roundtrip).unwrap()["file_events"][1].get("dst"),
            Some(&Value::Null)
        );
    }

    #[test]
    fn invalid_projection_fields_do_not_become_valid_metadata() {
        let raw = json!({
            "type":"INFERENCE", "id":"invalid", "uid":1, "timestamp":1000,
            "file_ids":[1, null, 3], "dst":"false",
            "file_event":{"path":"/mnt/sdcard/incomplete.jpg"}
        });
        let error = parse_detection_event(&raw).unwrap_err();
        let surfaced = anyhow::anyhow!("{error:#}");
        assert!(surfaced.to_string().contains("legacy file_event"));
        assert!(surfaced.to_string().contains("timestamp"));
    }

    #[test]
    fn legacy_projection_keeps_snapshot_and_omits_new_metadata() {
        let raw = json!({
            "type":"INFERENCE", "id":"legacy", "uid":7, "timestamp":1000,
            "file_event":{
                "op":"ADDED", "path":"/mnt/sdcard/legacy.jpg", "size":10,
                "event_uid":7, "timestamp":2000
            }
        });
        let event = parse_detection_event(&raw).unwrap();
        assert_eq!(event.file_events.len(), 1);
        assert_eq!(
            event.snapshot_path.as_deref(),
            Some("/mnt/sdcard/legacy.jpg")
        );
        let output = serde_json::to_value(event).unwrap();
        assert!(output.get("file_ids").is_none());
        assert!(output.get("tz").is_none());
        assert!(output.get("tz_offset").is_none());
        assert!(output.get("dst").is_none());
        assert!(output["file_events"][0].get("file_id").is_none());
    }
}
