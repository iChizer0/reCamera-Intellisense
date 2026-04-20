---
name: recamera-intellisense
description: Use when onboarding or controlling a reCamera V2 device — registering the device, configuring AI detection models / rules / schedules, watching detection events, capturing images, browsing recordings, managing storage slots, or driving GPIO. Exposes the same operations through MCP tools (default for agents) and a stdlib-only Python CLI (automation / shell). Any task mentioning reCamera, `sk_` camera tokens, on-device detection rules, or recorded clips on the device should trigger this skill.
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

Both transports read/write `~/.recamera/devices.json` (mode `0600`, atomic). Register a device once → available to both.

| Transport | When to use |
|---|---|
| **MCP tools** | Default for interactive agent turns. Structured arguments, typed errors, inline image results. Tool names are the bare operation name (e.g. `add_device`); any `server:` prefix is added by the MCP host. |
| **Python CLI** (`recamera_intellisense`) | Shell scripts, CI, cron, or hosts without an MCP client. Stdlib-only; no install — package is bundled under `scripts/`. |

Operation names are identical across both (`add_device`, `capture_image`, `set_detection_rules`, …). A small number of helpers are **SDK-only** and marked below as `[sdk]`; everything else is available in both transports with the same name.

## Setup

### MCP transport

Prefer **download → review → run** so the installer can be inspected before it touches the system. It downloads a signed release asset, verifies its SHA-256 against the published digest, and aborts on mismatch.

```bash
# 1. Download the installer:
curl -fsSLO https://raw.githubusercontent.com/iChizer0/reCamera-Intellisense/main/scripts/setup-mcp.py
# 2. Inspect it (pager / editor / diff against a pinned SHA):
less setup-mcp.py
# 3. Check if already installed (prints path; exit 0 = present, 1 = missing):
python3 setup-mcp.py check
# 4. Install (non-interactive) — auto-configures detected MCP clients,
#    installs binary to ~/.recamera/bin/, verifies SHA-256:
python3 setup-mcp.py install --yes
```

Subcommands: `install | configure | check | uninstall | list`. The last line of `install` is `BINARY_PATH=<path>` for scripting. Piped form (trusted environments only):

```bash
curl -fsSL https://raw.githubusercontent.com/iChizer0/reCamera-Intellisense/main/scripts/setup-mcp.py | python3 - install --yes
```

### CLI transport

No install. From the skill directory:

```bash
PYTHONPATH=scripts python3 -m recamera_intellisense <command> '<json-args>'
PYTHONPATH=scripts python3 -m recamera_intellisense list-commands   # dump all commands
PYTHONPATH=scripts python3 -m recamera_intellisense help            # per-command required/optional args
```

JSON args are a single object whose keys match the command's named parameters. Results print as pretty JSON on stdout; errors exit non-zero with `{"error": ..., "code": ..., "status": ...}` on stderr.

## Security considerations

- **Installer integrity.** Prefer download-then-review over `curl … | python3`. `setup-mcp.py` verifies the release asset's SHA-256 against `<asset>.sha256` / `SHA256SUMS` and aborts on mismatch. Bypass with `--skip-checksum` or `RECAMERA_SKIP_CHECKSUM=1` only when you understand the risk.
- **Credentials** live in `~/.recamera/devices.json`, written atomically at mode `0600` by both transports. Do not mix unrelated secrets there.
- **HTTPS cert verification is on by default** (`allow_unsecured=false`). Opt in per-device only for self-signed certs on a trusted LAN.
- **Plain HTTP by default** (port 80): tokens and images traverse the network unencrypted. Configure HTTPS on the device before using on untrusted networks.
- **`fetch_file` reads absolute paths via the daemon.** A registered device becomes a data-egress point for anything under the daemon's allowed prefix — register only devices you control.
- **Record relay URLs are not bearer-protected** for the lifetime of the relay (`records.rs`: "relay token is bearer-free"). Anyone with the URL on the same network can download the file; close the relay when done.
- **`set_record_trigger` supports `tty` and `http` triggers** that execute shell commands or call external endpoints. Only install triggers you have reviewed.
- **Per-camera tokens** (`sk_…`, format enforced by `_TOKEN_RE = ^sk_[A-Za-z0-9_\-]+$`) come from Web Console → Device Info → Connection Settings. Do not reuse tokens shared with other services.
- **Source review.** Full SDK sources are in `scripts/recamera_intellisense/`. Review before granting autonomous execution.

## Operation groups

Register a device first (`add_device`), then pass `device_name` to everything else.

