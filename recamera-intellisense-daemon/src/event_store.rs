use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::{Duration, Instant};
use tracing::{debug, trace};

use crate::models::*;

/// Configuration for the event store
#[derive(Debug, Clone)]
pub struct EventStoreConfig {
    /// Maximum number of raw events in the rule event queue
    pub rule_queue_capacity: usize,
    /// Maximum number of merged results to keep
    pub merged_capacity: usize,
    /// Time window for merged results (in seconds)
    pub merged_window_secs: u64,
    /// Timeout (in milliseconds) for unmatched rule events before promoting them
    pub rule_promote_timeout_ms: u64,
}

impl Default for EventStoreConfig {
    fn default() -> Self {
        Self {
            rule_queue_capacity: 1000,
            merged_capacity: 1000,
            merged_window_secs: 180, // 3 minutes
            rule_promote_timeout_ms: 1000,
        }
    }
}

const ORPHAN_FILE_TTL_SECS: u64 = 10;

/// A pending rule event waiting for a matching file event
struct PendingRule {
    rule: RuleEvent,
    received_at: Instant,
}

/// A file event temporarily cached until a matching rule arrives
struct OrphanFile {
    file: FileEvent,
    received_at: Instant,
}

/// The event store that manages queuing and merging
pub struct EventStore {
    config: EventStoreConfig,
    /// Pending rule events indexed by uid, with insertion-order tracking
    pending_rules: HashMap<u64, PendingRule>,
    /// Insertion order for pending rules (uid values)
    pending_rules_order: VecDeque<u64>,
    /// Temporary cache for file events that arrived before their matching rule
    orphan_files: HashMap<u64, OrphanFile>,
    /// Insertion order for orphan files (uid values)
    orphan_files_order: VecDeque<u64>,
    /// Merged results sorted by timestamp
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

    /// Process an incoming rule event
    pub fn handle_rule_event(&mut self, rule: RuleEvent) {
        let uid = rule.uid;

        // Check if there's an orphan file event waiting for this rule
        if let Some(orphan) = self.orphan_files.remove(&uid) {
            self.orphan_files_order
                .retain(|&orphan_uid| orphan_uid != uid);
            debug!(uid, "Merging rule event with cached file event");
            let merged = MergedEvent::from_rule_and_file(&rule, orphan.file);
            self.insert_merged(merged);
            return;
        }

        // Evict oldest if at capacity
        if self.pending_rules.len() >= self.config.rule_queue_capacity {
            if let Some(old_uid) = self.pending_rules_order.pop_front() {
                if let Some(old) = self.pending_rules.remove(&old_uid) {
                    debug!(uid = old_uid, "Evicting oldest pending rule (capacity)");
                    let merged = MergedEvent::from_rule(&old.rule);
                    self.insert_merged(merged);
                }
            }
        }

        // Store as pending
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

    /// Process an incoming file event
    pub fn handle_file_event(&mut self, file: FileEvent) {
        let event_uid = file.event_uid;

        self.cleanup_orphan_files();

        // Try to find matching rule event
        if let Some(pending) = self.pending_rules.remove(&event_uid) {
            debug!(uid = event_uid, "Merging file event with pending rule");
            // Remove from order tracking
            self.pending_rules_order.retain(|&uid| uid != event_uid);
            let merged = MergedEvent::from_rule_and_file(&pending.rule, file);
            self.insert_merged(merged);
        } else {
            // No matching rule event yet; cache the file event
            debug!(
                uid = event_uid,
                "No matching rule event found, caching file event"
            );

            // Keep insertion order unique by uid
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

    /// Promote timed-out pending rule events (no matching file event within timeout)
    pub fn promote_expired_rules(&mut self) {
        let timeout = Duration::from_millis(self.config.rule_promote_timeout_ms);
        let now = Instant::now();

        // Drain from front while expired
        while let Some(&uid) = self.pending_rules_order.front() {
            if let Some(pending) = self.pending_rules.get(&uid) {
                if now.duration_since(pending.received_at) >= timeout {
                    let pending = self.pending_rules.remove(&uid).unwrap();
                    self.pending_rules_order.pop_front();
                    trace!(uid, "Promoting expired rule event");
                    let merged = MergedEvent::from_rule(&pending.rule);
                    self.insert_merged(merged);
                } else {
                    break; // rest are newer, so not expired yet
                }
            } else {
                // uid in order list but not in map (already removed); clean up
                self.pending_rules_order.pop_front();
            }
        }

        // Also clean up old/or over-capacity orphan file events
        self.cleanup_orphan_files();
    }

    fn cleanup_orphan_files(&mut self) {
        let now = Instant::now();
        let ttl = Duration::from_secs(ORPHAN_FILE_TTL_SECS);

        // Evict expired in insertion order
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

        // Enforce orphan cache capacity with same bound as pending rule queue.
        while self.orphan_files.len() > self.config.rule_queue_capacity {
            if let Some(uid) = self.orphan_files_order.pop_front() {
                self.orphan_files.remove(&uid);
            } else {
                break;
            }
        }
    }

    /// Insert a merged event in sorted order (by timestamp) and enforce capacity/time window
    fn insert_merged(&mut self, event: MergedEvent) {
        let ts = event.timestamp;

        // Fast path: append if newer or equal to last
        if self.merged.back().is_none_or(|last| last.timestamp <= ts) {
            self.merged.push_back(event);
        } else {
            // Binary search for insertion position
            let pos = self
                .merged
                .binary_search_by(|e| e.timestamp.cmp(&ts))
                .unwrap_or_else(|pos| pos);
            // VecDeque doesn't have insert, so we use make_contiguous + insert workaround
            // Convert to index-based insertion
            self.merged.push_back(event);
            // Rotate the last element to the correct position
            let len = self.merged.len();
            if pos < len - 1 {
                // Move from end to pos
                for i in (pos..len - 1).rev() {
                    self.merged.swap(i, i + 1);
                }
            }
        }

        // Enforce capacity
        while self.merged.len() > self.config.merged_capacity {
            self.merged.pop_front();
        }

        // Enforce time window: remove events older than window
        let window_ms = self.config.merged_window_secs * 1000;
        if let Some(newest_ts) = self.merged.back().map(|e| e.timestamp) {
            let cutoff = newest_ts.saturating_sub(window_ms);
            while self.merged.front().is_some_and(|e| e.timestamp < cutoff) {
                self.merged.pop_front();
            }
        }
    }

    /// Query merged events by optional time range [start, end] inclusive.
    /// Timestamps are in milliseconds.
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

    /// Get the count of merged events in optional time range
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

    /// Clear all merged events
    pub fn clear_merged(&mut self) {
        self.merged.clear();
    }
}
