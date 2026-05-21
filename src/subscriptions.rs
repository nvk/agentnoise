use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::wn::MessageEvent;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SubscriptionState {
    Running,
    Exited,
    Restarting,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SubscriptionSnapshot {
    pub version: u8,
    pub updated_at: String,
    pub groups: Vec<SubscriptionStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SubscriptionStatus {
    pub group_id: String,
    pub state: SubscriptionState,
    pub pid: Option<u32>,
    pub started_at: Option<String>,
    pub last_json_at: Option<String>,
    pub last_event_at: Option<String>,
    pub last_error_at: Option<String>,
    pub last_error: Option<String>,
    pub last_exit_at: Option<String>,
    pub last_exit_status: Option<String>,
    pub restart_count: u32,
    pub parse_error_count: u32,
    pub last_poll_at: Option<String>,
    pub latest_polled_message_id: Option<String>,
    pub latest_stream_message_id: Option<String>,
    pub latest_journaled_message_id: Option<String>,
    pub recovered_inbound: u64,
    pub stale: bool,
}

#[derive(Debug, Clone)]
struct SubscriptionRecord {
    group_id: String,
    state: SubscriptionState,
    pid: Option<u32>,
    started_at: Option<String>,
    started_instant: Option<Instant>,
    last_json_at: Option<String>,
    last_json_instant: Option<Instant>,
    last_event_at: Option<String>,
    last_event_instant: Option<Instant>,
    last_error_at: Option<String>,
    last_error: Option<String>,
    last_exit_at: Option<String>,
    last_exit_status: Option<String>,
    restart_count: u32,
    parse_error_count: u32,
    last_poll_at: Option<String>,
    last_poll_instant: Option<Instant>,
    poll_in_progress: bool,
    latest_polled_message_id: Option<String>,
    latest_stream_message_id: Option<String>,
    latest_journaled_message_id: Option<String>,
    recovered_inbound: u64,
    missed_inbound_pending: bool,
    stale: bool,
}

impl SubscriptionRecord {
    fn new(group_id: &str) -> Self {
        Self {
            group_id: group_id.to_string(),
            state: SubscriptionState::Restarting,
            pid: None,
            started_at: None,
            started_instant: None,
            last_json_at: None,
            last_json_instant: None,
            last_event_at: None,
            last_event_instant: None,
            last_error_at: None,
            last_error: None,
            last_exit_at: None,
            last_exit_status: None,
            restart_count: 0,
            parse_error_count: 0,
            last_poll_at: None,
            last_poll_instant: None,
            poll_in_progress: false,
            latest_polled_message_id: None,
            latest_stream_message_id: None,
            latest_journaled_message_id: None,
            recovered_inbound: 0,
            missed_inbound_pending: false,
            stale: false,
        }
    }

    fn status(&self) -> SubscriptionStatus {
        SubscriptionStatus {
            group_id: self.group_id.clone(),
            state: self.state,
            pid: self.pid,
            started_at: self.started_at.clone(),
            last_json_at: self.last_json_at.clone(),
            last_event_at: self.last_event_at.clone(),
            last_error_at: self.last_error_at.clone(),
            last_error: self.last_error.clone(),
            last_exit_at: self.last_exit_at.clone(),
            last_exit_status: self.last_exit_status.clone(),
            restart_count: self.restart_count,
            parse_error_count: self.parse_error_count,
            last_poll_at: self.last_poll_at.clone(),
            latest_polled_message_id: self.latest_polled_message_id.clone(),
            latest_stream_message_id: self.latest_stream_message_id.clone(),
            latest_journaled_message_id: self.latest_journaled_message_id.clone(),
            recovered_inbound: self.recovered_inbound,
            stale: self.stale,
        }
    }
}

#[derive(Debug, Default)]
pub struct SubscriptionRegistry {
    records: BTreeMap<String, SubscriptionRecord>,
}

impl SubscriptionRegistry {
    pub fn is_running(&self, group_id: &str) -> bool {
        self.records
            .get(group_id.trim())
            .is_some_and(|record| record.state == SubscriptionState::Running)
    }

    pub fn mark_starting(&mut self, group_id: &str) {
        let record = self.record_mut(group_id);
        record.state = SubscriptionState::Restarting;
        record.stale = false;
    }

    pub fn mark_started(&mut self, group_id: &str, pid: u32) {
        let now = now_string();
        let record = self.record_mut(group_id);
        record.state = SubscriptionState::Running;
        record.pid = Some(pid);
        record.started_at = Some(now.clone());
        record.started_instant = Some(Instant::now());
        record.last_json_at = None;
        record.last_json_instant = None;
        record.last_event_at = None;
        record.last_event_instant = None;
        record.last_error = None;
        record.last_error_at = None;
        record.last_exit_at = None;
        record.last_exit_status = None;
        record.parse_error_count = 0;
        record.poll_in_progress = false;
        record.missed_inbound_pending = false;
        record.stale = false;
    }

    pub fn mark_json(&mut self, group_id: &str) {
        let now = now_string();
        let record = self.record_mut(group_id);
        record.last_json_at = Some(now);
        record.last_json_instant = Some(Instant::now());
    }

    pub fn mark_event(&mut self, event: &MessageEvent) {
        let Some(group_id) = event.group_id.as_deref() else {
            return;
        };
        let now = now_string();
        let record = self.record_mut(group_id);
        record.last_event_at = Some(now);
        record.last_event_instant = Some(Instant::now());
        if let Some(id) = event
            .id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
        {
            record.latest_stream_message_id = Some(id.to_string());
        }
        record.missed_inbound_pending = false;
        record.stale = false;
    }

    pub fn mark_journaled(&mut self, event: &MessageEvent) {
        let Some(group_id) = event.group_id.as_deref() else {
            return;
        };
        if let Some(id) = event
            .id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
        {
            let record = self.record_mut(group_id);
            record.latest_journaled_message_id = Some(id.to_string());
        }
    }

    pub fn mark_error(&mut self, group_id: &str, message: &str) {
        let record = self.record_mut(group_id);
        record.last_error_at = Some(now_string());
        record.last_error = Some(message.to_string());
        record.parse_error_count = record.parse_error_count.saturating_add(1);
    }

    pub fn mark_exit(&mut self, group_id: &str, status: &str) -> u32 {
        let record = self.record_mut(group_id);
        record.state = SubscriptionState::Exited;
        record.pid = None;
        record.poll_in_progress = false;
        record.last_exit_at = Some(now_string());
        record.last_exit_status = Some(status.to_string());
        record.restart_count = record.restart_count.saturating_add(1);
        record.restart_count
    }

    pub fn mark_failed(&mut self, group_id: &str, message: &str) {
        let record = self.record_mut(group_id);
        record.state = SubscriptionState::Failed;
        record.pid = None;
        record.poll_in_progress = false;
        record.last_error_at = Some(now_string());
        record.last_error = Some(message.to_string());
    }

    pub fn mark_poll(&mut self, group_id: &str, messages: &[MessageEvent]) {
        let record = self.record_mut(group_id);
        record.last_poll_at = Some(now_string());
        record.last_poll_instant = Some(Instant::now());
        record.poll_in_progress = false;
        if let Some(id) = latest_message_id(messages) {
            record.latest_polled_message_id = Some(id);
        }
    }

    pub fn mark_poll_start(&mut self, group_id: &str) -> bool {
        let record = self.record_mut(group_id);
        if record.state != SubscriptionState::Running || record.poll_in_progress {
            return false;
        }
        record.poll_in_progress = true;
        record.last_poll_instant = Some(Instant::now());
        true
    }

    pub fn mark_poll_error(&mut self, group_id: &str, message: &str) {
        self.mark_error(group_id, message);
        let record = self.record_mut(group_id);
        record.poll_in_progress = false;
    }

    pub fn latest_polled_message_id(&self, group_id: &str) -> Option<String> {
        self.records
            .get(group_id.trim())
            .and_then(|record| record.latest_polled_message_id.clone())
    }

    pub fn pid(&self, group_id: &str) -> Option<u32> {
        self.records
            .get(group_id.trim())
            .and_then(|record| record.pid)
    }

    pub fn mark_recovered(&mut self, group_id: &str, count: usize) {
        if count == 0 {
            return;
        }
        let record = self.record_mut(group_id);
        record.recovered_inbound = record.recovered_inbound.saturating_add(count as u64);
        record.missed_inbound_pending = true;
        record.stale = false;
    }

    pub fn mark_stale(&mut self, group_id: &str) {
        let record = self.record_mut(group_id);
        if record.state == SubscriptionState::Running {
            record.stale = true;
        }
    }

    pub fn due_for_reconciliation(&self, interval: Duration) -> Vec<String> {
        let now = Instant::now();
        self.records
            .values()
            .filter(|record| record.state == SubscriptionState::Running)
            .filter(|record| !record.poll_in_progress)
            .filter(|record| {
                record
                    .last_poll_instant
                    .is_none_or(|last| now.duration_since(last) >= interval)
            })
            .map(|record| record.group_id.clone())
            .collect()
    }

    pub fn stale_running_groups(&self, idle: Duration) -> Vec<String> {
        let now = Instant::now();
        self.records
            .values()
            .filter(|record| record.state == SubscriptionState::Running)
            .filter(|record| !record.stale)
            .filter(|record| record.missed_inbound_pending)
            .filter(|record| {
                if record.latest_polled_message_id.is_none()
                    || record.latest_polled_message_id == record.latest_stream_message_id
                    || record.latest_polled_message_id == record.latest_journaled_message_id
                {
                    return false;
                }
                record
                    .last_json_instant
                    .or(record.started_instant)
                    .is_some_and(|last| now.duration_since(last) >= idle)
            })
            .map(|record| record.group_id.clone())
            .collect()
    }

    pub fn snapshot(&self) -> SubscriptionSnapshot {
        SubscriptionSnapshot {
            version: 1,
            updated_at: now_string(),
            groups: self
                .records
                .values()
                .map(SubscriptionRecord::status)
                .collect(),
        }
    }

    fn record_mut(&mut self, group_id: &str) -> &mut SubscriptionRecord {
        let group_id = group_id.trim();
        self.records
            .entry(group_id.to_string())
            .or_insert_with(|| SubscriptionRecord::new(group_id))
    }
}

pub fn read_snapshot(path: &Path) -> Result<Option<SubscriptionSnapshot>> {
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let snapshot =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    Ok(Some(snapshot))
}

pub fn write_snapshot(path: &Path, snapshot: &SubscriptionSnapshot) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let text =
        serde_json::to_string_pretty(snapshot).context("serializing subscription snapshot")?;
    fs::write(path, text).with_context(|| format!("writing {}", path.display()))
}

pub fn latest_message_id(messages: &[MessageEvent]) -> Option<String> {
    messages
        .iter()
        .rev()
        .filter_map(|event| event.id.as_deref())
        .map(str::trim)
        .find(|id| !id.is_empty())
        .map(str::to_string)
}

fn now_string() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "unknown-time".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(group_id: &str, id: &str, text: &str) -> MessageEvent {
        MessageEvent {
            group_id: Some(group_id.to_string()),
            sender: Some("phone".to_string()),
            text: text.to_string(),
            unsupported: None,
            id: Some(id.to_string()),
            trigger: None,
            is_initial: false,
            attachments: Vec::new(),
        }
    }

    #[test]
    fn registry_tracks_start_event_poll_and_snapshot() {
        let mut registry = SubscriptionRegistry::default();
        registry.mark_started("group-a", 42);
        registry.mark_json("group-a");
        registry.mark_event(&event("group-a", "m1", "/status"));
        registry.mark_journaled(&event("group-a", "m1", "/status"));
        registry.mark_poll("group-a", &[event("group-a", "m1", "/status")]);

        let snapshot = registry.snapshot();
        assert_eq!(snapshot.groups.len(), 1);
        let group = &snapshot.groups[0];
        assert_eq!(group.group_id, "group-a");
        assert_eq!(group.state, SubscriptionState::Running);
        assert_eq!(group.pid, Some(42));
        assert_eq!(group.latest_stream_message_id.as_deref(), Some("m1"));
        assert_eq!(group.latest_journaled_message_id.as_deref(), Some("m1"));
        assert_eq!(group.latest_polled_message_id.as_deref(), Some("m1"));
        assert!(!group.stale);
    }

    #[test]
    fn registry_does_not_mark_baseline_poll_stale() {
        let mut registry = SubscriptionRegistry::default();
        registry.mark_started("group-a", 42);
        registry.mark_poll("group-a", &[event("group-a", "m2", "/status")]);

        assert!(
            registry
                .stale_running_groups(Duration::from_secs(0))
                .is_empty()
        );
    }

    #[test]
    fn registry_marks_recovered_unstreamed_message_stale_after_idle() {
        let mut registry = SubscriptionRegistry::default();
        registry.mark_started("group-a", 42);
        registry.mark_poll("group-a", &[event("group-a", "m2", "/status")]);
        registry.mark_recovered("group-a", 1);

        assert_eq!(
            registry.stale_running_groups(Duration::from_secs(0)),
            vec!["group-a".to_string()]
        );
        registry.mark_stale("group-a");
        assert!(registry.snapshot().groups[0].stale);
    }

    #[test]
    fn registry_does_not_mark_journaled_polled_message_stale() {
        let mut registry = SubscriptionRegistry::default();
        registry.mark_started("group-a", 42);
        let event = event("group-a", "m2", "/status");
        registry.mark_poll("group-a", std::slice::from_ref(&event));
        registry.mark_journaled(&event);

        assert!(
            registry
                .stale_running_groups(Duration::from_secs(0))
                .is_empty()
        );
    }

    #[test]
    fn registry_skips_duplicate_poll_while_reconciliation_is_running() {
        let mut registry = SubscriptionRegistry::default();
        registry.mark_started("group-a", 42);

        assert!(registry.mark_poll_start("group-a"));
        assert!(!registry.mark_poll_start("group-a"));
        assert!(
            registry
                .due_for_reconciliation(Duration::from_secs(0))
                .is_empty()
        );

        registry.mark_poll_error("group-a", "timeout");

        assert!(registry.mark_poll_start("group-a"));
    }

    #[test]
    fn snapshot_round_trips() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("subscriptions.json");
        let mut registry = SubscriptionRegistry::default();
        registry.mark_started("group-a", 7);
        let snapshot = registry.snapshot();

        write_snapshot(&path, &snapshot).unwrap();
        let reopened = read_snapshot(&path).unwrap().unwrap();

        assert_eq!(reopened.groups[0].group_id, "group-a");
        assert_eq!(reopened.groups[0].pid, Some(7));
    }
}
