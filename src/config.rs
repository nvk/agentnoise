use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::paths::{default_config_path, default_data_dir, default_log_dir, expand_tilde};
use crate::runner::AgentKind;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub whitenoise: WhitenoiseConfig,
    pub runner: RunnerConfig,
    pub agents: AgentsConfig,
    pub repos: Vec<RepoConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhitenoiseConfig {
    pub group_id: String,
    #[serde(default)]
    pub group_ids: Vec<String>,
    #[serde(default)]
    pub account: Option<String>,
    #[serde(default = "default_wn_bin")]
    pub wn_bin: String,
    #[serde(default)]
    pub socket: Option<String>,
    #[serde(default)]
    pub transport: WhitenoiseTransport,
    #[serde(default)]
    pub use_keychain_nsec: bool,
    #[serde(default)]
    pub dev_burner_nsec: bool,
    #[serde(default)]
    pub dev_burner_nsec_file: Option<String>,
    #[serde(default)]
    pub login_relay: Option<String>,
    #[serde(default = "default_pairing_relays")]
    pub pairing_relays: Vec<String>,
    #[serde(default = "default_message_relays")]
    pub message_relays: Vec<String>,
    #[serde(default = "default_keychain_service")]
    pub keychain_service: String,
    #[serde(default = "default_keychain_item")]
    pub keychain_item: String,
    #[serde(default = "default_subscribe_limit")]
    pub subscribe_limit: u32,
    #[serde(default = "default_max_message_chars")]
    pub max_message_chars: usize,
    #[serde(default = "default_true")]
    pub ignore_initial_messages: bool,
    #[serde(default)]
    pub allowed_senders: Vec<String>,
    #[serde(default = "default_true")]
    pub require_pairing_pin: bool,
    #[serde(default = "default_pairing_pin_seconds")]
    pub pairing_pin_seconds: u64,
    #[serde(default)]
    pub bot_sender: Option<String>,
    #[serde(default)]
    pub bot_npub: Option<String>,
    #[serde(default = "default_profile_name")]
    pub profile_name: String,
    #[serde(default = "default_profile_display_name")]
    pub profile_display_name: String,
    #[serde(default = "default_profile_about")]
    pub profile_about: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerConfig {
    #[serde(default = "default_bondage_bin")]
    pub bondage_bin: String,
    #[serde(default = "default_bondage_conf")]
    pub bondage_conf: String,
    #[serde(default = "default_data_dir_string")]
    pub data_dir: String,
    #[serde(default = "default_log_dir_string")]
    pub log_dir: String,
    #[serde(default = "default_max_prompt_chars")]
    pub max_prompt_chars: usize,
    #[serde(default = "default_max_output_chars")]
    pub max_output_chars: usize,
    #[serde(default = "default_progress_interval_seconds")]
    pub progress_interval_seconds: u64,
    #[serde(default = "default_approval_ttl_seconds")]
    pub approval_ttl_seconds: u64,
    #[serde(default = "default_worktree_dir_string")]
    pub worktree_dir: String,
    #[serde(default)]
    pub allow_generic_agent_profiles: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum WhitenoiseTransport {
    #[default]
    Cli,
    Socket,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentsConfig {
    pub codex: AgentConfig,
    pub claude: AgentConfig,
    #[serde(default = "default_hermes_agent_config")]
    pub hermes: AgentConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub profile: String,
    pub bin: String,
    #[serde(default)]
    pub permission_mode: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoConfig {
    pub alias: String,
    pub path: String,
}

impl Config {
    pub fn path_or_default(path: Option<PathBuf>) -> PathBuf {
        path.unwrap_or_else(default_config_path)
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        let config: Self =
            toml::from_str(&text).with_context(|| format!("parsing config {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn load_or_template(path: &Path) -> Result<Self> {
        if path.exists() {
            Self::load(path)
        } else {
            Ok(Self::template())
        }
    }

    pub fn write_template(path: &Path, force: bool) -> Result<()> {
        if path.exists() && !force {
            bail!(
                "{} already exists; use --force to overwrite",
                path.display()
            );
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        fs::write(path, Self::template_toml()?)
            .with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        let text = toml::to_string_pretty(self).context("serializing config")?;
        fs::write(path, text).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    pub fn template_toml() -> Result<String> {
        toml::to_string_pretty(&Self::template()).context("serializing template config")
    }

    pub fn template() -> Self {
        let repo_path = std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .display()
            .to_string();

        Self {
            whitenoise: WhitenoiseConfig {
                group_id: String::new(),
                group_ids: Vec::new(),
                account: None,
                wn_bin: default_wn_bin(),
                socket: None,
                transport: WhitenoiseTransport::Cli,
                use_keychain_nsec: false,
                dev_burner_nsec: false,
                dev_burner_nsec_file: None,
                login_relay: None,
                pairing_relays: default_pairing_relays(),
                message_relays: default_message_relays(),
                keychain_service: default_keychain_service(),
                keychain_item: default_keychain_item(),
                subscribe_limit: default_subscribe_limit(),
                max_message_chars: default_max_message_chars(),
                ignore_initial_messages: true,
                allowed_senders: Vec::new(),
                require_pairing_pin: true,
                pairing_pin_seconds: default_pairing_pin_seconds(),
                bot_sender: None,
                bot_npub: None,
                profile_name: default_profile_name(),
                profile_display_name: default_profile_display_name(),
                profile_about: default_profile_about(),
            },
            runner: RunnerConfig {
                bondage_bin: default_bondage_bin(),
                bondage_conf: default_bondage_conf(),
                data_dir: default_data_dir_string(),
                log_dir: default_log_dir_string(),
                max_prompt_chars: default_max_prompt_chars(),
                max_output_chars: default_max_output_chars(),
                progress_interval_seconds: default_progress_interval_seconds(),
                approval_ttl_seconds: default_approval_ttl_seconds(),
                worktree_dir: default_worktree_dir_string(),
                allow_generic_agent_profiles: false,
            },
            agents: AgentsConfig {
                codex: AgentConfig {
                    enabled: true,
                    profile: recommended_agentnoise_profile(AgentKind::Codex).to_string(),
                    bin: "codex".to_string(),
                    permission_mode: None,
                },
                claude: AgentConfig {
                    enabled: true,
                    profile: recommended_agentnoise_profile(AgentKind::Claude).to_string(),
                    bin: "claude".to_string(),
                    permission_mode: Some("auto".to_string()),
                },
                hermes: default_hermes_agent_config(),
            },
            repos: vec![RepoConfig {
                alias: "sandbox".to_string(),
                path: repo_path,
            }],
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.runner.max_prompt_chars == 0 {
            bail!("runner.max_prompt_chars must be greater than zero");
        }
        if self.runner.max_output_chars == 0 {
            bail!("runner.max_output_chars must be greater than zero");
        }
        if self.runner.progress_interval_seconds == 0 {
            bail!("runner.progress_interval_seconds must be greater than zero");
        }
        if self.runner.approval_ttl_seconds < 30 {
            bail!("runner.approval_ttl_seconds must be at least 30");
        }
        if self.whitenoise.pairing_pin_seconds < 10 {
            bail!("whitenoise.pairing_pin_seconds must be at least 10");
        }
        if self.whitenoise.profile_name.trim().is_empty() {
            bail!("whitenoise.profile_name cannot be empty");
        }
        if self.whitenoise.profile_display_name.trim().is_empty() {
            bail!("whitenoise.profile_display_name cannot be empty");
        }
        if self.whitenoise.transport == WhitenoiseTransport::Socket
            && self.whitenoise.resolved_socket().is_none()
        {
            bail!("whitenoise.transport = \"socket\" requires whitenoise.socket");
        }

        let mut aliases = HashSet::new();
        for repo in &self.repos {
            if repo.alias.trim().is_empty() {
                bail!("repo alias cannot be empty");
            }
            if repo.alias.contains(char::is_whitespace) {
                bail!("repo alias cannot contain whitespace: {}", repo.alias);
            }
            if !aliases.insert(repo.alias.as_str()) {
                bail!("duplicate repo alias: {}", repo.alias);
            }
        }
        Ok(())
    }

    pub fn repo_path(&self, alias: &str) -> Option<PathBuf> {
        self.repos
            .iter()
            .find(|repo| repo.alias == alias)
            .map(|repo| expand_tilde(&repo.path))
    }

    pub fn agent(&self, agent: AgentKind) -> &AgentConfig {
        match agent {
            AgentKind::Codex => &self.agents.codex,
            AgentKind::Claude => &self.agents.claude,
            AgentKind::Hermes => &self.agents.hermes,
        }
    }

    pub fn effective_agent_profile(&self, agent: AgentKind) -> String {
        let configured = self.agent(agent).profile.trim();
        if !self.runner.allow_generic_agent_profiles && is_generic_agent_profile(agent, configured)
        {
            return recommended_agentnoise_profile(agent).to_string();
        }
        configured.to_string()
    }

    pub fn agent_profile_warnings(&self) -> Vec<String> {
        [AgentKind::Codex, AgentKind::Claude, AgentKind::Hermes]
            .into_iter()
            .filter_map(|agent| {
                let config = self.agent(agent);
                if !config.enabled {
                    return None;
                }
                let configured = config.profile.trim();
                if !self.runner.allow_generic_agent_profiles
                    && is_generic_agent_profile(agent, configured)
                {
                    return Some(format!(
                        "{} configured with generic profile `{}`; using `{}` for agentnoise runs",
                        agent,
                        configured,
                        recommended_agentnoise_profile(agent)
                    ));
                }
                None
            })
            .collect()
    }

    pub fn resolved_data_dir(&self) -> PathBuf {
        expand_tilde(&self.runner.data_dir)
    }

    pub fn resolved_log_dir(&self) -> PathBuf {
        expand_tilde(&self.runner.log_dir)
    }

    pub fn resolved_jobs_path(&self) -> PathBuf {
        self.resolved_data_dir().join("jobs.json")
    }

    pub fn resolved_event_log_path(&self) -> PathBuf {
        self.resolved_data_dir().join("runtime-events.jsonl")
    }

    pub fn resolved_approvals_path(&self) -> PathBuf {
        self.resolved_data_dir().join("approvals.json")
    }

    pub fn resolved_attachments_path(&self) -> PathBuf {
        self.resolved_data_dir().join("attachments.json")
    }

    pub fn resolved_worktree_db_path(&self) -> PathBuf {
        self.resolved_data_dir().join("worktrees.json")
    }

    pub fn resolved_worktree_dir(&self) -> PathBuf {
        expand_tilde(&self.runner.worktree_dir)
    }

    pub fn resolved_chat_state_path(&self) -> PathBuf {
        self.resolved_data_dir().join("chat-state.json")
    }

    pub fn default_repo_alias(&self) -> Option<String> {
        self.repos.first().map(|repo| repo.alias.clone())
    }

    pub fn resolved_bondage_conf(&self) -> PathBuf {
        expand_tilde(&self.runner.bondage_conf)
    }

    pub fn secret_store(&self) -> crate::secrets::SecretStore {
        crate::secrets::SecretStore::new(
            &self.whitenoise.keychain_service,
            &self.whitenoise.keychain_item,
        )
    }
}

impl WhitenoiseConfig {
    pub fn control_group_ids(&self) -> Vec<String> {
        let mut group_ids = Vec::new();
        push_unique_group_id(&mut group_ids, &self.group_id);
        for group_id in &self.group_ids {
            push_unique_group_id(&mut group_ids, group_id);
        }
        group_ids
    }

    pub fn has_control_group(&self) -> bool {
        !self.control_group_ids().is_empty()
    }

    pub fn add_control_group_id(&mut self, group_id: &str) {
        let group_id = group_id.trim();
        if group_id.is_empty() {
            return;
        }
        if self.group_id.trim().is_empty() {
            self.group_id = group_id.to_string();
        }
        if !self.group_ids.iter().any(|existing| existing == group_id) {
            self.group_ids.push(group_id.to_string());
        }
    }

    pub fn resolved_socket(&self) -> Option<PathBuf> {
        self.socket
            .as_deref()
            .map(str::trim)
            .filter(|socket| !socket.is_empty())
            .map(expand_tilde)
    }
}

fn push_unique_group_id(group_ids: &mut Vec<String>, group_id: &str) {
    let group_id = group_id.trim();
    if group_id.is_empty() || group_ids.iter().any(|existing| existing == group_id) {
        return;
    }
    group_ids.push(group_id.to_string());
}

fn default_true() -> bool {
    true
}

fn default_wn_bin() -> String {
    "wn".to_string()
}

fn default_keychain_service() -> String {
    crate::secrets::DEFAULT_SERVICE.to_string()
}

fn default_keychain_item() -> String {
    crate::secrets::DEFAULT_ITEM.to_string()
}

fn default_pairing_relays() -> Vec<String> {
    crate::identity::DEFAULT_PAIRING_RELAYS
        .iter()
        .map(|relay| (*relay).to_string())
        .collect()
}

fn default_message_relays() -> Vec<String> {
    crate::identity::DEFAULT_MESSAGE_RELAYS
        .iter()
        .map(|relay| (*relay).to_string())
        .collect()
}

fn default_subscribe_limit() -> u32 {
    0
}

fn default_max_message_chars() -> usize {
    1_800
}

fn default_pairing_pin_seconds() -> u64 {
    crate::auth::DEFAULT_PAIRING_PIN_SECONDS
}

fn default_profile_name() -> String {
    "agentnoise".to_string()
}

fn default_profile_display_name() -> String {
    "agentnoise desktop".to_string()
}

fn default_profile_about() -> String {
    "Local agentnoise desktop helper.".to_string()
}

fn default_bondage_bin() -> String {
    "bondage".to_string()
}

fn default_bondage_conf() -> String {
    "~/.config/bondage/bondage.conf".to_string()
}

fn default_data_dir_string() -> String {
    default_data_dir().display().to_string()
}

fn default_log_dir_string() -> String {
    default_log_dir().display().to_string()
}

fn default_worktree_dir_string() -> String {
    default_data_dir().join("worktrees").display().to_string()
}

fn default_max_prompt_chars() -> usize {
    8_000
}

fn default_max_output_chars() -> usize {
    2_400
}

fn default_progress_interval_seconds() -> u64 {
    15
}

fn default_approval_ttl_seconds() -> u64 {
    600
}

fn default_hermes_agent_config() -> AgentConfig {
    AgentConfig {
        enabled: false,
        profile: recommended_agentnoise_profile(AgentKind::Hermes).to_string(),
        bin: "hermes".to_string(),
        permission_mode: None,
    }
}

pub fn recommended_agentnoise_profile(agent: AgentKind) -> &'static str {
    match agent {
        AgentKind::Codex => "codex-agentnoise",
        AgentKind::Claude => "claude-agentnoise",
        AgentKind::Hermes => "hermes-agentnoise",
    }
}

fn generic_agent_profile(agent: AgentKind) -> &'static str {
    match agent {
        AgentKind::Codex => "codex",
        AgentKind::Claude => "claude",
        AgentKind::Hermes => "hermes",
    }
}

fn is_generic_agent_profile(agent: AgentKind, profile: &str) -> bool {
    profile == generic_agent_profile(agent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_group_ids_deduplicate_legacy_and_list_values() {
        let mut config = Config::template().whitenoise;
        config.group_id = "abc".to_string();
        config.group_ids = vec![
            "abc".to_string(),
            "def".to_string(),
            "  ".to_string(),
            "def".to_string(),
        ];

        assert_eq!(
            config.control_group_ids(),
            vec!["abc".to_string(), "def".to_string()]
        );
    }

    #[test]
    fn add_control_group_id_keeps_legacy_first_group() {
        let mut config = Config::template().whitenoise;
        config.add_control_group_id("abc");
        config.add_control_group_id("def");
        config.add_control_group_id("abc");

        assert_eq!(config.group_id, "abc");
        assert_eq!(config.group_ids, vec!["abc".to_string(), "def".to_string()]);
        assert_eq!(
            config.control_group_ids(),
            vec!["abc".to_string(), "def".to_string()]
        );
    }

    #[test]
    fn missing_hermes_config_loads_disabled_default() {
        let text = r#"
[whitenoise]
group_id = "group"

[runner]
bondage_bin = "bondage"
bondage_conf = "/tmp/bondage.conf"
data_dir = "/tmp/agentnoise"
log_dir = "/tmp/agentnoise/logs"
max_prompt_chars = 8000
max_output_chars = 2400

[agents.codex]
enabled = true
profile = "codex"
bin = "codex"

[agents.claude]
enabled = true
profile = "claude"
bin = "claude"
permission_mode = "auto"

[[repos]]
alias = "work"
path = "/tmp"
"#;

        let config: Config = toml::from_str(text).unwrap();

        assert!(!config.agents.hermes.enabled);
        assert_eq!(config.agents.hermes.profile, "hermes-agentnoise");
        assert_eq!(config.agents.hermes.bin, "hermes");
        assert_eq!(config.whitenoise.profile_name, "agentnoise");
        assert_eq!(config.whitenoise.profile_display_name, "agentnoise desktop");
    }

    #[test]
    fn generic_agent_profiles_are_forced_to_agentnoise_profiles() {
        let mut config = Config::template();
        config.agents.codex.profile = "codex".to_string();
        assert_eq!(
            config.effective_agent_profile(AgentKind::Codex),
            "codex-agentnoise"
        );
        assert_eq!(config.agent_profile_warnings().len(), 1);

        config.runner.allow_generic_agent_profiles = true;
        assert_eq!(config.effective_agent_profile(AgentKind::Codex), "codex");
        assert!(config.agent_profile_warnings().is_empty());
    }
}
