use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::runner::AgentKind;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionState {
    pub repo_alias: Option<String>,
    #[serde(default = "default_cwd")]
    pub cwd: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub closed: bool,
    #[serde(default)]
    pub worktree: Option<String>,
    #[serde(default)]
    pub worktree_path: Option<PathBuf>,
    #[serde(default)]
    pub default_agent: Option<AgentKind>,
    #[serde(default)]
    pub default_profile: Option<String>,
    #[serde(default)]
    pub default_prompt_prefix: Option<String>,
    #[serde(default)]
    pub onboarding_hints_shown: u8,
}

impl SessionState {
    pub fn new(repo_alias: Option<String>) -> Self {
        Self {
            repo_alias,
            cwd: default_cwd(),
            name: None,
            closed: false,
            worktree: None,
            worktree_path: None,
            default_agent: None,
            default_profile: None,
            default_prompt_prefix: None,
            onboarding_hints_shown: 0,
        }
    }

    pub fn normalize(&mut self) {
        if self.cwd.trim().is_empty() || self.cwd == "/" {
            self.cwd = default_cwd();
        }
        self.name = self
            .name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string);
        self.worktree = self
            .worktree
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string);
        self.default_profile = self
            .default_profile
            .as_deref()
            .map(str::trim)
            .filter(|profile| !profile.is_empty())
            .map(str::to_string);
        self.default_prompt_prefix = self
            .default_prompt_prefix
            .as_deref()
            .map(str::trim)
            .filter(|prefix| !prefix.is_empty())
            .map(str::to_string);
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SessionDatabase {
    sessions: HashMap<String, SessionState>,
}

#[derive(Clone)]
pub struct ChatStateStore {
    inner: Arc<ChatStateStoreInner>,
}

struct ChatStateStoreInner {
    path: PathBuf,
    sessions: Mutex<HashMap<String, SessionState>>,
}

impl ChatStateStore {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }

        let sessions = if path.exists() {
            let text =
                fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
            let mut database: SessionDatabase = serde_json::from_str(&text)
                .with_context(|| format!("parsing {}", path.display()))?;
            for session in database.sessions.values_mut() {
                session.normalize();
            }
            database.sessions
        } else {
            HashMap::new()
        };

        let store = Self {
            inner: Arc::new(ChatStateStoreInner {
                path: path.to_path_buf(),
                sessions: Mutex::new(sessions),
            }),
        };
        store.save()?;
        Ok(store)
    }

    pub fn get_or_default(
        &self,
        key: &str,
        default_repo_alias: Option<String>,
    ) -> Result<SessionState> {
        let sessions = self
            .inner
            .sessions
            .lock()
            .map_err(|_| anyhow::anyhow!("chat state lock poisoned"))?;
        let mut session = sessions
            .get(key)
            .cloned()
            .unwrap_or_else(|| SessionState::new(default_repo_alias.clone()));
        if session.repo_alias.is_none() {
            session.repo_alias = default_repo_alias;
        }
        session.normalize();
        Ok(session)
    }

    pub fn set(&self, key: &str, mut session: SessionState) -> Result<()> {
        session.normalize();
        {
            let mut sessions = self
                .inner
                .sessions
                .lock()
                .map_err(|_| anyhow::anyhow!("chat state lock poisoned"))?;
            sessions.insert(key.to_string(), session);
        }
        self.save()
    }

    pub fn list(&self) -> Vec<(String, SessionState)> {
        let Ok(sessions) = self.inner.sessions.lock() else {
            return Vec::new();
        };
        let mut sessions = sessions
            .iter()
            .map(|(key, session)| (key.clone(), session.clone()))
            .collect::<Vec<_>>();
        sessions.sort_by(|left, right| left.0.cmp(&right.0));
        sessions
    }

    fn save(&self) -> Result<()> {
        let sessions = self
            .inner
            .sessions
            .lock()
            .map_err(|_| anyhow::anyhow!("chat state lock poisoned"))?;
        let database = SessionDatabase {
            sessions: sessions.clone(),
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
        })?;
        Ok(())
    }
}

fn default_cwd() -> String {
    ".".to_string()
}
