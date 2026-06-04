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
    #[serde(default)]
    pub nonce: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub local_path: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupportedMediaKind {
    Image,
    Video,
    Audio,
    Pdf,
}

impl SupportedMediaKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Video => "video",
            Self::Audio => "audio",
            Self::Pdf => "PDF",
        }
    }
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

    pub fn set_local_path(
        &self,
        record_id: &str,
        attachment_index: usize,
        path: &Path,
        size: Option<u64>,
    ) -> Result<AttachmentRecord> {
        let mut records = self
            .inner
            .records
            .lock()
            .map_err(|_| anyhow::anyhow!("attachment store lock poisoned"))?;
        let record = records
            .iter_mut()
            .find(|record| record.id == record_id)
            .with_context(|| format!("unknown attachment record: {record_id}"))?;
        let attachment = record
            .attachments
            .get_mut(attachment_index)
            .with_context(|| format!("attachment {} is out of range", attachment_index + 1))?;
        attachment.local_path = Some(path.display().to_string());
        if let Some(size) = size {
            attachment.size = Some(size);
        }
        let updated = record.clone();
        drop(records);
        self.save()?;
        Ok(updated)
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

pub fn extract_media_attachments_from_tags(
    tags: &[Vec<String>],
    caption: Option<&str>,
) -> Vec<AttachmentInfo> {
    let mut out = Vec::new();
    for tag in tags
        .iter()
        .filter(|tag| tag.first().is_some_and(|name| name == "imeta"))
    {
        let imeta = parse_imeta_tag(tag);
        let Some(url) = imeta_value(&imeta, "url") else {
            continue;
        };
        let name = imeta_value(&imeta, "filename").or_else(|| file_name_from_url(&url));
        let mime_type = imeta_value(&imeta, "m");
        let hash = imeta_value(&imeta, "x");
        let nonce = imeta_value(&imeta, "n");
        let version = imeta_value(&imeta, "v");
        let kind = caption
            .map(str::trim)
            .filter(|caption| !caption.is_empty())
            .map(|_| "media-with-caption")
            .unwrap_or("media")
            .to_string();
        out.push(AttachmentInfo {
            kind,
            name,
            mime_type,
            url: Some(url),
            size: None,
            hash,
            nonce,
            version,
            local_path: None,
        });
    }
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
            if let Some(info) = attachment_from_imeta_tag(values, parent_key) {
                out.push(info);
            }
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
                nonce: None,
                version: None,
                local_path: None,
            });
        }
        Value::String(_) => {}
        _ => {}
    }
}

fn attachment_from_object(value: &Value, parent_key: Option<&str>) -> Option<AttachmentInfo> {
    Some(AttachmentInfo {
        kind: parent_key.unwrap_or("attachment").to_string(),
        name: find_string(
            value,
            &[
                "name",
                "filename",
                "file_name",
                "fileName",
                "original_filename",
                "originalFilename",
                "title",
            ],
        ),
        mime_type: find_string(
            value,
            &[
                "mime_type",
                "mimeType",
                "content_type",
                "contentType",
                "media_type",
                "mediaType",
            ],
        ),
        url: find_string(
            value,
            &[
                "url",
                "uri",
                "download_url",
                "downloadUrl",
                "media_url",
                "mediaUrl",
                "blossom_url",
                "blossomUrl",
            ],
        ),
        size: find_u64(value, &["size", "bytes", "content_length", "contentLength"]),
        hash: find_hash(
            value,
            &[
                "hash",
                "sha256",
                "digest",
                "original_file_hash",
                "originalFileHash",
                "file_hash",
                "fileHash",
            ],
        ),
        nonce: find_string(value, &["nonce", "nonce_hex", "nonceHex"]),
        version: find_string(value, &["version", "v"]),
        local_path: find_string(value, &["local_path", "localPath", "file_path", "filePath"]),
    })
    .filter(|info| {
        info.name.is_some()
            || info.mime_type.is_some()
            || info.url.is_some()
            || info.size.is_some()
            || info.hash.is_some()
            || info.nonce.is_some()
            || info.version.is_some()
            || info.local_path.is_some()
    })
}

