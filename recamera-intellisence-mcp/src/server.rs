use std::path::PathBuf;
use std::sync::Arc;

use rmcp::{
    ErrorData as McpError, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::*,
    schemars::{self, JsonSchema},
    tool, tool_handler, tool_router,
};
use serde::Deserialize;
use serde_json::Value;

use crate::devices::client::DeviceClient;
use crate::event_store::SharedEventStore;
use crate::tools::{capture, detection, events, gpio};

/// Shared server state accessible from all tool handlers.
#[derive(Clone)]
pub struct ReCameraServer {
    pub device_client: Arc<DeviceClient>,
    pub event_store: SharedEventStore,
    pub allowed_file_prefix: PathBuf,
    tool_router: ToolRouter<Self>,
}

// ── Parameter structs ──────────────────────────────────────────────

#[derive(Debug, Deserialize, JsonSchema)]
pub struct StartCaptureParams {
    /// Capture format: JPG, RAW, or MP4
    pub format: Option<String>,
    /// Output directory path on the device
    pub output: Option<String>,
    /// Video length in seconds (for MP4 format only)
    pub video_length_seconds: Option<i64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CaptureImageParams {
    /// Output directory path on the device
    pub output: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetDetectionModelParams {
    /// Model ID to activate
    pub model_id: Option<i64>,
    /// Model name to activate
    pub model_name: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetDetectionScheduleParams {
    /// Schedule object with 'active_weekdays' array of [start, end] time pairs, or null to disable
    pub schedule: Option<Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetDetectionRulesParams {
    /// Array of detection rule objects
    pub rules: Vec<Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PinIdParams {
    /// GPIO pin ID
    pub pin_id: i64,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetGpioValueParams {
    /// GPIO pin ID
    pub pin_id: i64,
    /// Pin value: 0 or 1
    pub value: i64,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetGpioValueParams {
    /// GPIO pin ID
    pub pin_id: i64,
    /// Debounce time in milliseconds (default: 100)
    pub debounce_ms: Option<i64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TimeRangeParams {
    /// Start of time range (Unix timestamp in milliseconds)
    pub start_unix_ms: Option<u64>,
    /// End of time range (Unix timestamp in milliseconds)
    pub end_unix_ms: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SnapshotPathParams {
    /// Absolute path to the snapshot file on the device (from detection event's file_event.path)
    pub snapshot_path: String,
}

// ── Tool router ────────────────────────────────────────────────────

#[tool_router]
impl ReCameraServer {
    pub fn new(
        device_client: DeviceClient,
        event_store: SharedEventStore,
        allowed_file_prefix: PathBuf,
    ) -> Self {
        Self {
            device_client: Arc::new(device_client),
            event_store,
            allowed_file_prefix,
            tool_router: Self::tool_router(),
        }
    }

    // ── Capture tools ──────────────────────────────────────────

    #[tool(description = "Get the capture status from the camera, including the last/active capture event and readiness flags.")]
    async fn get_capture_status(&self) -> Result<CallToolResult, McpError> {
        capture::get_capture_status(&self.device_client).await
    }

    #[tool(description = "Start a new capture session on the camera. Supported formats: JPG (image), RAW (image), MP4 (video). For video captures, specifying video_length_seconds is recommended.")]
    async fn start_capture(
        &self,
        Parameters(params): Parameters<StartCaptureParams>,
    ) -> Result<CallToolResult, McpError> {
        capture::start_capture(
            &self.device_client,
            params.format,
            params.output,
            params.video_length_seconds,
        )
        .await
    }

    #[tool(description = "Stop the current capture session on the camera (video captures only).")]
    async fn stop_capture(&self) -> Result<CallToolResult, McpError> {
        capture::stop_capture(&self.device_client).await
    }

    #[tool(description = "Capture an image (JPG) from the camera, wait for completion, and return the image data. This is a high-level convenience function that starts capture, polls until completion, and downloads the captured image.")]
    async fn capture_image(
        &self,
        Parameters(params): Parameters<CaptureImageParams>,
    ) -> Result<CallToolResult, McpError> {
        capture::capture_image(&self.device_client, &self.allowed_file_prefix, params.output)
            .await
    }

    // ── Detection tools ────────────────────────────────────────

    #[tool(description = "Get information about all available detection models on the camera, including model IDs, names, and label mappings.")]
    async fn get_detection_models(&self) -> Result<CallToolResult, McpError> {
        detection::get_detection_models(&self.device_client).await
    }

    #[tool(description = "Get the currently active detection model on the camera. Returns null if detection is disabled.")]
    async fn get_detection_model(&self) -> Result<CallToolResult, McpError> {
        detection::get_detection_model(&self.device_client).await
    }

    #[tool(description = "Set the active detection model on the camera by model_id or model_name. Provide exactly one.")]
    async fn set_detection_model(
        &self,
        Parameters(params): Parameters<SetDetectionModelParams>,
    ) -> Result<CallToolResult, McpError> {
        detection::set_detection_model(&self.device_client, params.model_id, params.model_name)
            .await
    }

    #[tool(description = "Get the current detection schedule for the camera. Returns null if schedule is disabled.")]
    async fn get_detection_schedule(&self) -> Result<CallToolResult, McpError> {
        detection::get_detection_schedule(&self.device_client).await
    }

    #[tool(description = "Set the detection schedule for the camera. Pass null for schedule to disable (detection active all the time). Time format: 'Day HH:MM:SS' with case-sensitive short weekday (Sun, Mon, Tue, Wed, Thu, Fri, Sat).")]
    async fn set_detection_schedule(
        &self,
        Parameters(params): Parameters<SetDetectionScheduleParams>,
    ) -> Result<CallToolResult, McpError> {
        detection::set_detection_schedule(&self.device_client, params.schedule).await
    }

    #[tool(description = "Get the current activated detection rules for the camera. Returns empty list if record image or storage is not enabled.")]
    async fn get_detection_rules(&self) -> Result<CallToolResult, McpError> {
        detection::get_detection_rules(&self.device_client).await
    }

    #[tool(description = "Set detection rules for the camera. Automatically enables record image and storage if needed. Each rule has: name (string), debounce_times (int), confidence_range_filter ([min, max] floats 0-1), label_filter (array of label indices, empty=all), region_filter (array of polygons with normalized coords, null=no region).")]
    async fn set_detection_rules(
        &self,
        Parameters(params): Parameters<SetDetectionRulesParams>,
    ) -> Result<CallToolResult, McpError> {
        detection::set_detection_rules(&self.device_client, params.rules).await
    }

    // ── GPIO tools ─────────────────────────────────────────────

    #[tool(description = "List all available GPIO pins on the camera with their info and current settings (name, chip, line, capabilities, state, edge, debounce).")]
    async fn list_gpios(&self) -> Result<CallToolResult, McpError> {
        gpio::list_gpios(&self.device_client).await
    }

    #[tool(description = "Get detailed information about a specific GPIO pin including its info, capabilities, and current settings.")]
    async fn get_gpio_info(
        &self,
        Parameters(params): Parameters<PinIdParams>,
    ) -> Result<CallToolResult, McpError> {
        gpio::get_gpio_info(&self.device_client, params.pin_id).await
    }

    #[tool(description = "Set the value of a GPIO output pin (0 or 1). Automatically configures the pin as output if not already.")]
    async fn set_gpio_value(
        &self,
        Parameters(params): Parameters<SetGpioValueParams>,
    ) -> Result<CallToolResult, McpError> {
        gpio::set_gpio_value(&self.device_client, params.pin_id, params.value).await
    }

    #[tool(description = "Read the current value of a GPIO input pin. Automatically configures the pin as input if not already.")]
    async fn get_gpio_value(
        &self,
        Parameters(params): Parameters<GetGpioValueParams>,
    ) -> Result<CallToolResult, McpError> {
        gpio::get_gpio_value(&self.device_client, params.pin_id, params.debounce_ms).await
    }

    // ── Event monitoring tools ─────────────────────────────────

    #[tool(description = "Get detection events from the camera's event monitor within an optional time range. Events include rule triggers with optional associated file events (snapshots). Providing start_unix_ms is recommended to limit results.")]
    async fn get_detection_events(
        &self,
        Parameters(params): Parameters<TimeRangeParams>,
    ) -> Result<CallToolResult, McpError> {
        events::get_detection_events(&self.event_store, params.start_unix_ms, params.end_unix_ms)
            .await
    }

    #[tool(description = "Get the count of detection events in an optional time range without returning the full event data.")]
    async fn get_detection_events_count(
        &self,
        Parameters(params): Parameters<TimeRangeParams>,
    ) -> Result<CallToolResult, McpError> {
        events::get_detection_events_count(
            &self.event_store,
            params.start_unix_ms,
            params.end_unix_ms,
        )
        .await
    }

    #[tool(description = "Clear all cached detection events from the event monitor.")]
    async fn clear_detection_events(&self) -> Result<CallToolResult, McpError> {
        events::clear_detection_events(&self.event_store).await
    }

    #[tool(description = "Fetch the snapshot image associated with a detection event. The snapshot_path comes from the file_event.path field of a detection event.")]
    async fn fetch_detection_event_image(
        &self,
        Parameters(params): Parameters<SnapshotPathParams>,
    ) -> Result<CallToolResult, McpError> {
        events::fetch_detection_event_image(
            &self.device_client,
            &self.allowed_file_prefix,
            &params.snapshot_path,
        )
        .await
    }
}

// ── ServerHandler implementation ───────────────────────────────────

#[tool_handler]
impl ServerHandler for ReCameraServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                "recamera-intellisense-mcp",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "reCamera Intellisense MCP Server — middleware between reCamera HTTP API and MCP clients. \
                 Provides tools for camera capture, detection model management, GPIO control, and event monitoring."
                    .to_string(),
            )
    }
}
