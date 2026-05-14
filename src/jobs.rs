use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use crate::runner::{AgentKind, AgentRequest};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum JobStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    CancelRequested,
    Cancelled,
    Interrupted,
}

impl JobStatus {
    pub fn is_active(self) -> bool {
        matches!(
            self,
            JobStatus::Pending | JobStatus::Running | JobStatus::CancelRequested
        )
    }
}

impl fmt::Display for JobStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::CancelRequested => "cancel-requested",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        };
        formatter.write_str(value)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRecord {
    pub id: String,
    pub agent: AgentKind,
    pub repo_alias: Option<String>,
    pub prompt_preview: String,
    pub status: JobStatus,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub exit_code: Option<i32>,
    pub pid: Option<u32>,
    pub log_path: PathBuf,
    pub summary: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct JobDatabase {
    jobs: Vec<JobRecord>,
}

#[derive(Clone)]
pub struct JobStore {
    inner: Arc<JobStoreInner>,
}

struct JobStoreInner {
    path: PathBuf,
    log_dir: PathBuf,
    jobs: Mutex<Vec<JobRecord>>,
}

impl JobStore {
    pub fn open(path: &Path, log_dir: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        fs::create_dir_all(log_dir).with_context(|| format!("creating {}", log_dir.display()))?;

        let jobs = if path.exists() {
            let text =
                fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
            serde_json::from_str::<JobDatabase>(&text)
                .with_context(|| format!("parsing {}", path.display()))?
                .jobs
        } else {
            Vec::new()
        };

        let store = Self {
            inner: Arc::new(JobStoreInner {
                path: path.to_path_buf(),
                log_dir: log_dir.to_path_buf(),
                jobs: Mutex::new(jobs),
            }),
        };
        store.with_jobs(|_| ())?;
        Ok(store)
    }

    pub fn recover_interrupted_jobs(&self) -> Result<usize> {
        let mut recovered = 0;
        self.with_jobs(|jobs| {
            for job in jobs {
                if job.status.is_active() {
                    job.status = JobStatus::Interrupted;
                    job.finished_at = Some(now_string());
                    job.pid = None;
                    job.summary = Some("agentnoise restarted before this job finished".to_string());
                    recovered += 1;
                }
            }
        })?;
        Ok(recovered)
    }

    pub fn create(&self, request: &AgentRequest) -> Result<JobRecord> {
        let id = short_id();
        let log_path = self.inner.log_dir.join(format!("{id}.log"));
        let record = JobRecord {
            id,
            agent: request.agent,
            repo_alias: request.repo_alias.clone(),
            prompt_preview: preview(&request.prompt, 160),
            status: JobStatus::Pending,
            started_at: now_string(),
            finished_at: None,
            exit_code: None,
            pid: None,
            log_path,
            summary: None,
        };

        self.with_jobs(|jobs| jobs.push(record.clone()))?;
        Ok(record)
    }

    pub fn mark_running(&self, id: &str, pid: u32) -> Result<()> {
        self.update(id, |job| {
            job.status = JobStatus::Running;
            job.pid = Some(pid);
        })
    }

    pub fn finish(&self, id: &str, exit_code: Option<i32>, summary: String) -> Result<JobRecord> {
        let mut updated = None;
        self.update(id, |job| {
            let was_cancelled = job.status == JobStatus::CancelRequested;
            job.status = if was_cancelled {
                JobStatus::Cancelled
            } else if exit_code == Some(0) {
                JobStatus::Succeeded
            } else {
                JobStatus::Failed
            };
            job.finished_at = Some(now_string());
            job.exit_code = exit_code;
            job.pid = None;
            job.summary = Some(summary.clone());
            updated = Some(job.clone());
        })?;
        updated.context("updated job missing")
    }

    pub fn mark_failed(&self, id: &str, summary: String) -> Result<JobRecord> {
        let mut updated = None;
        self.update(id, |job| {
            job.status = JobStatus::Failed;
            job.finished_at = Some(now_string());
            job.pid = None;
            job.summary = Some(summary.clone());
            updated = Some(job.clone());
        })?;
        updated.context("updated job missing")
    }

    pub fn request_cancel(&self, id: &str) -> Result<Option<u32>> {
        let mut pid = None;
        self.update(id, |job| {
            if job.status.is_active() {
                job.status = JobStatus::CancelRequested;
                pid = job.pid;
            }
        })?;
        Ok(pid)
    }

    pub fn recent(&self, count: usize) -> Vec<JobRecord> {
        let Ok(jobs) = self.inner.jobs.lock() else {
            return Vec::new();
        };
        jobs.iter().rev().take(count).cloned().collect()
    }

    pub fn tail(&self, id: &str, max_bytes: usize) -> Result<Option<String>> {
        let Some(job) = self.get(id) else {
            return Ok(None);
        };
        let bytes = fs::read(&job.log_path)
            .with_context(|| format!("reading {}", job.log_path.display()))?;
        let start = bytes.len().saturating_sub(max_bytes);
        Ok(Some(String::from_utf8_lossy(&bytes[start..]).to_string()))
    }

    pub fn get(&self, id: &str) -> Option<JobRecord> {
        let Ok(jobs) = self.inner.jobs.lock() else {
            return None;
        };
        jobs.iter().find(|job| job.id == id).cloned()
    }

    fn update(&self, id: &str, update: impl FnOnce(&mut JobRecord)) -> Result<()> {
        self.with_jobs(|jobs| {
            let Some(job) = jobs.iter_mut().find(|job| job.id == id) else {
                bail!("no such job {id}");
            };
            update(job);
            Ok(())
        })?
    }

    fn with_jobs<T>(&self, action: impl FnOnce(&mut Vec<JobRecord>) -> T) -> Result<T> {
        let mut jobs = self
            .inner
            .jobs
            .lock()
            .map_err(|_| anyhow::anyhow!("job store lock poisoned"))?;
        let result = action(&mut jobs);
        self.save_locked(&jobs)?;
        Ok(result)
    }

    fn save_locked(&self, jobs: &[JobRecord]) -> Result<()> {
        let database = JobDatabase {
            jobs: jobs.to_vec(),
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

fn short_id() -> String {
    let uuid = Uuid::new_v4().simple().to_string();
    format!("an-{}", &uuid[..8])
}

fn preview(text: &str, max: usize) -> String {
    let mut value = text.chars().take(max).collect::<String>();
    if text.chars().count() > max {
        value.push_str("...");
    }
    value
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
    fn open_does_not_mark_active_jobs_interrupted() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("jobs.json");
        let log_dir = temp.path().join("logs");
        let store = JobStore::open(&path, &log_dir).unwrap();
        let request = AgentRequest::new(AgentKind::Codex, "work", "hello");
        let job = store.create(&request).unwrap();
        store.mark_running(&job.id, 1234).unwrap();

        let reopened = JobStore::open(&path, &log_dir).unwrap();
        let reopened_job = reopened.get(&job.id).unwrap();
        assert_eq!(reopened_job.status, JobStatus::Running);
        assert_eq!(reopened_job.pid, Some(1234));
    }

    #[test]
    fn daemon_recovery_marks_active_jobs_interrupted() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("jobs.json");
        let log_dir = temp.path().join("logs");
        let store = JobStore::open(&path, &log_dir).unwrap();
        let request = AgentRequest::new(AgentKind::Codex, "work", "hello");
        let job = store.create(&request).unwrap();
        store.mark_running(&job.id, 1234).unwrap();

        let recovered = store.recover_interrupted_jobs().unwrap();
        let recovered_job = store.get(&job.id).unwrap();

        assert_eq!(recovered, 1);
        assert_eq!(recovered_job.status, JobStatus::Interrupted);
        assert_eq!(recovered_job.pid, None);
        assert!(recovered_job.finished_at.is_some());
    }
}
