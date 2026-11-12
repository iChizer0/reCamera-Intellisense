use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::{Duration, Instant};
use tracing::{debug, warn};

use crate::models::*;

/// Display queues and recording correlation have independent bounds.
#[derive(Debug, Clone)]
pub struct EventStoreConfig {
    pub rule_queue_capacity: usize,
    pub merged_capacity: usize,
    /// How long a result remains visible after its first display or completion.
    pub merged_window_secs: u64,
    /// Delay before displaying a rule that has not received its recording FILE.
    pub rule_promote_timeout_ms: u64,
    pub recording_cache_capacity: usize,
    pub recording_cache_ttl_secs: u64,
}

impl Default for EventStoreConfig {
    fn default() -> Self {
        Self {
            rule_queue_capacity: 1000,
            merged_capacity: 1000,
            merged_window_secs: 180,
            rule_promote_timeout_ms: 1000,
            recording_cache_capacity: 1000,
            recording_cache_ttl_secs: 3600,
        }
    }
}

const MAX_EVENT_QUEUE_CAPACITY: usize = 4096;

#[derive(PartialEq, Eq)]
enum RecordingState {
    Waiting,
    Completed,
    /// A displayed result was explicitly cleared. Late messages cannot revive it.
    Suppressed,
}

struct RecordingAssociation {
    rule: RuleEvent,
    files: Vec<FileEvent>,
    received_at: Instant,
    displayed: bool,
    state: RecordingState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum FileKey {
    Id(u64),
    EventUid(u64),
}

fn association_key(rule: &RuleEvent) -> Result<Option<u64>, &'static str> {
    match rule.referenced_file_ids()? {
        Some(ids) => Ok((!ids.is_empty()).then_some(rule.uid)),
        None => rule.recording_key(),
    }
}

struct OrphanFile {
    file: FileEvent,
    received_at: Instant,
}

struct VisibleEvent {
    rule: RuleEvent,
    event: MergedEvent,
    updated_at: Instant,
}

pub struct EventStore {
    config: EventStoreConfig,
    // Keys include rule identity/metadata, so a rejected hit cannot replace an
    // accepted recording solely because an old producer reused a rule UID.
    pending_rules: HashMap<RuleEvent, Instant>,
    pending_rules_order: VecDeque<RuleEvent>,
    recordings: HashMap<u64, RecordingAssociation>,
    recordings_order: VecDeque<u64>,
    // A file may serve several rules. Values identify entries in recordings.
    file_references: HashMap<u64, Vec<u64>>,
    // Legacy orphans are consumed by their one owner. Identified files remain
    // available to later rules until the bounded cache expires or evicts them.
    orphan_files: HashMap<FileKey, OrphanFile>,
    orphan_files_order: VecDeque<FileKey>,
    // Bounded replay suppression for non-recording rules. Busy hits cannot evict
    // recording associations; their replay cache and display queues are separate.
    seen_rules: HashMap<RuleEvent, Instant>,
    seen_rules_order: VecDeque<RuleEvent>,
    // Stored in last meaningful update order for retention/capacity. Query output
    // is still sorted and filtered by the original trigger timestamp.
    merged: VecDeque<VisibleEvent>,
}

pub type SharedEventStore = Arc<Mutex<EventStore>>;

impl EventStore {
    pub fn new(config: EventStoreConfig) -> Self {
        let config = bounded_config(config);
        Self {
            pending_rules: HashMap::with_capacity(config.rule_queue_capacity),
            pending_rules_order: VecDeque::with_capacity(config.rule_queue_capacity),
            recordings: HashMap::with_capacity(config.recording_cache_capacity),
            recordings_order: VecDeque::with_capacity(config.recording_cache_capacity),
            file_references: HashMap::new(),
            orphan_files: HashMap::with_capacity(config.recording_cache_capacity),
            orphan_files_order: VecDeque::with_capacity(config.recording_cache_capacity),
            seen_rules: HashMap::new(),
            seen_rules_order: VecDeque::new(),
            merged: VecDeque::with_capacity(config.merged_capacity),
            config,
        }
    }

    pub fn new_shared(config: EventStoreConfig) -> SharedEventStore {
        Arc::new(Mutex::new(Self::new(config)))
    }

    pub fn handle_rule_event(&mut self, rule: RuleEvent) {
        self.handle_rule_event_at(rule, Instant::now());
    }

