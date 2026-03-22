use rmcp::schemars;
use serde::{Deserialize, Serialize};

// ─── Device Types ────────────────────────────────────────────────────────────

#[derive(Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DeviceRecord {
    pub name: String,
    pub host: String,
    #[serde(skip_serializing)]
    pub token: String,
    #[serde(default = "default_protocol")]
    pub protocol: String,
    #[serde(default = "default_allow_unsecured")]
    pub allow_unsecured: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
}

impl std::fmt::Debug for DeviceRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceRecord")
            .field("name", &self.name)
            .field("host", &self.host)
            .field("token", &"[REDACTED]")
            .field("protocol", &self.protocol)
            .field("allow_unsecured", &self.allow_unsecured)
            .field("port", &self.port)
            .finish()
    }
}

fn default_protocol() -> String {
    "http".to_string()
}
fn default_allow_unsecured() -> bool {
    true
}

/// Internal representation that includes token for storage
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceEntry {
    pub host: String,
    pub token: String,
    #[serde(default = "default_protocol")]
    pub protocol: String,
    #[serde(default = "default_allow_unsecured")]
    pub allow_unsecured: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
}

// ─── Detection Types ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DetectionModel {
    pub id: i32,
    pub name: String,
    pub labels: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ScheduleRange {
    /// Start time in "Day HH:MM:SS" format (e.g. "Mon 08:00:00")
    pub start: String,
    /// End time in "Day HH:MM:SS" format (e.g. "Mon 18:00:00")
    pub end: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DetectionRule {
    /// Rule name / identifier
    pub name: String,
    /// Number of consecutive frames that must meet the rule to trigger an event
    #[serde(default = "default_debounce_times")]
    pub debounce_times: i32,
    /// [min_confidence, max_confidence] values between 0 and 1
    #[serde(default = "default_confidence_range")]
    pub confidence_range_filter: Vec<f64>,
    /// Label names to include (empty means all labels)
    #[serde(default)]
    pub label_filter: Vec<String>,
    /// List of polygons, each polygon is a list of [x, y] normalized coordinates.
    /// Use [[[0,0],[1,0],[1,1],[0,1]]] for full region. null means no region filter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region_filter: Option<Vec<Vec<Vec<f64>>>>,
}

fn default_debounce_times() -> i32 {
    3
}
fn default_confidence_range() -> Vec<f64> {
    vec![0.25, 1.0]
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DetectionEvent {
    pub timestamp: String,
    pub timestamp_unix_ms: u64,
    pub rule_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot_path: Option<String>,
}

// ─── Storage Types ────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct StorageInfo {
    pub dev_path: String,
    pub uuid: String,
    pub is_configured: bool,
    pub is_enabled: bool,
    pub is_internal: bool,
    pub quota_rotate: bool,
    pub quota_limit_bytes: Option<i64>,
    pub mount_path: String,
    pub data_dir: String,
}

// ─── Capture Types ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CaptureEvent {
    pub id: String,
    pub output_directory: String,
    pub format: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video_length_seconds: Option<i32>,
    pub status: String,
    pub timestamp_unix_ms: u64,
    pub file_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CaptureStatus {
    pub last_capture: Option<CaptureEvent>,
    pub ready_to_start_new: bool,
    pub stop_requested: bool,
}

// ─── GPIO Types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PinInfo {
    pub name: String,
    pub chip: String,
    pub line: i32,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PinSettings {
    pub state: String,
    pub edge: String,
    pub debounce_ms: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PinDescriptor {
    pub pin_id: i32,
    pub info: PinInfo,
    pub settings: PinSettings,
}

// ─── MCP Tool Parameter Types ────────────────────────────────────────────────

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DetectLocalDeviceParams {
    /// Host address to check (default: 127.0.0.1)
    pub host: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AddDeviceParams {
    /// Unique name for the device
    pub name: String,
    /// Device host address (IP or hostname)
    pub host: String,
    /// Authentication token (format: sk_...)
    pub token: String,
    /// Protocol: "http" or "https" (default: "http")
    pub protocol: Option<String>,
    /// Allow self-signed certificates for HTTPS (default: true)
    pub allow_unsecured: Option<bool>,
    /// Custom port number
    pub port: Option<u16>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct UpdateDeviceParams {
    /// Name of the device to update
    pub name: String,
    /// New host address
    pub host: Option<String>,
    /// New authentication token
    pub token: Option<String>,
    /// New protocol
    pub protocol: Option<String>,
    /// New allow_unsecured setting
    pub allow_unsecured: Option<bool>,
    /// New port number
    pub port: Option<u16>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DeviceNameParams {
    /// Name of the registered device
    pub device_name: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SetDetectionModelParams {
    /// Name of the registered device
    pub device_name: String,
    /// Model ID to set (provide exactly one of model_id or model_name)
    pub model_id: Option<i32>,
    /// Model name to set (provide exactly one of model_id or model_name)
    pub model_name: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SetDetectionScheduleParams {
    /// Name of the registered device
    pub device_name: String,
    /// Schedule ranges, or null/empty to disable schedule
    pub schedule: Option<Vec<ScheduleRange>>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SetDetectionRulesParams {
    /// Name of the registered device
    pub device_name: String,
    /// Detection rules to set
    pub rules: Vec<DetectionRule>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetDetectionEventsParams {
    /// Name of the registered device
    pub device_name: String,
    /// Start time filter (Unix timestamp in milliseconds)
    pub start_unix_ms: Option<i64>,
    /// End time filter (Unix timestamp in milliseconds)
    pub end_unix_ms: Option<i64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FetchFileParams {
    /// Name of the registered device
    pub device_name: String,
    /// Remote file path on the device
    pub path: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DeleteFileParams {
    /// Name of the registered device
    pub device_name: String,
    /// Remote file path on the device to delete
    pub path: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct StartCaptureParams {
    /// Name of the registered device
    pub device_name: String,
    /// Output directory on device (default: /mnt/rc_mmcblk0p8/reCamera)
    pub output: Option<String>,
    /// Capture format: "JPG", "RAW", or "MP4" (default: "JPG")
    pub format: Option<String>,
    /// Video length in seconds (only for MP4 captures)
    pub video_length_seconds: Option<i32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CaptureImageParams {
    /// Name of the registered device
    pub device_name: String,
    /// Output directory on device (default: /mnt/rc_mmcblk0p8/reCamera)
    pub output: Option<String>,
    /// Timeout in seconds for waiting capture to complete (default: 5)
    pub timeout: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GpioInfoParams {
    /// Name of the registered device
    pub device_name: String,
    /// GPIO pin ID
    pub pin_id: i32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SetGpioValueParams {
    /// Name of the registered device
    pub device_name: String,
    /// GPIO pin ID
    pub pin_id: i32,
    /// Value to set (0 or 1)
    pub value: i32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetGpioValueParams {
    /// Name of the registered device
    pub device_name: String,
    /// GPIO pin ID
    pub pin_id: i32,
    /// Debounce time in milliseconds (default: 100)
    pub debounce_ms: Option<i32>,
}
