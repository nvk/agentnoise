//! Chat client over the embedded darkmatter (Marmot v2) runtime. `DmClient` is
//! bound to a single managed account but can subscribe to multiple groups
//! concurrently. It exposes both async and blocking methods so the existing
//! synchronous listener loop can call into it via an internal
//! [`tokio::runtime::Handle`].

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use cgka_traits::GroupId;
use marmot_app::{AppMessageQuery, AppMessageRecord, RuntimeMessageUpdate, SendSummary};

use crate::attachments::AttachmentInfo;
use crate::darkmatter_app::DarkmatterEngine;
use crate::text::format_chat_text;

/// Message shape consumed by [`crate::app::AgentApp`] routing. Field-for-field
/// compatible with the legacy v1 message event so the router does not need to
/// know about the protocol swap. `trigger`/`attachments` always come back empty
/// from darkmatter for now — attachments will land once the v2 media component
/// is bridged.
#[derive(Debug, Clone)]
pub struct MessageEvent {
    pub group_id: Option<String>,
    pub sender: Option<String>,
    pub text: String,
    pub unsupported: Option<String>,
    pub id: Option<String>,
    pub trigger: Option<String>,
    pub is_initial: bool,
    pub attachments: Vec<AttachmentInfo>,
}

impl MessageEvent {
    fn from_record(record: &AppMessageRecord, is_initial: bool) -> Self {
        Self {
            group_id: Some(record.group_id_hex.clone()),
            sender: Some(record.sender.clone()),
            text: record.plaintext.clone(),
            unsupported: None,
            id: Some(record.message_id_hex.clone()),
            trigger: None,
            is_initial,
            attachments: Vec::new(),
        }
    }
}

#[derive(Clone)]
pub struct DmClient {
    engine: DarkmatterEngine,
    account_id_hex: String,
    max_message_chars: usize,
    handle: tokio::runtime::Handle,
    send_lock: Arc<std::sync::Mutex<()>>,
}

impl DmClient {
    pub fn new(
        engine: DarkmatterEngine,
        account_id_hex: String,
        max_message_chars: usize,
        handle: tokio::runtime::Handle,
    ) -> Self {
        Self {
            engine,
            account_id_hex,
            max_message_chars: max_message_chars.max(200),
            handle,
            send_lock: Arc::new(std::sync::Mutex::new(())),
        }
    }

    pub fn account_id_hex(&self) -> &str {
        &self.account_id_hex
    }

    pub fn engine(&self) -> &DarkmatterEngine {
        &self.engine
    }

    pub fn handle(&self) -> tokio::runtime::Handle {
        self.handle.clone()
    }

    /// Subscribe to live messages for `group_id_hex`. Returns a snapshot of
    /// recently-projected messages plus an async stream of new updates.
    pub async fn subscribe_group(&self, group_id_hex: &str) -> Result<DmSubscription> {
        let query = AppMessageQuery {
            group_id_hex: Some(group_id_hex.to_string()),
            limit: None,
        };
        let inner = self
            .engine
            .runtime()
            .subscribe_messages(&self.account_id_hex, query)
            .map_err(|err| anyhow::anyhow!("darkmatter subscribe_messages: {err}"))?;
        Ok(DmSubscription { inner })
    }

    /// Send raw plaintext bytes to `group_id_hex` (async).
    pub async fn send_text(&self, group_id_hex: &str, text: &str) -> Result<SendSummary> {
        if text.is_empty() {
            bail!("attempted to send an empty darkmatter message");
        }
        let group_bytes =
            hex::decode(group_id_hex).context("decoding darkmatter group id hex for send")?;
        let group_id = GroupId::new(group_bytes);
        self.engine
            .runtime()
            .send_message(&self.account_id_hex, &group_id, text.as_bytes().to_vec())
            .await
            .map_err(|err| anyhow::anyhow!("darkmatter send_message: {err}"))
    }

