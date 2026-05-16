use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::config::Config;
use crate::runner::{AgentKind, AgentRequest};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApprovalStatus {
    Pending,
    Approved,
    Denied,
    Expired,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRecord {
    pub id: String,
    pub created_at: String,
    pub expires_at: String,
    pub sender_key: String,
    pub reason: String,
    pub status: ApprovalStatus,
    pub request: AgentRequest,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ApprovalDatabase {
    approvals: Vec<ApprovalRecord>,
}

#[derive(Clone)]
pub struct ApprovalStore {
    inner: Arc<ApprovalStoreInner>,
}

struct ApprovalStoreInner {
    path: PathBuf,
    approvals: Mutex<Vec<ApprovalRecord>>,
}

impl ApprovalStore {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        let approvals = if path.exists() {
            let text =
                fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
            serde_json::from_str::<ApprovalDatabase>(&text)
                .with_context(|| format!("parsing {}", path.display()))?
                .approvals
        } else {
            Vec::new()
        };
        let store = Self {
            inner: Arc::new(ApprovalStoreInner {
                path: path.to_path_buf(),
                approvals: Mutex::new(approvals),
            }),
        };
        store.expire_pending()?;
        store.save()?;
        Ok(store)
    }

    pub fn create(
        &self,
        sender_key: &str,
        request: AgentRequest,
        reason: impl Into<String>,
        ttl_seconds: u64,
    ) -> Result<ApprovalRecord> {
        let now = OffsetDateTime::now_utc();
        let record = ApprovalRecord {
            id: short_id(),
            created_at: format_time(now),
            expires_at: format_time(now + Duration::seconds(ttl_seconds as i64)),
            sender_key: sender_key.to_string(),
            reason: reason.into(),
            status: ApprovalStatus::Pending,
            request,
        };
        {
            let mut approvals = self
                .inner
                .approvals
                .lock()
                .map_err(|_| anyhow::anyhow!("approval store lock poisoned"))?;
            approvals.push(record.clone());
        }
        self.save()?;
        Ok(record)
    }

    pub fn pending(&self) -> Vec<ApprovalRecord> {
        self.expire_pending().ok();
        let Ok(approvals) = self.inner.approvals.lock() else {
            return Vec::new();
        };
        approvals
            .iter()
            .filter(|approval| approval.status == ApprovalStatus::Pending)
            .cloned()
            .collect()
    }

    pub fn approve(&self, approval_id: &str, sender_key: &str) -> Result<AgentRequest> {
        self.transition(approval_id, sender_key, ApprovalStatus::Approved)?
            .map(|record| record.request)
            .context("approval did not contain a request")
    }

    pub fn deny(&self, approval_id: &str, sender_key: &str) -> Result<()> {
        self.transition(approval_id, sender_key, ApprovalStatus::Denied)?;
        Ok(())
    }

    fn transition(
        &self,
        approval_id: &str,
        sender_key: &str,
        status: ApprovalStatus,
    ) -> Result<Option<ApprovalRecord>> {
        self.expire_pending()?;
        let approval_id = normalize_approval_id(approval_id);
        let updated = {
            let mut approvals = self
                .inner
                .approvals
                .lock()
                .map_err(|_| anyhow::anyhow!("approval store lock poisoned"))?;
            let Some(record) = approvals.iter_mut().find(|approval| {
                approval.id == approval_id || short_approval_id(&approval.id) == approval_id
            }) else {
                bail!("no such approval: {approval_id}");
            };
            if record.sender_key != sender_key {
                bail!("approval belongs to a different chat");
            }
            if record.status != ApprovalStatus::Pending {
                bail!("approval is {}", status_text(record.status));
            }
            record.status = status;
            record.clone()
        };
        self.save()?;
        Ok(Some(updated))
    }

    fn expire_pending(&self) -> Result<()> {
        let now = OffsetDateTime::now_utc();
        let mut changed = false;
        {
            let mut approvals = self
                .inner
                .approvals
                .lock()
                .map_err(|_| anyhow::anyhow!("approval store lock poisoned"))?;
            for approval in approvals.iter_mut() {
                if approval.status == ApprovalStatus::Pending
                    && parse_time(&approval.expires_at).is_some_and(|expires| expires <= now)
                {
                    approval.status = ApprovalStatus::Expired;
                    changed = true;
                }
            }
        }
        if changed {
            self.save()?;
        }
        Ok(())
    }

    fn save(&self) -> Result<()> {
        let approvals = self
            .inner
            .approvals
            .lock()
            .map_err(|_| anyhow::anyhow!("approval store lock poisoned"))?;
        let database = ApprovalDatabase {
            approvals: approvals.clone(),
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

pub fn approval_reason(config: &Config, request: &AgentRequest) -> Option<String> {
    let agent = config.agent(request.agent);
    let profile = config
        .effective_agent_profile_for_request(request)
        .unwrap_or_else(|_| config.effective_agent_profile(request.agent));
    let permission_mode = config
        .effective_permission_mode_for_request(request)
        .unwrap_or_else(|_| agent.permission_mode.clone());
    let mut risky_values = vec![profile.as_str(), agent.bin.as_str()];
    if let Some(permission_mode) = permission_mode.as_deref() {
        risky_values.push(permission_mode);
    }
    let risky = risky_values
        .iter()
        .any(|value| approval_keyword(value).is_some());
    if risky {
        return Some(format!(
            "{} profile may run with elevated local permissions",
            request.agent
        ));
    }

    if request.agent == AgentKind::Codex
        && request
            .prompt
            .to_ascii_lowercase()
            .contains("dangerously-bypass-approvals")
    {
        return Some("codex unsafe approval bypass was requested".to_string());
    }
    None
}

fn approval_keyword(value: &str) -> Option<&'static str> {
    let value = value.to_ascii_lowercase();
    ["unsafe", "danger", "full-access", "write-all", "rawdog"]
        .into_iter()
        .find(|keyword| value.contains(keyword))
}

pub fn render_pending(approvals: &[ApprovalRecord]) -> String {
    if approvals.is_empty() {
        return "No pending approvals.".to_string();
    }
    let lines = approvals
        .iter()
        .map(|approval| {
            format!(
                "- {} {} {}\n  reason: {}\n  expires: {}\n  prompt: {}",
                approval.id,
                approval.request.agent,
                approval.request.repo_alias.as_deref().unwrap_or("-"),
                approval.reason,
                approval.expires_at,
                preview(&approval.request.prompt, 120)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("Pending approvals\n{lines}")
}

pub fn render_approval_request(approval: &ApprovalRecord) -> String {
    format!(
        "Approval required: {}\nReason: {}\nAgent: {}\nWorkspace: {}\nExpires: {}\nSend /approve {} or /deny {}.",
        approval.id,
        approval.reason,
        approval.request.agent,
        approval.request.repo_alias.as_deref().unwrap_or("-"),
        approval.expires_at,
        approval.id,
        approval.id
    )
}

fn normalize_approval_id(id: &str) -> String {
    let id = id.trim();
    if id.starts_with("apr-") {
        id.to_string()
    } else {
        format!("apr-{id}")
    }
}

fn short_id() -> String {
    let uuid = Uuid::new_v4().simple().to_string();
    format!("apr-{}", &uuid[..8])
}

fn short_approval_id(id: &str) -> &str {
    id.strip_prefix("apr-").unwrap_or(id)
}

fn preview(text: &str, max: usize) -> String {
    let mut value = text.chars().take(max).collect::<String>();
    if text.chars().count() > max {
        value.push_str("...");
    }
    value
}

fn status_text(status: ApprovalStatus) -> &'static str {
    match status {
        ApprovalStatus::Pending => "pending",
        ApprovalStatus::Approved => "approved",
        ApprovalStatus::Denied => "denied",
        ApprovalStatus::Expired => "expired",
    }
}

fn format_time(time: OffsetDateTime) -> String {
    time.format(&Rfc3339)
        .unwrap_or_else(|_| "unknown-time".to_string())
}

fn parse_time(text: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(text, &Rfc3339).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AgentProfileConfig, Config};

    #[test]
    fn approval_replays_original_request_once() {
        let temp = tempfile::tempdir().unwrap();
        let store = ApprovalStore::open(&temp.path().join("approvals.json")).unwrap();
        let request = AgentRequest::new(AgentKind::Codex, "work", "ship it");
        let approval = store
            .create("group:a", request.clone(), "unsafe test", 60)
            .unwrap();
        assert_eq!(store.pending().len(), 1);
        assert_eq!(store.approve(&approval.id, "group:a").unwrap(), request);
        assert!(store.approve(&approval.id, "group:a").is_err());
    }

    #[test]
    fn unsafe_profile_variant_requires_approval() {
        let mut config = Config::template();
        config.agents.codex.profiles.push(AgentProfileConfig {
            name: "unsafe".to_string(),
            profile: "codex-unsafe".to_string(),
            permission_mode: None,
        });
        let request = AgentRequest::new(AgentKind::Codex, "work", "ship it").with_profile("unsafe");

        assert_eq!(
            approval_reason(&config, &request),
            Some("codex profile may run with elevated local permissions".to_string())
        );
    }
}