fn attachment_from_imeta_tag(values: &[Value], parent_key: Option<&str>) -> Option<AttachmentInfo> {
    if values.first().and_then(Value::as_str).map(str::trim) != Some("imeta") {
        return None;
    }

    let mut info = AttachmentInfo {
        kind: parent_key.unwrap_or("imeta").to_string(),
        name: None,
        mime_type: None,
        url: None,
        size: None,
        hash: None,
        nonce: None,
        version: None,
        local_path: None,
    };

    for value in values.iter().skip(1).filter_map(Value::as_str) {
        let value = value.trim();
        if let Some(rest) = value.strip_prefix("url ") {
            info.url = Some(rest.trim().to_string());
        } else if let Some(rest) = value.strip_prefix("m ") {
            info.mime_type = Some(rest.trim().to_string());
        } else if let Some(rest) = value.strip_prefix("x ") {
            info.hash = Some(rest.trim().to_string());
        } else if let Some(rest) = value.strip_prefix("n ") {
            info.nonce = Some(rest.trim().to_string());
        } else if let Some(rest) = value.strip_prefix("v ") {
            info.version = Some(rest.trim().to_string());
        } else if let Some(rest) = value.strip_prefix("filename ") {
            info.name = Some(rest.trim().to_string());
        } else if let Some(rest) = value.strip_prefix("size ")
            && let Ok(size) = rest.trim().parse::<u64>()
        {
            info.size = Some(size);
        }
    }

    (info.name.is_some()
        || info.mime_type.is_some()
        || info.url.is_some()
        || info.size.is_some()
        || info.hash.is_some()
        || info.nonce.is_some()
        || info.version.is_some())
    .then_some(info)
}

fn parse_imeta_tag(tag: &[String]) -> Vec<(String, String)> {
    tag.iter()
        .skip(1)
        .filter_map(|field| {
            let (key, value) = field.split_once(' ')?;
            Some((key.trim().to_string(), value.trim().to_string()))
        })
        .filter(|(key, value)| !key.is_empty() && !value.is_empty())
        .collect()
}

fn imeta_value(imeta: &[(String, String)], key: &str) -> Option<String> {
    imeta
        .iter()
        .find(|(candidate, _)| candidate == key)
        .map(|(_, value)| value.clone())
}

fn file_name_from_url(url: &str) -> Option<String> {
    let path = url.split('?').next().unwrap_or(url);
    path.rsplit('/')
        .next()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

fn find_string(value: &Value, keys: &[&str]) -> Option<String> {
    match value {
        Value::Object(object) => {
            for key in keys {
                if let Some(Value::String(value)) = object.get(*key) {
                    return Some(value.clone());
                }
            }
            object.values().find_map(|value| find_string(value, keys))
        }
        Value::Array(values) => values.iter().find_map(|value| find_string(value, keys)),
        _ => None,
    }
}

fn find_hash(value: &Value, keys: &[&str]) -> Option<String> {
    let object = value.as_object()?;
    for key in keys {
        let Some(value) = object.get(*key) else {
            continue;
        };
        if let Some(value) = value.as_str() {
            return Some(value.to_string());
        }
        if let Some(values) = value.as_array()
            && let Some(hash) = bytes_array_to_hex(values)
        {
            return Some(hash);
        }
    }
    None
}

fn bytes_array_to_hex(values: &[Value]) -> Option<String> {
    if values.is_empty() {
        return None;
    }
    let mut output = String::with_capacity(values.len() * 2);
    for value in values {
        let byte = value.as_u64()?;
        if byte > u8::MAX as u64 {
            return None;
        }
        output.push_str(&format!("{:02x}", byte));
    }
    Some(output)
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
        if let Some(existing) = out
            .iter_mut()
            .find(|existing| attachments_overlap(existing, &attachment))
        {
            merge_attachment(existing, attachment);
        } else {
            out.push(attachment);
        }
    }
    out
}

fn attachments_overlap(left: &AttachmentInfo, right: &AttachmentInfo) -> bool {
    left == right
        || same_nonempty(left.url.as_deref(), right.url.as_deref())
        || same_nonempty(left.hash.as_deref(), right.hash.as_deref())
        || same_nonempty(left.local_path.as_deref(), right.local_path.as_deref())
}

