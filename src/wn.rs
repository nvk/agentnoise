use std::io::Read;
use std::process::{Child, Command, Stdio};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::attachments::{AttachmentInfo, extract_attachments};
use crate::config::WhitenoiseConfig;
use crate::text::format_chat_text;
use crate::whitenoise_cli;

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

#[derive(Clone)]
pub struct WnClient {
    config: WhitenoiseConfig,
}

impl WnClient {
    pub fn new(config: WhitenoiseConfig) -> Self {
        Self { config }
    }

    pub fn configured_group_ids(&self) -> Vec<String> {
        self.config.control_group_ids()
    }

    pub fn discover_group_ids(&self) -> Result<Vec<String>> {
        Ok(whitenoise_cli::list_groups(&self.config)?
            .into_iter()
            .map(|group| group.group_id)
            .collect())
    }

    pub fn subscribe(&self) -> Result<Child> {
        let Some(group_id) = self.config.control_group_ids().into_iter().next() else {
            bail!("no White Noise group id is configured");
        };

        self.subscribe_group(&group_id)
    }

    pub fn subscribe_group(&self, group_id: &str) -> Result<Child> {
        self.subscribe_group_with_limit(group_id, self.config.subscribe_limit)
    }

    pub fn subscribe_group_with_limit(&self, group_id: &str, limit: u32) -> Result<Child> {
        let group_id = group_id.trim();
        if group_id.is_empty() {
            bail!("White Noise group id is empty");
        }
        let mut command = Command::new(whitenoise_cli::resolve_wn(&self.config.wn_bin));
        self.add_socket_arg(&mut command);
        command.arg("messages").arg("subscribe").arg("--json");
        self.add_account_arg(&mut command);
        command
            .arg("--limit")
            .arg(limit.to_string())
            .arg(group_id)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());

        command.spawn().context("starting wn messages subscribe")
    }

    pub fn send(&self, text: &str) -> Result<()> {
        let Some(group_id) = self.config.control_group_ids().into_iter().next() else {
            bail!("no White Noise group id is configured");
        };

        self.send_to(&group_id, text)
    }

    pub fn send_to(&self, group_id: &str, text: &str) -> Result<()> {
        let group_id = group_id.trim();
        if group_id.is_empty() {
            bail!("White Noise group id is empty");
        }
        let mut command = Command::new(whitenoise_cli::resolve_wn(&self.config.wn_bin));
        self.add_socket_arg(&mut command);
        command.arg("messages").arg("send").arg("--json");
        self.add_account_arg(&mut command);
        let output = command
            .arg(group_id)
            .arg(text)
            .output()
            .context("running wn messages send")?;

        if !output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let detail = format!("{stdout}\n{stderr}").trim().to_string();
            if detail.is_empty() {
                bail!("wn messages send exited with {}", output.status);
            }
            bail!("wn messages send exited with {}: {detail}", output.status);
        }

        Ok(())
    }

    pub fn send_reply(&self, text: &str) -> Result<()> {
        let Some(group_id) = self.config.control_group_ids().into_iter().next() else {
            bail!("no White Noise group id is configured");
        };
        self.send_reply_to(&group_id, text)
    }

    pub fn send_reply_to(&self, group_id: &str, text: &str) -> Result<()> {
        let text = format_chat_text(text);
        let text = if text.is_empty() {
            "agentnoise returned no text.".to_string()
        } else {
            text
        };
        let chunks = chunk_text(&text, self.config.max_message_chars.max(200));
        let total = chunks.len();
        for (index, chunk) in chunks.into_iter().enumerate() {
            if total == 1 {
                self.send_to(group_id, &chunk)?;
            } else {
                self.send_to(
                    group_id,
                    &format!("Part {}/{}\n\n{}", index + 1, total, chunk),
                )?;
            }
        }
        Ok(())
    }

    pub fn parse_events_from_reader(reader: impl Read) -> impl Iterator<Item = Result<Value>> {
        serde_json::Deserializer::from_reader(reader)
            .into_iter::<Value>()
            .map(|value| value.context("parsing wn JSON response"))
    }

    pub fn parse_event(line: &str) -> Option<MessageEvent> {
        let value: Value = serde_json::from_str(line).ok()?;
        Self::parse_events(&value).into_iter().next()
    }

    pub fn parse_events_for_group(value: &Value, group_id: &str) -> Vec<MessageEvent> {
        Self::parse_events(value)
            .into_iter()
            .map(|mut event| {
                if event.group_id.is_none() {
                    event.group_id = Some(group_id.to_string());
                }
                event
            })
            .collect()
    }

    pub fn parse_events(value: &Value) -> Vec<MessageEvent> {
        if value
            .get("stream_end")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return Vec::new();
        }

        if value.get("error").is_some_and(|error| !error.is_null()) {
            return Vec::new();
        }

        let result = value.get("result").unwrap_or(value);
        let trigger = result
            .get("trigger")
            .and_then(Value::as_str)
            .map(str::to_string);
        let is_initial = trigger.as_deref() == Some("InitialMessage");

        if let Some(message) = result.get("message") {
            return message_events(message, trigger, is_initial);
        }

        message_events(result, trigger, is_initial)
    }

    pub fn error_message(value: &Value) -> Option<String> {
        value
            .pointer("/error/message")
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    fn add_account_arg(&self, command: &mut Command) {
        if let Some(account) = self
            .config
            .account
            .as_deref()
            .map(str::trim)
            .filter(|account| !account.is_empty())
        {
            command.arg("--account").arg(account);
        }
    }

    fn add_socket_arg(&self, command: &mut Command) {
        if let Some(socket) = self.config.resolved_socket() {
            command.arg("--socket").arg(socket);
        }
    }
}