- **Device** — `detect_local_device`, `add_device`, `update_device`, `remove_device`, `get_device`, `list_devices`
- **Detection** (high level) — `get_detection_models_info`, `get_detection_model`, `set_detection_model`, `get_detection_schedule`, `set_detection_schedule`, `get_detection_rules`, `set_detection_rules`, `get_detection_events`, `clear_detection_events`
- **Rule pipeline** (low level) — `get_rule_system_info`, `get_record_config`, `set_record_config`, `get_schedule_rule`, `set_schedule_rule`, `get_record_trigger`, `set_record_trigger`, `activate_http_trigger`
- **Storage** — `get_storage_status`, `set_storage_slot`, `configure_storage_quota`, `storage_task_submit`, `storage_task_status`, `storage_task_cancel`
- **Records** (browse clips, relative paths) — `list_records`, `fetch_record`
- **Capture** — `get_capture_status`, `start_capture`, `stop_capture`, `capture_image`
- **File** (daemon, absolute paths) — `fetch_file`, `delete_file`
- **GPIO** — `list_gpios`, `get_gpio_info`, `set_gpio_value`, `get_gpio_value`
- **SDK-only** `[sdk]` — `get_intellisense_events`, `clear_intellisense_events` (low-level daemon event bus; `get_detection_events` / `clear_detection_events` are direct aliases and are the form exposed over MCP).

### `fetch_file` vs `fetch_record`

| Tool | Backend | Path | Scope | When |
|---|---|---|---|---|
| `fetch_file` | daemon `/api/v1/file` | **absolute** | daemon-allowed prefix | capture outputs, `snapshot_path` from detection events |
| `fetch_record` | Record relay + nginx autoindex | **relative** to enabled slot's `data_dir` | recordings only | browsing existing clips under `list_records` |

Both render images inline (base64). Payloads > 5 MB and any `video/*` MIME return the direct relay/daemon URL instead of bytes.

### `detect_local_device` — transport differs

| Transport | Default arg | What it probes | Success value |
|---|---|---|---|
| **MCP** | `socket_path = "/dev/shm/rcisd.sock"` | Local Unix socket (daemon IPC) | Returns the socket path. |
| **SDK** | `host = "127.0.0.1"` | `http://<host>:16384/api/v1/generate-204` | Returns `host` (or `None`). |

Use the MCP form on the device itself; use the SDK form when you can reach the daemon only over TCP.

## Parameter schemas (canonical)

All parameters below are keyword-only in the SDK; MCP tools take a JSON object with the same keys. **Bold = required.** Fields prefixed `[sdk]` are SDK-only; MCP auto-applies equivalent behaviour.

### `add_device`
`name*, host*, token*` (token must match `^sk_…$`), `protocol="http"|"https"` (default `"http"`), `allow_unsecured=false`, `port=None`. Connectivity is probed before the record is written; failure aborts.

### `update_device`
`device_name*` + any subset of `host, token, protocol, allow_unsecured, port`. Unspecified fields preserved.

### `set_detection_model`
`device_name*`, **exactly one of** `model_id: int` or `model_name: str`. SDK also accepts `fps: int = 30` `[sdk]`.

### `set_detection_schedule` / `set_schedule_rule`
`device_name*, schedule: list[{start, end}] | null`. Range format `"Day HH:MM:SS"` (e.g. `"Mon 08:00:00"`); `"Day 24:00:00"` allowed as end-of-day. `null` or `[]` disables the schedule (rule active 24/7).

### `set_detection_rules`
`device_name*, rules: list[DetectionRule]`. Each rule:

| Field | Type | Default | Meaning |
|---|---|---|---|
| `name` | string | *required* | Rule ID (unique per device). |
| `label_filter` | string[] | `[]` (any) | Class labels the model emits. |
| `confidence_range_filter` | `[min, max]` | `[0.25, 1.0]` | Per-detection score window. |
| `debounce_times` | int | `3` | Consecutive matching frames before firing. |
| `region_filter` | `number[][][]` or `null` | full frame | List of polygons of normalized `[x, y]` in `[0, 1]`. |

MCP auto-arms the pipeline (`rule_enabled=true`, writer `JPG`) and ensures a storage slot. SDK does the same via `ensure_writer=True, ensure_storage=True` `[sdk]` which can be disabled for advanced flows.

### `set_record_config`
`device_name*, rule_enabled*: bool, writer_format*: "JPG"|"MP4"|"RAW", writer_interval_ms: int = 0` (`0` = continuous).

### `set_record_trigger`
`device_name*, trigger: RecordTrigger`. Tagged union on `kind`:

