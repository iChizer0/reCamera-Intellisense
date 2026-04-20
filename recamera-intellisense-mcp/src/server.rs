use std::sync::Arc;
use tokio::sync::RwLock;

use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::*,
    tool, tool_handler, tool_router, ServerHandler,
};

use crate::api::{
    capture as api_capture, daemon as api_daemon, gpio as api_gpio, rule as api_rule,
    storage as api_storage,
};
use crate::api_client::ApiClient;
use crate::detection;
use crate::device_store::DeviceStore;
use crate::records::{self, RelayCache};
use crate::types::*;
use crate::util::mime_from_ext;

// MARK: Helpers

macro_rules! try_tool {
    ($result:expr) => {
        match $result {
            Ok(val) => val,
            Err(e) => return Ok(CallToolResult::error(vec![Content::text(format!("{e}"))])),
        }
    };
}

fn validate_not_empty(value: &str, field: &str) -> anyhow::Result<()> {
    anyhow::ensure!(!value.trim().is_empty(), "'{field}' must not be empty.");
    Ok(())
}

fn text_result(text: &str) -> CallToolResult {
    CallToolResult::success(vec![Content::text(text)])
}

fn json_result<T: serde::Serialize>(value: &T) -> anyhow::Result<CallToolResult> {
    let json = serde_json::to_string_pretty(value)?;
    Ok(CallToolResult::success(vec![Content::text(json)]))
}

const MAX_INLINE_BYTES: usize = 5 * 1024 * 1024;

fn b64(bytes: &[u8]) -> String {
    base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes)
}

fn render_bytes(path: &str, bytes: Vec<u8>, url_hint: Option<&str>) -> CallToolResult {
    let mime = mime_from_ext(path);
    if mime.starts_with("video/") {
        return text_result(&format!(
            "File '{}' is a video ({}). Video files are too large for inline transfer.{}",
            path,
            mime,
            url_hint
                .map(|u| format!(" Direct URL: {u}"))
                .unwrap_or_default()
        ));
    }
    if bytes.len() > MAX_INLINE_BYTES {
        return text_result(&format!(
            "File '{}' is {:.1} MB which is too large for inline transfer.{}",
            path,
            bytes.len() as f64 / (1024.0 * 1024.0),
            url_hint
                .map(|u| format!(" Direct URL: {u}"))
                .unwrap_or_default()
        ));
    }
    if mime.starts_with("image/") {
        return CallToolResult::success(vec![Content::image(b64(&bytes), mime)]);
    }
    if mime.starts_with("text/") || mime == "application/json" || mime == "application/xml" {
        return match String::from_utf8(bytes) {
            Ok(text) => CallToolResult::success(vec![Content::text(text)]),
            Err(e) => {
                let raw = e.into_bytes();
                CallToolResult::success(vec![Content::text(format!(
                    "File '{}' (not valid UTF-8, {} bytes, base64-encoded):\n{}",
                    path,
                    raw.len(),
                    b64(&raw)
                ))])
            }
        };
    }
    CallToolResult::success(vec![Content::text(format!(
        "File '{}' ({}, {} bytes, base64-encoded):\n{}",
        path,
        mime,
        bytes.len(),
        b64(&bytes)
    ))])
}

// MARK: Server

#[derive(Clone)]
pub struct ReCameraServer {
    store: Arc<RwLock<DeviceStore>>,
    client: Arc<ApiClient>,
    relay_cache: RelayCache,
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl ReCameraServer {
    pub fn new(store: DeviceStore, client: ApiClient) -> Self {
        Self {
            store: Arc::new(RwLock::new(store)),
            client: Arc::new(client),
            relay_cache: RelayCache::new(),
            tool_router: Self::tool_router(),
        }
    }

    // MARK: Device management

    #[tool(
        description = "Detect a reCamera Intellisense daemon running locally by checking its Unix socket. Returns a structured result: {detected: bool, socket_path: string, hint?: string}. When detected=false, 'hint' explains how to launch the daemon.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn detect_local_device(
        &self,
        Parameters(params): Parameters<DetectLocalDeviceParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let socket_path = params
            .socket_path
            .as_deref()
            .unwrap_or("/dev/shm/rcisd.sock");
        let found = ApiClient::detect_local(socket_path).await;
        let payload = if found {
            serde_json::json!({
                "detected": true,
                "socket_path": socket_path,
            })
        } else {
            serde_json::json!({
                "detected": false,
                "socket_path": socket_path,
                "hint": format!(
                    "No rcisd daemon found at '{socket_path}'. Start the daemon (recamera-intellisense-daemon) or pass socket_path explicitly."
                ),
            })
        };
        Ok(try_tool!(json_result(&payload)))
    }

