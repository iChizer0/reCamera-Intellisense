# Observation task playbook (S01–S07)

Read this playbook whenever a user asks to inspect a scene, watch/monitor a target, alert on a condition, query observation history, or modify/cancel an observation task. It maps natural-language goals to existing reCamera primitives without inventing a task engine.

These are **design and configuration templates, not a runnable watcher**. The SDK and MCP expose capture, model, recording-rule, event, file, and notification primitives. They do not create a durable observation task, run a scheduler, or persist task history for the agent.

## 1. Control contract: code owns the loop

Ashby's law of requisite variety is practical here: an unconstrained chat loop cannot reliably absorb all timing, occlusion, network, duplication, restart, and cancellation cases. Keep authority in a deterministic external skeleton.

| Layer | Owns | Must not be credited with |
| --- | --- | --- |
| Device model + recording rules | Positive class/ROI matches, debounce, evidence files | Absence, crossing direction, identity, exact dwell, task lifecycle |
| External deterministic runner (normally Python) | Scheduling, state machine, task CRUD, checkpoints, dedupe, persistence, retries, cancellation | Visual judgment it has not obtained |
| LLM/VLM, when needed | Bounded interpretation of one selected image or a small evidence set | The main loop, timers, delivery success, unobserved history |
| Notification channel | Transport offered by that channel | Durable business records or exactly-once delivery unless separately proven |

The LLM is a **stateless cognitive operator**: code chooses when to call it, which image/region to supply, and which schema to return; code validates that output before changing task state. Do not let an LLM conversation become an open-ended `while True` monitor.

If no persistent runner or agent scheduler is available, do **not** say “I will keep watching” or “you will be notified later.” With user permission, persistent device recording rules may still collect evidence, but explain that somebody must query the bounded event buffer before it expires. Otherwise offer a fresh one-shot check or an explicitly bounded check while the agent is still running.

## 2. Route the request before changing anything

### Step A — Classify the intent

- **Look now / look again (S01):** capture a new image. No detection-rule write.
- **Watch for a duration / alert later (S02–S06):** requires a runner and a task contract in addition to any device rules.
- **Write observations to another system (S07):** requires durable event and delivery state, idempotent retries, and a test endpoint.
- **Query / modify / cancel:** identify the external task ID and its current generation. Device rule names are not automatically external task IDs.

### Step B — Establish the target and region

For a spatial request, take a fresh reference image with `capture_image` and show it when the host supports image display. Resolve “this one”, “that sofa”, and “inside the door” against that image. Ask a focused question only when the target or boundary is ambiguous; do not silently choose one of several parcels, pets, vehicles, or regions.

Record the confirmed normalized polygon and the reference image. Never reuse a convenient example polygon as the user's actual ROI.

### Step C — Discover only the capabilities needed

For S01, discovery normally stops at capture. For observation tasks, use the existing commands instead of probing arbitrary endpoints:

```bash
rci get_detection_models_info device_name=cam1
rci get_detection_model device_name=cam1
rci get_record_sources device_name=cam1
rci list_apps device_name=cam1
rci get_record_trigger device_name=cam1
rci get_record_config device_name=cam1
rci get_detection_schedule device_name=cam1
rci get_storage_status device_name=cam1
rci get_resource_info device_name=cam1
```

Read existing configuration before overwriting it. Inspect `get_notify_config` only when push delivery is relevant; its secrets are redacted and must remain out of prompts and logs.

Match exact, case-sensitive labels reported by the selected model/source. Do not assume that a generic model detects **package**, **cardboard box**, a specific pet, or a **license plate**. `person`, `cat`, `dog`, and `car` are usable only when actually reported. Vehicle detection is not OCR, an ROI match is not an entry transition, and a class match is not identity recognition.

### Step D — If no installed capability matches, ask in user terms

Do not improvise a training project or silently send images to an external service. Ask one compact question, for example:

> 设备现有模型不能直接识别“这个包裹/车牌”。您更希望：1）在允许上传快照的前提下尝试在线视觉服务（通常较快，但需要联网，图片会离开设备）；2）查找可安装的现成模型或应用（可能离线运行，但需要兼容性验证）；3）把需求简化为现有模型能识别的目标；还是 4）先暂停？

Choose the recommendation from the user's constraints rather than technical novelty. If images may leave the device and a provider is available, an online VLM can be the quickest experiment. If privacy/offline operation matters, search existing applications/models first. Always disclose upload, provider, cost/quota, network, and retention implications known for the selected route.

With approval, candidate sources include the device App Center/model store and:

