use std::sync::Arc;
use tokio::sync::RwLock;

use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::*,
    tool, tool_handler, tool_router, ServerHandler,
};

use crate::api_client::ApiClient;
use crate::capture;
use crate::detection;
use crate::device_store::DeviceStore;
use crate::gpio;
use crate::storage;
use crate::types::*;

/// Convert any fallible result into a tool-level error (`is_error: true`) visible
/// to the agent, instead of raising a protocol-level JSON-RPC error.
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

fn mime_from_ext(path: &str) -> &str {
    match path
        .rsplit('.')
        .next()
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("gif") => "image/gif",
        Some("bmp") => "image/bmp",
        Some("webp") => "image/webp",
        Some("mp4") => "video/mp4",
        Some("avi") => "video/x-msvideo",
        Some("mkv") => "video/x-matroska",
        Some("txt" | "log" | "csv") => "text/plain",
        Some("json") => "application/json",
        Some("xml") => "application/xml",
        _ => "application/octet-stream",
    }
}

#[derive(Clone)]
pub struct ReCameraServer {
    store: Arc<RwLock<DeviceStore>>,
    client: Arc<ApiClient>,
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl ReCameraServer {
    pub fn new(store: DeviceStore, client: ApiClient) -> Self {
        Self {
            store: Arc::new(RwLock::new(store)),
            client: Arc::new(client),
            tool_router: Self::tool_router(),
        }
    }

    // ─── Device Management Tools ─────────────────────────────────────────

    #[tool(
        description = "Detect a reCamera Intellisense daemon running locally by checking its Unix socket. Returns the socket path if found."
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
        if found {
            Ok(text_result(socket_path))
        } else {
            Ok(text_result("No daemon detected"))
        }
    }