    #[tool(
        description = "Register (add) a new reCamera device. Connectivity is tested before saving.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn add_device(
        &self,
        Parameters(params): Parameters<AddDeviceParams>,
    ) -> Result<CallToolResult, ErrorData> {
        try_tool!(validate_not_empty(&params.name, "name"));
        try_tool!(validate_not_empty(&params.host, "host"));
        try_tool!(validate_not_empty(&params.token, "token"));
        let protocol = params.protocol.unwrap_or(Protocol::Http);
        // Secure-by-default: verify TLS certs unless the caller opts in.
        let allow_unsecured = params.allow_unsecured.unwrap_or(false);
        try_tool!(
            self.client
                .test_connection(
                    &params.host,
                    &params.token,
                    protocol.as_str(),
                    allow_unsecured,
                    params.port
                )
                .await
        );
        let mut store = self.store.write().await;
        try_tool!(
            store
                .add_device(
                    &params.name,
                    &params.host,
                    &params.token,
                    protocol.as_str(),
                    allow_unsecured,
                    params.port
                )
                .await
        );
        Ok(text_result(&format!(
            "Device '{}' added successfully.",
            params.name
        )))
    }

    #[tool(
        description = "Update an existing reCamera device's credentials. Connectivity is re-tested only when network-relevant fields change.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn update_device(
        &self,
        Parameters(params): Parameters<UpdateDeviceParams>,
    ) -> Result<CallToolResult, ErrorData> {
        try_tool!(validate_not_empty(&params.device_name, "device_name"));
        let (host, token, protocol, allow_unsecured, port, must_reprobe) = {
            let store = self.store.read().await;
            let existing = match store.get_device(&params.device_name) {
                Some(d) => d,
                None => {
                    return Ok(CallToolResult::error(vec![Content::text(format!(
                        "Device '{}' not found. Use list_devices to see registered profiles.",
                        params.device_name
                    ))]))
                }
            };
            let host_changed = params.host.as_ref().is_some_and(|h| h != &existing.host);
            let token_changed = params.token.as_ref().is_some_and(|t| t != &existing.token);
            let proto_changed = params
                .protocol
                .as_ref()
                .is_some_and(|p| p.as_str() != existing.protocol);
            let port_changed = matches!(params.port, Some(p) if Some(p) != existing.port);
            let unsec_changed = params
                .allow_unsecured
                .is_some_and(|u| u != existing.allow_unsecured);
            let host = params.host.clone().unwrap_or(existing.host.clone());
            let token = params.token.clone().unwrap_or(existing.token.clone());
            let protocol = params
                .protocol
                .map(|p| p.as_str().to_string())
                .unwrap_or(existing.protocol);
            let allow_unsecured = params.allow_unsecured.unwrap_or(existing.allow_unsecured);
            let port = params.port.or(existing.port);
            let must_reprobe =
                host_changed || token_changed || proto_changed || port_changed || unsec_changed;
            (host, token, protocol, allow_unsecured, port, must_reprobe)
        };
        if must_reprobe {
            try_tool!(
                self.client
                    .test_connection(&host, &token, &protocol, allow_unsecured, port)
                    .await
            );
        }
        let mut store = self.store.write().await;
        try_tool!(
            store
                .replace_device(
                    &params.device_name,
                    &host,
                    &token,
                    &protocol,
                    allow_unsecured,
                    port
                )
                .await
        );
        Ok(text_result(&format!(
            "Device '{}' updated successfully.",
            params.device_name
        )))
    }

    #[tool(
        description = "Remove a registered reCamera device.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn remove_device(
        &self,
        Parameters(params): Parameters<DeviceNameParams>,
    ) -> Result<CallToolResult, ErrorData> {
        try_tool!(validate_not_empty(&params.device_name, "device_name"));
        let mut store = self.store.write().await;
        if store.remove_device(&params.device_name).await {
            Ok(text_result(&format!(
                "Device '{}' removed successfully.",
                params.device_name
            )))
        } else {
            Ok(CallToolResult::error(vec![Content::text(format!(
                "Device '{}' not found.",
                params.device_name
            ))]))
        }
    }

    #[tool(
        description = "Get the connection credentials of a registered reCamera device.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn get_device(
        &self,
        Parameters(params): Parameters<DeviceNameParams>,
    ) -> Result<CallToolResult, ErrorData> {
        try_tool!(validate_not_empty(&params.device_name, "device_name"));
        let store = self.store.read().await;
        match store.get_device(&params.device_name) {
            Some(device) => Ok(try_tool!(json_result(&device))),
            None => Ok(CallToolResult::error(vec![Content::text(format!(
                "Device '{}' not found.",
                params.device_name
            ))])),
        }
    }

    #[tool(
        description = "List all registered reCamera devices sorted by name.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn list_devices(&self) -> Result<CallToolResult, ErrorData> {
        let store = self.store.read().await;
        Ok(try_tool!(json_result(&store.list_devices())))
    }

    // MARK: Detection

    #[tool(
        description = "Get information about available detection models on a reCamera device.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn get_detection_models_info(
        &self,
        Parameters(params): Parameters<DeviceNameParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let device = try_tool!(self.resolve(&params.device_name).await);
        let models = try_tool!(detection::get_detection_models_info(&self.client, &device).await);
        Ok(try_tool!(json_result(&models)))
    }

    #[tool(
        description = "Get the currently active detection model. Returns null if detection is disabled.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn get_detection_model(
        &self,
        Parameters(params): Parameters<DeviceNameParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let device = try_tool!(self.resolve(&params.device_name).await);
        let model = try_tool!(detection::get_detection_model(&self.client, &device).await);
        Ok(try_tool!(json_result(&model)))
    }

    #[tool(
        description = "Set the active detection model by model_id or model_name (provide exactly one).",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn set_detection_model(
        &self,
        Parameters(params): Parameters<SetDetectionModelParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let device = try_tool!(self.resolve(&params.device_name).await);
        try_tool!(
            detection::set_detection_model(
                &self.client,
                &device,
                params.model_id,
                params.model_name.as_deref()
            )
            .await
        );
        Ok(text_result("Detection model set successfully."))
    }

    #[tool(
        description = "Get the current detection schedule. Returns null if disabled.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn get_detection_schedule(
        &self,
        Parameters(params): Parameters<DeviceNameParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let device = try_tool!(self.resolve(&params.device_name).await);
        let schedule = try_tool!(detection::get_detection_schedule(&self.client, &device).await);
        Ok(try_tool!(json_result(&schedule)))
    }

    #[tool(
        description = "Set the detection schedule (equivalent to set_schedule_rule). Use null/empty to disable. Time format: 'Day HH:MM:SS'.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn set_detection_schedule(
        &self,
        Parameters(params): Parameters<SetDetectionScheduleParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let device = try_tool!(self.resolve(&params.device_name).await);
        try_tool!(
            detection::set_detection_schedule(&self.client, &device, params.schedule.as_deref())
                .await
        );
        Ok(text_result("Detection schedule set successfully."))
    }

    #[tool(
        description = "Get current detection rules. Empty if prerequisites are not met or trigger is not INFERENCE_SET.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn get_detection_rules(
        &self,
        Parameters(params): Parameters<DeviceNameParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let device = try_tool!(self.resolve(&params.device_name).await);
        let rules = try_tool!(detection::get_detection_rules(&self.client, &device).await);
        Ok(try_tool!(json_result(&rules)))
    }

    #[tool(
        description = "Set detection rules (INFERENCE_SET trigger). Auto-enables record image and default storage. confidence_range_filter must be [min,max] in [0,1].",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn set_detection_rules(
        &self,
        Parameters(params): Parameters<SetDetectionRulesParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let device = try_tool!(self.resolve(&params.device_name).await);
        try_tool!(detection::set_detection_rules(&self.client, &device, &params.rules).await);
        Ok(text_result("Detection rules set successfully."))
    }

    #[tool(
        description = "Get detection events within an optional [start_unix_ms, end_unix_ms] window.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn get_detection_events(
        &self,
        Parameters(params): Parameters<GetDetectionEventsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let device = try_tool!(self.resolve(&params.device_name).await);
        let events = try_tool!(
            detection::get_detection_events(
                &self.client,
                &device,
                params.start_unix_ms,
                params.end_unix_ms
            )
            .await
        );
        Ok(try_tool!(json_result(&events)))
    }

    #[tool(
        description = "Clear all cached detection events on a reCamera device.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn clear_detection_events(
        &self,
        Parameters(params): Parameters<DeviceNameParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let device = try_tool!(self.resolve(&params.device_name).await);
        try_tool!(detection::clear_detection_events(&self.client, &device).await);
        Ok(text_result("Detection events cleared."))
    }

    // MARK: Rule system

    #[tool(
        description = "Get low-level rule-system info: ready flag, last event, available GPIOs/TTYs, media state.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn get_rule_system_info(
        &self,
        Parameters(params): Parameters<DeviceNameParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let device = try_tool!(self.resolve(&params.device_name).await);
        let info = try_tool!(api_rule::get_info(&self.client, &device).await);
        Ok(try_tool!(json_result(&info)))
    }

    #[tool(
        description = "Get the global record configuration (rule enabled + writer format/interval).",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn get_record_config(
        &self,
        Parameters(params): Parameters<DeviceNameParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let device = try_tool!(self.resolve(&params.device_name).await);
        let cfg = try_tool!(api_rule::get_config(&self.client, &device).await);
        Ok(try_tool!(json_result(&cfg)))
    }

    #[tool(
        description = "Set the global record configuration. writer_format: MP4 | JPG | RAW. writer_interval_ms: 0 = continuous.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn set_record_config(
        &self,
        Parameters(params): Parameters<SetRuleConfigParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let device = try_tool!(self.resolve(&params.device_name).await);
        let cfg = RuleConfig {
            rule_enabled: params.rule_enabled,
            writer: WriterConfig {
                format: params.writer_format.as_str().to_string(),
                interval_ms: params.writer_interval_ms,
            },
        };
        try_tool!(api_rule::set_config(&self.client, &device, &cfg).await);
        Ok(text_result("Record config updated."))
    }

    #[tool(
        description = "Get the schedule rule. Returns null if disabled.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn get_schedule_rule(
        &self,
        Parameters(params): Parameters<DeviceNameParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let device = try_tool!(self.resolve(&params.device_name).await);
        let s = try_tool!(api_rule::get_schedule(&self.client, &device).await);
        Ok(try_tool!(json_result(&s)))
    }

    #[tool(
        description = "Set the schedule rule (equivalent to set_detection_schedule). Use null/empty to disable. Time format: 'Day HH:MM:SS'.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn set_schedule_rule(
        &self,
        Parameters(params): Parameters<SetDetectionScheduleParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let device = try_tool!(self.resolve(&params.device_name).await);
        try_tool!(api_rule::set_schedule(&self.client, &device, params.schedule.as_deref()).await);
        Ok(text_result("Schedule rule updated."))
    }

    #[tool(
        description = "Get the current record trigger (tagged: inference_set | timer | gpio | tty | http | always_on).",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn get_record_trigger(
        &self,
        Parameters(params): Parameters<DeviceNameParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let device = try_tool!(self.resolve(&params.device_name).await);
        let trigger = try_tool!(api_rule::get_trigger(&self.client, &device).await);
        Ok(try_tool!(json_result(&trigger)))
    }

    #[tool(
        description = "Set the record trigger. Provide a tagged union with 'kind' = inference_set|timer|gpio|tty|http|always_on.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn set_record_trigger(
        &self,
        Parameters(params): Parameters<SetRecordTriggerParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let device = try_tool!(self.resolve(&params.device_name).await);
        try_tool!(api_rule::set_trigger(&self.client, &device, &params.trigger).await);
        Ok(text_result("Record trigger updated."))
    }

    #[tool(
        description = "Fire an HTTP-triggered record event. Only valid when the current trigger kind is 'http' (call get_record_trigger first to confirm).",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn activate_http_trigger(
        &self,
        Parameters(params): Parameters<DeviceNameParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let device = try_tool!(self.resolve(&params.device_name).await);
        try_tool!(api_rule::activate_http_trigger(&self.client, &device).await);
        Ok(text_result("HTTP trigger activated."))
    }

    // MARK: Storage

    #[tool(
        description = "Get status of all storage slots with mount/state/quota details.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn get_storage_status(
        &self,
        Parameters(params): Parameters<DeviceNameParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let device = try_tool!(self.resolve(&params.device_name).await);
        let slots = try_tool!(api_storage::get_status(&self.client, &device).await);
        Ok(try_tool!(json_result(&slots)))
    }

    #[tool(
        description = "Select the storage slot to enable. Leave both selectors empty to disable all slots.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn set_storage_slot(
        &self,
        Parameters(params): Parameters<SetStorageSlotParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let device = try_tool!(self.resolve(&params.device_name).await);
        try_tool!(
            api_storage::set_selection(&self.client, &device, &params.by_dev_path, &params.by_uuid)
                .await
        );
        Ok(text_result("Storage slot selection updated."))
    }

    #[tool(
        description = "Configure quota on a slot. quota_limit_bytes: -1 = no limit. quota_rotate: recommended true.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn configure_storage_quota(
        &self,
        Parameters(params): Parameters<ConfigureStorageQuotaParams>,
    ) -> Result<CallToolResult, ErrorData> {
        try_tool!(validate_not_empty(&params.dev_path, "dev_path"));
        let device = try_tool!(self.resolve(&params.device_name).await);
        try_tool!(
            api_storage::control_config(
                &self.client,
                &device,
                &params.dev_path,
                params.quota_limit_bytes,
                params.quota_rotate
            )
            .await
        );
        Ok(text_result("Storage quota configured."))
    }

    #[tool(
        description = "Submit a storage maintenance task: FORMAT | FREE_UP | EJECT | REMOVE_FILES_OR_DIRECTORIES. Defaults to async; set sync=true only for EJECT or REMOVE_FILES_OR_DIRECTORIES. REMOVE requires non-empty 'files'.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn storage_task_submit(
        &self,
        Parameters(params): Parameters<StorageTaskSubmitParams>,
    ) -> Result<CallToolResult, ErrorData> {
        try_tool!(validate_not_empty(&params.dev_path, "dev_path"));
        if matches!(params.action, StorageAction::RemoveFilesOrDirectories)
            && params.files.is_empty()
        {
            return Ok(CallToolResult::error(vec![Content::text(
                "REMOVE_FILES_OR_DIRECTORIES requires a non-empty 'files' list.".to_string(),
            )]));
        }
        if params.sync && matches!(params.action, StorageAction::Format | StorageAction::FreeUp) {
            return Ok(CallToolResult::error(vec![Content::text(format!(
                "Action '{}' may take a long time and cannot be run with sync=true. Submit async (sync=false) and poll with storage_task_status.",
                params.action.as_str()
            ))]));
        }
        let device = try_tool!(self.resolve(&params.device_name).await);
        let action_str = params.action.as_str();
        let resp = if params.sync {
            try_tool!(
                api_storage::control_sync(
                    &self.client,
                    &device,
                    action_str,
                    &params.dev_path,
                    &params.files
                )
                .await
            )
        } else {
            try_tool!(
                api_storage::control_async_submit(
                    &self.client,
                    &device,
                    action_str,
                    &params.dev_path,
                    &params.files
                )
                .await
            )
        };
        Ok(try_tool!(json_result(&resp)))
    }

    #[tool(
        description = "Query an async storage task's status history.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn storage_task_status(
        &self,
        Parameters(params): Parameters<StorageTaskQueryParams>,
    ) -> Result<CallToolResult, ErrorData> {
        try_tool!(validate_not_empty(&params.dev_path, "dev_path"));
        let device = try_tool!(self.resolve(&params.device_name).await);
        let resp = try_tool!(
            api_storage::control_async_status(
                &self.client,
                &device,
                params.action.as_str(),
                &params.dev_path,
                params.task_uid.as_deref(),
            )
            .await
        );
        Ok(try_tool!(json_result(&resp)))
    }

    #[tool(
        description = "Cancel an in-flight async storage task.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn storage_task_cancel(
        &self,
        Parameters(params): Parameters<StorageTaskQueryParams>,
    ) -> Result<CallToolResult, ErrorData> {
        try_tool!(validate_not_empty(&params.dev_path, "dev_path"));
        let device = try_tool!(self.resolve(&params.device_name).await);
        let resp = try_tool!(
            api_storage::control_async_cancel(
                &self.client,
                &device,
                params.action.as_str(),
                &params.dev_path,
                params.task_uid.as_deref(),
            )
            .await
        );
        Ok(try_tool!(json_result(&resp)))
    }

    // MARK: Records (relay-backed, recommended for browsing recordings)

    #[tool(
        description = "List entries under the record data directory on the target (or enabled) slot. path is relative to the data directory (empty = root). Supports pagination via limit (default 100, max 500) and offset (default 0); response includes total count and has_more flag. Note: internally refreshes the relay TTL.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn list_records(
        &self,
        Parameters(params): Parameters<ListRecordsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let device = try_tool!(self.resolve(&params.device_name).await);
        let listing = try_tool!(
            records::list_records(
                &self.relay_cache,
                &self.client,
                &device,
                params.dev_path.as_deref(),
                &params.path,
                params.limit,
                params.offset,
            )
            .await
        );
        Ok(try_tool!(json_result(&listing)))
    }

    #[tool(
        description = "Fetch a record file via the relay. path is relative to the data directory. Videos or files > 5 MB return the direct relay URL. Note: internally refreshes the relay TTL.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn fetch_record(
        &self,
        Parameters(params): Parameters<FetchRecordParams>,
    ) -> Result<CallToolResult, ErrorData> {
        try_tool!(validate_not_empty(&params.path, "path"));
        let device = try_tool!(self.resolve(&params.device_name).await);
        let mime = mime_from_ext(&params.path);

        if mime.starts_with("video/") {
            let url = try_tool!(
                records::fetch_record_url(
                    &self.relay_cache,
                    &self.client,
                    &device,
                    params.dev_path.as_deref(),
                    &params.path
                )
                .await
            );
            return Ok(text_result(&format!(
                "Video file '{}' ({}). Direct URL (valid while relay is open): {url}",
                params.path, mime
            )));
        }

        let (bytes, url) = try_tool!(
            records::fetch_record(
                &self.relay_cache,
                &self.client,
                &device,
                params.dev_path.as_deref(),
                &params.path
            )
            .await
        );
        Ok(render_bytes(&params.path, bytes, Some(&url)))
    }

    // MARK: Files (daemon-backed, arbitrary absolute paths)

    #[tool(
        description = "Fetch an arbitrary file via the daemon (/api/v1/file). Path must be absolute and under the daemon's allowed prefix. For captures and detection-event snapshots. Images inline; videos / >5 MB skipped.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn fetch_file(
        &self,
        Parameters(params): Parameters<FetchFileParams>,
    ) -> Result<CallToolResult, ErrorData> {
        try_tool!(validate_not_empty(&params.path, "path"));
        let device = try_tool!(self.resolve(&params.device_name).await);
        let bytes = try_tool!(api_daemon::fetch_file(&self.client, &device, &params.path).await);
        Ok(render_bytes(&params.path, bytes, None))
    }

    #[tool(
        description = "Delete a file via the daemon (/api/v1/file). Path must be absolute.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn delete_file(
        &self,
        Parameters(params): Parameters<DeleteFileParams>,
    ) -> Result<CallToolResult, ErrorData> {
        try_tool!(validate_not_empty(&params.path, "path"));
        let device = try_tool!(self.resolve(&params.device_name).await);
        try_tool!(api_daemon::delete_file(&self.client, &device, &params.path).await);
        Ok(text_result("File deleted."))
    }

    // MARK: Capture

    #[tool(
        description = "Get the current capture status, including the last/active capture event and readiness flags.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn get_capture_status(
        &self,
        Parameters(params): Parameters<DeviceNameParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let device = try_tool!(self.resolve(&params.device_name).await);
        let status = try_tool!(api_capture::get_status(&self.client, &device).await);
        Ok(try_tool!(json_result(&status)))
    }

    #[tool(
        description = "Start a new capture session. Supported formats: JPG (image), RAW (image), MP4 (video). For MP4, specify video_length_seconds.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn start_capture(
        &self,
        Parameters(params): Parameters<StartCaptureParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let device = try_tool!(self.resolve(&params.device_name).await);
        let event = try_tool!(
            api_capture::start(
                &self.client,
                &device,
                params.output.as_deref(),
                params.format.map(|f| f.as_str()),
                params.video_length_seconds
            )
            .await
        );
        Ok(try_tool!(json_result(&event)))
    }

    #[tool(
        description = "Stop the current capture session (video only).",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn stop_capture(
        &self,
        Parameters(params): Parameters<DeviceNameParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let device = try_tool!(self.resolve(&params.device_name).await);
        try_tool!(api_capture::stop(&self.client, &device).await);
        Ok(text_result("Capture stopped."))
    }

    #[tool(
        description = "Capture a JPG image, wait for completion, and return the image bytes inline.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn capture_image(
        &self,
        Parameters(params): Parameters<CaptureImageParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let device = try_tool!(self.resolve(&params.device_name).await);
        let (event, bytes) = try_tool!(
            api_capture::capture_image(
                &self.client,
                &device,
                params.output.as_deref(),
                params.timeout
            )
            .await
        );
        let meta_json = try_tool!(serde_json::to_string_pretty(&event));
        Ok(CallToolResult::success(vec![
            Content::text(meta_json),
            Content::image(b64(&bytes), "image/jpeg"),
        ]))
    }

    // MARK: GPIO

    #[tool(
        description = "List all GPIO pins with their info and current settings.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn list_gpios(
        &self,
        Parameters(params): Parameters<DeviceNameParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let device = try_tool!(self.resolve(&params.device_name).await);
        let pins = try_tool!(api_gpio::list(&self.client, &device).await);
        Ok(try_tool!(json_result(&pins)))
    }

    #[tool(
        description = "Get detailed information about a specific GPIO pin.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn get_gpio_info(
        &self,
        Parameters(params): Parameters<GpioInfoParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let device = try_tool!(self.resolve(&params.device_name).await);
        let pin = try_tool!(api_gpio::info(&self.client, &device, params.pin_id).await);
        Ok(try_tool!(json_result(&pin)))
    }

    #[tool(
        description = "Set the value of a GPIO pin (0 or 1). Reconfigures the pin as a push-pull output if it is not already.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn set_gpio_value(
        &self,
        Parameters(params): Parameters<SetGpioValueParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let device = try_tool!(self.resolve(&params.device_name).await);
        let value = try_tool!(
            api_gpio::set_value(&self.client, &device, params.pin_id, params.value).await
        );
        Ok(text_result(&format!(
            "GPIO pin {} set to {value}.",
            params.pin_id
        )))
    }

    #[tool(
        description = "Get the current value of a GPIO pin (0 or 1). Reconfigures the pin as a floating input if it is not already, so this is NOT purely read-only.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn get_gpio_value(
        &self,
        Parameters(params): Parameters<GetGpioValueParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let device = try_tool!(self.resolve(&params.device_name).await);
        let value = try_tool!(
            api_gpio::get_value(&self.client, &device, params.pin_id, params.debounce_ms).await
        );
        Ok(text_result(&value.to_string()))
    }
}

