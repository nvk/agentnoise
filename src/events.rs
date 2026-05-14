use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::wn::MessageEvent;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EventDirection {
    Inbound,
    Outbound,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeEvent {
    pub version: u8,
    pub at: String,
    pub direction: EventDirection,
    pub group_id: String,
    pub sender: Option<String>,
    pub message_id: Option<String>,
    pub kind: String,
    pub ok: bool,
    pub detail: Option<String>,
    pub preview: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct EventSummary {
    pub inbound: usize,
    pub outbound: usize,
    pub failed_outbound: usize,
    pub seen_message_ids: usize,
}

pub struct EventJournal {
    path: PathBuf,
    seen_ids: HashSet<String>,
    summary: EventSummary,
}

impl EventJournal {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }

        let mut journal = Self {
            path: path.to_path_buf(),
            seen_ids: HashSet::new(),
            summary: EventSummary::default(),
        };
        journal.load_existing()?;
        Ok(journal)
    }

    pub fn summary(&self) -> EventSummary {
        self.summary.clone()
    }

    pub fn already_seen(&self, group_id: &str, message_id: Option<&str>) -> bool {
        let Some(message_id) = message_id.map(str::trim).filter(|id| !id.is_empty()) else {
            return false;
        };
        self.seen_ids.contains(&seen_key(group_id, message_id))
    }

    pub fn record_inbound(&mut self, event: &MessageEvent) -> Result<()> {
        let group_id = event.group_id.clone().unwrap_or_default();
        if let Some(message_id) = event.id.as_deref() {
            self.seen_ids.insert(seen_key(&group_id, message_id));
        }
        self.summary.inbound += 1;
        self.summary.seen_message_ids = self.seen_ids.len();
        self.append(&RuntimeEvent {
            version: 1,
            at: now_string(),
            direction: EventDirection::Inbound,
            group_id,
            sender: event.sender.clone(),
            message_id: event.id.clone(),
            kind: if event.unsupported.is_some() {
                "unsupported".to_string()
            } else {
                "message".to_string()
            },
            ok: true,
            detail: event.trigger.clone(),
            preview: preview(&event.text, 180),
        })
    }

    pub fn record_outbound(
        &mut self,
        group_id: &str,
        text: &str,
        ok: bool,
        detail: Option<String>,
    ) -> Result<()> {
        self.summary.outbound += 1;
        if !ok {
            self.summary.failed_outbound += 1;
        }
        self.append(&RuntimeEvent {
            version: 1,
            at: now_string(),
            direction: EventDirection::Outbound,
            group_id: group_id.to_string(),
            sender: None,
            message_id: None,
            kind: "reply".to_string(),
            ok,
            detail,
            preview: preview(text, 180),
        })
    }

    fn load_existing(&mut self) -> Result<()> {
        if !self.path.exists() {
            return Ok(());
        }
        let text = fs::read_to_string(&self.path)
            .with_context(|| format!("reading {}", self.path.display()))?;
        for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
            let Ok(event) = serde_json::from_str::<RuntimeEvent>(line) else {
                continue;
            };
            match event.direction {
                EventDirection::Inbound => {
                    self.summary.inbound += 1;
                    if let Some(message_id) = event.message_id.as_deref() {
                        self.seen_ids.insert(seen_key(&event.group_id, message_id));
                    }
                }
                EventDirection::Outbound => {
                    self.summary.outbound += 1;
                    if !event.ok {
                        self.summary.failed_outbound += 1;
                    }
                }
            }
        }
        self.summary.seen_message_ids = self.seen_ids.len();
        Ok(())
    }

    fn append(&self, event: &RuntimeEvent) -> Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("opening {}", self.path.display()))?;
        serde_json::to_writer(&mut file, event).context("serializing runtime event")?;
        file.write_all(b"\n")
            .with_context(|| format!("writing {}", self.path.display()))
    }
}

pub fn summarize_event_log(path: &Path) -> Result<EventSummary> {
    Ok(EventJournal::open(path)?.summary())
}

fn seen_key(group_id: &str, message_id: &str) -> String {
    format!("{}:{}", group_id.trim(), message_id.trim())
}

fn preview(text: &str, max: usize) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let mut value = text.chars().take(max).collect::<String>();
    if text.chars().count() > max {
        value.push_str("...");
    }
    Some(value)
}

fn now_string() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "unknown-time".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_persists_seen_message_ids() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("events.jsonl");
        let mut journal = EventJournal::open(&path).unwrap();
        let event = MessageEvent {
            group_id: Some("group".to_string()),
            sender: Some("phone".to_string()),
            text: "/status".to_string(),
            unsupported: None,
            id: Some("msg1".to_string()),
            trigger: Some("MessageReceived".to_string()),
            is_initial: false,
            attachments: Vec::new(),
        };
        journal.record_inbound(&event).unwrap();
        assert!(journal.already_seen("group", Some("msg1")));

        let reopened = EventJournal::open(&path).unwrap();
        assert!(reopened.already_seen("group", Some("msg1")));
        assert_eq!(reopened.summary().inbound, 1);
    }
}
