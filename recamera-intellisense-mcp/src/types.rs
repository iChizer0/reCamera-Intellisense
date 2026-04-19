use std::collections::BTreeMap;

use rmcp::schemars;
use serde::{Deserialize, Serialize};
use serde_json::Value;

// MARK: Device

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
    // Secure-by-default: require a trusted certificate chain when using HTTPS.
    // Callers that need to skip verification (e.g. a self-signed device cert on
    // a trusted LAN) must opt in explicitly via `allow_unsecured=true`.
    false
}

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

// MARK: Detection

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DetectionModel {
    pub id: i32,
    pub name: String,
    pub labels: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ScheduleRange {
    /// Start in "Day HH:MM:SS" format (e.g. "Mon 08:00:00").
    pub start: String,
    /// End in "Day HH:MM:SS" format (e.g. "Mon 18:00:00"); "Day 24:00:00" allowed.
    pub end: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DetectionRule {
    /// Rule name / identifier.
    pub name: String,
    /// Consecutive frames that must match to fire an event.
    #[serde(default = "default_debounce_times")]
    pub debounce_times: i32,
    /// [min, max] in [0.0, 1.0].
    #[serde(default = "default_confidence_range")]
    pub confidence_range_filter: Vec<f64>,
    /// Label names to match (empty = any).
    #[serde(default)]
    pub label_filter: Vec<String>,
    /// List of polygons of normalized [x, y] in [0,1]; omit or empty for full frame.
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

// MARK: Rule system

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WriterConfig {
    /// One of: "MP4", "JPG", "RAW".
    pub format: String,
    /// Delay between writes in ms; 0 = continuous.
    pub interval_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RuleConfig {
    /// Global evaluation of all rules.
    pub rule_enabled: bool,
    pub writer: WriterConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RuleEvent {
    /// "PENDING" | "WRITING" | "COMPLETED" | "FAILED" | "INTERRUPTED" | "CANCELED" | "UNKNOWN".
    pub status: String,
    pub timestamp_unix_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RuleEventOwner {
    pub rule_type: String,
    pub rule_id: String,
    pub timestamp_unix_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AvailableGpio {
    pub num: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    pub capabilities: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AvailableTty {
    pub socket_path: String,
    pub buffer_size: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RuleInfo {
    pub ready_for_new_event: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_event: Option<RuleEvent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_event_owner: Option<RuleEventOwner>,
    pub available_gpios: BTreeMap<String, AvailableGpio>,
    pub available_ttys: BTreeMap<String, AvailableTty>,
    pub media_paused: bool,
    pub video_clip_length_seconds: i64,
}

// MARK: Record trigger (unified)

/// Current record-rule trigger; maps to `/record/rule/record-rule-config.sCurrentSelected`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RecordTrigger {
    /// AI inference set of detection rules; fires when any rule matches.
    InferenceSet { rules: Vec<DetectionRule> },
    /// Periodic timer trigger.
    Timer {
        /// Trigger interval in seconds, >= 0.
        interval_seconds: u64,
    },
    /// GPIO signal trigger.
    Gpio(GpioTrigger),
    /// TTY command trigger.
    Tty(TtyTrigger),
    /// External HTTP trigger (fire via `activate_http_trigger`).
    Http,
    /// Continuous re-arm using writer interval pacing.
    AlwaysOn,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GpioTrigger {
    /// GPIO pin name, e.g. "GPIO_01" (provide `name` or `num`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// GPIO pin number (provide `name` or `num`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub num: Option<i32>,
    /// "DISABLED" | "FLOATING" | "PULL_UP" | "PULL_DOWN".
    pub state: String,
    /// "HIGH" | "LOW" | "RISING" | "FALLING".
    pub signal: String,
    /// Debounce in ms, >= 0.
    pub debounce_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TtyTrigger {
    pub name: String,
    /// Non-empty command string.
    pub command: String,
}

// MARK: Storage

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct StorageSlot {
    pub dev_path: String,
    pub mount_path: String,
    pub removable: bool,
    pub internal: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fs_type: Option<String>,
    pub selected: bool,
    pub enabled: bool,
    pub syncing: bool,
    pub writing: bool,
    pub rotating: bool,
    /// Numeric slot state enum (1..=8).
    pub state_code: i64,
    /// ERROR | NOT_FORMATTED_OR_FORMAT_UNSUPPORTED | FORMATTING | NOT_MOUNTED | MOUNTED | CONFIGURED | INDEXING | READY.
    pub state: String,
    pub size_bytes: i64,
    pub free_bytes: i64,
    pub quota_min_recommend_bytes: i64,
    pub quota_preserved_bytes: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quota_used_bytes: Option<i64>,
    pub quota_limit_bytes: i64,
    pub quota_rotate: bool,
    /// Record data directory name shared across slots.
    pub data_dir: String,
}

impl StorageSlot {
    /// Slot is CONFIGURED or higher.
    pub fn is_configured(&self) -> bool {
        self.state_code >= 6
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RelayStatus {
    /// Relay directory UUID; segment of the relay URL.
    pub uuid: String,
    /// Configured inactivity timeout in seconds.
    pub timeout: i64,
    /// Remaining inactivity timeout in seconds.
    pub timeout_remain: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DirEntry {
    /// Entry name (not a path).
    pub name: String,
    pub is_dir: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mtime: Option<String>,
}

// MARK: Capture

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

// MARK: GPIO

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

// MARK: MCP params - device

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DetectLocalDeviceParams {
    /// Unix socket path to probe (default: /dev/shm/rcisd.sock).
    pub socket_path: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AddDeviceParams {
    /// Unique device name.
    pub name: String,
    /// Device host (IP or hostname).
    pub host: String,
    /// Auth token (format: sk_...).
    pub token: String,
    /// "http" or "https" (default: "http").
    pub protocol: Option<String>,
    /// Accept self-signed certs for HTTPS (default: true).
    pub allow_unsecured: Option<bool>,
    /// Custom port.
    pub port: Option<u16>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct UpdateDeviceParams {
    pub device_name: String,
    pub host: Option<String>,
    pub token: Option<String>,
    pub protocol: Option<String>,
    pub allow_unsecured: Option<bool>,
    pub port: Option<u16>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DeviceNameParams {
    /// Name of the registered device.
    pub device_name: String,
}

// MARK: MCP params - detection

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SetDetectionModelParams {
    pub device_name: String,
    /// Provide exactly one of `model_id` or `model_name`.
    pub model_id: Option<i32>,
    pub model_name: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SetDetectionScheduleParams {
    pub device_name: String,
    /// Schedule ranges, or null/empty to disable (active at all times).
    pub schedule: Option<Vec<ScheduleRange>>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SetDetectionRulesParams {
    pub device_name: String,
    pub rules: Vec<DetectionRule>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetDetectionEventsParams {
    pub device_name: String,
    /// Inclusive lower bound (Unix ms).
    pub start_unix_ms: Option<i64>,
    /// Inclusive upper bound (Unix ms).
    pub end_unix_ms: Option<i64>,
}

// MARK: MCP params - rule system

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SetRuleConfigParams {
    pub device_name: String,
    pub rule_enabled: bool,
    /// "MP4" | "JPG" | "RAW".
    pub writer_format: String,
    /// Write pacing in ms; 0 = continuous.
    pub writer_interval_ms: u64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SetRecordTriggerParams {
    pub device_name: String,
    pub trigger: RecordTrigger,
}

// MARK: MCP params - storage

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SetStorageSlotParams {
    pub device_name: String,
    /// Slot selector; leave both null/empty to disable all slots.
    #[serde(default, deserialize_with = "crate::util::deserialize_nullable_string")]
    pub by_dev_path: String,
    #[serde(default, deserialize_with = "crate::util::deserialize_nullable_string")]
    pub by_uuid: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ConfigureStorageQuotaParams {
    pub device_name: String,
    /// Target slot device path.
    pub dev_path: String,
    /// Quota limit in bytes; -1 for no limit.
    pub quota_limit_bytes: i64,
    pub quota_rotate: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct StorageTaskSubmitParams {
    pub device_name: String,
    /// "FORMAT" | "FREE_UP" | "EJECT" | "REMOVE_FILES_OR_DIRECTORIES".
    pub action: String,
    pub dev_path: String,
    /// Required for REMOVE_FILES_OR_DIRECTORIES; paths relative to the data directory.
    #[serde(default)]
    pub files: Vec<String>,
    /// Run synchronously (default: false = ASYNC_SUBMIT).
    #[serde(default)]
    pub sync: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct StorageTaskQueryParams {
    pub device_name: String,
    /// Action family.
    pub action: String,
    pub dev_path: String,
}

// MARK: MCP params - records

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ListRecordsParams {
    pub device_name: String,
    pub dev_path: Option<String>,
    /// Relative path under the data directory (empty = root).
    #[serde(default)]
    pub path: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FetchRecordParams {
    pub device_name: String,
    pub dev_path: Option<String>,
    /// Relative path under the data directory.
    pub path: String,
}

// MARK: MCP params - files / capture / gpio

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FetchFileParams {
    pub device_name: String,
    /// Absolute remote path under the daemon-allowed prefix.
    pub path: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DeleteFileParams {
    pub device_name: String,
    /// Absolute remote path under the daemon-allowed prefix.
    pub path: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct StartCaptureParams {
    pub device_name: String,
    /// Absolute output path under storage mount point.
    pub output: Option<String>,
    /// "JPG" | "RAW" | "MP4" (default: "JPG").
    pub format: Option<String>,
    /// Clip length in seconds for MP4.
    pub video_length_seconds: Option<i32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CaptureImageParams {
    pub device_name: String,
    pub output: Option<String>,
    /// Timeout for completion in seconds (default: 5).
    pub timeout: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GpioInfoParams {
    pub device_name: String,
    pub pin_id: i32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SetGpioValueParams {
    pub device_name: String,
    pub pin_id: i32,
    /// 0 or 1.
    pub value: i32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetGpioValueParams {
    pub device_name: String,
    pub pin_id: i32,
    /// Debounce in ms (default: 100).
    pub debounce_ms: Option<i32>,
}

// MARK: Storage action helpers

pub fn normalize_storage_action(action: &str) -> Option<&'static str> {
    match action.to_ascii_uppercase().as_str() {
        "FORMAT" => Some("FORMAT"),
        "FREE_UP" => Some("FREE_UP"),
        "EJECT" => Some("EJECT"),
        "REMOVE_FILES_OR_DIRECTORIES" | "REMOVE" | "REMOVE_FILES" => {
            Some("REMOVE_FILES_OR_DIRECTORIES")
        }
        _ => None,
    }
}

/// Storage async task history is passed through transparently.
pub type StorageTaskHistory = Value;