// MARK: Server handler + resolve

impl ReCameraServer {
    async fn resolve(&self, device_name: &str) -> anyhow::Result<DeviceRecord> {
        anyhow::ensure!(
            !device_name.trim().is_empty(),
            "'device_name' must not be empty. Use list_devices to see registered devices."
        );
        let store = self.store.read().await;
        store.resolve_device(device_name)
    }
}

#[tool_handler]
impl ServerHandler for ReCameraServer {
    fn get_info(&self) -> ServerInfo {
        let _ = &self.tool_router;

        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                "recamera-intellisense-mcp",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "reCamera Intellisense MCP Server. Provides tools to manage reCamera devices, \
                 configure AI detection + record triggers, query events, capture images/video, \
                 browse recorded clips (fetch_record), and control GPIO.\n\n\
                 Recommended workflow:\n\
                 1) Register a device with `add_device` (or confirm one exists via `list_devices`).\n\
                 2) Verify readiness with `get_storage_status` before writing; run `set_storage_slot` / \
                 `configure_storage_quota` if storage is not CONFIGURED or READY.\n\
                 3) Choose an AI model with `set_detection_model`, set a window with `set_detection_schedule`, \
                 and install rules via `set_detection_rules` (automatically picks INFERENCE_SET).\n\
                 4) Or configure a non-AI record trigger via `set_record_trigger` (`timer`, `gpio`, `tty`, \
                 `http`, `always_on`).\n\
                 5) Poll `get_detection_events` for recent matches and `fetch_file` / `fetch_record` to \
                 retrieve snapshots.\n\n\
                 For HTTPS devices, keep `allow_unsecured=false` on the trusted network and only opt in to \
                 `true` for self-signed certs on a trusted LAN."
                    .to_string(),
            )
    }
}
