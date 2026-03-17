use serde::{Deserialize, Serialize};

/// Rule event types
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RuleType {
    Inference,
    InferenceSet,
    Timer,
    Gpio,
    Tty,
    Schedule,
    Http,
    Unknown,
}

/// File operation types
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FileOp {
    Added,
    Removed,
}

/// Raw incoming event from WebSocket
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "event")]
pub enum IncomingEvent {
    #[serde(rename = "RULE")]
    Rule(RuleEvent),
    #[serde(rename = "FILE")]
    File(FileEvent),
}

/// Rule event as received from WebSocket
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleEvent {
    #[serde(rename = "type")]
    pub rule_type: RuleType,
    #[serde(default)]
    pub id: Option<String>,
    pub uid: u64,
    pub timestamp: u64,
}

/// File event as received from WebSocket
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEvent {
    pub op: FileOp,
    pub path: String,
    pub size: u64,
    #[serde(default)]
    pub attributes: Option<String>,
    pub event_uid: u64,
    pub timestamp: u64,
}

/// File event data embedded in merged result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEventData {
    pub op: FileOp,
    pub path: String,
    pub size: u64,
    #[serde(default)]
    pub attributes: Option<String>,
    pub event_uid: u64,
    pub timestamp: u64,
}

impl From<FileEvent> for FileEventData {
    fn from(fe: FileEvent) -> Self {
        Self {
            op: fe.op,
            path: fe.path,
            size: fe.size,
            attributes: fe.attributes,
            event_uid: fe.event_uid,
            timestamp: fe.timestamp,
        }
    }
}

/// Merged inference result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergedEvent {
    pub event: String,
    #[serde(rename = "type")]
    pub rule_type: RuleType,
    #[serde(default)]
    pub id: Option<String>,
    pub uid: u64,
    pub timestamp: u64,
    pub file_event: Option<FileEventData>,
}

impl MergedEvent {
    pub fn from_rule(rule: &RuleEvent) -> Self {
        Self {
            event: "RULE".to_string(),
            rule_type: rule.rule_type.clone(),
            id: rule.id.clone(),
            uid: rule.uid,
            timestamp: rule.timestamp,
            file_event: None,
        }
    }

    pub fn from_rule_and_file(rule: &RuleEvent, file: FileEvent) -> Self {
        Self {
            event: "RULE".to_string(),
            rule_type: rule.rule_type.clone(),
            id: rule.id.clone(),
            uid: rule.uid,
            timestamp: rule.timestamp,
            file_event: Some(file.into()),
        }
    }
}
