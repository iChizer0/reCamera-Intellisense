---
name: recamera-intellisense
description: Registers reCamera devices, configures AI detection models/rules/schedules, monitors and clears detection events, fetches event snapshots, runs manual image/video capture, and controls GPIO pins. Uses local Python CLI scripts with JSON I/O. Triggers on camera onboarding, detection setup, event polling, snapshot capture, GPIO control, or reCamera automation tasks.
metadata: {
  "openclaw": {
    "emoji": "📷",
    "requires": {
      "bins":["python3"]
    }
  },
  "version": "1.0.2"
}
user-invocable: true
---

# reCamera Intellisense

## Requirements

- `python3` (no external packages)
- Reachable reCamera HTTP API (default port `80`)
- Credentials stored in `~/.recamera/devices.json`

## Scripts

All scripts live under `{baseDir}/scripts` and accept **one JSON object** as CLI argument (optional for `detect_local_device` and `list_devices`).

- **`rc_device.py`**: add/update/remove/list/get device credentials, file download
- **`rc_detection.py`**: models, schedule, rules, events, event-image fetch
- **`rc_capture.py`**: capture status/start/stop, one-shot image capture
- **`rc_gpio.py`**: GPIO pin listing, info, set/get value (auto-configures direction)
- **`rc_common.py`**: shared HTTP helpers, JSON serialization, argument validation

**Full API signatures and CLI schemas**: See [REFERENCE.md](REFERENCE.md)

## Agent rules

1. Always pass complete JSON; never use interactive prompts.
2. Use exactly one of `device_name` (preferred) or inline `device`.
3. Auth token format: `sk_xxx` (from Web Console → Device Info → Connection Settings → HTTP/HTTPS Settings).
4. To detect by label name: call `get_detection_models_info`, map name → label index, use index in `label_filter`.
5. Poll `get_detection_events` every 1–10s; pass `start_unix_ms` for incremental reads.
6. Prefer event metadata first; fetch images only when needed.
7. CLI output: success = JSON on stdout (mutating commands may produce no stdout, check exit code `0`); failure = actionable stderr. On error, surface stderr and provide one concrete fix.

## Execution checklist

Copy and track for multi-step tasks:

```text
reCamera Task Progress
- [ ] Resolve device (device_name or inline device)
- [ ] Validate JSON arguments
- [ ] Run CLI command
- [ ] If polling, checkpoint start_unix_ms
- [ ] Handle errors with one fix suggestion
```

## CLI quickstart

Run from `{baseDir}`:

```bash
python3 scripts/rc_device.py add_device '{"name":"cam1","host":"192.168.1.100","token":"sk_xxxxxxxx"}'
python3 scripts/rc_device.py list_devices
python3 scripts/rc_detection.py get_detection_models_info '{"device_name":"cam1"}'
python3 scripts/rc_detection.py set_detection_model '{"device_name":"cam1","model_id":0}'
python3 scripts/rc_detection.py get_detection_events '{"device_name":"cam1"}'
python3 scripts/rc_detection.py clear_detection_events '{"device_name":"cam1"}'
python3 scripts/rc_detection.py fetch_detection_event_image '{"device_name":"cam1","snapshot_path":"/mnt/.../event.jpg","local_save_path":"./event.jpg"}'
python3 scripts/rc_capture.py capture_image '{"device_name":"cam1","local_save_path":"./capture.jpg"}'
python3 scripts/rc_gpio.py list_gpios '{"device_name":"cam1"}'
python3 scripts/rc_gpio.py get_gpio_info '{"device_name":"cam1","pin_id":42}'
python3 scripts/rc_gpio.py set_gpio_value '{"device_name":"cam1","pin_id":42,"value":1}'
python3 scripts/rc_gpio.py get_gpio_value '{"device_name":"cam1","pin_id":42}'
```

## Python pattern (long-running automation)

```python
from datetime import datetime, timezone
import sys
sys.path.append("./scripts")

from rc_device import get_device
from rc_detection import get_detection_events
from rc_gpio import list_gpios, set_gpio_value, get_gpio_value

device = get_device("cam1")
events = get_detection_events(device, start_unix_ms=int(datetime.now(timezone.utc).timestamp() * 1000))
pins = list_gpios(device)
set_gpio_value(device, pin_id=42, value=1)
current = get_gpio_value(device, pin_id=42)
```

Use a loop with checkpointed `start_unix_ms` for incremental polling.

## Workflows

### Onboard a device

1. `add_device` with host + token.
2. `list_devices` to verify.

### Configure object detection by name

1. `get_detection_models_info` → map object name to label index.
2. `set_detection_model`.
3. `set_detection_rules` with `label_filter` containing the index.
4. `clear_detection_events` to start fresh.

### Monitor events

1. Poll `get_detection_events` with `start_unix_ms` every 1–10s.
2. Track last timestamp for next poll.
3. Fetch images only when needed via `fetch_detection_event_image`.

### On-demand snapshot

- **CLI**: `capture_image` with `local_save_path` → returns `{capture, saved_path, bytes}`.
- **Python**: `capture_image` → persist returned `content` bytes.
- **Alternative**: `fetch_detection_event_image` with `local_save_path`.

### GPIO control

1. `list_gpios` to discover available pins and their capabilities.
2. `set_gpio_value` to set a pin high/low (auto-configures as output).
3. `get_gpio_value` to read a pin (auto-configures as input with debounce).
4. `get_gpio_info` for detailed pin state/settings.

## Troubleshooting

| Symptom | Fix |
|---|---|
| 401/403 auth error | Re-copy token from Web Console |
| Timeout / connection refused | Verify host, network path, device power |
| Schedule rejected | Use `Day HH:MM:SS` format |
| Empty rules or events | Enable rule/storage prerequisites; check region filter; poll more frequently |
| Image fetch failed | Use fresh `snapshot_path`; data may rotate out |
| Import errors in Python mode | Run from `{baseDir}`; append `./scripts` to `sys.path` |
| GPIO value rejected | Value must be 0 or 1; verify pin_id exists via `list_gpios` |
