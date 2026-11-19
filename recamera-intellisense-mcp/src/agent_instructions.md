# reCamera Intellisense agent instructions

Use these tools to orchestrate existing reCamera capabilities: registration, AI and sound-event detection, recording rules, recent event retrieval, capture, records/files, storage, result push, apps, and GPIO.

The server does **not** provide durable observation-task CRUD, a relative-time scheduler, a persistent observation database, or business-delivery retries. `set_detection_rules` writes device recording rules; it does not start an external monitoring program.

## 1. Control and truthfulness contract

For watch/monitor/alert requests, keep the main loop in deterministic external code (normally Python), not in a free-running LLM conversation:

- Device model + rules: supported positive inference matches and evidence.
- External runner: schedule, state machine, task ID/generation, checkpoints, dedupe, persistence, notification decisions, cancellation, and retries.
- LLM/VLM: only a stateless operator over code-selected images/input, with a strict schema and validated output. It never owns timers or delivery success.

If no persistent runner/scheduler and real delivery channel exist, do not claim “I will keep watching” or “I will notify you later.” After user approval, persistent device rules may collect evidence, but explain that a consumer must query the bounded event buffer before expiry. Otherwise offer a fresh one-shot check or a bounded check while the agent is demonstrably active.

Keep positive observation, negative observation, unknown/occluded, capture failure, and no retained event distinct. Never turn a camera/network gap into absence, exit, recovery, or continued dwell. Answer history only from persisted records, not from what the model assumes happened.

## 2. Route the request before writes

### Current view / continuous follow-up (S01)

Call `capture_image` for every “now”, “again”, or “I moved it” request. Bind the answer to the returned image and capture event/timestamp. Resolve references against that image and ask if ambiguous. If text is unreadable, state the visual limitation instead of guessing. On failure, never reuse an old image as current evidence. S01 needs no detection-rule change.

### Observation task (S02–S06)

1. Capture a reference when a target or ROI needs confirmation. Resolve “this parcel”, “that sofa”, and the region boundary; never copy an example ROI.
2. Discover actual capabilities with `get_detection_models_info`, `get_detection_model`, and `get_record_sources`. Use `list_apps` when an app source may be involved. Match exact, case-sensitive labels. Check `supports_roi`; built-in source classes follow the selected vision model.
3. Before changing shared state, read `get_record_trigger`, `get_record_config`, `get_detection_schedule`, `get_storage_status`, and `get_resource_info` as relevant. Only one record trigger and one built-in vision model are active at a time. Do not overwrite an unrelated task.
4. Present a task summary: target, ROI/reference, absolute start/end or weekly schedule, requested and actual cadence, trigger/reset conditions, delivery channel, device changes, and limitations. Obtain permission for proposed shared model/rule/schedule/writer/storage/notify changes.
5. A durable external task record should include ID, generation, pending/running/paused/failed/completed/cancelled state, target/ROI, timing, unknown/gap policy, reset/re-arm rule, delivery ID, last successful observation, evidence, gaps, and prior device configuration.
6. “Started/running” requires a live runner and successful initial observation plus configuration read-back. A successful rule write alone is insufficient.

A weekly `set_detection_schedule` range is not a one-shot “next 20 minutes” lifetime. External code enforces relative expiration and capacity limits. Start with one observation task per device and measure before running tasks in parallel.

### No matching installed model or source

Do not infer unsupported labels. In particular, never assume generic parcel, cardboard-box, individual identity, or license-plate OCR support. Vehicle detection is not OCR; class presence is not identity; ROI presence is not an entry transition.

Ask the user in non-technical terms whether to:

1. try an online VLM with explicit snapshot-upload permission (network, privacy, provider, cost/quota and retention implications disclosed);
2. search the device App Center/model store or approved internet sources for an existing compatible model/app (offline potential, compatibility unverified);
3. narrow the goal to an existing label; or
4. pause.

Candidate sources include https://github.com/airockchip/rknn_model_zoo and https://github.com/airockchip/rknn-llm. A hit is only a lead: verify RV1126B/RKNN compatibility, runtime, pre/post-processing, exact labels, ROI, resources, and license. `rknn-llm` is not proof of a ready detector/OCR app. Do not download, convert, install, or start anything without authorization. Prefer existing compatible models and do not turn the request into an impromptu training pipeline. For a custom zones/tracking/temporal/OCR app, hand off to the `recamera-pysdk` skill for separate capability assessment and validation.

## 3. Scenario routing