fn same_nonempty(left: Option<&str>, right: Option<&str>) -> bool {
    let Some(left) = left.map(str::trim).filter(|value| !value.is_empty()) else {
        return false;
    };
    let Some(right) = right.map(str::trim).filter(|value| !value.is_empty()) else {
        return false;
    };
    left == right
}

fn merge_attachment(existing: &mut AttachmentInfo, incoming: AttachmentInfo) {
    if existing.name.is_none() {
        existing.name = incoming.name;
    }
    if existing.mime_type.is_none() {
        existing.mime_type = incoming.mime_type;
    }
    if existing.url.is_none() {
        existing.url = incoming.url;
    }
    if existing.size.is_none() {
        existing.size = incoming.size;
    }
    if existing.hash.is_none() {
        existing.hash = incoming.hash;
    }
    if existing.nonce.is_none() {
        existing.nonce = incoming.nonce;
    }
    if existing.version.is_none() {
        existing.version = incoming.version;
    }
    if existing.local_path.is_none() {
        existing.local_path = incoming.local_path;
    }
}

fn is_attachment_key(key: &str) -> bool {
    matches!(
        key,
        "attachment"
            | "attachments"
            | "media"
            | "image"
            | "images"
            | "picture"
            | "pictures"
            | "photo"
            | "photos"
            | "video"
            | "videos"
            | "audio"
            | "file"
            | "files"
            | "document"
            | "documents"
            | "pdf"
            | "pdfs"
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
            | "fileName"
            | "download_url"
            | "downloadUrl"
            | "media_url"
            | "mediaUrl"
            | "original_file_hash"
            | "originalFileHash"
            | "file_hash"
            | "fileHash"
            | "blossom_url"
            | "blossomUrl"
            | "file_path"
            | "filePath"
    )
}

fn looks_like_media_url(value: &str) -> bool {
    let value = value.trim();
    (value.starts_with("http://") || value.starts_with("https://"))
        && has_supported_media_extension(value)
}

fn media_mime_hint(value: &str) -> Option<String> {
    let value = value.trim();
    let lower = value.to_ascii_lowercase();
    if media_kind_for_mime(&lower).is_some() {
        return Some(lower);
    }
    mime_hint_for_extension(value).map(str::to_string)
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
        if let Some(nonce) = &attachment.nonce {
            lines.push(format!("   nonce: {nonce}"));
        }
        if let Some(version) = &attachment.version {
            lines.push(format!("   version: {version}"));
        }
        if let Some(local_path) = &attachment.local_path {
            lines.push(format!("   local: {local_path}"));
        } else if is_downloadable_media(attachment) {
            lines.push(format!(
                "   download: /download {} {}",
                record.id,
                index + 1
            ));
        }
    }
    lines.join("\n")
}

pub fn is_downloadable_media(attachment: &AttachmentInfo) -> bool {
    attachment
        .url
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
        && attachment
            .hash
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        && attachment
            .nonce
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        && attachment
            .version
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        && attachment
            .name
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        && attachment
            .mime_type
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
}

pub fn is_supported_whitenoise_media(attachment: &AttachmentInfo) -> bool {
    supported_media_kind(attachment).is_some()
}

pub fn is_picture_attachment(attachment: &AttachmentInfo) -> bool {
    supported_media_kind(attachment) == Some(SupportedMediaKind::Image)
}

pub fn supported_media_kind(attachment: &AttachmentInfo) -> Option<SupportedMediaKind> {
    if let Some(kind) = attachment
        .mime_type
        .as_deref()
        .map(str::trim)
        .and_then(media_kind_for_mime)
    {
        return Some(kind);
    }

    if let Some(kind) = attachment
        .kind
        .to_ascii_lowercase()
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .find_map(media_kind_for_kind_word)
    {
        return Some(kind);
    }

    for value in [
        attachment.name.as_deref(),
        attachment.url.as_deref(),
        attachment.local_path.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        if let Some(kind) = media_kind_for_extension(value) {
            return Some(kind);
        }
    }

    None
}

