# reCamera Intellisense API Reference

The bundled SDK is the authoritative implementation for the skill. It uses only
Python's standard library and accepts one JSON object per CLI invocation.

## Invocation

From the skill directory:

```bash
PYTHONPATH="./scripts" python3 -m recamera_intellisense <command> key=value ...
```

Argument forms (never mixed in one call):

| Form | Example |
|---|---|
| `key=value` | `recamera get_device device_name=cam1` |
| `--key value` / `--key=value` | `recamera get_device --device-name cam1` |
| single JSON object | `recamera get_device '{"device_name":"cam1"}'` |

Values are coerced using the command function's type annotations:

- **integers/floats** — `pin_id=106`, `quota_limit_bytes=-1`, `timeout=30`
- **booleans** — strict: `true/false`, `yes/no`, `on/off`, `1/0`; anything else is an error (no silent `bool("false") == true` surprises)
- **`null` / `none`** — selects `None` for optional parameters (e.g. `schedule=null` disables the schedule)
- **JSON arrays/objects** — required for structured parameters (`rules`, `schedule`, `trigger`, `files`), inline (`'rules=[{"name":"person"}]'`) or from a file (`trigger=@trigger.json`; `@-` reads stdin)
- **`@path`** — in the key=value form, any value may be loaded from a file (e.g. `token=@token.txt`); prefix `@@` for a literal leading `@`. The single-JSON-object form does not perform `@` expansion

A successful command prints JSON to stdout: a Python `None` result prints
`null`, an empty dictionary prints `{}`. Every usage error prints the
command's `usage:` line and a copy-pasteable `example:` to stderr and exits
non-zero. `recamera <command> --help` prints the same information on demand.

Run the command without arguments to print the complete runtime catalogue:

```bash
PYTHONPATH="./scripts" python3 -m recamera_intellisense
```

Most device commands require a registered `device_name`. Device records are
stored in `~/.recamera/devices.json` with mode `0600`.

## Device registry

| Command | Required keys | Optional keys |
|---|---|---|
| `detect_local_device` | `host` | `port`, `token`, `timeout` |
| `add_device` | `name`, `host`, `token` | `protocol`, `allow_unsecured`, `port` |
| `update_device` | `device_name` | `host`, `token`, `protocol`, `allow_unsecured`, `port` |
| `remove_device` | `device_name` | — |
| `get_device` | `device_name` | — |
| `list_devices` | — | — |

`protocol` is `http` or `https`. Use `allow_unsecured: true` only for trusted
LAN devices with self-signed HTTPS certificates. `token` may be empty only
when the device does not require authentication.

## Detection models and events

| Command | Required keys | Optional keys |
|---|---|---|
| `get_detection_models_info` | `device_name` | — |
| `get_detection_model` | `device_name` | — |
| `set_detection_model` | `device_name` | one of `model_id` or `model_name`; `fps` |
| `get_detection_schedule` | `device_name` | — |
| `set_detection_schedule` | `device_name` | `schedule` |
| `get_detection_rules` | `device_name` | — |
| `set_detection_rules` | `device_name`, `rules` | `ensure_writer`, `ensure_storage` |
| `get_detection_events` | `device_name` | `start_unix_ms`, `end_unix_ms` |
| `clear_detection_events` | `device_name` | — |
| `get_active_acoustic_model` | `device_name` | — |

Detection labels are names returned by the selected model, not numeric
indexes. A schedule is a list such as:

```json
[{"start":"Mon 08:00:00","end":"Mon 18:00:00"}]
```

Pass `null`, `[]`, or omit `schedule` to disable the schedule and make it
always active. A detection rule can contain `name`, `debounce_times`,
`confidence_range_filter`, `label_filter`, and `region_filter`.

## Rule system

| Command | Required keys | Optional keys |
|---|---|---|
| `get_rule_system_info` | `device_name` | — |
| `get_record_config` | `device_name` | — |
| `set_record_config` | `device_name`, `rule_enabled`, `writer_format` | `writer_interval_ms` |
| `get_schedule_rule` | `device_name` | — |
| `set_schedule_rule` | `device_name` | `schedule` |
| `get_record_trigger` | `device_name` | — |
| `set_record_trigger` | `device_name`, `trigger` | — |
| `activate_http_trigger` | `device_name` | — |