- <https://github.com/airockchip/rknn_model_zoo>
- <https://github.com/airockchip/rknn-llm>

A repository hit is only a lead. Verify RV1126B/RKNN compatibility, runtime and pre/post-processing, exact labels, ROI support, resource budget, and license. `rknn-llm` is not evidence of a ready-made detector or OCR application. Do not download, convert, install, or start anything without the corresponding user authorization. Prefer an existing compatible model; do not turn the request into model training.

For a custom on-device pipeline, hand off to the `recamera-pysdk` skill and assess its actual runtime prerequisites. Its Kit routes can support zones, tracking, temporal state, or OCR with suitable existing models; that is a separate development and validation task, not something `set_detection_rules` adds automatically.

## 3. Turn the approved intent into a task contract

Before writing device configuration, present a concise summary and obtain permission for the proposed model/rule/schedule/notify changes. Defaults should be visible rather than silently invented. Preserve prior configuration so an owned change can be restored deliberately.

An external runner should persist at least these fields:

| Field group | Required content |
| --- | --- |
| Identity/state | Task ID, generation/version, `pending/running/paused/failed/completed/cancelled`, owner |
| Goal | Target, exact labels or approved VLM route, confirmed ROI/reference image |
| Time | Start/end instant or recurring schedule, requested and actual cadence |
| Trigger | Positive condition, confirmation count/duration, unknown/gap policy |
| Reset | Exit/absence confirmation, re-arm rule, one-shot vs repeatable |
| Delivery | Real channel, evidence attachments, dedupe/event ID, retry policy |
| Health | Last successful observation, last device event, last error, resource limits |
| Evidence | Image/file references, observation timestamps, persisted event history, gaps |
| Device changes | Previous model/trigger/rules/schedule/writer/storage and which changes this task owns |

A weekly `set_detection_schedule` window is not a one-shot “next 20 minutes” lifetime. The external runner must enforce relative start/end instants. Only one record trigger and one built-in vision model are active at a time, so a new task may disrupt an existing one; detect this and ask rather than overwriting an unrelated task.

“Started” is truthful only after the runner is actually alive and the initial observation/read-back checks succeed. Return the task ID, real cadence, end time, notification channel, and last successful check when queried.

## 4. Compile only semantics the primitives really have

### Positive detection rules

`set_detection_rules` installs positive-match `inference_set` recording rules. It does not create the external task above and does not directly express:

- target disappeared or returned;
- entered versus merely present, exited, or crossed in a direction;
- exact dwell time or “clear again”;
- persistent identity, arrival dedupe, business delivery, or task cancellation.

`region_filter` filters source results by normalized ROI only where `supports_roi` is true. It does not prove a boundary crossing. `debounce_times` counts consecutive matching frames/hops, not seconds; do not convert it to an exact dwell threshold without measured source timing and controlled validation.

### Observation states and gaps

Keep these outcomes distinct:

- **observed positive/negative:** a usable observation supports the claim;
- **unknown:** target/region occluded, blurred, ambiguous, or model output failed validation;
- **capture/device failure:** no fresh observation exists;
- **no retained event:** the event query succeeded but returned no matching row.

No event, no detection, absence, and recovery are not interchangeable. A camera gap must never count as an empty region, a departure, or continued dwell. Persist enough observations/events to answer history queries after the volatile buffer has expired.

When a VLM is approved, give it one fresh, task-relevant image and a small schema such as:

```json
{
  "region_visible": true,
  "target": "visible",
  "reason": "short factual description"
}
```

Allowed `target` values are `visible`, `not_visible`, and `uncertain`. The runner attaches image ID and capture time, validates the enum/schema, and computes all transitions, durations, dedupe, and notifications. `not_visible` is an absence candidate only when the region itself is visible and evaluable; occlusion or blur is `uncertain`. A VLM-reported confidence is not a calibrated probability and must not substitute for acceptance tests.

### Event consumption and evidence

The daemon's default visible-event retention is bounded (normally 500 results for 100 seconds; recording associations have separate bounds). Restart, TTL, capacity eviction, and storage rotation can remove history. Poll promptly, normally every 1–10 seconds, with an overlap behind the checkpoint.

Merge whole rows by `uid_str` (legacy fallback: decimal `uid`) and files by `file_id_str` (legacy fallback: path + creator UID). Late FILE updates retain the original RULE timestamp. Process the complete batch before advancing a checkpoint, retain an overlap for late updates, and fetch every relevant `file_events[].path`; `snapshot_path` is only the first-file/legacy fallback. Persist event IDs, evidence paths, and observations externally. An empty result means “nothing retained in this query”, not proof that nothing happened.

