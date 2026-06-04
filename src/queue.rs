use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use crate::runner::AgentRequest;

const SCHEMA_VERSION: i64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum QueueStatus {
    Queued,
    Claimed,
    Running,
    Succeeded,
    Failed,
}

impl QueueStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Claimed => "claimed",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "claimed" => Self::Claimed,
            "running" => Self::Running,
            "succeeded" => Self::Succeeded,
            "failed" => Self::Failed,
            _ => Self::Queued,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QueuedJob {
    pub id: String,
    pub status: QueueStatus,
    pub request: AgentRequest,
    pub source_group_id: String,
    pub reply_group_id: String,
    pub source_event_id: String,
    pub created_at: String,
    pub claimed_by: Option<String>,
    pub claimed_at: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub log_path: Option<String>,
    pub summary: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct QueueCounts {
    pub queued: u64,
    pub claimed: u64,
    pub running: u64,
    pub succeeded: u64,
    pub failed: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnqueueOutcome {
    pub id: String,
    pub inserted: bool,
}

#[derive(Debug, Clone)]
pub struct JobQueue {
    path: PathBuf,
}

impl JobQueue {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating queue dir {}", parent.display()))?;
        }
        let queue = Self { path };
        queue.with_connection(|conn| {
            conn.pragma_update(None, "journal_mode", "WAL")
                .context("enabling queue WAL mode")?;
            conn.pragma_update(None, "foreign_keys", "ON")
                .context("enabling queue foreign keys")?;
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)
                .context("setting queue schema version")?;
            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS jobs (
                    id TEXT PRIMARY KEY,
                    status TEXT NOT NULL,
                    agent TEXT NOT NULL,
                    request_json TEXT NOT NULL,
                    source_group_id TEXT NOT NULL,
                    reply_group_id TEXT NOT NULL,
                    source_event_id TEXT NOT NULL UNIQUE,
                    created_at TEXT NOT NULL,
                    claimed_by TEXT,
                    claimed_at TEXT,
                    started_at TEXT,
                    finished_at TEXT,
                    log_path TEXT,
                    summary TEXT,
                    error TEXT
                );

                CREATE INDEX IF NOT EXISTS jobs_status_created_idx
                    ON jobs(status, created_at);

                CREATE TABLE IF NOT EXISTS outbox (
                    id TEXT PRIMARY KEY,
                    job_id TEXT,
                    group_id TEXT NOT NULL,
                    text TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    sent_at TEXT,
                    error TEXT,
                    FOREIGN KEY(job_id) REFERENCES jobs(id)
                );
                "#,
            )
            .context("creating queue schema")?;
            Ok(())
        })?;
        Ok(queue)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn enqueue(
        &self,
        request: &AgentRequest,
        source_group_id: &str,
        reply_group_id: &str,
        source_event_id: &str,
    ) -> Result<EnqueueOutcome> {
        let id = format!("q-{}", short_uuid());
        let now = now_rfc3339();
        let request_json = serde_json::to_string(request).context("serializing queued request")?;
        let source_event_id = source_event_id.trim();

        self.with_connection(|conn| {
            let inserted = conn
                .execute(
                    r#"
                    INSERT OR IGNORE INTO jobs (
                        id, status, agent, request_json, source_group_id, reply_group_id,
                        source_event_id, created_at
                    )
                    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                    "#,
                    params![
                        id,
                        QueueStatus::Queued.as_str(),
                        request.agent.to_string(),
                        request_json,
                        source_group_id,
                        reply_group_id,
                        source_event_id,
                        now,
                    ],
                )
                .context("enqueueing job")?
                == 1;
            let resolved_id = conn
                .query_row(
                    "SELECT id FROM jobs WHERE source_event_id = ?1",
                    params![source_event_id],
                    |row| row.get::<_, String>(0),
                )
                .context("reading queued job id")?;
            Ok(EnqueueOutcome {
                id: resolved_id,
                inserted,
            })
        })
    }

    pub fn claim_next(&self, worker_id: &str) -> Result<Option<QueuedJob>> {
        let now = now_rfc3339();
        self.with_connection(|conn| {
            let tx = conn
                .unchecked_transaction()
                .context("starting queue claim transaction")?;
            let selected = tx
                .query_row(
                    r#"
                    SELECT id FROM jobs
                    WHERE status = ?1
                    ORDER BY created_at ASC
                    LIMIT 1
                    "#,
                    params![QueueStatus::Queued.as_str()],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .context("selecting next queued job")?;
            let Some(id) = selected else {
                tx.commit().context("committing empty queue claim")?;
                return Ok(None);
            };

            let changed = tx
                .execute(
                    r#"
                    UPDATE jobs
                    SET status = ?1, claimed_by = ?2, claimed_at = ?3, error = NULL
                    WHERE id = ?4 AND status = ?5
                    "#,
                    params![
                        QueueStatus::Claimed.as_str(),
                        worker_id,
                        now,
                        id,
                        QueueStatus::Queued.as_str(),
                    ],
                )
                .context("claiming queued job")?;
            if changed == 0 {
                tx.commit().context("committing lost queue claim")?;
                return Ok(None);
            }
            let job = select_job(&tx, &id)?;
            tx.commit().context("committing queue claim")?;
            Ok(Some(job))
        })
    }

    pub fn mark_running(&self, id: &str) -> Result<()> {
        let now = now_rfc3339();
        self.with_connection(|conn| {
            conn.execute(
                r#"
                UPDATE jobs
                SET status = ?1, started_at = COALESCE(started_at, ?2), error = NULL
                WHERE id = ?3
                "#,
                params![QueueStatus::Running.as_str(), now, id],
            )
            .with_context(|| format!("marking queued job {id} running"))?;
            Ok(())
        })
    }

    pub fn mark_succeeded(&self, id: &str, summary: &str, log_path: Option<&Path>) -> Result<()> {
        self.mark_finished(id, QueueStatus::Succeeded, summary, None, log_path)
    }

    pub fn mark_failed(&self, id: &str, error: &str, log_path: Option<&Path>) -> Result<()> {
        self.mark_finished(id, QueueStatus::Failed, "", Some(error), log_path)
    }

    pub fn counts(&self) -> Result<QueueCounts> {
        self.with_connection(|conn| {
            let mut counts = QueueCounts::default();
            let mut statement = conn
                .prepare("SELECT status, COUNT(*) FROM jobs GROUP BY status")
                .context("preparing queue counts")?;
            let rows = statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
                })
                .context("reading queue counts")?;
            for row in rows {
                let (status, count) = row.context("reading queue count row")?;
                match QueueStatus::parse(&status) {
                    QueueStatus::Queued => counts.queued = count,
                    QueueStatus::Claimed => counts.claimed = count,
                    QueueStatus::Running => counts.running = count,
                    QueueStatus::Succeeded => counts.succeeded = count,
                    QueueStatus::Failed => counts.failed = count,
                }
            }
            Ok(counts)
        })
    }

    pub fn active_for_reply_group(&self, reply_group_id: &str) -> Result<Option<QueuedJob>> {
        self.with_connection(|conn| {
            conn.query_row(
                r#"
                SELECT
                    id, status, request_json, source_group_id, reply_group_id, source_event_id,
                    created_at, claimed_by, claimed_at, started_at, finished_at, log_path, summary, error
                FROM jobs
                WHERE reply_group_id = ?1
                  AND status IN (?2, ?3, ?4)
                ORDER BY
                  CASE status
                    WHEN ?4 THEN 0
                    WHEN ?3 THEN 1
                    ELSE 2
                  END,
                  created_at DESC
                LIMIT 1
                "#,
                params![
                    reply_group_id,
                    QueueStatus::Queued.as_str(),
                    QueueStatus::Claimed.as_str(),
                    QueueStatus::Running.as_str(),
                ],
                row_to_job,
            )
            .optional()
            .with_context(|| format!("reading active job for reply group {reply_group_id}"))
        })
    }

    fn mark_finished(
        &self,
        id: &str,
        status: QueueStatus,
        summary: &str,
        error: Option<&str>,
        log_path: Option<&Path>,
    ) -> Result<()> {
        let now = now_rfc3339();
        let log_path = log_path.map(|path| path.display().to_string());
        self.with_connection(|conn| {
            conn.execute(
                r#"
                UPDATE jobs
                SET status = ?1, finished_at = ?2, summary = ?3, error = ?4, log_path = ?5
                WHERE id = ?6
                "#,
                params![status.as_str(), now, summary, error, log_path, id],
            )
            .with_context(|| format!("marking queued job {id} finished"))?;
            Ok(())
        })
    }

    fn with_connection<T>(&self, f: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        let mut conn = Connection::open(&self.path)
            .with_context(|| format!("opening queue {}", self.path.display()))?;
        conn.busy_timeout(Duration::from_secs(5))
            .context("setting queue busy timeout")?;
        f(&mut conn)
    }
}