- **S02 parcel removed/returned:** needs timed fresh images and an external `PRESENT -> ABSENCE_CANDIDATE -> ABSENT` state machine (optionally back to PRESENT). Experimental defaults: 30 minutes, 5-second cadence, two evaluable absence observations, one alert. Occlusion/blur/failure is unknown, not removal. Report last-seen and first-confirmed-absent times, not exact removal time. A person rule or missing positive events is not parcel detection.
- **S03 person in region:** preferred evidence route is an exact `person` label, confirmed ROI, `inference_set` rule, and prompt event consumption. This proves person detected in ROI, not entry/exit/dwell. External state needs a valid outside baseline before inside for entry, valid observation times for dwell, and valid negative observations for exit/re-entry. Missing events alone do not prove exit. Reset after confirmed outside to avoid repeated alerts.
- **S04 cat/dog in forbidden region:** same evidence route only when the exact label and ROI support exist. Full-frame appearance is not “on the sofa”. Require valid absence for the requested 30-second reset; an outage is not absence. Do not claim individual-pet identity, health, or mood.
- **S05 specific vehicle + plate:** require a separately approved and validated OCR route before promising plate matching. Preserve original image, plate crop when available, raw OCR, exact-match decision, time, and region. Blur, occlusion, conflicts, and similar characters produce unknown, never fuzzy success. Deduplicate arrival externally and enforce the absolute window. Printed-plate and real-car tests are separate; neither proves night/highway support or authorizes gate control.
- **S06 sustained passage obstruction:** use timed fresh images and `CLEAR -> OCCUPY_CANDIDATE -> OCCUPIED -> RECOVERY_CANDIDATE -> CLEAR`. Experimental defaults: 10-second cadence and 60-second valid occupancy, then one recovery alert. Merge repeated samples into one episode. Unknown frames cannot prove occupancy or recovery; cadence bounds time precision. A passing person is not box occupancy, and this is not a certified safety alarm.
- **S07 business integration:** persist a stable event ID and evidence separately from delivery state (`pending/succeeded/failed/unknown`, attempts, response/error, next retry). Retry with the same idempotency key and bounded backoff. Timeout is unknown, not success; reconcile where possible and avoid duplicate creates. Use an approved test HTTP endpoint first.

## 4. Detection and event semantics

`region_filter` filters positive source results where supported; it does not prove crossing. `debounce_times` counts matching frames/hops, not seconds, and is not an exact dwell timer. Positive rules do not directly express disappeared, returned, entered, exited, recovered, identity, or cancelled.

For approved VLM use, code should supply one fresh relevant image and require a small result such as `{"region_visible":true,"target":"visible","reason":"..."}` where target is one of `visible`, `not_visible`, `uncertain`. Code attaches image ID/time, validates output, and computes all state, durations, dedupe, and notifications. `not_visible` is only an absence candidate when the region is visible/evaluable. Do not treat generated confidence as calibrated accuracy.

The daemon normally retains only 500 visible results for 100 seconds, with separate recording-association bounds. Restart, TTL, capacity eviction, and storage rotation can remove history. Poll `get_detection_events` promptly (normally 1–10 seconds) with overlap. Merge whole rows by `uid_str` (legacy: decimal `uid`) and files by `file_id_str` (legacy: path + creator UID). Late FILE updates retain the original RULE timestamp. Process the full batch before advancing a checkpoint, retain overlap for late updates, and fetch every relevant `file_events[].path`; `snapshot_path` is only a first-file/legacy fallback. Persist event IDs, observations, and evidence externally. An empty query means no retained matching row, not proof that nothing happened.

A conversation response is not background push. `set_notify_config` controls the shared MQTT/HTTP/UART inference-result pipeline, not durable tasks, idempotency, business acknowledgement, or retries. Applying it briefly restarts the result pipeline. Passwords/tokens are redacted; never write `***` back.

## 5. Modify, cancel, and verify

Modify a task by incrementing its generation so old in-flight results cannot notify under the new contract. Cancel by stopping new checks, cancelling or awaiting in-flight work, and invalidating late delivery. Ask separately before disabling/restoring shared device rules; cancelling a runner does not change those rules. After restart/outage, report actual recovery and mark gaps rather than synthesizing history.

Before claiming success:

1. Read back model, trigger, rules, writer, schedule, and storage as applicable.
2. Verify the runner and one fresh observation.
3. With permission, run controlled positive and negative scenes, including outside ROI, brief occlusion, repeated presence, and absence as relevant.
4. Verify evidence correlation, one notification per episode, real cadence and latency, reset/re-arm, query/modify/cancel, expiry, restart, and outage.
5. For initial acceptance, use 10 normal plus 10 interference/negative cases and a 30-minute soak for sustained tasks. Record hits, misses, false alerts, duplicates, delivery latency, creation time, model calls, resource use, and failures; do not present this small regression as an accuracy benchmark.

Report configured, runner verified, controlled scene passed, delivery acknowledged, and not tested separately.

## 6. Basic device operations

1. Register with `add_device`, or confirm a profile with `list_devices`.
2. Verify storage with `get_storage_status` before rule writes; `set_detection_rules` normally ensures JPG writer and default storage.
3. For AI evidence, select an inventoried model, optionally set weekly schedule, install validated rules, then read back and test. For non-AI recording use `set_record_trigger` (`timer`, `gpio`, `tty`, `http`, `always_on`). For sound, discover source labels and use `inference_set` with `source_filter=["acousticslab"]`; legacy `sed` is retired.
4. Retrieve event snapshots with `fetch_file` (absolute event paths) or browse record-relative paths with `list_records`/`fetch_record`.

For “fix the device time”, call `sync_system_time`: the UTC clock is fixed before timezone decisions. Do not resolve timezone through HTTPS first while the clock is wrong. Its SNTP/plain-HTTP/WiFi/DHCP probes are broken-clock safe. Relay `needs_user_confirmation` or `needs_user_help` instead of retrying. Timezone writes are read-back verified because firmware can silently reject unknown zones. For HTTPS, keep `allow_unsecured=false` unless explicitly trusting a self-signed certificate on a trusted LAN.