fn message_events(value: &Value, trigger: Option<String>, is_initial: bool) -> Vec<MessageEvent> {
    match value {
        Value::Array(values) => values
            .iter()
            .filter_map(|value| message_event(value, trigger.clone(), is_initial))
            .collect(),
        _ => message_event(value, trigger, is_initial)
            .into_iter()
            .collect(),
    }
}

fn message_event(value: &Value, trigger: Option<String>, is_initial: bool) -> Option<MessageEvent> {
    let text = find_string(value, &["content", "text", "body", "plaintext"]).unwrap_or_default();
    let attachments = extract_attachments(value);
    let unsupported = unsupported_message(&text, &attachments);
    if text.trim().is_empty() && unsupported.is_none() && attachments.is_empty() {
        return None;
    }

    Some(MessageEvent {
        group_id: find_group_id(value),
        sender: find_string(
            value,
            &["sender_npub", "sender", "author", "pubkey", "from"],
        ),
        id: find_string(value, &["message_id", "id", "event_id"]),
        text,
        unsupported,
        trigger,
        is_initial,
        attachments,
    })
}

fn unsupported_message(text: &str, attachments: &[AttachmentInfo]) -> Option<String> {
    if !text.trim().is_empty() || attachments.is_empty() {
        return None;
    }

    Some("Attachment received. Metadata was saved; send /attachments or /attach <id>.".to_string())
}

fn find_group_id(value: &Value) -> Option<String> {
    match value {
        Value::Object(object) => {
            if let Some(group_id) = direct_group_id(value) {
                return Some(group_id);
            }
            object.values().find_map(find_group_id)
        }
        Value::Array(values) => values.iter().find_map(find_group_id),
        _ => None,
    }
}

fn direct_group_id(value: &Value) -> Option<String> {
    let Value::Object(object) = value else {
        return None;
    };
    for key in ["group_id", "groupId", "mls_group_id", "mlsGroupId"] {
        if let Some(value) = object.get(key)
            && let Some(group_id) = group_id_value(value)
        {
            return Some(group_id);
        }
    }
    None
}

