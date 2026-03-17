use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::{Duration, Instant};
use tracing::{debug, trace};

use crate::models::*;

/// Configuration for the event store
#[derive(Debug, Clone)]
pub struct EventStoreConfig {
    pub rule_queue_capacity: usize,
    pub merged_capacity: usize,
    pub merged_window_secs: u64,
    pub rule_promote_timeout_ms: u64,
}

impl Default for EventStoreConfig {
    fn default() -> Self {
        Self {
            rule_queue_capacity: 1000,
            merged_capacity: 1000,
            merged_window_secs: 180,
            rule_promote_timeout_ms: 1000,
        }
    }
}

const ORPHAN_FILE_TTL_SECS: u64 = 10;

struct PendingRule {
    rule: RuleEvent,
    received_at: Instant,
}

struct OrphanFile {
    file: FileEvent,
    received_at: Instant,
}

pub struct EventStore {
    config: EventStoreConfig,
    pending_rules: HashMap<u64, PendingRule>,
    pending_rules_order: VecDeque<u64>,
    orphan_files: HashMap<u64, OrphanFile>,
    orphan_files_order: VecDeque<u64>,
    merged: VecDeque<MergedEvent>,
}

pub type SharedEventStore = Arc<Mutex<EventStore>>;

impl EventStore {
    pub fn new(config: EventStoreConfig) -> Self {
        Self {
            config,
            pending_rules: HashMap::new(),
            pending_rules_order: VecDeque::new(),
            orphan_files: HashMap::new(),
            orphan_files_order: VecDeque::new(),
            merged: VecDeque::new(),
        }
    }

    pub fn new_shared(config: EventStoreConfig) -> SharedEventStore {
        Arc::new(Mutex::new(Self::new(config)))
    }

    pub fn handle_rule_event(&mut self, rule: RuleEvent) {
        let uid = rule.uid;

        if let Some(orphan) = self.orphan_files.remove(&uid) {
            self.orphan_files_order.retain(|&orphan_uid| orphan_uid != uid);
            debug!(uid, "Merging rule event with cached file event");
            let merged = MergedEvent::from_rule_and_file(&rule, orphan.file);
            self.insert_merged(merged);
            return;
        }

        if self.pending_rules.len() >= self.config.rule_queue_capacity {
            if let Some(old_uid) = self.pending_rules_order.pop_front() {
                if let Some(old) = self.pending_rules.remove(&old_uid) {
                    debug!(uid = old_uid, "Evicting oldest pending rule (capacity)");
                    let merged = MergedEvent::from_rule(&old.rule);
                    self.insert_merged(merged);
                }
            }
        }

        self.pending_rules.insert(
            uid,
            PendingRule {
                rule,
                received_at: Instant::now(),
            },
        );
        self.pending_rules_order.push_back(uid);
        trace!(uid, "Rule event queued as pending");
    }

    pub fn handle_file_event(&mut self, file: FileEvent) {
        let event_uid = file.event_uid;

        self.cleanup_orphan_files();

        if let Some(pending) = self.pending_rules.remove(&event_uid) {
            debug!(uid = event_uid, "Merging file event with pending rule");
            self.pending_rules_order.retain(|&uid| uid != event_uid);
            let merged = MergedEvent::from_rule_and_file(&pending.rule, file);
            self.insert_merged(merged);
        } else {
            debug!(uid = event_uid, "No matching rule event found, caching file event");
            self.orphan_files_order.retain(|&uid| uid != event_uid);
            self.orphan_files.insert(
                event_uid,
                OrphanFile {
                    file,
                    received_at: Instant::now(),
                },
            );
            self.orphan_files_order.push_back(event_uid);
            self.cleanup_orphan_files();
        }
    }

    pub fn promote_expired_rules(&mut self) {
        let timeout = Duration::from_millis(self.config.rule_promote_timeout_ms);
        let now = Instant::now();

        while let Some(&uid) = self.pending_rules_order.front() {
            if let Some(pending) = self.pending_rules.get(&uid) {
                if now.duration_since(pending.received_at) >= timeout {
                    let pending = self.pending_rules.remove(&uid).unwrap();
                    self.pending_rules_order.pop_front();
                    trace!(uid, "Promoting expired rule event");
                    let merged = MergedEvent::from_rule(&pending.rule);
                    self.insert_merged(merged);
                } else {
                    break;
                }
            } else {
                self.pending_rules_order.pop_front();
            }
        }

        self.cleanup_orphan_files();
    }

    fn cleanup_orphan_files(&mut self) {
        let now = Instant::now();
        let ttl = Duration::from_secs(ORPHAN_FILE_TTL_SECS);

        while let Some(&uid) = self.orphan_files_order.front() {
            let should_remove = match self.orphan_files.get(&uid) {
                Some(orphan) => now.duration_since(orphan.received_at) >= ttl,
                None => true,
            };

            if should_remove {
                self.orphan_files_order.pop_front();
                self.orphan_files.remove(&uid);
            } else {
                break;
            }
        }

        while self.orphan_files.len() > self.config.rule_queue_capacity {
            if let Some(uid) = self.orphan_files_order.pop_front() {
                self.orphan_files.remove(&uid);
            } else {
                break;
            }
        }
    }

    fn insert_merged(&mut self, event: MergedEvent) {
        let ts = event.timestamp;

        if self.merged.back().is_none_or(|last| last.timestamp <= ts) {
            self.merged.push_back(event);
        } else {
            let pos = self
                .merged
                .binary_search_by(|e| e.timestamp.cmp(&ts))
                .unwrap_or_else(|pos| pos);
            self.merged.push_back(event);
            let len = self.merged.len();
            if pos < len - 1 {
                for i in (pos..len - 1).rev() {
                    self.merged.swap(i, i + 1);
                }
            }
        }

        while self.merged.len() > self.config.merged_capacity {
            self.merged.pop_front();
        }

        let window_ms = self.config.merged_window_secs * 1000;
        if let Some(newest_ts) = self.merged.back().map(|e| e.timestamp) {
            let cutoff = newest_ts.saturating_sub(window_ms);
            while self.merged.front().is_some_and(|e| e.timestamp < cutoff) {
                self.merged.pop_front();
            }
        }
    }

    pub fn query_events(&self, start: Option<u64>, end: Option<u64>) -> Vec<MergedEvent> {
        let start = start.unwrap_or(0);
        let end = end.unwrap_or(u64::MAX);

        if start > end {
            return Vec::new();
        }

        let start_idx = self.lower_bound(start);
        let end_exclusive = self.upper_bound(end);

        self.merged
            .range(start_idx..end_exclusive)
            .cloned()
            .collect()
    }

    pub fn query_events_size(&self, start: Option<u64>, end: Option<u64>) -> usize {
        let start = start.unwrap_or(0);
        let end = end.unwrap_or(u64::MAX);

        if start > end {
            return 0;
        }

        let start_idx = self.lower_bound(start);
        let end_exclusive = self.upper_bound(end);
        end_exclusive.saturating_sub(start_idx)
    }

    fn lower_bound(&self, target: u64) -> usize {
        let mut lo = 0usize;
        let mut hi = self.merged.len();
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.merged[mid].timestamp < target {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }

    fn upper_bound(&self, target: u64) -> usize {
        let mut lo = 0usize;
        let mut hi = self.merged.len();
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.merged[mid].timestamp <= target {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }

    pub fn clear_merged(&mut self) {
        self.merged.clear();
    }
}
