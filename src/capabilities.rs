use serde::{Deserialize, Serialize};

use crate::approvals;
use crate::config::Config;
use crate::runner::{AgentKind, AgentRequest};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentCapability {
    pub agent: AgentKind,
    pub enabled: bool,
    pub profile: String,
    pub bin: String,
    pub permission_mode: Option<String>,
    pub approval_required: bool,
    pub commands: Vec<String>,
}

pub fn capabilities(config: &Config) -> Vec<AgentCapability> {
    [AgentKind::Codex, AgentKind::Claude, AgentKind::Hermes]
        .into_iter()
        .map(|agent| {
            let config_agent = config.agent(agent);
            let approval_required = approvals::approval_reason(
                config,
                &AgentRequest::prompt(agent, "capability probe"),
            )
            .is_some();
            AgentCapability {
                agent,
                enabled: config_agent.enabled,
                profile: config.effective_agent_profile(agent),
                bin: config_agent.bin.clone(),
                permission_mode: config_agent.permission_mode.clone(),
                approval_required,
                commands: commands(agent),
            }
        })
        .collect()
}

pub fn render_capabilities(config: &Config) -> String {
    let lines = capabilities(config)
        .into_iter()
        .map(|capability| {
            let status = if capability.enabled {
                "enabled"
            } else {
                "disabled"
            };
            let approval = if capability.approval_required {
                " approval"
            } else {
                ""
            };
            let permission = capability
                .permission_mode
                .as_deref()
                .filter(|value| !value.is_empty())
                .map(|value| format!(" permission:{value}"))
                .unwrap_or_default();
            format!(
                "- {} {status}{approval}{permission}\n  profile: {}\n  bin: {}\n  commands: {}",
                capability.agent,
                capability.profile,
                capability.bin,
                capability.commands.join(", ")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("Agents\n{lines}")
}

fn commands(agent: AgentKind) -> Vec<String> {
    match agent {
        AgentKind::Codex => vec![
            "/codex <prompt>".to_string(),
            "/codex-resume <session> <prompt>".to_string(),
            "/wiki <prompt>".to_string(),
        ],
        AgentKind::Claude => vec![
            "/claude <prompt>".to_string(),
            "/claude-resume <session> <prompt>".to_string(),
            "/claude-wiki <prompt>".to_string(),
        ],
        AgentKind::Hermes => vec![
            "/hermes <prompt>".to_string(),
            "/hermes-resume <session> <prompt>".to_string(),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hermes_is_reported_disabled_by_default() {
        let config = Config::template();
        let caps = capabilities(&config);
        let hermes = caps
            .into_iter()
            .find(|capability| capability.agent == AgentKind::Hermes)
            .unwrap();
        assert!(!hermes.enabled);
    }
}
