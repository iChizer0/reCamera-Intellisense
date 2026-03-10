# API Reference

## Contents

- [rc_device.py](#reCamera-Device-API) — credentials, connectivity, file download (internal)
- [rc_detection.py](#reCamera-Detection-API) — models, schedules, rules, events, snapshots
- [rc_capture.py](#reCamera-Capture-API) — capture status, start/stop, one-shot image

## reCamera Device API

| Function | Signature | Returns | Errors |
|---|---|---|---|
| `detect_local_device` | `(host="127.0.0.1")` | `Optional[str]` — host or `None` | — |
| `add_device` | `(name, host, token, protocol="http", allow_unsecured=True, port=None)` | `None` | `ValueError`, `ConnectionError`, `RuntimeError` |
| `update_device` | `(name, host=None, token=None, protocol=None, allow_unsecured=None, port=None)` | `None` | `ValueError`, `ConnectionError` |
| `remove_device` | `(name)` | `bool` — `False` if not found | — |
| `get_device` | `(name)` | `Optional[DeviceRecord]` `{name,host,token,protocol,allow_unsecured,port}` | — |
| `list_devices` | `()` | `List[DeviceRecord]` — sorted | — |
| `fetch_file` *(internal)* | `(device, remote_path)` | `bytes` | `RuntimeError` |

### CLI argument schemas

```text
add_device(name, host, token, [protocol], [allow_unsecured], [port])
update_device(name, [host], [token], [protocol], [allow_unsecured], [port])
remove_device(name)
get_device(name)
detect_local_device()
list_devices()
```

## reCamera Detection API

All commands require exactly one of `device_name` (preferred) or inline `device`.

| Function | Signature | Returns |
|---|---|---|
| `get_detection_models_info` | `(device)` | `List[DetectionModel]` — IDs/names/labels |
| `get_detection_model` | `(device)` | `Optional[DetectionModel]` — active model |
| `set_detection_model` | `(device, model_id=None, model_name=None)` | `None` — exactly one param required |
| `get_detection_schedule` | `(device)` | `Optional[DetectionSchedule]` |
| `set_detection_schedule` | `(device, schedule\|None)` | `None` — format `Day HH:MM:SS`; `ValueError` on bad format |
| `get_detection_rules` | `(device)` | `List[DetectionRule]` — may be empty if prereqs disabled |
| `set_detection_rules` | `(device, rules)` | `None` — auto-enables record-image and default storage |
| `get_detection_events` | `(device, start_unix_ms=None, end_unix_ms=None)` | `List[DetectionEvent]` — with optional `snapshot_path` |
| `clear_detection_events` | `(device)` | `None` |
| `fetch_detection_event_image` | `(device, snapshot_path)` | `bytes` (Python); CLI requires `local_save_path`, returns `{saved_path, bytes}` |

### CLI argument schemas

```text
All: exactly one of device_name or device
set_detection_model(model_id | model_name)
set_detection_schedule([schedule | null])
set_detection_rules(rules)
get_detection_events([start_unix_ms], [end_unix_ms])
clear_detection_events()
fetch_detection_event_image(snapshot_path, local_save_path)
```

## reCamera Capture API

All commands require exactly one of `device_name` (preferred) or inline `device`.

| Function | Signature | Returns |
|---|---|---|
| `get_capture_status` | `(device)` | `CaptureStatus` — `last_capture`, `ready_to_start_new`, `stop_requested` |
| `start_capture` | `(device, output=..., format="JPG", video_length_seconds=None)` | `CaptureEvent` — JPG/RAW/MP4 |
| `stop_capture` | `(device)` | `None` |
| `capture_image` | `(device, output=..., poll_interval=0.5, poll_timeout=5)` | `CaptureResult` — Python: `content`, `content_bytes`; CLI requires `local_save_path`, returns `{capture, saved_path, bytes}` |

### CLI argument schemas

```text
All: exactly one of device_name or device
start_capture([output], [format], [video_length_seconds])
capture_image(local_save_path, [output], [poll_interval], [poll_timeout])
get_capture_status()
stop_capture()
```

## Common runtime failures

All scripts raise `RuntimeError("Failed to ...")` for network/API/file errors.