    fn handle_rule_event_at(&mut self, rule: RuleEvent, now: Instant) {
        self.cleanup(now);
        if self.seen_rules.contains_key(&rule)
            || self.pending_rules.contains_key(&rule)
            || self.merged.iter().any(|visible| visible.rule == rule)
        {
            return;
        }

        let recording_uid = match association_key(&rule) {
            Ok(uid) => uid,
            Err(reason) => {
                warn!(
                    uid = rule.uid,
                    reason, "RULE will not be associated with a recording"
                );
                None
            }
        };
        if let Some(uid) = recording_uid {
            if let Some(existing) = self.recordings.get(&uid) {
                if existing.rule == rule {
                    // Includes completed and explicitly cleared records, even
                    // after their ordinary display/replay entries were evicted.
                    return;
                }
                warn!(
                    recording_uid = uid,
                    "Conflicting recording ID; keeping the first association"
                );
                self.remember_rule(rule.clone(), now);
                self.display_rule(rule, now);
                return;
            }
        }
        self.remember_rule(rule.clone(), now);

        let can_cache =
            self.config.recording_cache_capacity != 0 && self.config.recording_cache_ttl_secs != 0;
        if let Some(uid) = recording_uid.filter(|_| can_cache) {
            while self.recordings.len() >= self.config.recording_cache_capacity {
                if let Some(old_uid) = self.recordings_order.pop_front() {
                    self.remove_recording(old_uid);
                    debug!(
                        recording_uid = old_uid,
                        "Recording association evicted at capacity"
                    );
                }
            }
            self.recordings.insert(
                uid,
                RecordingAssociation {
                    rule: rule.clone(),
                    files: Vec::new(),
                    received_at: now,
                    displayed: false,
                    state: RecordingState::Waiting,
                },
            );
            self.recordings_order.push_back(uid);

            if let Ok(Some(ids)) = rule.referenced_file_ids() {
                for id in ids {
                    self.file_references.entry(*id).or_default().push(uid);
                }
                // file_ids describes membership, not publication order. Keep
                // file_event tied to the first accepted FILE even when several
                // files were cached before their RULE arrived.
                let cached_files: Vec<_> = self
                    .orphan_files_order
                    .iter()
                    .filter_map(|key| match key {
                        FileKey::Id(id) if ids.contains(id) => {
                            self.orphan_files.get(key).map(|orphan| orphan.file.clone())
                        }
                        _ => None,
                    })
                    .collect();
                let matched = !cached_files.is_empty();
                for file in cached_files {
                    self.attach_file(uid, file, now);
                }
                if matched {
                    return;
                }
            } else if let Some(orphan) = self.orphan_files.remove(&FileKey::EventUid(uid)) {
                self.orphan_files_order
                    .retain(|key| *key != FileKey::EventUid(uid));
                self.attach_file(uid, orphan.file, now);
                return;
            }
        }

        if recording_uid.is_none() || self.config.rule_queue_capacity == 0 {
            self.display_rule(rule, now);
            return;
        }

        if self.pending_rules.len() >= self.config.rule_queue_capacity {
            if let Some(old_rule) = self.pending_rules_order.pop_front() {
                self.pending_rules.remove(&old_rule);
                self.display_rule(old_rule, now);
            }
        }
        self.pending_rules.insert(rule.clone(), now);
        self.pending_rules_order.push_back(rule);
    }

    pub fn handle_file_event(&mut self, file: FileEvent) {
        self.handle_file_event_at(file, Instant::now());
    }

    fn handle_file_event_at(&mut self, file: FileEvent, now: Instant) {
        self.cleanup(now);
        match &file.file_id {
            OptionalField::Value(0) => {
                warn!("FILE with a zero file_id will not be associated");
            }
            OptionalField::Value(id) => {
                let key = FileKey::Id(*id);
                if self.orphan_files.contains_key(&key) {
                    return;
                }
                self.cache_orphan(key, file.clone(), now);
                let associations = self.file_references.get(id).cloned().unwrap_or_default();
                for uid in associations {
                    self.attach_file(uid, file.clone(), now);
                }
            }
            OptionalField::Missing => {
                let uid = file.event_uid;
                if self.recordings.contains_key(&uid) {
                    self.attach_file(uid, file, now);
                    return;
                }
                self.cache_orphan(FileKey::EventUid(uid), file, now);
            }
            OptionalField::Null => {
                warn!("FILE with a null file_id will not be associated");
            }
        }
    }

