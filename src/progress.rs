use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::runner::AgentKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProgressKind {
    Started,
    Step,
    Tool,
    Approval,
    Finished,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgressEvent {
    pub kind: ProgressKind,
    pub agent: AgentKind,
    pub job_id: Option<String>,
    pub label: String,
    pub detail: Option<String>,
    pub final_event: bool,
}

pub struct ProgressRateLimiter {
    interval: Duration,
    last_sent: Option<Instant>,
}

impl ProgressRateLimiter {
    pub fn new(interval_seconds: u64) -> Self {
        Self {
            interval: Duration::from_secs(interval_seconds.max(1)),
            last_sent: None,
        }
    }

    pub fn should_send(&mut self, event: &ProgressEvent) -> bool {
        if event.final_event || matches!(event.kind, ProgressKind::Started | ProgressKind::Error) {
            self.last_sent = Some(Instant::now());
            return true;
        }
        let now = Instant::now();
        let should_send = self
            .last_sent
            .is_none_or(|last_sent| now.duration_since(last_sent) >= self.interval);
        if should_send {
            self.last_sent = Some(now);
        }
        should_send
    }
}

pub fn started(agent: AgentKind, job_id: &str) -> ProgressEvent {
    ProgressEvent {
        kind: ProgressKind::Started,
        agent,
        job_id: Some(job_id.to_string()),
        label: "started".to_string(),
        detail: None,
        final_event: false,
    }
}

pub fn finished(agent: AgentKind, job_id: &str, status: &str) -> ProgressEvent {
    ProgressEvent {
        kind: ProgressKind::Finished,
        agent,
        job_id: Some(job_id.to_string()),
        label: status.to_string(),
        detail: None,
        final_event: true,
    }
}

pub fn still_running(
    agent: AgentKind,
    job_id: &str,
    elapsed_seconds: u64,
    idle_seconds: u64,
) -> ProgressEvent {
    ProgressEvent {
        kind: ProgressKind::Step,
        agent,
        job_id: Some(job_id.to_string()),
        label: "still running".to_string(),
        detail: Some(format!(
            "Running {}; no new output for {}. Use /tail {} for logs or /cancel {} to stop it.",
            format_duration(elapsed_seconds),
            format_duration(idle_seconds),
            job_id,
            job_id
        )),
        final_event: false,
    }
}

pub fn retrying_after_silence(
    agent: AgentKind,
    job_id: &str,
    silence_seconds: u64,
    next_attempt: usize,
    total_attempts: usize,
) -> ProgressEvent {
    ProgressEvent {
        kind: ProgressKind::Step,
        agent,
        job_id: Some(job_id.to_string()),
        label: "retrying".to_string(),
        detail: Some(format!(
            "No output after {}. Restarting launch attempt {}/{}.",
            format_duration(silence_seconds),
            next_attempt,
            total_attempts
        )),
        final_event: false,
    }
}

pub fn parse_progress_line(agent: AgentKind, line: &str) -> Option<ProgressEvent> {
    let value: Value = serde_json::from_str(line.trim()).ok()?;
    match agent {
        AgentKind::Codex => parse_codex(&value),
        AgentKind::Claude => parse_claude(&value),
        AgentKind::Hermes => None,
    }
    .map(|mut event| {
        event.agent = agent;
        event
    })
}

pub fn render_progress(event: &ProgressEvent) -> String {
    let job = event
        .job_id
        .as_deref()
        .map(|id| format!("Job {id} "))
        .unwrap_or_else(|| "Job ".to_string());
    let detail = event
        .detail
        .as_deref()
        .filter(|detail| !detail.trim().is_empty())
        .map(|detail| format!("\n{detail}"))
        .unwrap_or_default();
    format!("{job}{}: {}{detail}", event.agent, event.label)
}

fn format_duration(seconds: u64) -> String {
    if seconds < 60 {
        return format!("{seconds}s");
    }

    let minutes = seconds / 60;
    let seconds = seconds % 60;
    if seconds == 0 {
        format!("{minutes}m")
    } else {
        format!("{minutes}m {seconds}s")
    }
}

fn parse_codex(value: &Value) -> Option<ProgressEvent> {
    let item = value.get("item").unwrap_or(value);
    let item_type = item
        .get("type")
        .and_then(Value::as_str)
        .or_else(|| value.get("type").and_then(Value::as_str))
        .unwrap_or_default();

    if item_type.contains("approval") {
        return Some(event(
            ProgressKind::Approval,
            "approval requested",
            text_field(item),
        ));
    }
    if item_type.contains("tool") || item_type.contains("command") {
        return Some(event(ProgressKind::Tool, item_type, text_field(item)));
    }
    if matches!(item_type, "reasoning" | "task" | "plan" | "agent_message") {
        return Some(event(ProgressKind::Step, item_type, text_field(item)));
    }
    None
}

fn parse_claude(value: &Value) -> Option<ProgressEvent> {
    let event_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match event_type {
        "system" => Some(event(ProgressKind::Step, "system", None)),
        "assistant" => Some(event(ProgressKind::Step, "assistant", text_field(value))),
        "tool_use" | "tool_result" => {
            Some(event(ProgressKind::Tool, event_type, text_field(value)))
        }
        "error" => Some(event(ProgressKind::Error, "error", text_field(value))),
        _ => None,
    }
}

fn event(kind: ProgressKind, label: &str, detail: Option<String>) -> ProgressEvent {
    ProgressEvent {
        kind,
        agent: AgentKind::Codex,
        job_id: None,
        label: label.to_string(),
        detail,
        final_event: false,
    }
}

fn text_field(value: &Value) -> Option<String> {
    for key in ["text", "summary", "message", "result", "name", "command"] {
        if let Some(text) = value.get(key).and_then(Value::as_str) {
            let text = text.trim();
            if !text.is_empty() {
                return Some(text.chars().take(300).collect());
            }
        }
    }
    value
        .get("content")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| text.chars().take(300).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_codex_tool_progress() {
        let event = parse_progress_line(
            AgentKind::Codex,
            r#"{"item":{"type":"tool_call","name":"rg"}}"#,
        )
        .unwrap();
        assert_eq!(event.kind, ProgressKind::Tool);
        assert_eq!(event.agent, AgentKind::Codex);
    }

    #[test]
    fn rate_limiter_always_sends_final() {
        let mut limiter = ProgressRateLimiter::new(60);
        let event = finished(AgentKind::Codex, "an-1", "succeeded");
        assert!(limiter.should_send(&event));
    }
}
