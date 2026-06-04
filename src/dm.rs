//! Chat client over the embedded darkmatter (Marmot v2) runtime. `DmClient` is
//! bound to a single managed account but can subscribe to multiple groups
//! concurrently. It exposes both async and blocking methods so the existing
//! synchronous listener loop can call into it via an internal
//! [`tokio::runtime::Handle`].

use std::fs;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use cgka_traits::GroupId;
use marmot_app::{
    AppMessageQuery, AppMessageRecord, MediaDownloadResult, MediaReference, MediaUploadRequest,
    MediaUploadResult, RelayPlaneHealth, RuntimeMessageUpdate, SendSummary,
};

use crate::attachments::{self, AttachmentInfo};
use crate::darkmatter_app::DarkmatterEngine;
use crate::text::format_chat_text;

/// Message shape consumed by [`crate::app::AgentApp`] routing. Field-for-field
/// compatible with the legacy v1 message event so the router does not need to
/// know about the protocol swap. Marmot v2 media messages are projected from
/// NIP-92 `imeta` tags into `attachments`.
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
            attachments: attachments::extract_media_attachments_from_tags(
                &record.tags,
                Some(&record.plaintext),
            ),
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

    /// Return the currently projected visible group ids for this account.
    pub fn visible_group_ids(&self) -> Result<Vec<String>> {
        let _runtime = self.handle.enter();
        let subscription = self
            .engine
            .runtime()
            .subscribe_chats(&self.account_id_hex, false)
            .map_err(|err| anyhow::anyhow!("darkmatter subscribe_chats: {err}"))?;
        Ok(subscription
            .snapshot
            .into_iter()
            .map(|group| group.group_id_hex)
            .collect())
    }

    pub async fn catch_up(&self) -> Result<()> {
        self.engine.catch_up().await
    }

    pub async fn relay_health(&self) -> RelayPlaneHealth {
        self.engine.relay_health().await
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
        Ok(DmSubscription {
            inner,
            account_id_hex: self.account_id_hex.clone(),
        })
    }

    /// Send raw plaintext bytes to `group_id_hex` (async).
    pub async fn send_text(&self, group_id_hex: &str, text: &str) -> Result<SendSummary> {
        if text.is_empty() {
            bail!("attempted to send an empty darkmatter message");
        }
        let group_id = group_id_from_hex(group_id_hex)?;
        self.engine
            .runtime()
            .send_message(&self.account_id_hex, &group_id, text.as_bytes().to_vec())
            .await
            .map_err(|err| anyhow::anyhow!("darkmatter send_message: {err}"))
    }

    pub fn upload_file_blocking(
        &self,
        group_id_hex: &str,
        path: &Path,
        caption: Option<String>,
    ) -> Result<MediaUploadResult> {
        let plaintext =
            fs::read(path).with_context(|| format!("reading upload file {}", path.display()))?;
        if plaintext.is_empty() {
            bail!("cannot upload an empty file: {}", path.display());
        }
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .with_context(|| format!("upload path has no file name: {}", path.display()))?;
        let request = MediaUploadRequest {
            file_name,
            media_type: guess_media_type(path).to_string(),
            plaintext,
            caption,
            send: true,
            blossom_server: None,
        };
        let group_id = group_id_from_hex(group_id_hex)?;
        let _guard = self
            .send_lock
            .lock()
            .map_err(|_| anyhow::anyhow!("darkmatter send lock poisoned"))?;
        self.handle
            .block_on(
                self.engine
                    .runtime()
                    .upload_media(&self.account_id_hex, &group_id, request),
            )
            .map_err(|err| anyhow::anyhow!("darkmatter upload_media: {err}"))
    }

    pub fn download_attachment_blocking(
        &self,
        group_id_hex: &str,
        attachment: &AttachmentInfo,
    ) -> Result<MediaDownloadResult> {
        let group_id = group_id_from_hex(group_id_hex)?;
        let reference = media_reference_from_attachment(attachment)?;
        self.handle
            .block_on(self.engine.runtime().download_media(
                &self.account_id_hex,
                &group_id,
                reference,
            ))
            .map_err(|err| anyhow::anyhow!("darkmatter download_media: {err}"))
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
    account_id_hex: String,
}

impl DmSubscription {
    pub fn snapshot(&self) -> Vec<MessageEvent> {
        self.inner
            .snapshot
            .iter()
            .filter(|record| !is_self_sender(&record.sender, &self.account_id_hex))
            .map(|record| MessageEvent::from_record(record, true))
            .collect()
    }

    pub async fn next_message(&mut self) -> Option<MessageEvent> {
        loop {
            let update = self.inner.recv().await?;
            match update {
                RuntimeMessageUpdate::Message(received) => {
                    if is_self_sender(&received.message.sender, &self.account_id_hex) {
                        continue;
                    }
                    let attachments = attachments::extract_media_attachments_from_tags(
                        &received.message.tags,
                        Some(&received.message.plaintext),
                    );
                    return Some(MessageEvent {
                        group_id: Some(hex::encode(received.message.group_id.as_slice())),
                        sender: Some(received.message.sender),
                        text: received.message.plaintext,
                        unsupported: None,
                        id: Some(received.message.message_id_hex),
                        trigger: None,
                        is_initial: false,
                        attachments,
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

fn is_self_sender(sender: &str, account_id_hex: &str) -> bool {
    sender.eq_ignore_ascii_case(account_id_hex)
}

fn group_id_from_hex(group_id_hex: &str) -> Result<GroupId> {
    let group_bytes =
        hex::decode(group_id_hex).context("decoding darkmatter group id hex for media/send")?;
    Ok(GroupId::new(group_bytes))
}

fn media_reference_from_attachment(attachment: &AttachmentInfo) -> Result<MediaReference> {
    let required = |field: &'static str, value: &Option<String>| -> Result<String> {
        value
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .with_context(|| format!("attachment is missing media {field}"))
    };
    Ok(MediaReference {
        url: required("url", &attachment.url)?,
        file_hash_hex: required("hash", &attachment.hash)?,
        nonce_hex: required("nonce", &attachment.nonce)?,
        file_name: required("file name", &attachment.name)?,
        media_type: required("mime type", &attachment.mime_type)?,
        version: required("version", &attachment.version)?,
    })
}

fn guess_media_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "heic" => "image/heic",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "mp3" => "audio/mpeg",
        "ogg" => "audio/ogg",
        "m4a" => "audio/mp4",
        "wav" => "audio/wav",
        "pdf" => "application/pdf",
        "txt" | "md" | "rs" | "toml" | "json" | "yaml" | "yml" => "text/plain",
        _ => "application/octet-stream",
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
    fn self_authored_darkmatter_messages_are_ignored() {
        assert!(is_self_sender("ABCDEF", "abcdef"));
        assert!(!is_self_sender("phone", "abcdef"));
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

    #[test]
    fn guesses_mainline_supported_media_types() {
        assert_eq!(guess_media_type(Path::new("clip.webm")), "video/webm");
        assert_eq!(guess_media_type(Path::new("voice.ogg")), "audio/ogg");
        assert_eq!(guess_media_type(Path::new("report.pdf")), "application/pdf");
    }
}