fn group_id_value(value: &Value) -> Option<String> {
    match value {
        Value::String(value) if looks_like_group_id(value) => Some(value.clone()),
        Value::Array(values) => bytes_array_to_hex(values),
        Value::Object(object) => {
            if let Some(Value::Array(values)) = value.pointer("/value/vec") {
                return bytes_array_to_hex(values);
            }
            if let Some(Value::Array(values)) = object.get("vec") {
                return bytes_array_to_hex(values);
            }
            object.values().find_map(group_id_value)
        }
        _ => None,
    }
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
    looks_like_group_id(&output).then_some(output)
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

fn looks_like_group_id(value: &str) -> bool {
    let value = value.trim();
    (32..=512).contains(&value.len()) && value.chars().all(|ch| ch.is_ascii_hexdigit())
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
    fn parses_nested_event() {
        let event = WnClient::parse_event(
            r#"{"result":{"trigger":"MessageReceived","message":{"content":"/status","sender_npub":"npub123","id":"abc"}}}"#,
        )
        .unwrap();

        assert_eq!(event.text, "/status");
        assert_eq!(event.group_id.as_deref(), None);
        assert_eq!(event.sender.as_deref(), Some("npub123"));
        assert_eq!(event.id.as_deref(), Some("abc"));
        assert!(!event.is_initial);
    }

    #[test]
    fn parses_pretty_json_stream() {
        let input = br#"{
  "result": {
    "trigger": "InitialMessage",
    "message": {
      "content": "/status",
      "author": "npub123",
      "id": "abc"
    }
  }
}
{
  "result": {
    "trigger": "MessageReceived",
    "message": {
      "content": "/jobs",
      "author": "npub123",
      "id": "def"
    }
  }
}
"#;

        let values = WnClient::parse_events_from_reader(&input[..])
            .collect::<Result<Vec<_>>>()
            .unwrap();
        let events = values
            .iter()
            .flat_map(WnClient::parse_events)
            .collect::<Vec<_>>();

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].text, "/status");
        assert!(events[0].is_initial);
        assert_eq!(events[1].text, "/jobs");
        assert!(!events[1].is_initial);
    }

    #[test]
    fn attaches_subscription_group_to_events() {
        let value: Value = serde_json::from_str(
            r#"{"result":{"trigger":"MessageReceived","message":{"content":"/status","sender_npub":"npub123","id":"abc"}}}"#,
        )
        .unwrap();
        let events = WnClient::parse_events_for_group(&value, "feedfacefeedfacefeedfacefeedface");

        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].group_id.as_deref(),
            Some("feedfacefeedfacefeedfacefeedface")
        );
    }

    #[test]
    fn parses_attachment_only_message_as_unsupported() {
        let event = WnClient::parse_event(
            r#"{"result":{"trigger":"MessageReceived","message":{"content":"","sender_npub":"npub123","id":"abc","attachments":[{"mime_type":"image/png"}]}}}"#,
        )
        .unwrap();

        assert_eq!(event.text, "");
        assert!(
            event
                .unsupported
                .as_deref()
                .unwrap()
                .contains("Attachment received")
        );
    }

    #[test]
    fn parses_group_id_from_whitenoise_vec_shape() {
        let event = WnClient::parse_event(
            r#"{"result":{"message":{"content":"/status","sender_npub":"npub123","mls_group_id":{"value":{"vec":[1,35,69,103,137,171,205,239,1,35,69,103,137,171,205,239]}}}}}"#,
        )
        .unwrap();

        assert_eq!(
            event.group_id.as_deref(),
            Some("0123456789abcdef0123456789abcdef")
        );
    }

    #[test]
    fn does_not_mistake_message_id_for_group_id() {
        let value: Value = serde_json::from_str(
            r#"{"result":{"message":{"content":"/status","sender_npub":"npub123","id":"0798720570d07b57d1d7a6b11241419efc3271ea73dc90d84d44ecb6103f41c4"}}}"#,
        )
        .unwrap();
        let event = WnClient::parse_events(&value).into_iter().next().unwrap();
        assert_eq!(event.group_id, None);

        let event = WnClient::parse_events_for_group(&value, "0123456789abcdef0123456789abcdef")
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(
            event.group_id.as_deref(),
            Some("0123456789abcdef0123456789abcdef")
        );
    }

    #[test]
    fn chunks_long_text() {
        let chunks = chunk_text("abcdef", 2);
        assert!(chunks.iter().all(|chunk| chunk.chars().count() <= 2));
        assert_eq!(chunks.concat(), "abcdef");
    }
}