pub fn safe_file_name(name: &str) -> String {
    let sanitized = name
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' | '\0' => '_',
            ch if ch.is_control() => '_',
            ch => ch,
        })
        .collect::<String>();
    let sanitized = sanitized.trim().trim_matches('.').trim();
    if sanitized.is_empty() || sanitized.chars().all(|ch| ch == '_') {
        "attachment".to_string()
    } else {
        sanitized.chars().take(180).collect()
    }
}

pub fn has_image_extension(value: &str) -> bool {
    media_kind_for_extension(value) == Some(SupportedMediaKind::Image)
}

pub fn has_supported_media_extension(value: &str) -> bool {
    media_kind_for_extension(value).is_some()
}

pub fn media_kind_for_extension(value: &str) -> Option<SupportedMediaKind> {
    let path = value.split(['?', '#']).next().unwrap_or(value);
    let extension = Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "jpg" | "jpeg" | "png" | "gif" | "webp" => Some(SupportedMediaKind::Image),
        "mp4" | "webm" | "mov" => Some(SupportedMediaKind::Video),
        "mp3" | "ogg" | "m4a" | "wav" => Some(SupportedMediaKind::Audio),
        "pdf" => Some(SupportedMediaKind::Pdf),
        _ => None,
    }
}

pub fn media_kind_for_mime(mime_type: &str) -> Option<SupportedMediaKind> {
    let mime_type = mime_type
        .trim()
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    match mime_type.as_str() {
        "image/jpeg" | "image/jpg" | "image/png" | "image/gif" | "image/webp" => {
            Some(SupportedMediaKind::Image)
        }
        "video/mp4" | "video/webm" | "video/quicktime" => Some(SupportedMediaKind::Video),
        "audio/mpeg" | "audio/ogg" | "audio/mp4" | "audio/m4a" | "audio/wav" | "audio/x-wav" => {
            Some(SupportedMediaKind::Audio)
        }
        "application/pdf" => Some(SupportedMediaKind::Pdf),
        _ => None,
    }
}

fn media_kind_for_kind_word(word: &str) -> Option<SupportedMediaKind> {
    match word {
        "image" | "images" | "picture" | "pictures" | "photo" | "photos" => {
            Some(SupportedMediaKind::Image)
        }
        "video" | "videos" | "movie" | "movies" => Some(SupportedMediaKind::Video),
        "audio" | "sound" | "sounds" | "music" => Some(SupportedMediaKind::Audio),
        "pdf" => Some(SupportedMediaKind::Pdf),
        _ => None,
    }
}