    fn cache_orphan(&mut self, key: FileKey, file: FileEvent, now: Instant) {
        if self.config.recording_cache_capacity == 0
            || self.config.recording_cache_ttl_secs == 0
            || self.orphan_files.contains_key(&key)
        {
            return;
        }
        while self.orphan_files.len() >= self.config.recording_cache_capacity {
            if let Some(old_key) = self.orphan_files_order.pop_front() {
                self.orphan_files.remove(&old_key);
            }
        }
        self.orphan_files.insert(
            key,
            OrphanFile {
                file,
                received_at: now,
            },
        );
        self.orphan_files_order.push_back(key);
    }

    fn attach_file(&mut self, uid: u64, file: FileEvent, now: Instant) {
        let Some(recording) = self.recordings.get_mut(&uid) else {
            return;
        };
        if recording.state == RecordingState::Suppressed {
            return;
        }
        let complete = match recording.rule.referenced_file_ids() {
            Ok(Some(ids)) => {
                let id = match &file.file_id {
                    OptionalField::Value(id) => *id,
                    OptionalField::Missing | OptionalField::Null => return,
                };
                if !ids.contains(&id)
                    || recording
                        .files
                        .iter()
                        .any(|existing| existing.file_id == OptionalField::Value(id))
                {
                    return;
                }
                recording.files.push(file);
                recording.files.len() == ids.len()
            }
            Ok(None) => {
                if !recording.files.is_empty() {
                    return;
                }
                recording.files.push(file);
                true
            }
            Err(_) => return,
        };
        recording.state = if complete {
            RecordingState::Completed
        } else {
            RecordingState::Waiting
        };
        recording.displayed = true;
        let rule = recording.rule.clone();
        let files = recording.files.clone();
        self.pending_rules.remove(&rule);
        self.pending_rules_order.retain(|key| key != &rule);
        let event = MergedEvent::from_rule_and_files(rule.clone(), files);
        self.insert_merged(rule, event, now);
    }

    fn remove_recording(&mut self, uid: u64) {
        let Some(recording) = self.recordings.remove(&uid) else {
            return;
        };
        if let Ok(Some(ids)) = recording.rule.referenced_file_ids() {
            for id in ids {
                let remove_entry = if let Some(references) = self.file_references.get_mut(id) {
                    references.retain(|reference| *reference != uid);
                    references.is_empty()
                } else {
                    false
                };
                if remove_entry {
                    self.file_references.remove(id);
                }
            }
        }
    }

    pub fn promote_expired_rules(&mut self) {
        self.promote_expired_rules_at(Instant::now());
    }

    fn promote_expired_rules_at(&mut self, now: Instant) {
        self.cleanup(now);
        let timeout = Duration::from_millis(self.config.rule_promote_timeout_ms);
        while let Some(rule) = self.pending_rules_order.front() {
            if self
                .pending_rules
                .get(rule)
                .is_some_and(|received_at| now.saturating_duration_since(*received_at) < timeout)
            {
                break;
            }
            let rule = self.pending_rules_order.pop_front().unwrap();
            if self.pending_rules.remove(&rule).is_some() {
                self.display_rule(rule, now);
            }
        }
    }

    fn display_rule(&mut self, rule: RuleEvent, now: Instant) {
        if let Ok(Some(uid)) = association_key(&rule) {
            if let Some(recording) = self.recordings.get_mut(&uid) {
                if recording.rule == rule {
                    if recording.state != RecordingState::Waiting {
                        return;
                    }
                    recording.displayed = true;
                }
            }
        }
        let event = MergedEvent::from_rule(rule.clone());
        self.insert_merged(rule, event, now);
    }

    fn insert_merged(&mut self, rule: RuleEvent, event: MergedEvent, now: Instant) {
        self.merged.retain(|visible| visible.rule != rule);
        if self.config.merged_capacity == 0 {
            return;
        }
        self.merged.push_back(VisibleEvent {
            rule,
            event,
            updated_at: now,
        });
        while self.merged.len() > self.config.merged_capacity {
            self.merged.pop_front();
        }
    }

