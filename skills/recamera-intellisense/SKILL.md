---
name: recamera-intellisense
description: Manages reCamera devices via MCP server tools — registers devices, configures AI detection models/rules/schedules, queries detection events, fetches event snapshots, captures images/video, and controls GPIO pins. Triggers on camera onboarding, detection setup, event monitoring, image capture, GPIO control, or any reCamera task.
metadata: {
  "openclaw": {
    "emoji": "📷",
    "version": "2.0.0"
  }
}
user-invocable: true
---

# reCamera Intellisense

MCP server providing tools for [reCamera V2](https://wiki.seeedstudio.com/recamera/) device management, AI detection, image/video capture, and GPIO control.

## Setup

The MCP server binary (`recamera-intellisense-mcp`) must be installed and configured.

**Check if already installed:**

```bash
python3 scripts/setup-mcp.py --check
```

Exits 0 and prints the binary path if installed; exits 1 otherwise.

**Install (non-interactive — recommended for agents):**

```bash
curl -fsSL https://raw.githubusercontent.com/iChizer0/reCamera-Intellisense/main/scripts/setup-mcp.py | python3 - --yes
```

The `--yes` flag auto-configures all detected MCP clients without prompting. Installs to `~/.recamera/bin/`. The last output line is `BINARY_PATH=<path>` for easy parsing.

Note you can also manually configure the MCP server with the printed binary path if it's already installed but not configured for you.

## Tool overview

All tools are exposed through the `recamera` MCP server. Register a device first with `recamera:add_device` before using device-specific tools.

- **Device**: `detect_local_device`, `add_device`, `update_device`, `remove_device`, `get_device`, `list_devices`
- **Detection**: `get_detection_models_info`, `get_detection_model`, `set_detection_model`, `get_detection_schedule`, `set_detection_schedule`, `get_detection_rules`, `set_detection_rules`, `get_detection_events`, `clear_detection_events`
- **Capture**: `get_capture_status`, `start_capture`, `stop_capture`, `capture_image`
- **File**: `fetch_file`, `delete_file`
- **GPIO**: `list_gpios`, `get_gpio_info`, `set_gpio_value`, `get_gpio_value`

## Rules

1. Auth token format: `sk_...` (from reCamera Web Console → Device Info → Connection Settings).
2. Always pass `device_name` — the name given when the device was registered with `add_device`.
3. Prefer metadata before images: use `get_detection_events` first; call `fetch_file` with `snapshot_path` only when the image is needed.
4. `fetch_file` returns images inline, text as text, and skips video/large files (>5 MB). Use the file path on device for large data.
5. Tool errors return `is_error: true` with an actionable message. Surface the message and suggest one concrete fix.

## Workflows

### Onboard a device

1. `add_device` with `name`, `host`, `token` (connectivity is tested automatically).
2. `list_devices` to verify registration.

### Configure object detection by label name

1. `get_detection_models_info` → find the model containing the target label.
2. `set_detection_model` by `model_id` or `model_name`.
3. `set_detection_rules` with `label_filter` containing label names.
4. `clear_detection_events` to reset the event log.

### Monitor detection events

1. Call `get_detection_events` with `start_unix_ms` set to now.
2. Track the last `timestamp_unix_ms` returned; pass it as `start_unix_ms` on next call.
3. Fetch snapshots only when needed: `fetch_file` with `snapshot_path` from the event.

### On-demand image capture

`capture_image` captures a JPG and returns both metadata and the image inline.

### GPIO control

1. `list_gpios` to discover pins and capabilities.
2. `set_gpio_value` to write (auto-configures as output).
3. `get_gpio_value` to read (auto-configures as input with debounce).

## Troubleshooting

| Symptom | Fix |
|---|---|
| 401/403 auth error | Re-copy token from reCamera Web Console |
| Timeout / connection refused | Verify host, network path, device power |
| `'device_name' must not be empty` | Pass the device name from `add_device`; run `list_devices` to check |
| Empty detection rules or events | Check that a model is active (`get_detection_model`) and storage is enabled |
| Image fetch failed | Use a fresh `snapshot_path` from `get_detection_events`; old data may rotate out |
| Schedule rejected | Use `Day HH:MM:SS` format (e.g. `Mon 08:00:00`) |
| GPIO value rejected | Value must be 0 or 1; verify `pin_id` via `list_gpios` |
