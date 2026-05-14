use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentInfo {
    pub kind: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachmentRecord {
    pub id: String,
    pub received_at: String,
    pub group_id: Option<String>,
    pub sender: Option<String>,
    pub message_id: Option<String>,
    pub attachments: Vec<AttachmentInfo>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct AttachmentDatabase {
    records: Vec<AttachmentRecord>,
}

#[derive(Clone)]
pub struct AttachmentStore {
    inner: Arc<AttachmentStoreInner>,
}

struct AttachmentStoreInner {
    path: PathBuf,
    records: Mutex<Vec<AttachmentRecord>>,
}

impl AttachmentStore {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        let records = if path.exists() {
            let text =
                fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
            serde_json::from_str::<AttachmentDatabase>(&text)
                .with_context(|| format!("parsing {}", path.display()))?
                .records
        } else {
            Vec::new()
        };
        let store = Self {
            inner: Arc::new(AttachmentStoreInner {
                path: path.to_path_buf(),
                records: Mutex::new(records),
            }),
        };
        store.save()?;
        Ok(store)
    }

    pub fn add(
        &self,
        group_id: Option<String>,
        sender: Option<String>,
        message_id: Option<String>,
        attachments: Vec<AttachmentInfo>,
    ) -> Result<AttachmentRecord> {
        if attachments.is_empty() {
            bail!("no attachment metadata was present");
        }
        let record = AttachmentRecord {
            id: short_id(),
            received_at: now_string(),
            group_id,
            sender,
            message_id,
            attachments,
        };
        {
            let mut records = self
                .inner
                .records
                .lock()
                .map_err(|_| anyhow::anyhow!("attachment store lock poisoned"))?;
            records.push(record.clone());
        }
        self.save()?;
        Ok(record)
    }

    pub fn list_recent(&self, count: usize) -> Vec<AttachmentRecord> {
        let Ok(records) = self.inner.records.lock() else {
            return Vec::new();
        };
        records.iter().rev().take(count).cloned().collect()
    }

    pub fn get(&self, target: &str) -> Option<AttachmentRecord> {
        let target = target.trim();
        if target.is_empty() {
            return None;
        }
        let Ok(records) = self.inner.records.lock() else {
            return None;
        };
        if let Ok(index) = target.parse::<usize>()
            && index > 0
        {
            return records.iter().rev().nth(index - 1).cloned();
        }
        records
            .iter()
            .find(|record| record.id == target || short_record_id(&record.id) == target)
            .cloned()
    }

    fn save(&self) -> Result<()> {
        let records = self
            .inner
            .records
            .lock()
            .map_err(|_| anyhow::anyhow!("attachment store lock poisoned"))?;
        let database = AttachmentDatabase {
            records: records.clone(),
        };
        let text = serde_json::to_string_pretty(&database)?;
        let tmp = self
            .inner
            .path
            .with_extension(format!("json.{}.tmp", Uuid::new_v4().simple()));
        fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
        fs::rename(&tmp, &self.inner.path).with_context(|| {
            format!(
                "renaming {} to {}",
                tmp.display(),
                self.inner.path.display()
            )
        })
    }
}

pub fn extract_attachments(value: &Value) -> Vec<AttachmentInfo> {
    let mut out = Vec::new();
    collect_attachments(value, None, &mut out);
    dedupe_attachments(out)
}

fn collect_attachments(value: &Value, parent_key: Option<&str>, out: &mut Vec<AttachmentInfo>) {
    match value {
        Value::Object(object) => {
            let current_is_attachment = parent_key.is_some_and(is_attachment_key)
                || object.keys().any(|key| is_attachment_metadata_key(key));
            if current_is_attachment && let Some(info) = attachment_from_object(value, parent_key) {
                out.push(info);
            }
            for (key, value) in object {
                collect_attachments(value, Some(key), out);
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_attachments(value, parent_key, out);
            }
        }
        Value::String(text) if looks_like_media_url(text) => {
            out.push(AttachmentInfo {
                kind: parent_key.unwrap_or("attachment").to_string(),
                name: None,
                mime_type: media_mime_hint(text),
                url: Some(text.clone()),
                size: None,
                hash: None,
            });
        }
        Value::String(_) => {}
        _ => {}
    }
}

fn attachment_from_object(value: &Value, parent_key: Option<&str>) -> Option<AttachmentInfo> {
    Some(AttachmentInfo {
        kind: parent_key.unwrap_or("attachment").to_string(),
        name: find_string(value, &["name", "filename", "file_name", "title"]),
        mime_type: find_string(
            value,
            &["mime_type", "mimeType", "content_type", "contentType"],
        ),
        url: find_string(value, &["url", "uri", "download_url", "media_url"]),
        size: find_u64(value, &["size", "bytes", "content_length", "contentLength"]),
        hash: find_string(value, &["hash", "sha256", "digest"]),
    })
    .filter(|info| {
        info.name.is_some()
            || info.mime_type.is_some()
            || info.url.is_some()
            || info.size.is_some()
            || info.hash.is_some()
    })
}

fn find_string(value: &Value, keys: &[&str]) -> Option<String> {
    let object = value.as_object()?;
    for key in keys {
        if let Some(Value::String(value)) = object.get(*key) {
            return Some(value.clone());
        }
    }
    None
}

fn find_u64(value: &Value, keys: &[&str]) -> Option<u64> {
    let object = value.as_object()?;
    for key in keys {
        if let Some(value) = object.get(*key).and_then(Value::as_u64) {
            return Some(value);
        }
    }
    None
}

fn dedupe_attachments(attachments: Vec<AttachmentInfo>) -> Vec<AttachmentInfo> {
    let mut out = Vec::new();
    for attachment in attachments {
        if !out.iter().any(|existing| existing == &attachment) {
            out.push(attachment);
        }
    }
    out
}

fn is_attachment_key(key: &str) -> bool {
    matches!(
        key,
        "attachment" | "attachments" | "media" | "image" | "images" | "file" | "files"
    )
}

fn is_attachment_metadata_key(key: &str) -> bool {
    matches!(
        key,
        "mime_type"
            | "mimeType"
            | "content_type"
            | "contentType"
            | "filename"
            | "file_name"
            | "download_url"
            | "media_url"
    )
}

fn looks_like_media_url(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    (value.starts_with("http://") || value.starts_with("https://"))
        && [
            ".png", ".jpg", ".jpeg", ".gif", ".webp", ".mp4", ".mov", ".pdf",
        ]
        .iter()
        .any(|suffix| value.contains(suffix))
}

fn media_mime_hint(value: &str) -> Option<String> {
    if value.starts_with("image/") || value.starts_with("video/") || value.starts_with("audio/") {
        return Some(value.to_string());
    }
    None
}

pub fn render_record_summary(record: &AttachmentRecord) -> String {
    let count = record.attachments.len();
    let suffix = if count == 1 { "file" } else { "files" };
    format!("{} {} {}", record.id, count, suffix)
}

pub fn render_record_details(record: &AttachmentRecord) -> String {
    let mut lines = vec![format!("Attachment {}", record.id)];
    if let Some(sender) = &record.sender {
        lines.push(format!("sender: {sender}"));
    }
    if let Some(group_id) = &record.group_id {
        lines.push(format!("group: {group_id}"));
    }
    lines.push(format!("received: {}", record.received_at));
    for (index, attachment) in record.attachments.iter().enumerate() {
        lines.push(format!("{}. {}", index + 1, attachment.kind));
        if let Some(name) = &attachment.name {
            lines.push(format!("   name: {name}"));
        }
        if let Some(mime_type) = &attachment.mime_type {
            lines.push(format!("   type: {mime_type}"));
        }
        if let Some(size) = attachment.size {
            lines.push(format!("   size: {size} bytes"));
        }
        if let Some(url) = &attachment.url {
            lines.push(format!("   url: {url}"));
        }
        if let Some(hash) = &attachment.hash {
            lines.push(format!("   hash: {hash}"));
        }
    }
    lines.join("\n")
}

fn short_id() -> String {
    let uuid = Uuid::new_v4().simple().to_string();
    format!("att-{}", &uuid[..8])
}

fn short_record_id(id: &str) -> &str {
    id.strip_prefix("att-").unwrap_or(id)
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
    fn extracts_nested_attachment_metadata() {
        let value: Value = serde_json::from_str(
            r#"{"message":{"attachments":[{"mime_type":"image/png","filename":"shot.png","size":42}]}}"#,
        )
        .unwrap();
        let attachments = extract_attachments(&value);
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].mime_type.as_deref(), Some("image/png"));
        assert_eq!(attachments[0].name.as_deref(), Some("shot.png"));
    }
}