fn mime_hint_for_extension(value: &str) -> Option<&'static str> {
    let path = value.split(['?', '#']).next().unwrap_or(value);
    let extension = Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "jpg" | "jpeg" => Some("image/jpeg"),
        "png" => Some("image/png"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "mp4" => Some("video/mp4"),
        "webm" => Some("video/webm"),
        "mov" => Some("video/quicktime"),
        "mp3" => Some("audio/mpeg"),
        "ogg" => Some("audio/ogg"),
        "m4a" => Some("audio/m4a"),
        "wav" => Some("audio/wav"),
        "pdf" => Some("application/pdf"),
        _ => None,
    }
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

    #[test]
    fn extracts_marmot_imeta_media_reference() {
        let tags = vec![vec![
            "imeta".to_string(),
            "url https://blossom.example/blob".to_string(),
            "m image/png".to_string(),
            "filename shot.png".to_string(),
            format!("x {}", "11".repeat(32)),
            format!("n {}", "22".repeat(12)),
            "v mip04-v2".to_string(),
        ]];
        let attachments = extract_media_attachments_from_tags(&tags, Some("caption"));
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].kind, "media-with-caption");
        assert_eq!(attachments[0].name.as_deref(), Some("shot.png"));
        assert!(is_downloadable_media(&attachments[0]));
    }

    #[test]
    fn merges_marmot_media_metadata_from_nested_sources() {
        let value: Value = serde_json::from_str(
            r#"{
              "message": {
                "media_attachments": [{
                  "mime_type": "image/png",
                  "blossom_url": "https://blossom.example/hash",
                  "file_path": "/tmp/dm-cache/hash.png"
                }],
                "tags": [[
                  "imeta",
                  "url https://blossom.example/hash",
                  "m image/png",
                  "x b8f8384ea6047270b32b2870e30c5c8c79f083d247bb322ddfa812927f74172e",
                  "n 111111111111111111111111",
                  "v mip04-v2",
                  "filename phone-input.png",
                  "dim 3x3"
                ]]
              }
            }"#,
        )
        .unwrap();

        let attachments = extract_attachments(&value);

        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].name.as_deref(), Some("phone-input.png"));
        assert_eq!(attachments[0].mime_type.as_deref(), Some("image/png"));
        assert_eq!(
            attachments[0].url.as_deref(),
            Some("https://blossom.example/hash")
        );
        assert_eq!(
            attachments[0].hash.as_deref(),
            Some("b8f8384ea6047270b32b2870e30c5c8c79f083d247bb322ddfa812927f74172e")
        );
        assert_eq!(
            attachments[0].nonce.as_deref(),
            Some("111111111111111111111111")
        );
        assert_eq!(attachments[0].version.as_deref(), Some("mip04-v2"));
        assert_eq!(
            attachments[0].local_path.as_deref(),
            Some("/tmp/dm-cache/hash.png")
        );
    }

    #[test]
    fn extracts_array_hashes_as_hex() {
        let value: Value = serde_json::from_str(
            r#"{"media":{"mime_type":"image/png","original_file_hash":[184,248,56,78]}}"#,
        )
        .unwrap();

        let attachments = extract_attachments(&value);

        assert_eq!(attachments[0].hash.as_deref(), Some("b8f8384e"));
    }

    #[test]
    fn sanitizes_download_file_names() {
        assert_eq!(safe_file_name("../secret.png"), "_secret.png");
        assert_eq!(safe_file_name(" \n "), "attachment");
        assert_eq!(safe_file_name("report.pdf"), "report.pdf");
    }

    #[test]
    fn detects_picture_attachments() {
        let attachment = AttachmentInfo {
            kind: "media".to_string(),
            name: Some("shot.png".to_string()),
            mime_type: None,
            url: None,
            size: None,
            hash: None,
            nonce: None,
            version: None,
            local_path: None,
        };
        assert!(is_picture_attachment(&attachment));

        let attachment = AttachmentInfo {
            kind: "file".to_string(),
            name: Some("notes.txt".to_string()),
            mime_type: Some("text/plain".to_string()),
            url: None,
            size: None,
            hash: None,
            nonce: None,
            version: None,
            local_path: None,
        };
        assert!(!is_picture_attachment(&attachment));
    }

    #[test]
    fn detects_supported_chat_media() {
        let video = AttachmentInfo {
            kind: "media_attachments".to_string(),
            name: Some("clip.mov".to_string()),
            mime_type: Some("video/quicktime".to_string()),
            url: None,
            size: None,
            hash: Some("ab".repeat(32)),
            nonce: None,
            version: None,
            local_path: None,
        };
        assert_eq!(
            supported_media_kind(&video),
            Some(SupportedMediaKind::Video)
        );
        assert!(is_supported_whitenoise_media(&video));

        let pdf = AttachmentInfo {
            kind: "file".to_string(),
            name: Some("report.pdf".to_string()),
            mime_type: Some("application/pdf".to_string()),
            url: None,
            size: None,
            hash: None,
            nonce: None,
            version: None,
            local_path: None,
        };
        assert_eq!(supported_media_kind(&pdf), Some(SupportedMediaKind::Pdf));

        let unsupported = AttachmentInfo {
            kind: "file".to_string(),
            name: Some("notes.txt".to_string()),
            mime_type: Some("text/plain".to_string()),
            url: None,
            size: None,
            hash: None,
            nonce: None,
            version: None,
            local_path: None,
        };
        assert_eq!(supported_media_kind(&unsupported), None);
    }

    #[test]
    fn hints_supported_media_urls_by_extension() {
        let value: Value =
            serde_json::from_str(r#"{"attachments":["https://example.com/video.webm?x=1"]}"#)
                .unwrap();

        let attachments = extract_attachments(&value);

        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].mime_type.as_deref(), Some("video/webm"));
        assert_eq!(
            supported_media_kind(&attachments[0]),
            Some(SupportedMediaKind::Video)
        );
    }
}
