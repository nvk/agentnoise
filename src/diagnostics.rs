use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::events::{EventSummary, summarize_event_log};
use crate::jobs::JobStore;
use crate::runtime;
use crate::whitenoise_cli;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusReport {
    pub engine_running: bool,
    pub runtime: Option<runtime::RuntimeStatus>,
    pub agent_launcher: String,
    pub groups: Vec<String>,
    pub event_summary: EventSummary,
    pub recent_jobs: usize,
    pub active_jobs: usize,
    pub key_store: String,
    pub whitenoise_daemon: String,
}

pub fn status_report(config: &Config) -> Result<StatusReport> {
    let jobs = JobStore::open(&config.resolved_jobs_path(), &config.resolved_log_dir())?;
    let recent_jobs = jobs.recent(20);
    let active_jobs = recent_jobs
        .iter()
        .filter(|job| job.status.is_active())
        .count();
    let key_store = if config.whitenoise.dev_burner_nsec {
        "dev-burner".to_string()
    } else if config.whitenoise.use_keychain_nsec {
        match config.secret_store().nsec_status() {
            Ok(true) => "keychain:present".to_string(),
            Ok(false) => "keychain:missing".to_string(),
            Err(error) => format!("keychain:error:{error:#}"),
        }
    } else {
        "none".to_string()
    };
    let wn = whitenoise_cli::resolve_wn(&config.whitenoise.wn_bin);
    let whitenoise_daemon = match whitenoise_cli::daemon_status_with_socket(
        &wn,
        config.whitenoise.resolved_socket().as_deref(),
    ) {
        Ok(status) => status,
        Err(error) => format!("unavailable: {error:#}"),
    };

    Ok(StatusReport {
        engine_running: runtime::engine_is_running(config)?,
        runtime: runtime::read_status(config)?,
        agent_launcher: config.runner.launcher.to_string(),
        groups: config.whitenoise.control_group_ids(),
        event_summary: summarize_event_log(&config.resolved_event_log_path())?,
        recent_jobs: recent_jobs.len(),
        active_jobs,
        key_store,
        whitenoise_daemon,
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
                format!("agent launcher: {}", report.agent_launcher),
                format!("groups: {}", report.groups.len()),
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
                format!("wn daemon: {}", report.whitenoise_daemon),
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
