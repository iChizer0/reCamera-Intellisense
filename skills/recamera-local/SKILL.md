---
name: recamera-local
description: Controls the reCamera Pro this agent is running on — takes snapshots, configures AI object/person and sound-event detection, waits for detection events in real time, records video clips, browses and deletes recordings, manages storage, adjusts ISP image settings, and drives GPIO pins. Use when the agent runs on the camera itself and the user asks what it sees, to watch for something ("alert me when a person appears"), or to record on a trigger. Not for controlling remote cameras from another host.
metadata: {
  "openclaw": {
    "emoji": "📷",
    "requires": {
      "bins": ["python3"]
    }
  }
}
user-invocable: true
allowed-tools: "Bash"
---

# reCamera Local

Controls the reCamera this agent runs on. Everything is local: the API is loopback, captures and recordings are plain file paths (open them with your own file/vision tools), and events can be awaited with one blocking call.

## Setup

Zero configuration on stock firmware. The SDK probes `http://127.0.0.1:80` then `https://127.0.0.1:443` and reads the bearer token from `/userdata/config/system/http_key.json`. Overrides exist for unusual setups (`RECAMERA_HOST` / `RECAMERA_PORT` / `RECAMERA_PROTOCOL` / `RECAMERA_TOKEN`); ignore them otherwise.

## Invocation

```bash
export PYTHONPATH="{baseDir}/scripts"
alias rcl='python3 -m recamera_local'

rcl <command> key=value ...      # default form
rcl <command> --help             # usage + working example, per command
rcl list-commands
```

Alternative argument forms when quoting gets awkward: `--key value`, or one single `'{"key":"value"}'` JSON object. Values coerce from the command's signature: strict booleans (`true/false`), `null`/`none`, JSON arrays/objects inline or `@file.json`. Output is compact one-line JSON; errors print a message plus usage/example on stderr with a non-zero exit — surface the message and apply the hint.

**Event loops and low-latency use**: avoid process-per-call overhead. Either import in-process (`sys.path.insert(0, "{baseDir}/scripts")`, then `from recamera_local import wait_event, capture_image`), or keep one `rcl serve` process running — JSONL stdin→stdout: `{"cmd": "<name>", "args": {...}, "id": n}` → `{"id": n, "ok": true, "result": ...}`.

## Command map

Full schemas (trigger kinds, detection rules, ISP sections, event shape): **[REFERENCE.md](REFERENCE.md)**. Runtime truth: `rcl <command> --help`.

