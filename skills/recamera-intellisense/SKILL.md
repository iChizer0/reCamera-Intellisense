---
name: recamera-intellisense
description: Controls reCamera V2 devices — registers devices, configures AI detection, monitors events, captures images, manages storage, and drives GPIO. Exposes the same operations through MCP tools (default for agents) and a stdlib-only Python CLI (automation / shell). Triggers on camera onboarding, detection setup, event monitoring, image capture, GPIO control, storage management, or any reCamera task.
metadata: {
  "openclaw": {
    "emoji": "📷",
    "version": "2.0.0",
    "requires": {
      "bins": ["python3"],
      "config_paths": ["~/.recamera/devices.json"]
    }
  }
}
user-invocable: true
---

# reCamera Intellisense

Device management, AI detection, capture, storage, and GPIO for [reCamera V2](https://wiki.seeedstudio.com/recamera/).

## Two transports, one device store

Both transports read/write `~/.recamera/devices.json`, so registering a device once makes it available to both.

| Transport | When to use |
|---|---|
| **MCP tools** | Default for interactive agent turns — structured arguments, typed errors, inline image results. Tool names are the bare operation name (e.g. `add_device`); any `server:` prefix is added by the MCP host. |
| **Python CLI** (`recamera_intellisense`) | Shell scripts, CI, cron, or environments without an MCP host. No install needed — the package is bundled under `scripts/`. |

Operation names are identical across both (e.g. `add_device`, `capture_image`, `set_detection_rules`).

## Setup

### MCP transport

Prefer a two-step **download → review → run** flow so you can inspect the
installer before it touches your system. The installer verifies the downloaded
release asset's SHA-256 against the published digest and aborts on mismatch.

```bash
# 1. Download the installer:
curl -fsSLO https://raw.githubusercontent.com/iChizer0/reCamera-Intellisense/main/scripts/setup-mcp.py
# 2. Inspect it (pager / editor / diff against a pinned SHA):
less setup-mcp.py
# 3. Check if already installed (prints path, exits 0/1):
python3 setup-mcp.py check
# 4. Install (non-interactive) — auto-configures detected MCP clients,
#    installs binary to ~/.recamera/bin/, verifies SHA-256:
python3 setup-mcp.py install --yes
```

Express (piped) form is available for trusted environments:

```bash
curl -fsSL https://raw.githubusercontent.com/iChizer0/reCamera-Intellisense/main/scripts/setup-mcp.py | python3 - install --yes
```

The installer's last line is `BINARY_PATH=<path>` for parsing.

### CLI transport

No install. From the skill directory:

```bash
PYTHONPATH=scripts python3 -m recamera_intellisense <command> '<json-args>'
PYTHONPATH=scripts python3 -m recamera_intellisense list-commands # lists all commands
```

JSON args are a single object whose keys match the command's named parameters. Results print as pretty JSON; errors exit non-zero with a JSON message on stderr.

## Security considerations

- **Installer integrity.** Prefer download-then-review over `curl … | python3`.
  `setup-mcp.py` verifies the release asset's SHA-256 against
  `<asset>.sha256` / `SHA256SUMS` and aborts on mismatch; bypass with
  `--skip-checksum` or `RECAMERA_SKIP_CHECKSUM=1` only when you understand the risk.
- **Credentials** live in `~/.recamera/devices.json`, written atomically with mode
  `0600` by both transports. Do not mix unrelated secrets in that file.
- **HTTPS certificate verification is on by default** (`allow_unsecured=false`).
  Opt into unverified TLS per-device only for self-signed certs on a trusted LAN.
- **Plain HTTP by default** (port 80) — tokens and images traverse the network
  unencrypted. Configure HTTPS on the device before using on untrusted networks.
- **`fetch_file` reads absolute paths via the daemon.** A registered device
  becomes a data-egress point for anything under the daemon's allowed prefix —
  register only devices you control.
- **Record relay URLs are not bearer-protected** for the lifetime of the relay
  (`records.rs` calls out “relay token is bearer-free”). Anyone with the URL on
  the same network can download the file; close the relay when you are done.
- **`set_record_trigger` supports `tty` and `http` triggers** that cause the
  device to execute shell commands or call external endpoints. Only install
  triggers you have reviewed.
- **Trusted networks only.** This skill polls devices and downloads snapshots;
  treat every registered device as a data egress point.
- **Per-camera tokens** (`sk_...` from Web Console → Device Info → Connection Settings). Do not reuse tokens shared with other services.
- **Source review.** Full Python sources are in `scripts/recamera_intellisense/`. Review before granting autonomous execution.

## Operation groups

Register a device first (`add_device`), then pass `device_name` to everything else.

- **Device** — `detect_local_device`, `add_device`, `update_device`, `remove_device`, `get_device`, `list_devices`
- **Detection** (high level) — `get_detection_models_info`, `get_detection_model`, `set_detection_model`, `get_detection_schedule`, `set_detection_schedule`, `get_detection_rules`, `set_detection_rules`, `get_detection_events`, `clear_detection_events`
- **Rule pipeline** (low level) — `get_rule_system_info`, `get_record_config`, `set_record_config`, `get_schedule_rule`, `set_schedule_rule`, `get_record_trigger`, `set_record_trigger`, `activate_http_trigger`
- **Storage** — `get_storage_status`, `set_storage_slot`, `configure_storage_quota`, `storage_task_submit`, `storage_task_status`, `storage_task_cancel`
- **Records** (browse clips) — `list_records`, `fetch_record`
- **Capture** — `get_capture_status`, `start_capture`, `stop_capture`, `capture_image`
- **File** (daemon, absolute paths) — `fetch_file`, `delete_file`, `get_intellisense_events`, `clear_intellisense_events`
- **GPIO** — `list_gpios`, `get_gpio_info`, `set_gpio_value`, `get_gpio_value`

### `fetch_file` vs `fetch_record`

- `fetch_file` — daemon `/api/v1/file`, takes an **absolute path**. Use for capture outputs and `snapshot_path` values from detection events.
- `fetch_record` — read-only Record relay + nginx autoindex, scoped to the enabled slot's data dir, takes a **relative path**. Use for browsing existing recordings.
- Both return images / ≤5 MB payloads inline (base64) and hand back a direct URL for anything larger.

## Rules of thumb

1. Prefer metadata over bytes: call `get_detection_events` first; only `fetch_file` the `snapshot_path` when the image is actually needed.
2. Cursor pagination for events: keep the last `timestamp_unix_ms` and pass it as `start_unix_ms` on the next call.
3. Set the detection model before installing rules; rules reference class labels the model knows.
4. Arm the pipeline with `set_record_config(rule_enabled=true, writer_format="JPG")` after changing the trigger, or `set_detection_rules` will do it for you.
5. On error, surface the message verbatim and suggest exactly one concrete fix.

## Workflows

### Onboard a device

1. `add_device` with `name`, `host`, `token`. Connectivity is probed automatically — failure aborts the write.
2. `list_devices` to confirm.

### Object detection by label

> **Schedule gates detection.** If the active schedule excludes the current time window, inference rules stay installed but no new events are produced. When debugging “no events”, check `get_detection_schedule` and ensure `now` is inside at least one configured range.

1. `get_detection_models_info` — pick the model whose `labels` contain the target class.
2. `set_detection_model` with `model_id` or `model_name`.
3. `set_detection_rules` with `rules: [{ "name": "person-rule", "label_filter": ["person"], "confidence_range_filter": [0.5, 1.0], "debounce_times": 0 }]`.
4. `clear_detection_events` to reset the event log.

### Monitor events (polling loop)

1. `start_ms = now_ms()`.
2. `get_detection_events(device_name, start_unix_ms=start_ms)`.
3. If any events: `start_ms = max(event.timestamp_unix_ms) + 1`; for each of interest, `fetch_file(path=event.snapshot_path)`.
4. Sleep, repeat.

### On-demand image capture

`capture_image` starts a JPG capture, polls to terminal state, and returns `{ event, path, size, content_base64 }`. No plumbing required.

### GPIO control

1. `list_gpios` to see pins + capabilities.
2. `set_gpio_value(pin_id, value=0|1)` — auto-configures as output.
3. `get_gpio_value(pin_id, debounce_ms=100)` — auto-configures as input; debounce auto-enables edge detection.

### Switch record trigger

> **Replaces any active detection rules.** Detection is itself an `inference_set` trigger, so picking any other `kind` stops event generation until `set_detection_rules` is called again. Read the current trigger first and confirm before overwriting.

1. `get_record_trigger` to inspect the current shape.
2. `set_record_trigger` with a tagged union:
   - `{ "kind": "timer", "interval_seconds": 60 }`
   - `{ "kind": "gpio", "name": "GPIO_01", "state": "FLOATING", "signal": "RISING", "debounce_ms": 50 }`
   - `{ "kind": "http" }` → then `activate_http_trigger` to fire one-shot events
   - `{ "kind": "always_on" }` — continuous while armed
   - `{ "kind": "tty", "name": "...", "command": "..." }`
   - `{ "kind": "inference_set", "rules": [...] }` — detection-driven
3. `set_record_config(rule_enabled=true, writer_format="JPG")` to arm the pipeline.

### Manage storage

> **Affects detection output.** Disabling all slots (`set_storage_slot` with both empty) leaves the pipeline with nowhere to write — detection continues but snapshots/events silently vanish. `FREE_UP` / `REMOVE_FILES_OR_DIRECTORIES` can delete snapshots still referenced by recent events, causing `fetch_file` 404s. Prefer quota rotation over bulk deletion when detection is active.

1. `get_storage_status` — inspect slots, state, used bytes.
2. `set_storage_slot(by_dev_path="/dev/mmcblk0p8")` (or `by_uuid`). Both empty disables all.
3. `configure_storage_quota(dev_path, quota_limit_bytes=-1, quota_rotate=true)` — `-1` = no limit.
4. Bulk actions: `storage_task_submit(action="FREE_UP", dev_path, sync=false)` → poll `storage_task_status`. `REMOVE_FILES_OR_DIRECTORIES` also requires `files: [...]`.

### Browse recordings

1. `list_records(path="")` — top of the data dir; relay opens implicitly.
2. Descend: `list_records(path="YYYY-MM-DD")` (directories are ISO dates).
3. `fetch_record(path="YYYY-MM-DD/clip_xxx.jpg")`. Videos / >5 MB return a direct URL.

## Troubleshooting

| Symptom | Fix |
|---|---|
| HTTP 401 / 403 | Re-copy token from Web Console → Device Info → Connection Settings. |
| Timeout / connection refused | Verify host, network path, device power. `detect_local_device` to probe. |
| `'device_name' must not be empty` | Pass the registered name; `list_devices` to confirm. |
| Empty detection events | Confirm a model is active (`get_detection_model`), storage is enabled (`get_storage_status`), and the current time is inside the active detection schedule (`get_detection_schedule`). |
| Snapshot fetch 404 | Snapshots rotate; re-fetch events and use a fresh `snapshot_path`. |
| Schedule rejected | Range format is `Day HH:MM:SS` (e.g. `Mon 08:00:00`). |
| GPIO write rejected | Value must be 0 or 1; confirm `pin_id` via `list_gpios`. |
| CLI: `ModuleNotFoundError: recamera_intellisense` | Set `PYTHONPATH=scripts` (or run from the skill's `scripts/` directory). |