    fn remember_rule(&mut self, rule: RuleEvent, now: Instant) {
        let capacity = self
            .config
            .rule_queue_capacity
            .max(self.config.merged_capacity)
            .max(self.config.recording_cache_capacity);
        if capacity == 0 {
            return;
        }
        while self.seen_rules.len() >= capacity {
            if let Some(old_rule) = self.seen_rules_order.pop_front() {
                self.seen_rules.remove(&old_rule);
            }
        }
        self.seen_rules.insert(rule.clone(), now);
        self.seen_rules_order.push_back(rule);
    }

    fn cleanup(&mut self, now: Instant) {
        let ttl = Duration::from_secs(self.config.recording_cache_ttl_secs);
        while let Some(&uid) = self.recordings_order.front() {
            if self
                .recordings
                .get(&uid)
                .is_some_and(|recording| now.saturating_duration_since(recording.received_at) < ttl)
            {
                break;
            }
            self.recordings_order.pop_front();
            self.remove_recording(uid);
        }
        while let Some(&key) = self.orphan_files_order.front() {
            if self
                .orphan_files
                .get(&key)
                .is_some_and(|orphan| now.saturating_duration_since(orphan.received_at) < ttl)
            {
                break;
            }
            self.orphan_files_order.pop_front();
            self.orphan_files.remove(&key);
        }
        let seen_ttl = Duration::from_secs(
            self.config
                .recording_cache_ttl_secs
                .max(self.config.merged_window_secs)
                .max(1),
        );
        while let Some(rule) = self.seen_rules_order.front() {
            if self
                .seen_rules
                .get(rule)
                .is_some_and(|received_at| now.saturating_duration_since(*received_at) < seen_ttl)
            {
                break;
            }
            let rule = self.seen_rules_order.pop_front().unwrap();
            self.seen_rules.remove(&rule);
        }
        let window = Duration::from_secs(self.config.merged_window_secs);
        self.merged
            .retain(|visible| now.saturating_duration_since(visible.updated_at) < window);
    }

    /// Query by original trigger time, regardless of when a recording completed.
    pub fn query_events(&self, start: Option<u64>, end: Option<u64>) -> Vec<MergedEvent> {
        self.query_events_at(start, end, Instant::now())
    }

    fn query_events_at(
        &self,
        start: Option<u64>,
        end: Option<u64>,
        now: Instant,
    ) -> Vec<MergedEvent> {
        let start = start.unwrap_or(0);
        let end = end.unwrap_or(u64::MAX);
        let window = Duration::from_secs(self.config.merged_window_secs);
        let mut events: Vec<_> = self
            .merged
            .iter()
            .filter(|visible| {
                now.saturating_duration_since(visible.updated_at) < window
                    && visible.event.timestamp >= start
                    && visible.event.timestamp <= end
            })
            .map(|visible| visible.event.clone())
            .collect();
        events.sort_by_key(|event| event.timestamp);
        events
    }

    pub fn query_events_size(&self, start: Option<u64>, end: Option<u64>) -> usize {
        let now = Instant::now();
        let start = start.unwrap_or(0);
        let end = end.unwrap_or(u64::MAX);
        let window = Duration::from_secs(self.config.merged_window_secs);
        self.merged
            .iter()
            .filter(|visible| {
                now.saturating_duration_since(visible.updated_at) < window
                    && visible.event.timestamp >= start
                    && visible.event.timestamp <= end
            })
            .count()
    }

    /// Preserve the original clear semantics for still-pending rules. Previously
    /// displayed rules are suppressed even if display retention already evicted
    /// them; otherwise a late FILE could resurrect an explicitly cleared result.
    pub fn clear_merged(&mut self) {
        self.merged.clear();
        for recording in self.recordings.values_mut() {
            if recording.displayed {
                recording.state = RecordingState::Suppressed;
            }
        }
    }
}