Supported trigger kinds are `inference_set`, `timer`, `gpio`, `tty`, `http`,
`always_on`, and `sed`. Only one record trigger is active at a time.

Examples:

```json
{"kind":"timer","interval_seconds":60}
```

```json
{"kind":"gpio","num":106,"state":"PULL_UP","signal":"FALLING","debounce_ms":50}
```

```json
{"kind":"sed","model_id":"","consecutive_window_ms":0,"confidence_range_filter":[0.5,1.0],"label_filter":["Yes"]}
```

## Capture

| Command | Required keys | Optional keys |
|---|---|---|
| `get_capture_status` | `device_name` | — |
| `start_capture` | `device_name` | `output`, `format`, `video_length_seconds` |
| `stop_capture` | `device_name` | — |
| `capture_image` | `device_name` | `output`, `timeout` |

`format` is `JPG`, `RAW`, or `MP4`. `output` is an absolute **on-device**
directory under a mounted storage slot, not a local directory. Omit it to use
the selected slot. `capture_image` waits for completion and returns the event,
remote path, size, and inline base64 content.

## Storage

| Command | Required keys | Optional keys |
|---|---|---|
| `get_storage_status` | `device_name` | — |
| `set_storage_slot` | `device_name` | `by_dev_path`, `by_uuid` |
| `configure_storage_quota` | `device_name`, `dev_path`, `quota_limit_bytes` | `quota_rotate` |
| `storage_task_submit` | `device_name`, `action`, `dev_path` | `sync`, `files` |
| `storage_task_status` | `device_name`, `action`, `dev_path` | `task_uid` |
| `storage_task_cancel` | `device_name`, `action`, `dev_path` | `task_uid` |

Actions are `FORMAT`, `FREE_UP`, `EJECT`, and
`REMOVE_FILES_OR_DIRECTORIES`. `FORMAT` and `FREE_UP` must be submitted
asynchronously (`sync: false`) and polled with `storage_task_status`.
Storage operations can destroy recordings; use them only with explicit
authorization.

## Records and files

| Command | Required keys | Optional keys |
|---|---|---|
| `list_records` | `device_name` | `path`, `dev_path`, `limit`, `offset` |
| `fetch_record` | `device_name`, `path` | `dev_path`, `max_inline_bytes` |
| `fetch_file` | `device_name`, `path` | `max_inline_bytes` |
| `delete_file` | `device_name`, `path` | — |
| `get_intellisense_events` | `device_name` | `start_unix_ms`, `end_unix_ms` |
| `clear_intellisense_events` | `device_name` | — |

`list_records` paths are relative to the selected record data directory.
`fetch_file` paths are absolute on-device paths and reject traversal segments
and NUL bytes. Images and payloads within the inline limit are returned as
base64; larger payloads return metadata and a retrieval note.

## GPIO

| Command | Required keys | Optional keys |
|---|---|---|
| `list_gpios` | `device_name` | — |
| `get_gpio_info` | `device_name`, `pin_id` | — |
| `set_gpio_value` | `device_name`, `pin_id`, `value` | — |
| `get_gpio_value` | `device_name`, `pin_id` | `debounce_ms` |

`set_gpio_value` accepts only `0` or `1` and configures the pin as push-pull
output. `get_gpio_value` configures the pin as floating input and can enable
edge detection when debouncing; both operations therefore have hardware side
effects and should not be treated as passive inspection.

## Python API

The same functions are available in-process:

```python
import sys
sys.path.insert(0, "./scripts")
from recamera_intellisense import capture_image, get_storage_status

image = capture_image(device_name="cam1")
```

The public SDK exports the 44 CLI commands listed above. `relay.py` also has
internal helpers used by record browsing; relay lifecycle is managed
automatically by `list_records` and `fetch_record`.