### Notifications

A conversation response is not a background push channel. `set_notify_config` configures the device's shared MQTT/HTTP/UART result pipeline; it does not add external task state, durable event IDs, business-system acknowledgement, or retry semantics. Applying it briefly restarts the result pipeline. Never put the redacted `***` secret placeholder back into a write.

## 5. Scenario templates

The routes below deliberately separate evidence acquisition from the promised high-level condition.

| Scenario | Evidence route | Additional deterministic requirement |
| --- | --- | --- |
| S01 current view | Fresh `capture_image` | Bind answer to new image ID/time; admit uncertainty |
| S02 parcel removed | Timed fresh images + approved detector/VLM | Present/absent/unknown state machine and end time |
| S03 person in region | Person model + ROI recording rule + events | External entry/dwell/exit state if those words are promised |
| S04 pet in forbidden region | Cat/dog model + ROI recording rule + events | Presence state, 30 s valid-absence reset, no identity claim |
| S05 vehicle + plate | Suitable vehicle evidence plus a separately approved OCR route | Exact plate validation, arrival dedupe, time window |
| S06 prolonged obstruction | Timed fresh images + approved detector/VLM | Occupancy/recovery state machine over valid observations |
| S07 business integration | Persisted observation event + test HTTP integration | Idempotency, delivery status, timeout handling, retry |

### S01 — Look now and follow up

1. Call `capture_image` for every “now”, “again”, or “I moved it” request.
2. Answer from the returned image and include its capture timestamp/event. Keep object references (“the right one”) anchored to that image; ask if ambiguous.
3. If text is unreadable, say whether resolution, angle, blur, occlusion, or lighting prevents the answer. Never invent a label.
4. On capture failure, report failure. Never reuse an old image as current evidence. No background task is needed for this scenario.

### S02 — Parcel removed (and optionally returned)

Suggested initial experiment: 30 minutes, one fresh check every 5 seconds, two consecutive evaluable absence observations before one notification. Show the actual achievable cadence rather than promising it.

Use an initial confirmed reference and a state machine such as `PRESENT -> ABSENCE_CANDIDATE -> ABSENT`, with optional `ABSENT -> PRESENT` for the modified “tell me when it returns” request. Short occlusion, blur, and capture failure are unknown, not confirmed absence. Define whether unknown resets or pauses a candidate and disclose that policy. Notify once per generation/transition, with before/after evidence, last-seen time, and first-confirmed-absent time. Do not claim the exact removal instant.

A positive person rule is not a parcel-removal detector, and a gap in positive parcel events is not proof of removal. If no suitable parcel model exists, use the approved VLM/model route above or narrow the request honestly.

### S03 — Person enters or stays in a region

The preferred **evidence-first** route is an installed model that actually reports `person`, a user-confirmed ROI, and an `inference_set` recording rule. The daemon then supplies timestamped positive matches and image evidence for a runner to inspect/persist.

Illustrative configuration only: replace the model and polygon with discovered and user-confirmed values. This example asserts **person detected in ROI**, not entry, exit, or ten seconds of dwell.

```bash
# Preconditions already verified: yolo11n is installed and reports "person";
# storage/source are ready; the user approved this shared-config change.
rci set_detection_model device_name=cam1 model_name=yolo11n
rci set_detection_rules device_name=cam1 'rules=[{"name":"s03-person-region","debounce_times":3,"confidence_range_filter":[0.25,1.0],"label_filter":["person"],"source_filter":["builtin"],"region_filter":[[[0.1,0.6],[0.9,0.6],[0.9,0.95],[0.1,0.95]]]}]'

# Read back; then run a controlled positive and negative scene test.
rci get_detection_model device_name=cam1
rci get_detection_rules device_name=cam1
rci get_record_trigger device_name=cam1
```

For “enters”, start with a valid outside/absent baseline and require a later valid inside observation. For “stays over 10 seconds”, accumulate only valid observed occupancy and treat acquisition gaps explicitly. For exit/re-entry, obtain valid negative observations; absence of positive events alone cannot prove exit. Reset after the confirmed outside condition so a continuously present person does not spam notifications. The external runner owns these semantics even though the device owns detection and recording.

### S04 — Cat/dog in a forbidden region

Reuse S03 only when the active model reports the required `cat` or `dog` label and the source supports ROI. “Animal appeared somewhere in frame” is not “cat on the sofa.” Apply the same outside/inside state separation.