```jsonc
{ "kind": "inference_set", "rules": [/* DetectionRule[] — see above */] }
{ "kind": "timer",        "interval_seconds": 60 }
{ "kind": "gpio",         "name": "GPIO_01", /* OR "num": 1 */
                          "state": "DISABLED|FLOATING|PULL_UP|PULL_DOWN",
                          "signal": "HIGH|LOW|RISING|FALLING",
                          "debounce_ms": 0 }
{ "kind": "tty",          "name": "<tty socket name>", "command": "<non-empty>" }
{ "kind": "http" }        // followed by activate_http_trigger to fire one-shot events
{ "kind": "always_on" }   // re-arms using writer_interval_ms pacing
```

### `set_storage_slot`
`device_name*, by_dev_path: str = "", by_uuid: str = ""`. Both empty → disable all slots. Specify exactly one selector to choose a slot.

### `configure_storage_quota`
`device_name*, dev_path*: str, quota_limit_bytes*: int` (`-1` = no limit), `quota_rotate: bool = true`.

### `storage_task_submit`
`device_name*, action*: "FORMAT"|"FREE_UP"|"EJECT"|"REMOVE_FILES_OR_DIRECTORIES", dev_path*: str, files: string[] = []` (required for `REMOVE_*`, relative to `data_dir`), `sync: bool = false` (default submits async; poll with `storage_task_status`, cancel with `storage_task_cancel`).

