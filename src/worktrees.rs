use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use crate::config::Config;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorktreeRecord {
    pub id: String,
    pub name: String,
    pub repo_alias: String,
    pub branch: String,
    pub path: PathBuf,
    pub created_at: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct WorktreeDatabase {
    worktrees: Vec<WorktreeRecord>,
}

#[derive(Clone)]
pub struct WorktreeStore {
    inner: Arc<WorktreeStoreInner>,
}

struct WorktreeStoreInner {
    path: PathBuf,
    worktrees: Mutex<Vec<WorktreeRecord>>,
}

impl WorktreeStore {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        let worktrees = if path.exists() {
            let text =
                fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
            serde_json::from_str::<WorktreeDatabase>(&text)
                .with_context(|| format!("parsing {}", path.display()))?
                .worktrees
        } else {
            Vec::new()
        };
        let store = Self {
            inner: Arc::new(WorktreeStoreInner {
                path: path.to_path_buf(),
                worktrees: Mutex::new(worktrees),
            }),
        };
        store.save()?;
        Ok(store)
    }

    pub fn list(&self, repo_alias: Option<&str>) -> Vec<WorktreeRecord> {
        let Ok(worktrees) = self.inner.worktrees.lock() else {
            return Vec::new();
        };
        let mut records = worktrees
            .iter()
            .filter(|record| repo_alias.is_none_or(|alias| record.repo_alias == alias))
            .cloned()
            .collect::<Vec<_>>();
        records.sort_by(|left, right| left.name.cmp(&right.name));
        records
    }

    pub fn find(&self, repo_alias: &str, name: &str) -> Option<WorktreeRecord> {
        let name = sanitize_name(name).ok()?;
        let Ok(worktrees) = self.inner.worktrees.lock() else {
            return None;
        };
        worktrees
            .iter()
            .find(|record| record.repo_alias == repo_alias && record.name == name)
            .cloned()
    }

    pub fn create(&self, config: &Config, repo_alias: &str, name: &str) -> Result<WorktreeRecord> {
        let name = sanitize_name(name)?;
        if let Some(existing) = self.find(repo_alias, &name)
            && existing.path.is_dir()
        {
            return Ok(existing);
        }
        let repo_root = repo_root(config, repo_alias)?;
        ensure_git_repo(&repo_root)?;
        let branch = format!("agentnoise/{name}");
        let path = config.resolved_worktree_dir().join(repo_alias).join(&name);
        if path.exists() {
            bail!("worktree path already exists: {}", path.display());
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }

        let status = Command::new("git")
            .arg("-C")
            .arg(&repo_root)
            .arg("worktree")
            .arg("add")
            .arg("-B")
            .arg(&branch)
            .arg(&path)
            .status()
            .with_context(|| format!("creating git worktree {}", path.display()))?;
        if !status.success() {
            bail!("git worktree add exited with {status}");
        }

        let record = WorktreeRecord {
            id: short_id(),
            name,
            repo_alias: repo_alias.to_string(),
            branch,
            path: path
                .canonicalize()
                .with_context(|| format!("canonicalizing {}", path.display()))?,
            created_at: now_string(),
        };
        {
            let mut worktrees = self
                .inner
                .worktrees
                .lock()
                .map_err(|_| anyhow::anyhow!("worktree store lock poisoned"))?;
            worktrees.retain(|existing| {
                !(existing.repo_alias == record.repo_alias && existing.name == record.name)
            });
            worktrees.push(record.clone());
        }
        self.save()?;
        Ok(record)
    }

    pub fn remove(&self, config: &Config, repo_alias: &str, name: &str) -> Result<WorktreeRecord> {
        let record = self
            .find(repo_alias, name)
            .with_context(|| format!("unknown worktree: {name}"))?;
        let repo_root = repo_root(config, repo_alias)?;
        let status = Command::new("git")
            .arg("-C")
            .arg(&repo_root)
            .arg("worktree")
            .arg("remove")
            .arg(&record.path)
            .status()
            .with_context(|| format!("removing git worktree {}", record.path.display()))?;
        if !status.success() {
            bail!("git worktree remove exited with {status}");
        }
        {
            let mut worktrees = self
                .inner
                .worktrees
                .lock()
                .map_err(|_| anyhow::anyhow!("worktree store lock poisoned"))?;
            worktrees.retain(|existing| existing.id != record.id);
        }
        self.save()?;
        Ok(record)
    }

    fn save(&self) -> Result<()> {
        let worktrees = self
            .inner
            .worktrees
            .lock()
            .map_err(|_| anyhow::anyhow!("worktree store lock poisoned"))?;
        let database = WorktreeDatabase {
            worktrees: worktrees.clone(),
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

pub fn render_worktrees(records: &[WorktreeRecord], active: Option<&str>) -> String {
    if records.is_empty() {
        return "No worktrees yet. Send /worktree new <name>.".to_string();
    }
    let lines = records
        .iter()
        .map(|record| {
            let marker = if active == Some(record.name.as_str()) {
                "*"
            } else {
                "-"
            };
            format!(
                "{marker} {} {}\n  {}",
                record.name,
                record.branch,
                record.path.display()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("Worktrees\n{lines}")
}

fn repo_root(config: &Config, repo_alias: &str) -> Result<PathBuf> {
    let Some(path) = config.repo_path(repo_alias) else {
        bail!("unknown repo alias: {repo_alias}");
    };
    if !path.is_dir() {
        bail!("repo path is not a directory: {}", path.display());
    }
    Ok(path)
}

fn ensure_git_repo(repo_root: &Path) -> Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("rev-parse")
        .arg("--show-toplevel")
        .output()
        .with_context(|| format!("checking git repo {}", repo_root.display()))?;
    if !output.status.success() {
        bail!("not a git repo: {}", repo_root.display());
    }
    Ok(())
}

fn sanitize_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        bail!("worktree name cannot be empty");
    }
    let cleaned = name
        .chars()
        .filter_map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                Some(ch)
            } else if ch.is_whitespace() || ch == '/' {
                Some('-')
            } else {
                None
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .chars()
        .take(48)
        .collect::<String>();
    if cleaned.is_empty() {
        bail!("worktree name contains no usable characters");
    }
    Ok(cleaned)
}

fn short_id() -> String {
    let uuid = Uuid::new_v4().simple().to_string();
    format!("wt-{}", &uuid[..8])
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
    fn sanitizes_worktree_names() {
        assert_eq!(sanitize_name("fix ui").unwrap(), "fix-ui");
        assert_eq!(sanitize_name("../fix").unwrap(), "fix");
        assert!(sanitize_name("!!!").is_err());
    }
}