For “new event only after leaving for 30 seconds”, require valid negative observations covering the reset policy; do not count an observation outage as 30 seconds away. Do not distinguish two similar animals or infer health/mood without a separately validated identity capability.

### S05 — Specific vehicle arrival and plate record

Stop at capability discovery until both detection and OCR routes are approved. A `car`/`truck` label or a generic VLM response does not establish reliable plate recognition. Confirm installation angle, resolution, lighting, and the initial static/low-speed scope.

Keep original image, plate crop when available, raw OCR result, exact-match result, and time/region evidence. Similar characters, blur, occlusion, or conflicting reads produce `unknown`, not a fuzzy match. Deduplicate one arrival externally and apply the absolute time window in the runner. Printed-plate and real-vehicle tests are separate evidence; neither proves night/high-speed performance. No gate control is implied.

### S06 — Passage occupied for a duration, then clear

Suggested experiment: sample every 10 seconds, confirm continuous occupancy for 60 seconds, send one occupied alert and one recovery alert. First verify that the chosen model can actually detect the target object; do not equate a person walking through with a cardboard-box obstruction.

Use `CLEAR -> OCCUPY_CANDIDATE -> OCCUPIED -> RECOVERY_CANDIDATE -> CLEAR`. Repeated samples are one episode, not repeated alerts. Save occupied start, first threshold confirmation, recovery, and corresponding evidence. Unknown frames cannot prove either continued occupancy or recovery; apply the declared pause/reset and gap policy. Time accuracy is bounded by cadence and observation validity. This is inspection assistance, not a safety-certified alarm.

### S07 — Write observations to a business system

Separate the visual event from delivery. Persist at least:

| Record | Minimum state |
| --- | --- |
| Observation | Stable event ID, task ID/generation, evidence, visual judgment status |
| Delivery | `pending/succeeded/failed/unknown`, attempts, last error, response, next retry |
| Retry | Same event ID/idempotency key, bounded backoff, reconciliation after timeout |

A timeout is `unknown`, not success; a retry must not create a duplicate record. Use a user-approved test HTTP endpoint first and query failures from persisted delivery state. The shared device notify pipeline may be a transport input, but it does not replace this external ledger or prove end-to-end acceptance.

## 6. Modify, cancel, restart, and report honestly

- **Modify:** identify the task, increment its generation, validate the new contract, and prevent late results from the old generation from notifying.
- **Cancel:** stop scheduling new work, cancel/await in-flight work, invalidate late deliveries, and verify runner state. Separately ask before disabling or restoring shared device rules; stopping a runner does not itself change them.
- **Restart/reconnect:** report whether the runner and rules resumed. Mark the unobserved interval as a gap; do not synthesize events across it.
- **History:** answer only from persisted observations/files. Distinguish “not observed”, “unknown”, “capture failed”, and “history expired/unavailable”.
- **Capacity:** start with one observation task per device. Measure actual cadence, latency, CPU/NPU/memory/storage, and model calls before parallel tasks; reject or visibly reduce cadence rather than silently dropping work.

## 7. Required verification before claiming success

Configuration read-back proves configuration only. It does not prove scene semantics or delivery.

1. Read back model, trigger, rules, writer, schedule, and storage as applicable.
2. Verify the external runner state and one successful fresh observation.
3. With user permission, perform a controlled positive event and relevant negatives (outside ROI, brief occlusion, repeated presence, no target).
4. Verify event-to-file correlation, one notification per episode, timestamps, actual cadence/latency, and the requested reset condition.
5. Test query, modify, cancel (including an in-flight result), expiration, restart, and network/camera interruption for a sustained task.
6. For initial scenario acceptance, run 10 normal events and 10 interference / negative cases, plus a 30-minute sustained-task soak where relevant. Record hits, misses, false positives, duplicate notifications, event-to-delivery latency, creation time, model calls, resource use, and failure reasons. These are regression results, not an accuracy advertisement.

Report each evidence level separately: **configured**, **runner verified**, **controlled scene passed**, **delivery acknowledged**, and **not tested**. Never collapse them into “monitoring works”.

## 8. Implementation hand-off

When the user approves implementation, prefer a small deterministic Python runner using this SDK for scheduling, state, persistence, and retries. If the built-in model/rule path cannot supply the required observation semantics, assess a custom app through `recamera-pysdk` using an existing compatible model and its zones/tracking/temporal/OCR facilities. Use an online VLM only as the explicitly approved bounded observation operator. If none of those routes is available, narrow the deliverable instead of letting the agent invent a persistent main loop.