### `list_records` / `fetch_record`
`device_name*`, `path: str = ""` (relative to the enabled slot's `data_dir`; empty = root for `list_records`; required for `fetch_record`), `dev_path: str | null = None` (override the slot).

### `fetch_file` / `delete_file`
`device_name*, path*: str` — absolute path on the device under the daemon-allowed prefix.

### `start_capture` / `capture_image`
`device_name*, output: str | null = None` (absolute path under a mount; defaults to the enabled slot's `<mount>/<data_dir>`, falling back to `/mnt/rc_mmcblk0p8/reCamera`), `format: "JPG"|"RAW"|"MP4" = "JPG"`, `video_length_seconds: int | null = None` (MP4 only). `capture_image` also takes `timeout: float = 5.0` and ignores `format` / `video_length_seconds` (always `JPG`).

### GPIO
- `get_gpio_info`: `device_name*, pin_id*: int`.
- `set_gpio_value`: `device_name*, pin_id*: int, value*: 0|1`. Auto-configures as output.
- `get_gpio_value`: `device_name*, pin_id*: int, debounce_ms: int = 100`. Auto-configures as input; non-zero debounce enables both-edge detection.

### `get_detection_events`
`device_name*, start_unix_ms: int | null = None, end_unix_ms: int | null = None`. Both bounds inclusive. Returns `[{timestamp, timestamp_unix_ms, rule_name, snapshot_path?}]`.

## Return-shape notes

- `capture_image` (SDK): `{"event", "path", "size", "content_base64"}`. (MCP): JSON metadata block + an inline `image/jpeg` Content item.
- `fetch_record` (SDK): `{"path", "content_type", "content_base64", "size", "url"}` for inline, `{"path", "url", "size", "content_type", "note"}` for oversized. (MCP): inline image/text Content, or a text message containing the direct URL for video / >5 MB.
- `fetch_file` (SDK): same inline/oversized shapes; `raw=True` returns raw `bytes`.
- `list_records` entries: `{"name", "type": "file"|"directory", "mtime"?, "size"?}`.

## Rules of thumb

1. Prefer metadata over bytes: call `get_detection_events` first; only `fetch_file` the `snapshot_path` when the image is actually needed.
2. Cursor pagination for events: keep the last `timestamp_unix_ms` and pass `start_unix_ms = last + 1` next call.
3. Set the detection model *before* installing rules — rules reference class labels the model knows.
4. After changing the trigger manually, arm the pipeline with `set_record_config(rule_enabled=true, writer_format="JPG")`. `set_detection_rules` does this for you.
5. On error, surface the device message verbatim and suggest exactly one concrete fix.

## Workflows

### Onboard a device

1. `add_device` with `name`, `host`, `token`. Connectivity is probed automatically — failure aborts the write.
2. `list_devices` to confirm.

```bash
PYTHONPATH=scripts python3 -m recamera_intellisense add_device \
  '{"name":"lab","host":"192.168.1.42","token":"sk_abc123"}'
```

### Object detection by label

> **Schedule gates detection.** If the active schedule excludes the current time, inference rules stay installed but no new events are produced. When debugging "no events", check `get_detection_schedule` and ensure `now` is inside at least one range.

1. `get_detection_models_info` → pick the model whose `labels` contain the target class.
2. `set_detection_model` with `model_id` *or* `model_name`.
3. `set_detection_rules` (auto-arms pipeline + storage):

   ```json
   {
     "device_name": "lab",
     "rules": [{
       "name": "person-rule",
       "label_filter": ["person"],
       "confidence_range_filter": [0.5, 1.0],
       "debounce_times": 3
     }]
   }
   ```
4. `clear_detection_events` to reset the event log.

### Monitor events (polling loop)

1. `start_ms = now_ms()`.
2. `get_detection_events(device_name, start_unix_ms=start_ms)`.
3. If non-empty: `start_ms = max(event.timestamp_unix_ms for event in events) + 1`; for each event of interest, `fetch_file(path=event.snapshot_path)`.
4. Sleep, repeat.

### On-demand image capture

`capture_image` starts a JPG capture, polls to terminal state (`COMPLETED|FAILED|INTERRUPTED|CANCELED`), fetches the file via the daemon, and returns the bytes. `output` is optional and defaults to the enabled slot's record directory.

### GPIO control

1. `list_gpios` → pins + capabilities.
2. `set_gpio_value(pin_id, value=0|1)` — auto-switches to output.
3. `get_gpio_value(pin_id, debounce_ms=100)` — auto-switches to input; `debounce_ms > 0` auto-enables edge detection.

### Switch record trigger

> **Replaces any active detection rules.** Detection *is* an `inference_set` trigger, so choosing any other `kind` stops event generation until `set_detection_rules` is called again. Read the current trigger first and confirm before overwriting.

1. `get_record_trigger` — inspect current shape.
2. `set_record_trigger` with a tagged-union payload (see schema above).
3. `set_record_config(rule_enabled=true, writer_format="JPG")` to arm the pipeline.
4. For `kind: "http"`, fire one-shot events with `activate_http_trigger`.

### Manage storage

> **Affects detection output.** Disabling all slots (`set_storage_slot` with both selectors empty) leaves the pipeline with nowhere to write — detection continues but snapshots/events silently vanish. `FREE_UP` / `REMOVE_FILES_OR_DIRECTORIES` can delete snapshots still referenced by recent events, causing `fetch_file` 404s. Prefer quota rotation over bulk deletion when detection is active.

1. `get_storage_status` → slots, state, used bytes, `data_dir`.
2. `set_storage_slot(by_dev_path="/dev/mmcblk0p8")` (or `by_uuid`).
3. `configure_storage_quota(dev_path, quota_limit_bytes=-1, quota_rotate=true)`.
4. `storage_task_submit(action="FREE_UP", dev_path, sync=false)` → poll `storage_task_status(action, dev_path)`. `REMOVE_FILES_OR_DIRECTORIES` also requires `files: [...]`.

### Browse recordings

1. `list_records(path="")` — top of the data dir; relay opens implicitly.
2. Descend: `list_records(path="YYYY-MM-DD")` (directories are ISO dates).
3. `fetch_record(path="YYYY-MM-DD/clip_xxx.jpg")`. Videos or files > 5 MB return a direct URL valid for the relay lifetime.

## Troubleshooting

| Symptom | Fix |
|---|---|
| HTTP 401 / 403 | Re-copy token from Web Console → Device Info → Connection Settings. |
| Timeout / connection refused | Verify host, network path, power. `detect_local_device` to probe. |
| `'device_name' must not be empty` | Pass the registered name; `list_devices` to confirm. |
| `Invalid token format: expected 'sk_<chars>'` | Token must match `^sk_[A-Za-z0-9_\-]+$`; re-copy from the Web Console. |
| Empty detection events | Confirm a model is active (`get_detection_model`), the trigger is `inference_set` (`get_record_trigger`), storage is enabled (`get_storage_status`), and `now` is inside the detection schedule (`get_detection_schedule`). |
| Snapshot fetch 404 | Snapshots rotate; re-fetch events and use a fresh `snapshot_path`. |
| Schedule rejected | Range format is `Day HH:MM:SS` (e.g. `Mon 08:00:00`); `Day 24:00:00` allowed as end. |
| `set_detection_model requires 'model_id' or 'model_name'` | Pass exactly one — not both, not neither. |
| GPIO write rejected | Value must be 0 or 1; confirm `pin_id` via `list_gpios`. |
| `REMOVE_FILES_OR_DIRECTORIES requires non-empty 'files'` | Populate `files: ["<rel-path>", …]` relative to `data_dir`. |
| CLI: `ModuleNotFoundError: recamera_intellisense` | Set `PYTHONPATH=scripts` (or run from the skill's `scripts/` directory). |
