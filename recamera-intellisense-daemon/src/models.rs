use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::hash::{Hash, Hasher};

/// Preserve the distinction between an absent legacy field and an explicit null.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum OptionalField<T> {
    #[default]
    Missing,
    Null,
    Value(T),
}

impl<T> OptionalField<T> {
    pub fn is_missing(&self) -> bool {
        matches!(self, Self::Missing)
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for OptionalField<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match Option::<T>::deserialize(deserializer)? {
            Some(value) => Self::Value(value),
            None => Self::Null,
        })
    }
}

impl<T: Serialize> Serialize for OptionalField<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Value(value) => value.serialize(serializer),
            Self::Missing | Self::Null => serializer.serialize_none(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SourceKind {
    Builtin,
    App,
    LegacyExternal,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SourceDelivery {
    Frame,
    Event,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct RuleSource {
    /// Historical metadata only; new recording sources have no kind taxonomy.
    #[serde(default, skip_serializing_if = "OptionalField::is_missing")]
    pub kind: OptionalField<SourceKind>,
    pub id: String,
    pub delivery: SourceDelivery,
}

/// Rule event types
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RuleType {
    Inference,
    InferenceSet,
    Timer,
    Gpio,
    Tty,
    Schedule,
    Http,
    AlwaysOn,
    Sed,
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

/// Timezone metadata describing the event's own Unix timestamp.
/// Offset seconds include DST; a missing/null DST value means unknown.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
pub struct TimezoneMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tz: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tz_offset: Option<i64>,
    #[serde(default, skip_serializing_if = "OptionalField::is_missing")]
    pub dst: OptionalField<bool>,
}

/// Rule event as received from WebSocket
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleEvent {
    #[serde(rename = "type")]
    pub rule_type: RuleType,
    /// Event id: may be empty or null
    #[serde(default)]
    pub id: Option<String>,
    /// Unique identifier for the event
    pub uid: u64,
    /// Unix timestamp in milliseconds
    pub timestamp: u64,
    #[serde(default, skip_serializing_if = "OptionalField::is_missing")]
    pub source: OptionalField<RuleSource>,
    /// Whether this trigger created a new file task, not whether its file_ids
    /// were newly created or will eventually be published.
    #[serde(default, skip_serializing_if = "OptionalField::is_missing")]
    pub writer_created: OptionalField<bool>,
    /// Concrete files referenced by this event. Absence selects legacy
    /// event_uid correlation; an empty array explicitly references no files.
    #[serde(default, skip_serializing_if = "OptionalField::is_missing")]
    pub file_ids: OptionalField<Vec<u64>>,
    #[serde(default, skip_serializing_if = "OptionalField::is_missing")]
    pub recording_accepted: OptionalField<bool>,
    #[serde(default, skip_serializing_if = "OptionalField::is_missing")]
    pub recording_uid: OptionalField<u64>,
    #[serde(flatten)]
    pub timezone: TimezoneMetadata,
}

/// Recording identity is intentionally separate from presentation metadata.
/// In particular, a replay after a timezone change is still the same RULE.
#[derive(PartialEq, Eq, Hash)]
struct RuleEventIdentity<'a> {
    rule_type: &'a RuleType,
    id: &'a Option<String>,
    uid: u64,
    timestamp: u64,
    source: &'a OptionalField<RuleSource>,
    writer_created: &'a OptionalField<bool>,
    file_ids: &'a OptionalField<Vec<u64>>,
    recording_accepted: &'a OptionalField<bool>,
    recording_uid: &'a OptionalField<u64>,
}

impl RuleEvent {
    fn identity(&self) -> RuleEventIdentity<'_> {
        RuleEventIdentity {
            rule_type: &self.rule_type,
            id: &self.id,
            uid: self.uid,
            timestamp: self.timestamp,
            source: &self.source,
            writer_created: &self.writer_created,
            file_ids: &self.file_ids,
            recording_accepted: &self.recording_accepted,
            recording_uid: &self.recording_uid,
        }
    }

    pub fn referenced_file_ids(&self) -> Result<Option<&[u64]>, &'static str> {
        let ids = match &self.file_ids {
            OptionalField::Missing => return Ok(None),
            OptionalField::Null => return Err("file_ids must be an array when present"),
            OptionalField::Value(ids) => ids,
        };
        if ids.len() > 2 || ids.contains(&0) {
            return Err("file_ids must contain at most two nonzero IDs");
        }
        if ids.len() == 2 && ids[0] == ids[1] {
            return Err("file_ids must not contain duplicates");
        }
        Ok(Some(ids))
    }

    /// Legacy protocol: a created writer means this event's uid IS the
    /// recording uid (file events carry it as event_uid). Only feeds predating
    /// writer_created fall back to the acceptance-metadata rules below.
    pub fn recording_key(&self) -> Result<Option<u64>, &'static str> {
        use OptionalField::{Missing, Null, Value};
        // Current protocol: the rule event uid is the recording uid exactly
        // when a writer was created.
        match &self.writer_created {
            Value(true) => return Ok(Some(self.uid)),
            Value(false) => return Ok(None),
            Missing | Null => {}
        }
        // Legacy protocol from feeds that still send acceptance metadata.
        match (&self.recording_accepted, &self.recording_uid) {
            (Missing, Missing) if self.source.is_missing() => Ok(Some(self.uid)),
            (Value(true), Value(uid)) if *uid != 0 => Ok(Some(*uid)),
            (Value(false), Missing | Null) => Ok(None),
            _ => Err("incomplete or inconsistent recording acceptance metadata"),
        }
    }
}

