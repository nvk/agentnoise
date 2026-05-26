use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::events::{EventSummary, summarize_event_log};
use crate::jobs::JobStore;
use crate::queue::{JobQueue, QueueCounts};
use crate::runtime;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusReport {
    pub engine_running: bool,
    pub transport_running: bool,
    pub worker_running: bool,
    pub runtime: Option<runtime::RuntimeStatus>,
    pub agent_launcher: String,
    pub groups: Vec<String>,
    pub queue: QueueCounts,
    pub event_summary: EventSummary,
    pub recent_jobs: usize,
    pub active_jobs: usize,
    pub key_store: String,
    /// v2 migration: see [`crate::darkmatter_app::DarkmatterEngine`] for the
    /// embedded engine state. Live engine introspection (relay health, account
    /// running state) needs a tokio runtime and is a follow-up to this phase.
    pub darkmatter_engine: String,
}

pub fn status_report(config: &Config) -> Result<StatusReport> {
    let jobs = JobStore::open(&config.resolved_jobs_path(), &config.resolved_log_dir())?;
    let recent_jobs = jobs.recent(20);
    let active_jobs = recent_jobs
        .iter()
        .filter(|job| job.status.is_active())
        .count();
    let key_store = if config.darkmatter.dev_burner_nsec {
        "file-backed dev burner identity".to_string()
    } else {
        "keychain (via marmot-account KeychainSecretStore)".to_string()
    };
    let darkmatter_engine = "embedded (use `agentnoise darkmatter probe` for liveness)".to_string();

    Ok(StatusReport {
        engine_running: runtime::engine_is_running(config)?,
        transport_running: runtime::role_is_running(config, runtime::RuntimeRole::Transport)?,
        worker_running: runtime::role_is_running(config, runtime::RuntimeRole::Worker)?,
        runtime: runtime::read_status(config)?,
        agent_launcher: config.runner.launcher.to_string(),
        groups: config.darkmatter.control_group_ids(),
        queue: JobQueue::open(config.resolved_queue_path())?.counts()?,
        event_summary: summarize_event_log(&config.resolved_event_log_path())?,
        recent_jobs: recent_jobs.len(),
        active_jobs,
        key_store,
        darkmatter_engine,
    })
}

pub fn render_status_report(config_path: &Path, config: &Config) -> String {
    match status_report(config) {
        Ok(report) => {
            let mut lines = vec![
                "agentnoise status".to_string(),
                format!("config: {}", config_path.display()),
                format!(
                    "engine: {}",
                    if report.engine_running {
                        "running"
                    } else {
                        "stopped"
                    }
                ),
                format!(
                    "transport: {}",
                    if report.transport_running {
                        "running"
                    } else {
                        "stopped"
                    }
                ),
                format!(
                    "worker: {}",
                    if report.worker_running {
                        "running"
                    } else {
                        "stopped"
                    }
                ),
                format!("agent launcher: {}", report.agent_launcher),
                format!("groups: {}", report.groups.len()),
                format!(
                    "queue: {} queued, {} claimed, {} running, {} failed",
                    report.queue.queued,
                    report.queue.claimed,
                    report.queue.running,
                    report.queue.failed
                ),
                format!(
                    "jobs: {} recent, {} active",
                    report.recent_jobs, report.active_jobs
                ),
                format!(
                    "events: {} inbound, {} outbound, {} queued, {} outbound failed",
                    report.event_summary.inbound,
                    report.event_summary.outbound,
                    report.event_summary.outbound_enqueued,
                    report.event_summary.failed_outbound
                ),
                format!("identity store: {}", report.key_store),
                format!("darkmatter: {}", report.darkmatter_engine),
            ];
            if let Some(runtime) = report.runtime {
                lines.push(format!("pid: {}", runtime.pid));
                lines.push(format!("started: {}", runtime.started_at));
                if let Some(pairing) = runtime.pairing {
                    lines.push(format!("pairing: {}s PIN window", pairing.pin_seconds));
                    lines.push(format!("pairing npub: {}", pairing.npub));
                    if let Some(pin) = pairing.current_pin {
                        match pin.remaining_seconds() {
                            Some(seconds) => lines.push(format!(
                                "pairing PIN: {} (expires in {}s)",
                                pin.code, seconds
                            )),
                            None => lines.push(format!(
                                "pairing PIN: {} (expires at {})",
                                pin.code, pin.expires_at
                            )),
                        }
                    }
                }
            }
            lines.join("\n")
        }
        Err(error) => format!("agentnoise status\nError: {error:#}"),
    }
}