    /// Async chunked chat reply.
    pub async fn send_reply_to(&self, group_id_hex: &str, text: &str) -> Result<()> {
        let formatted = format_chat_text(text);
        let formatted = if formatted.is_empty() {
            "agentnoise returned no text.".to_string()
        } else {
            formatted
        };
        let chunks = chunk_text(&formatted, self.max_message_chars);
        let total = chunks.len();
        for (index, chunk) in chunks.into_iter().enumerate() {
            let payload = if total == 1 {
                chunk
            } else {
                format!("Part {}/{}\n\n{}", index + 1, total, chunk)
            };
            self.send_text(group_id_hex, &payload).await?;
        }
        Ok(())
    }

    /// Blocking wrapper around [`Self::send_text`] for callers in sync code.
    pub fn send_to(&self, group_id_hex: &str, text: &str) -> Result<()> {
        let _guard = self
            .send_lock
            .lock()
            .map_err(|_| anyhow::anyhow!("darkmatter send lock poisoned"))?;
        self.handle
            .block_on(self.send_text(group_id_hex, text))
            .map(|_| ())
    }

    /// Blocking wrapper around [`Self::send_reply_to`].
    pub fn send_reply_to_blocking(&self, group_id_hex: &str, text: &str) -> Result<()> {
        let _guard = self
            .send_lock
            .lock()
            .map_err(|_| anyhow::anyhow!("darkmatter send lock poisoned"))?;
        self.handle.block_on(self.send_reply_to(group_id_hex, text))
    }
}

/// Live message subscription with snapshot + async stream.
pub struct DmSubscription {
    inner: marmot_app::RuntimeMessagesSubscription,
}

impl DmSubscription {
    pub fn snapshot(&self) -> Vec<MessageEvent> {
        self.inner
            .snapshot
            .iter()
            .map(|record| MessageEvent::from_record(record, true))
            .collect()
    }

    pub async fn next_message(&mut self) -> Option<MessageEvent> {
        loop {
            let update = self.inner.recv().await?;
            match update {
                RuntimeMessageUpdate::Message(received) => {
                    return Some(MessageEvent {
                        group_id: Some(hex::encode(received.message.group_id.as_slice())),
                        sender: Some(received.message.sender),
                        text: received.message.plaintext,
                        unsupported: None,
                        id: Some(received.message.message_id_hex),
                        trigger: None,
                        is_initial: false,
                        attachments: Vec::new(),
                    });
                }
                // A kind-1200 stream start opens the live-preview channel. The
                // durable stream final arrives as a normal kind-9 message, so
                // it flows through the arm above like any other chat reply.
                RuntimeMessageUpdate::AgentStreamStarted(_) => continue,
            }
        }
    }
}

fn chunk_text(text: &str, max_chars: usize) -> Vec<String> {
    if text.chars().count() <= max_chars {
        return vec![text.to_string()];
    }
    let mut chunks = Vec::new();
    let mut current = String::new();
    for line in text.lines() {
        let candidate_len = current.chars().count() + line.chars().count() + 1;
        if !current.is_empty() && candidate_len > max_chars {
            chunks.push(std::mem::take(&mut current));
        }
        if line.chars().count() > max_chars {
            for ch in line.chars() {
                if current.chars().count() >= max_chars {
                    chunks.push(std::mem::take(&mut current));
                }
                current.push(ch);
            }
            current.push('\n');
        } else {
            current.push_str(line);
            current.push('\n');
        }
    }
    if !current.is_empty() {
        chunks.push(current.trim_end().to_string());
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projected_snapshot_records_are_marked_initial() {
        let record = AppMessageRecord {
            message_id_hex: "message-1".to_string(),
            direction: "inbound".to_string(),
            group_id_hex: "group-1".to_string(),
            sender: "phone".to_string(),
            plaintext: "/codex old command".to_string(),
            kind: 9,
            tags: Vec::new(),
            recorded_at: 1,
            received_at: 1,
        };

        let event = MessageEvent::from_record(&record, true);

        assert!(event.is_initial);
        assert_eq!(event.text, "/codex old command");
    }

    #[test]
    fn chunks_long_text() {
        let chunks = chunk_text("abcdef", 2);
        assert!(chunks.iter().all(|chunk| chunk.chars().count() <= 2));
        assert_eq!(chunks.concat(), "abcdef");
    }

    #[test]
    fn chunk_short_text_is_passthrough() {
        let chunks = chunk_text("hello", 200);
        assert_eq!(chunks, vec!["hello".to_string()]);
    }
}