    #[tool(
        description = "Register (add) a new reCamera device with connection credentials. A connectivity test is performed before saving."
    )]
    async fn add_device(
        &self,
        Parameters(params): Parameters<AddDeviceParams>,
    ) -> Result<CallToolResult, ErrorData> {
        try_tool!(validate_not_empty(&params.name, "name"));
        try_tool!(validate_not_empty(&params.host, "host"));
        try_tool!(validate_not_empty(&params.token, "token"));
        let protocol = params.protocol.as_deref().unwrap_or("http");
        let allow_unsecured = params.allow_unsecured.unwrap_or(true);
        try_tool!(
            self.client
                .test_connection(
                    &params.host,
                    &params.token,
                    protocol,
                    allow_unsecured,
                    params.port,
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
                    protocol,
                    allow_unsecured,
                    params.port,
                )
                .await
        );
        Ok(text_result(&format!(
            "Device '{}' added successfully.",
            params.name
        )))
    }

    #[tool(
        description = "Update an existing reCamera device's connection credentials. At least one field must be provided. A connectivity test is performed before saving."
    )]
    async fn update_device(
        &self,
        Parameters(params): Parameters<UpdateDeviceParams>,
    ) -> Result<CallToolResult, ErrorData> {
        try_tool!(validate_not_empty(&params.name, "name"));
        // Merge params with existing device under lock, then release for network I/O
        let (host, token, protocol, allow_unsecured, port) = {
            let store = self.store.read().await;
            let existing = match store.get_device(&params.name) {
                Some(d) => d,
                None => {
                    return Ok(CallToolResult::error(vec![Content::text(format!(
                        "Device '{}' not found. Use list_devices to see registered devices.",
                        params.name
                    ))]))
                }
            };
            (
                params.host.clone().unwrap_or(existing.host),
                params.token.clone().unwrap_or(existing.token),
                params.protocol.clone().unwrap_or(existing.protocol),
                params.allow_unsecured.unwrap_or(existing.allow_unsecured),
                params.port.or(existing.port),
            )
        };
        // Test connection with the exact merged config (lock released)
        try_tool!(
            self.client
                .test_connection(&host, &token, &protocol, allow_unsecured, port)
                .await
        );
        // Re-acquire lock and save the exact tested configuration (not a partial update)
        let mut store = self.store.write().await;
        try_tool!(
            store
                .replace_device(
                    &params.name,
                    &host,
                    &token,
                    &protocol,
                    allow_unsecured,
                    port,
                )
                .await
        );
        Ok(text_result(&format!(
            "Device '{}' updated successfully.",
            params.name
        )))
    }

    #[tool(description = "Remove a registered reCamera device.")]
    async fn remove_device(
        &self,
        Parameters(params): Parameters<DeviceNameParams>,
    ) -> Result<CallToolResult, ErrorData> {
        try_tool!(validate_not_empty(&params.device_name, "device_name"));
        let mut store = self.store.write().await;
        let removed = store.remove_device(&params.device_name).await;
        if removed {
            Ok(text_result(&format!(
                "Device '{}' removed successfully.",
                params.device_name
            )))
        } else {
            Ok(CallToolResult::error(vec![Content::text(format!(
                "Device '{}' not found. Use list_devices to see registered devices.",
                params.device_name
            ))]))
        }
    }

    #[tool(description = "Get the connection credentials of a registered reCamera device.")]
    async fn get_device(
        &self,
        Parameters(params): Parameters<DeviceNameParams>,
    ) -> Result<CallToolResult, ErrorData> {
        try_tool!(validate_not_empty(&params.device_name, "device_name"));
        let store = self.store.read().await;
        match store.get_device(&params.device_name) {
            Some(device) => Ok(try_tool!(json_result(&device))),
            None => Ok(CallToolResult::error(vec![Content::text(format!(
                "Device '{}' not found. Use list_devices to see registered devices.",
                params.device_name
            ))])),
        }
    }

    #[tool(description = "List all registered reCamera devices sorted by name.")]
    async fn list_devices(&self) -> Result<CallToolResult, ErrorData> {
        let store = self.store.read().await;
        let devices = store.list_devices();
        Ok(try_tool!(json_result(&devices)))
    }

    // ─── Detection Tools ─────────────────────────────────────────────────

    #[tool(
        description = "Get information about available detection models on a reCamera device, including model IDs, names, and label mappings."
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
        description = "Get the currently active detection model on a reCamera device. Returns null if detection is disabled."
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
        description = "Set the active detection model on a reCamera device by model_id or model_name (provide exactly one)."
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
                params.model_name.as_deref(),
            )
            .await
        );
        Ok(text_result("Detection model set successfully."))
    }

    #[tool(
        description = "Get the current detection schedule (active weekdays and time ranges) for a reCamera device. Returns null if schedule is disabled."
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
        description = "Set the detection schedule for a reCamera device. Use null/empty schedule to disable (detection active all the time). Time format: 'Day HH:MM:SS' (e.g. 'Mon 08:00:00')."
    )]
    async fn set_detection_schedule(
        &self,
        Parameters(params): Parameters<SetDetectionScheduleParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let device = try_tool!(self.resolve(&params.device_name).await);
        let schedule = params.schedule.as_deref();
        try_tool!(detection::set_detection_schedule(&self.client, &device, schedule).await);
        Ok(text_result("Detection schedule set successfully."))
    }

    #[tool(
        description = "Get the current activated detection rules for a reCamera device. Returns empty list if prerequisites (record image, storage) are not enabled."
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
        description = "Set detection rules for a reCamera device. Auto-enables record image and default storage when needed."
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
        description = "Get detection events from a reCamera device within an optional time range. Specifying start_unix_ms is recommended to limit results."
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
                params.end_unix_ms,
            )
            .await
        );
        Ok(try_tool!(json_result(&events)))
    }

    #[tool(description = "Clear all cached detection events on a reCamera device.")]
    async fn clear_detection_events(
        &self,
        Parameters(params): Parameters<DeviceNameParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let device = try_tool!(self.resolve(&params.device_name).await);
        try_tool!(detection::clear_detection_events(&self.client, &device).await);
        Ok(text_result("Detection events cleared."))
    }

    #[tool(
        description = "Fetch a file from a reCamera device by its remote path. Returns images as inline image content; returns text files as text; skips downloading video or files larger than 5 MB."
    )]
    async fn fetch_file(
        &self,
        Parameters(params): Parameters<FetchFileParams>,
    ) -> Result<CallToolResult, ErrorData> {
        try_tool!(validate_not_empty(&params.path, "path"));
        let device = try_tool!(self.resolve(&params.device_name).await);
        let mime = mime_from_ext(&params.path);
        // Video files: too large for inline transfer
        if mime.starts_with("video/") {
            return Ok(text_result(&format!(
                "File '{}' is a video ({}). Video files are too large for inline transfer. \
                 Use the file path directly on the device.",
                params.path, mime
            )));
        }
        let bytes = try_tool!(storage::fetch_file(&self.client, &device, &params.path).await);
        // Large files: skip inline transfer
        if bytes.len() > 5 * 1024 * 1024 {
            return Ok(text_result(&format!(
                "File '{}' is {:.1} MB which is too large for inline transfer. \
                 Use the file path directly on the device.",
                params.path,
                bytes.len() as f64 / (1024.0 * 1024.0)
            )));
        }
        if mime.starts_with("image/") {
            let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes);
            Ok(CallToolResult::success(vec![Content::image(b64, mime)]))
        } else if mime.starts_with("text/")
            || mime == "application/json"
            || mime == "application/xml"
        {
            match String::from_utf8(bytes) {
                Ok(text) => Ok(CallToolResult::success(vec![Content::text(text)])),
                Err(e) => {
                    let raw = e.into_bytes();
                    let b64 =
                        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &raw);
                    Ok(CallToolResult::success(vec![Content::text(format!(
                        "File '{}' (not valid UTF-8, {} bytes, base64-encoded):\n{}",
                        params.path,
                        raw.len(),
                        b64
                    ))]))
                }
            }
        } else {
            let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes);
            Ok(CallToolResult::success(vec![Content::text(format!(
                "File '{}' ({}, {} bytes, base64-encoded):\n{}",
                params.path,
                mime,
                bytes.len(),
                b64
            ))]))
        }
    }

    #[tool(description = "Delete a file from a reCamera device by its remote path.")]
    async fn delete_file(
        &self,
        Parameters(params): Parameters<DeleteFileParams>,
    ) -> Result<CallToolResult, ErrorData> {
        try_tool!(validate_not_empty(&params.path, "path"));
        let device = try_tool!(self.resolve(&params.device_name).await);
        try_tool!(storage::delete_file(&self.client, &device, &params.path).await);
        Ok(text_result("File deleted."))
    }

    // ─── Capture Tools ───────────────────────────────────────────────────

    #[tool(
        description = "Get the current capture status from a reCamera device, including the last/active capture event and readiness flags."
    )]
    async fn get_capture_status(
        &self,
        Parameters(params): Parameters<DeviceNameParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let device = try_tool!(self.resolve(&params.device_name).await);
        let status = try_tool!(capture::get_capture_status(&self.client, &device).await);
        Ok(try_tool!(json_result(&status)))
    }

    #[tool(
        description = "Start a new capture session on a reCamera device. Supported formats: JPG (image), RAW (image), MP4 (video). For MP4, specify video_length_seconds."
    )]
    async fn start_capture(
        &self,
        Parameters(params): Parameters<StartCaptureParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let device = try_tool!(self.resolve(&params.device_name).await);
        if let Some(ref fmt) = params.format {
            let upper = fmt.to_uppercase();
            if !["JPG", "RAW", "MP4"].contains(&upper.as_str()) {
                return Ok(CallToolResult::error(vec![Content::text(format!(
                    "Unsupported capture format '{fmt}'. Supported formats: JPG, RAW, MP4."
                ))]));
            }
        }
        let event = try_tool!(
            capture::start_capture(
                &self.client,
                &device,
                params.output.as_deref(),
                params.format.as_deref(),
                params.video_length_seconds,
            )
            .await
        );
        Ok(try_tool!(json_result(&event)))
    }

    #[tool(description = "Stop the current capture session (video only) on a reCamera device.")]
    async fn stop_capture(
        &self,
        Parameters(params): Parameters<DeviceNameParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let device = try_tool!(self.resolve(&params.device_name).await);
        try_tool!(capture::stop_capture(&self.client, &device).await);
        Ok(text_result("Capture stopped."))
    }

    #[tool(
        description = "Capture a JPG image from a reCamera device, wait for completion, and return the image. Returns capture metadata and the image as base64/JPEG content."
    )]
    async fn capture_image(
        &self,
        Parameters(params): Parameters<CaptureImageParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let device = try_tool!(self.resolve(&params.device_name).await);
        let (event, bytes) = try_tool!(
            capture::capture_image(
                &self.client,
                &device,
                params.output.as_deref(),
                params.timeout,
            )
            .await
        );
        let meta_json = try_tool!(serde_json::to_string_pretty(&event));
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes);
        Ok(CallToolResult::success(vec![
            Content::text(meta_json),
            Content::image(b64, "image/jpeg"),
        ]))
    }

    // ─── GPIO Tools ──────────────────────────────────────────────────────

    #[tool(
        description = "List all GPIO pins on a reCamera device with their info and current settings."
    )]
    async fn list_gpios(
        &self,
        Parameters(params): Parameters<DeviceNameParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let device = try_tool!(self.resolve(&params.device_name).await);
        let pins = try_tool!(gpio::list_gpios(&self.client, &device).await);
        Ok(try_tool!(json_result(&pins)))
    }

    #[tool(
        description = "Get detailed information about a specific GPIO pin on a reCamera device."
    )]
    async fn get_gpio_info(
        &self,
        Parameters(params): Parameters<GpioInfoParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let device = try_tool!(self.resolve(&params.device_name).await);
        let pin = try_tool!(gpio::get_gpio_info(&self.client, &device, params.pin_id).await);
        Ok(try_tool!(json_result(&pin)))
    }

    #[tool(
        description = "Set the value of a GPIO pin (0 or 1) on a reCamera device. Auto-configures the pin as output if not already."
    )]
    async fn set_gpio_value(
        &self,
        Parameters(params): Parameters<SetGpioValueParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let device = try_tool!(self.resolve(&params.device_name).await);
        let value = try_tool!(
            gpio::set_gpio_value(&self.client, &device, params.pin_id, params.value).await
        );
        Ok(text_result(&format!(
            "GPIO pin {} set to {value}.",
            params.pin_id
        )))
    }

    #[tool(
        description = "Get the current value of a GPIO pin (0 or 1) on a reCamera device. Auto-configures the pin as input if not already."
    )]
    async fn get_gpio_value(
        &self,
        Parameters(params): Parameters<GetGpioValueParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let device = try_tool!(self.resolve(&params.device_name).await);
        let value = try_tool!(
            gpio::get_gpio_value(&self.client, &device, params.pin_id, params.debounce_ms).await
        );
        Ok(text_result(&value.to_string()))
    }
}

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
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                "recamera-intellisense-mcp",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "reCamera Intellisense MCP Server. Provides tools to manage reCamera devices, \
                 configure AI detection models/schedules/rules, query detection events, \
                 capture images/video, and control GPIO pins. \
                 Register devices with add_device before using other tools."
                    .to_string(),
            )
    }
}