- **Status**: `get_status` (one-call snapshot: device, load, battery, storage, capture, record pipeline, active model — call this first), `get_device_info`, `get_resource_info`, `get_battery_status`, `get_system_time`, `export_device_config output=…` (snapshot config before making changes), `reboot_device` (**confirm**).
- **Capture**: `capture_image` (JPG → `{event, path, size}`; `path` is local — open it, don't re-fetch; `inline=true` adds base64), `start_capture` (`format=JPG|RAW|MP4`, `output` is an **on-device** dir), `get_capture_status`, `stop_capture`.
- **Detect**: `get_detection_models_info` (vision label **names**), `get/set_detection_model`, `get/set_detection_rules`, `get/set_detection_schedule`, `wait_event`, `get_detection_events`, `clear_detection_events`.
- **Sound**: `get_record_sources` (source ids + producible classes — check BEFORE compiling rules), `get_active_acoustic_model`, `list_acoustic_models`, `set_acoustic_model` (switch head / `default=true`).
- **Triggers**: `get/set_record_config`, `get/set_record_trigger`, `get/set_schedule_rule`, `activate_http_trigger`.
- **Records/files**: `list_records`, `read_file` (absolute path under `/mnt`), `delete_file` (**confirm**).
- **Storage**: `get_storage_status`, `set_storage_slot`, `configure_storage_quota`.
- **GPIO**: `list_gpios`, `get_gpio_info`, `set_gpio_value`, `get_gpio_value`.
- **Image (ISP)**: `get_image_settings`, `set_image_settings`.
- **Video**: `get/set_video_encode stream=main|sub` (codec/resolution/fps/GOP/rate control).
- **Apps**: `list_apps` (a stopped app produces no recording frames), `get_app_logs`, `start_app`, `stop_app`/`restart_app` (**confirm** for system apps).
- **Result push**: `get_notify_config` (MQTT/HTTP/UART; secrets redacted `***`), `set_notify_config` (merge-preserving; never echo `***` back; applying briefly restarts the result pipeline).

## Recipes

```bash
# See right now — then open the printed path with your file/vision tool
rcl capture_image

# Watch for events: blocks until one arrives; feed the watermark back
rcl wait_event timeout_s=60                      # → {events, watermark_unix_ms, timed_out}
rcl wait_event timeout_s=60 since_unix_ms=1745152496000

# Person detection, with read-back verification
rcl get_detection_models_info                    # pick a label name
rcl set_detection_model model_name=yolo11n
rcl set_detection_rules 'rules=[{"name":"person","label_filter":["person"]}]'
rcl get_detection_rules                          # verify the rules took effect

# Sound-triggered recording (e.g. cat meows → MP4)
rcl get_record_sources                           # acousticslab running? pick a class name
rcl set_record_config rule_enabled=true writer_format=MP4
rcl set_detection_rules 'rules=[{"name":"cat","source_filter":["acousticslab"],"label_filter":["Cat"],"debounce_times":3}]'

# Record clips on a GPIO pulse, with read-back verification
rcl set_record_config rule_enabled=true writer_format=MP4
rcl set_record_trigger 'trigger={"kind":"gpio","num":106,"state":"PULL_UP","signal":"FALLING","debounce_ms":50}'
rcl get_record_trigger                           # verify the trigger took effect

# Browse what was recorded
rcl list_records path=2026-04-20
rcl read_file path=/mnt/rc_mmcblk0p8/reCamera/2026-04-20/snap-001.jpg
```

## Hard rules

1. Only **one record trigger** is active at a time (`inference_set` | `timer` | `gpio` | `tty` | `http` | `always_on`). AI detection (vision or sound) → `set_detection_rules`; anything else → `set_record_trigger`. `get_detection_rules` returns `[]` when another trigger kind owns the pipeline. The legacy `sed` kind is **retired** — sound is an `inference_set` rule with `source_filter=["acousticslab"]`.
2. `label_filter` takes label **names** — vision from `get_detection_models_info`, sound from `get_record_sources` (or `get_active_acoustic_model`) — never indexes. Compile against reality: check `get_record_sources` first; unknown source ids and unproducible labels are loud errors. Rules without `source_filter` are scoped to the `builtin` vision source (an explicit empty filter matches EVERY source, including audio).
3. Schedules: `[{"start":"Mon 08:00:00","end":"Mon 18:00:00"}]`; `schedule=null` disables (always active).
4. `wait_event` loops must pass the returned `watermark_unix_ms` back as `since_unix_ms` — never recompute it.
5. After changing detection or trigger configuration, read it back (`get_detection_rules` / `get_record_trigger`) before telling the user it is done.
6. Confirm gates: `reboot_device`, `delete_file`, and `stop_app`/`restart_app` on system apps refuse without `confirm=true` — ask the user first. `clear_detection_events` only purges the transient event buffer (recordings on disk are unaffected).
7. `set_gpio_value` reconfigures the pin as output and `get_gpio_value` as input — both have hardware side effects.

## Scope and hand-off

This skill **orchestrates existing device capabilities** (record rules, capture, storage, GPIO, ISP, apps) — it cannot add new on-device behavior. When a request needs a new capability (a custom model, app, or post-processing pipeline), say so plainly: new apps are built with the reCamera Pro app SDK and installed via the Web Console / App Center, and new sound classes are trained in the AcousticsLab console (`/extension/acousticslab`). Never invent capabilities.

## Troubleshooting

| Symptom | Fix |
|---|---|
| "Local reCamera API not reachable" | This skill must run on the camera (intellisense firmware). Check `RECAMERA_*` overrides. |
| HTTP 401/403 | Token mismatch — unset `RECAMERA_TOKEN` to use the device key store. |
| Rules set but no events | No enabled storage slot (`get_storage_status`), inactive schedule window, filters too tight, debounce too high, or the source app stopped (`list_apps` / `get_record_sources`). |
| `unknown source_filter` / `cannot be produced` | Check `get_record_sources`; start the app that owns the source (`start_app`). |
| Acoustic model is `null` / sound rule never fires | The AcousticsLab app is stopped or has no active head — `start_app app_id=acousticslab`, then verify via `get_record_sources` / `list_acoustic_models`. |
| `start_capture` code 30022 | `output` must be under a mounted slot — omit it to use the default. |
| `list_records` fails | No slot enabled — `set_detection_rules` auto-enables internal storage, or use `set_storage_slot`. |
| `read_file` rejects a path | Only absolute paths under `/mnt`, no `..`. |