fn bounded_config(mut config: EventStoreConfig) -> EventStoreConfig {
    for (name, capacity) in [
        ("rule", &mut config.rule_queue_capacity),
        ("merged", &mut config.merged_capacity),
        ("recording", &mut config.recording_cache_capacity),
    ] {
        if *capacity > MAX_EVENT_QUEUE_CAPACITY {
            warn!(
                queue = name,
                configured = *capacity,
                max = MAX_EVENT_QUEUE_CAPACITY,
                "Queue capacity capped to protect memory budget"
            );
            *capacity = MAX_EVENT_QUEUE_CAPACITY;
        }
    }
    config
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn config() -> EventStoreConfig {
        EventStoreConfig {
            rule_queue_capacity: 4,
            merged_capacity: 8,
            recording_cache_capacity: 8,
            ..EventStoreConfig::default()
        }
    }

    fn rule(uid: u64, file_ids: &[u64], writer_created: bool) -> RuleEvent {
        serde_json::from_value(json!({
            "type":"INFERENCE_SET", "id":format!("rule-{uid}"), "uid":uid,
            "timestamp":1_700_000_000_000_u64 + uid,
            "writer_created":writer_created, "file_ids":file_ids,
            "tz":"UTC", "tz_offset":0, "dst":false
        }))
        .unwrap()
    }

    fn file(file_id: u64, event_uid: u64) -> FileEvent {
        serde_json::from_value(json!({
            "op":"ADDED", "path":format!("/mnt/sdcard/{file_id}.jpg"),
            "size":100, "attributes":"COMPLETED", "event_uid":event_uid,
            "file_id":file_id, "timestamp":1_700_000_000_100_u64 + file_id,
            "tz":"America/New_York", "tz_offset":-18_000, "dst":false
        }))
        .unwrap()
    }

    #[test]
    fn multiple_shared_and_early_files_retain_identity_and_timezone() {
        let now = Instant::now();
        let mut store = EventStore::new(config());
        store.handle_file_event_at(file(900, 1), now);
        store.handle_file_event_at(file(901, 1), now);
        store.handle_rule_event_at(rule(1, &[900, 901], true), now);
        store.handle_rule_event_at(rule(2, &[901], false), now);
        let events = store.query_events_at(None, None, now);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].file_events.len(), 2);
        assert_eq!(events[0].file_event.as_ref().unwrap().file_id, Some(900));
        assert_eq!(events[1].file_events[0].event_uid, 1);
        assert_eq!(events[1].file_events[0].file_id, Some(901));
        assert_eq!(events[0].timezone.tz.as_deref(), Some("UTC"));
        assert_eq!(events[0].file_events[0].timezone.tz_offset, Some(-18_000));
    }

    #[test]
    fn early_files_keep_arrival_order_independent_of_reference_order() {
        let now = Instant::now();
        let mut store = EventStore::new(config());
        store.handle_file_event_at(file(901, 1), now);
        store.handle_file_event_at(file(900, 1), now + Duration::from_millis(1));
        store.handle_rule_event_at(rule(1, &[900, 901], true), now + Duration::from_millis(2));
        let events = store.query_events_at(None, None, now + Duration::from_millis(2));
        assert_eq!(events.len(), 1);
        let ids: Vec<_> = events[0]
            .file_events
            .iter()
            .map(|file| file.file_id)
            .collect();
        assert_eq!(ids, [Some(901), Some(900)]);
        assert_eq!(events[0].file_event.as_ref().unwrap().file_id, Some(901));
    }

    #[test]
    fn late_file_updates_promoted_rule_and_empty_ids_never_use_uid_fallback() {
        let now = Instant::now();
        let mut store = EventStore::new(config());
        store.handle_rule_event_at(rule(1, &[900], true), now);
        let promoted_at = now + Duration::from_millis(1000);
        store.promote_expired_rules_at(promoted_at);
        let promoted = store.query_events_at(None, None, promoted_at);
        assert_eq!(promoted.len(), 1);
        assert!(promoted[0].file_events.is_empty());
        store.handle_file_event_at(file(900, 1), now + Duration::from_secs(30));
        let events = store.query_events_at(None, None, now + Duration::from_secs(30));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].file_events.len(), 1);

        store.handle_rule_event_at(rule(2, &[], false), now + Duration::from_secs(30));
        store.handle_file_event_at(file(901, 2), now + Duration::from_secs(30));
        let events = store.query_events_at(None, None, now + Duration::from_secs(30));
        assert!(events
            .iter()
            .find(|event| event.uid == 2)
            .unwrap()
            .file_events
            .is_empty());
    }

    #[test]
    fn timezone_only_replay_does_not_duplicate_or_conflict() {
        let now = Instant::now();
        let mut store = EventStore::new(config());
        let original = rule(1, &[900], true);
        let mut replay = original.clone();
        replay.timezone.tz = Some("Asia/Shanghai".into());
        replay.timezone.tz_offset = Some(28_800);
        store.handle_rule_event_at(original, now);
        store.handle_rule_event_at(replay, now + Duration::from_millis(900));
        assert_eq!(store.pending_rules.len(), 1);
        assert_eq!(store.recordings.len(), 1);
        store.handle_file_event_at(file(900, 1), now + Duration::from_secs(1));
        let events = store.query_events_at(None, None, now + Duration::from_secs(1));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].timezone.tz.as_deref(), Some("UTC"));
    }

    #[test]
    fn legacy_and_continuous_identity_rules_remain_distinct() {
        let now = Instant::now();
        let mut store = EventStore::new(config());
        let mut legacy_rule = serde_json::to_value(rule(1, &[900], true)).unwrap();
        let object = legacy_rule.as_object_mut().unwrap();
        object.remove("file_ids");
        object.remove("writer_created");
        let legacy_rule: RuleEvent = serde_json::from_value(legacy_rule).unwrap();
        let mut legacy_file = serde_json::to_value(file(900, 1)).unwrap();
        legacy_file.as_object_mut().unwrap().remove("file_id");
        let legacy_file: FileEvent = serde_json::from_value(legacy_file).unwrap();
        store.handle_rule_event_at(legacy_rule, now);
        store.handle_file_event_at(legacy_file, now);
        assert_eq!(
            store.query_events_at(None, None, now)[0].file_events.len(),
            1
        );

        let mut store = EventStore::new(config());
        store.handle_file_event_at(file(900, 0), now);
        store.handle_file_event_at(file(901, 0), now);
        store.handle_rule_event_at(rule(1, &[900], false), now);
        store.handle_rule_event_at(rule(2, &[901], false), now);
        let events = store.query_events_at(None, None, now);
        assert_eq!(events.len(), 2);
        assert!(events
            .iter()
            .all(|event| event.file_events[0].event_uid == 0));
    }

    #[test]
    fn invalid_identities_and_cleared_results_fail_closed() {
        let now = Instant::now();
        let mut store = EventStore::new(config());
        let mut invalid_rule = serde_json::to_value(rule(1, &[900], true)).unwrap();
        invalid_rule["file_ids"] = serde_json::Value::Null;
        store.handle_rule_event_at(serde_json::from_value(invalid_rule).unwrap(), now);
        store.handle_file_event_at(file(900, 1), now);
        assert!(store.recordings.is_empty());
        assert!(store.query_events_at(None, None, now)[0]
            .file_events
            .is_empty());

        let mut store = EventStore::new(config());
        store.handle_rule_event_at(rule(1, &[900], true), now);
        store.handle_file_event_at(file(900, 1), now);
        store.clear_merged();
        store.handle_file_event_at(file(900, 1), now);
        assert!(store.query_events_at(None, None, now).is_empty());
    }

    #[test]
    fn recording_ttl_and_trigger_time_queries_use_their_own_clocks() {
        let now = Instant::now();
        let mut cfg = config();
        cfg.recording_cache_ttl_secs = 2;
        let mut store = EventStore::new(cfg);
        store.handle_rule_event_at(rule(1, &[900], true), now);
        store.promote_expired_rules_at(now + Duration::from_secs(1));
        assert_eq!(
            store
                .query_events_at(None, None, now + Duration::from_secs(1))
                .len(),
            1
        );
        store.handle_file_event_at(file(900, 1), now + Duration::from_secs(2));
        assert!(
            store.query_events_at(None, None, now + Duration::from_secs(2))[0]
                .file_events
                .is_empty()
        );

        let mut store = EventStore::new(config());
        store.handle_rule_event_at(rule(2, &[], false), now);
        let first = store.query_events_at(None, None, now)[0].timestamp;
        assert_eq!(
            store.query_events_at(Some(first), Some(first), now).len(),
            1
        );
        assert!(store.query_events_at(Some(first + 1), None, now).is_empty());
        assert!(store.query_events_at(None, Some(first - 1), now).is_empty());
        assert_eq!(store.query_events_size(Some(first), Some(first)), 1);
        assert!(store
            .query_events_at(None, None, now + Duration::from_secs(180))
            .is_empty());
    }
}