fn select_job(conn: &Connection, id: &str) -> Result<QueuedJob> {
    conn.query_row(
        r#"
        SELECT
            id, status, request_json, source_group_id, reply_group_id, source_event_id,
            created_at, claimed_by, claimed_at, started_at, finished_at, log_path, summary, error
        FROM jobs
        WHERE id = ?1
        "#,
        params![id],
        row_to_job,
    )
    .with_context(|| format!("reading queued job {id}"))
}

fn row_to_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<QueuedJob> {
    let request_json: String = row.get(2)?;
    let request = serde_json::from_str(&request_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            request_json.len(),
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })?;
    Ok(QueuedJob {
        id: row.get(0)?,
        status: QueueStatus::parse(row.get::<_, String>(1)?.as_str()),
        request,
        source_group_id: row.get(3)?,
        reply_group_id: row.get(4)?,
        source_event_id: row.get(5)?,
        created_at: row.get(6)?,
        claimed_by: row.get(7)?,
        claimed_at: row.get(8)?,
        started_at: row.get(9)?,
        finished_at: row.get(10)?,
        log_path: row.get(11)?,
        summary: row.get(12)?,
        error: row.get(13)?,
    })
}

fn short_uuid() -> String {
    Uuid::new_v4()
        .simple()
        .to_string()
        .chars()
        .take(8)
        .collect()
}

fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "unknown-time".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::AgentKind;

    fn request() -> AgentRequest {
        AgentRequest::new(AgentKind::Codex, "sandbox", "what is 1+1?")
    }

    #[test]
    fn enqueue_deduplicates_source_events() {
        let temp = tempfile::tempdir().unwrap();
        let queue = JobQueue::open(temp.path().join("queue.sqlite3")).unwrap();

        let first = queue
            .enqueue(&request(), "inbox", "reply", "event-1")
            .unwrap();
        let second = queue
            .enqueue(&request(), "inbox", "reply", "event-1")
            .unwrap();

        assert!(first.inserted);
        assert!(!second.inserted);
        assert_eq!(first.id, second.id);
        assert_eq!(queue.counts().unwrap().queued, 1);
    }

    #[test]
    fn claim_next_claims_only_once() {
        let temp = tempfile::tempdir().unwrap();
        let queue = JobQueue::open(temp.path().join("queue.sqlite3")).unwrap();
        queue
            .enqueue(&request(), "inbox", "reply", "event-1")
            .unwrap();

        let job = queue.claim_next("worker-a").unwrap().unwrap();
        assert_eq!(job.status, QueueStatus::Claimed);
        assert_eq!(job.claimed_by.as_deref(), Some("worker-a"));
        assert!(queue.claim_next("worker-b").unwrap().is_none());

        let counts = queue.counts().unwrap();
        assert_eq!(counts.queued, 0);
        assert_eq!(counts.claimed, 1);
    }
}