impl PartialEq for RuleEvent {
    fn eq(&self, other: &Self) -> bool {
        self.identity() == other.identity()
    }
}

impl Eq for RuleEvent {}

impl Hash for RuleEvent {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.identity().hash(state);
    }
}

/// File event as received from WebSocket
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileEvent {
    pub op: FileOp,
    pub path: String,
    pub size: u64,
    #[serde(default)]
    pub attributes: Option<String>,
    pub event_uid: u64,
    #[serde(default)]
    pub file_id: OptionalField<u64>,
    pub timestamp: u64,
    #[serde(flatten)]
    pub timezone: TimezoneMetadata,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_id: Option<u64>,
    pub timestamp: u64,
    #[serde(flatten)]
    pub timezone: TimezoneMetadata,
}

impl From<FileEvent> for FileEventData {
    fn from(fe: FileEvent) -> Self {
        Self {
            op: fe.op,
            path: fe.path,
            size: fe.size,
            attributes: fe.attributes,
            event_uid: fe.event_uid,
            file_id: match fe.file_id {
                OptionalField::Value(id) => Some(id),
                OptionalField::Missing | OptionalField::Null => None,
            },
            timestamp: fe.timestamp,
            timezone: fe.timezone,
        }
    }
}

/// Event kind embedded in merged results.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EventKind {
    Rule,
}

/// Merged inference result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergedEvent {
    pub event: EventKind,
    #[serde(rename = "type")]
    pub rule_type: RuleType,
    #[serde(default)]
    pub id: Option<String>,
    pub uid: u64,
    pub timestamp: u64,
    /// Backward-compatible projection of the first associated file.
    pub file_event: Option<FileEventData>,
    pub file_events: Vec<FileEventData>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_ids: Option<Vec<u64>>,
    #[serde(default, skip_serializing_if = "OptionalField::is_missing")]
    pub source: OptionalField<RuleSource>,
    #[serde(default, skip_serializing_if = "OptionalField::is_missing")]
    pub writer_created: OptionalField<bool>,
    #[serde(default, skip_serializing_if = "OptionalField::is_missing")]
    pub recording_accepted: OptionalField<bool>,
    #[serde(default, skip_serializing_if = "OptionalField::is_missing")]
    pub recording_uid: OptionalField<u64>,
    #[serde(flatten)]
    pub timezone: TimezoneMetadata,
}

impl MergedEvent {
    pub fn from_rule(rule: RuleEvent) -> Self {
        Self {
            event: EventKind::Rule,
            rule_type: rule.rule_type,
            id: rule.id,
            uid: rule.uid,
            timestamp: rule.timestamp,
            file_event: None,
            file_events: Vec::new(),
            file_ids: match rule.file_ids {
                OptionalField::Value(ids) => Some(ids),
                OptionalField::Missing | OptionalField::Null => None,
            },
            source: rule.source,
            writer_created: rule.writer_created,
            recording_accepted: rule.recording_accepted,
            recording_uid: rule.recording_uid,
            timezone: rule.timezone,
        }
    }

    pub fn from_rule_and_files(rule: RuleEvent, files: Vec<FileEvent>) -> Self {
        let mut event = Self::from_rule(rule);
        event.file_events = files.into_iter().map(FileEventData::from).collect();
        event.file_event = event.file_events.first().cloned();
        event
    }
}
