use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use uuid::Uuid;

use crate::auth::{PairingGate, is_pairing_pin_message};
use crate::chat::{ChatCommand, parse_chat_command};
use crate::config::{Config, RepoConfig};
use crate::jobs::JobStore;
use crate::runner::{AgentRequest, Runner};
use crate::session::{ChatStateStore, SessionState};
use crate::workspace;

#[derive(Debug)]
pub enum RouteAction {
    Ignore,
    Reply(String),
    NewSession(NewSessionRequest),
    ResumeSession(ResumeSessionRequest),
    Run(AgentRequest),
}

#[derive(Debug, Clone)]
pub struct NewSessionRequest {
    pub name: String,
    pub sender: String,
    pub state: SessionState,
}

impl NewSessionRequest {
    pub fn group_name(&self) -> String {
        format!("agentnoise: {}", self.name)
    }

    pub fn ready_text(&self) -> String {
        format!(
            "Session: {}\nWorkspace: {}\nReady. Send /codex or /claude.",
            self.name,
            workspace_text(&self.state)
        )
    }

    pub fn created_text(&self) -> String {
        format!(
            "Created session: {}\nOpen the new agentnoise chat in White Noise.",
            self.name
        )
    }
}

#[derive(Debug, Clone)]
pub struct ResumeSessionRequest {
    pub group_id: String,
    pub reply_text: String,
    pub target_text: String,
}

#[derive(Clone)]
pub struct AgentApp {
    config: Config,
    config_path: Option<PathBuf>,
    jobs: JobStore,
    runner: Runner,
    sessions: ChatStateStore,
    auth: AuthState,
}

impl AgentApp {
    pub fn from_config_path(path: &Path) -> Result<Self> {
        let config = Config::load(path)?;
        Self::new_with_auth(path.to_path_buf(), config, None)
    }

    pub fn new(config: Config) -> Result<Self> {
        Self::build(None, config, None)
    }

    pub fn new_with_auth(
        config_path: PathBuf,
        config: Config,
        pairing_gate: Option<PairingGate>,
    ) -> Result<Self> {
        Self::build(Some(config_path), config, pairing_gate)
    }

    fn build(
        config_path: Option<PathBuf>,
        config: Config,
        pairing_gate: Option<PairingGate>,
    ) -> Result<Self> {
        let jobs = JobStore::open(&config.resolved_jobs_path(), &config.resolved_log_dir())
            .context("opening job store")?;
        let sessions = ChatStateStore::open(&config.resolved_chat_state_path())
            .context("opening chat state")?;
        let runner = Runner::new(config.clone(), jobs.clone());
        let auth = AuthState::new(&config, pairing_gate);
        Ok(Self {
            config,
            config_path,
            jobs,
            runner,
            sessions,
            auth,
        })
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn recover_interrupted_jobs(&self) -> Result<usize> {
        self.jobs.recover_interrupted_jobs()
    }

    pub fn route_message(
        &self,
        group_id: Option<&str>,
        sender: Option<&str>,
        text: &str,
    ) -> Result<RouteAction> {
        if self.should_ignore_bot(sender) {
            return Ok(RouteAction::Ignore);
        }
        if let Some(reply) = self.try_pair_sender(sender, text)? {
            return Ok(RouteAction::Reply(reply));
        }
        if self.should_ignore_sender(sender) {
            return Ok(RouteAction::Ignore);
        }

        let command = match parse_chat_command(text) {
            Ok(command) => command,
            Err(_) => return Ok(RouteAction::Ignore),
        };

        let session_key = session_key(group_id, sender);
        match command {
            ChatCommand::Help => Ok(RouteAction::Reply(help_text())),
            ChatCommand::Status => Ok(RouteAction::Reply(self.status_text(&session_key))),
            ChatCommand::New { name } => self.new_session_action(&session_key, sender, name),
            ChatCommand::Rename { name } => Ok(RouteAction::Reply(self.rename_text(
                &session_key,
                group_id,
                name,
            )?)),
            ChatCommand::Sessions => Ok(RouteAction::Reply(self.sessions_text(&session_key))),
            ChatCommand::Resume { target } => {
                self.resume_session_action(&session_key, group_id, target.as_deref())
            }
            ChatCommand::Close => Ok(RouteAction::Reply(self.close_text(&session_key)?)),
            ChatCommand::Repos => Ok(RouteAction::Reply(self.repos_text(&session_key))),
            ChatCommand::Use { repo_alias } => Ok(RouteAction::Reply(
                self.use_repo_text(&session_key, &repo_alias),
            )),
            ChatCommand::Pwd => Ok(RouteAction::Reply(self.pwd_text(&session_key))),
            ChatCommand::Ls { path } => Ok(RouteAction::Reply(
                self.ls_text(&session_key, path.as_deref()),
            )),
            ChatCommand::Cd { path } => Ok(RouteAction::Reply(self.cd_text(&session_key, &path))),
            ChatCommand::Jobs => Ok(RouteAction::Reply(self.jobs_text())),
            ChatCommand::Tail { job_id } => Ok(RouteAction::Reply(self.tail_text(&job_id))),
            ChatCommand::Cancel { job_id } => Ok(RouteAction::Reply(self.cancel_text(&job_id))),
            ChatCommand::Run(request) => match self.prepare_request(&session_key, request) {
                Ok(request) => Ok(RouteAction::Run(request)),
                Err(error) => Ok(RouteAction::Reply(format!("Error: {error:#}"))),
            },
        }
    }

    pub fn route_unsupported_message(
        &self,
        sender: Option<&str>,
        message: &str,
    ) -> Result<RouteAction> {
        if self.should_ignore_bot(sender) || self.should_ignore_sender(sender) {
            return Ok(RouteAction::Ignore);
        }

        Ok(RouteAction::Reply(message.to_string()))
    }

    pub fn accepts_current_pairing_pin(&self, sender: Option<&str>, text: &str) -> bool {
        let Some(sender) = sender.map(str::trim).filter(|sender| !sender.is_empty()) else {
            return false;
        };
        self.auth.accepts_pairing_pin(sender, text)
    }

    pub fn run_request(&self, request: AgentRequest) -> Result<String> {
        let record = self.runner.run_blocking(request)?;
        Ok(format!(
            "Job {} {}\nDetails: /tail {}\n\n{}",
            record.id,
            record.status,
            record.id,
            record
                .summary
                .unwrap_or_else(|| "no output captured".to_string())
        ))
    }

    pub fn create_session_record(&self, group_id: &str, state: SessionState) -> Result<String> {
        let key = session_key(Some(group_id), None);
        self.sessions.set(&key, state)?;
        Ok(key)
    }

    fn should_ignore_bot(&self, sender: Option<&str>) -> bool {
        let Some(sender) = sender else {
            return false;
        };

        self.config.whitenoise.bot_sender.as_deref() == Some(sender)
            || self.config.whitenoise.bot_npub.as_deref() == Some(sender)
    }

    fn should_ignore_sender(&self, sender: Option<&str>) -> bool {
        let Some(sender) = sender else {
            return self.auth.pairing_required();
        };

        self.auth.should_ignore_sender(sender)
    }

    fn try_pair_sender(&self, sender: Option<&str>, text: &str) -> Result<Option<String>> {
        let Some(sender) = sender.map(str::trim).filter(|sender| !sender.is_empty()) else {
            return Ok(None);
        };
        if !self.auth.accepts_pairing_pin(sender, text) {
            if self.auth.pairing_required()
                && self.auth.should_ignore_sender(sender)
                && is_pairing_pin_message(text)
            {
                return Ok(Some(
                    "Pairing PIN invalid or expired. Check the desktop log for the current PIN."
                        .to_string(),
                ));
            }
            return Ok(None);
        }

        self.auth.allow_sender(sender)?;
        if let Some(config_path) = &self.config_path {
            let mut config = Config::load(config_path)?;
            if !config
                .whitenoise
                .allowed_senders
                .iter()
                .any(|allowed| allowed == sender)
            {
                config.whitenoise.allowed_senders.push(sender.to_string());
                config.save(config_path)?;
            }
        }

        Ok(Some("Paired. Send /help for commands.".to_string()))
    }

    fn status_text(&self, sender_key: &str) -> String {
        let jobs = self.jobs.recent(20);
        let active = jobs.iter().filter(|job| job.status.is_active()).count();
        let stored_group_count = self
            .sessions
            .list()
            .iter()
            .filter(|(key, _session)| key.starts_with("group:"))
            .count();
        let group_count = self
            .config
            .whitenoise
            .control_group_ids()
            .len()
            .max(stored_group_count);
        let groups = if group_count == 0 {
            "none".to_string()
        } else if group_count == 1 {
            "1 group".to_string()
        } else {
            format!("{group_count} groups")
        };
        let session = self.session(sender_key).ok();
        let session_name = session
            .as_ref()
            .map(|session| session_display_name(sender_key, session))
            .unwrap_or_else(|| "none".to_string());
        let workspace = session
            .as_ref()
            .map(workspace_text)
            .unwrap_or_else(|| "none".to_string());

        format!(
            "agentnoise\nStatus: OK\nSession: {session_name}\nChats: {groups}\nWorkspace: {workspace}\nJobs: {active} active\nRepos: {}",
            self.config.repos.len()
        )
    }

    fn new_session_action(
        &self,
        sender_key: &str,
        sender: Option<&str>,
        name: Option<String>,
    ) -> Result<RouteAction> {
        let Some(sender) = sender
            .map(str::trim)
            .filter(|sender| !sender.is_empty())
            .map(str::to_string)
        else {
            return Ok(RouteAction::Reply(
                "Error: /new needs a White Noise sender identity.".to_string(),
            ));
        };

        let name = normalize_session_name(name.as_deref()).unwrap_or_else(generated_session_name);
        let mut state = self.session(sender_key)?;
        state.name = Some(name.clone());
        state.closed = false;

        Ok(RouteAction::NewSession(NewSessionRequest {
            name,
            sender,
            state,
        }))
    }

    fn rename_text(
        &self,
        sender_key: &str,
        group_id: Option<&str>,
        name: Option<String>,
    ) -> Result<String> {
        if group_id
            .map(str::trim)
            .filter(|group| !group.is_empty())
            .is_none()
        {
            return Ok("Error: /rename only works inside a White Noise chat.".to_string());
        }

        let mut session = self.session(sender_key)?;
        let name = normalize_session_name(name.as_deref())
            .or_else(|| session.name.clone())
            .unwrap_or_else(|| session_display_name(sender_key, &session));
        session.name = Some(name.clone());
        session.closed = false;
        self.sessions.set(sender_key, session.clone())?;

        Ok(format!(
            "Session: {name}\nWorkspace: {}",
            workspace_text(&session)
        ))
    }

    fn sessions_text(&self, sender_key: &str) -> String {
        let sessions = self.session_entries(sender_key);
        if sessions.is_empty() {
            return "No saved sessions yet. Send /rename <name> or /new <name>.".to_string();
        }

        let lines = sessions
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let marker = if entry.key == sender_key { "*" } else { "-" };
                let closed = if entry.state.closed { " closed" } else { "" };
                format!(
                    "{}. {marker} {}{} {} id:{}",
                    index + 1,
                    entry.name,
                    closed,
                    workspace_text(&entry.state),
                    short_group_id(&entry.group_id)
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        format!("Sessions\n{lines}")
    }

    fn resume_session_action(
        &self,
        sender_key: &str,
        current_group_id: Option<&str>,
        target: Option<&str>,
    ) -> Result<RouteAction> {
        let entry = match target.map(str::trim).filter(|target| !target.is_empty()) {
            Some(target) => {
                let Some(entry) = self.find_session_entry(sender_key, target) else {
                    return Ok(RouteAction::Reply(format!(
                        "No matching session: {target}\nSend /list to see sessions."
                    )));
                };
                entry
            }
            None => {
                let Some(group_id) = current_group_id
                    .map(str::trim)
                    .filter(|group_id| !group_id.is_empty())
                else {
                    return Ok(RouteAction::Reply(
                        "Usage: /resume <number|name|id>".to_string(),
                    ));
                };
                let key = session_key(Some(group_id), None);
                let state = self.session(&key)?;
                SessionEntry::new(key, group_id.to_string(), state)
            }
        };

        let mut state = entry.state.clone();
        state.closed = false;
        self.sessions.set(&entry.key, state.clone())?;

        let target_text = format!(
            "Session resumed: {}\nWorkspace: {}\nReady.",
            entry.name,
            workspace_text(&state)
        );

        let current_group_id = current_group_id.map(str::trim).unwrap_or_default();
        if current_group_id == entry.group_id {
            return Ok(RouteAction::Reply(target_text));
        }

        Ok(RouteAction::ResumeSession(ResumeSessionRequest {
            group_id: entry.group_id.clone(),
            reply_text: format!(
                "Resumed session: {}\nOpen the agentnoise chat with id:{}.",
                entry.name,
                short_group_id(&entry.group_id)
            ),
            target_text,
        }))
    }

    fn find_session_entry(&self, sender_key: &str, target: &str) -> Option<SessionEntry> {
        let target = target.trim();
        let sessions = self.session_entries(sender_key);
        if let Ok(index) = target.parse::<usize>()
            && index > 0
        {
            return sessions.get(index - 1).cloned();
        }

        sessions.into_iter().find(|entry| {
            entry.name.eq_ignore_ascii_case(target)
                || entry.group_id == target
                || short_group_id(&entry.group_id).eq_ignore_ascii_case(target)
        })
    }

    fn session_entries(&self, sender_key: &str) -> Vec<SessionEntry> {
        let mut sessions = self
            .sessions
            .list()
            .into_iter()
            .filter_map(|(key, session)| {
                key.strip_prefix("group:")
                    .map(|group_id| SessionEntry::new(key.clone(), group_id.to_string(), session))
            })
            .collect::<Vec<_>>();
        if sender_key.starts_with("group:")
            && !sessions.iter().any(|entry| entry.key == sender_key)
            && let Ok(session) = self.session(sender_key)
            && let Some(group_id) = sender_key.strip_prefix("group:")
        {
            sessions.push(SessionEntry::new(
                sender_key.to_string(),
                group_id.to_string(),
                session,
            ));
        }

        sessions.sort_by(|left, right| left.name.cmp(&right.name));
        sessions
    }

    fn close_text(&self, sender_key: &str) -> Result<String> {
        let mut session = self.session(sender_key)?;
        session.closed = true;
        let name = session_display_name(sender_key, &session);
        self.sessions.set(sender_key, session)?;
        Ok(format!("Closed session: {name}"))
    }

    fn repos_text(&self, sender_key: &str) -> String {
        if self.config.repos.is_empty() {
            return "No repos configured.".to_string();
        }
        let selected = self
            .session(sender_key)
            .ok()
            .and_then(|session| session.repo_alias);

        let repos = self
            .config
            .repos
            .iter()
            .map(|repo| {
                let marker = if selected.as_deref() == Some(repo.alias.as_str()) {
                    "*"
                } else {
                    "-"
                };
                format!("{marker} {}\n  {}", repo.alias, repo.path)
            })
            .collect::<Vec<_>>()
            .join("\n");

        format!("Repos\n{repos}")
    }

    fn use_repo_text(&self, sender_key: &str, repo_alias: &str) -> String {
        if self.repo(repo_alias).is_none() {
            return format!("Unknown repo: {repo_alias}");
        }

        let mut session = self
            .session(sender_key)
            .unwrap_or_else(|_| SessionState::new(None));
        session.repo_alias = Some(repo_alias.to_string());
        session.cwd = ".".to_string();
        match self.sessions.set(sender_key, session) {
            Ok(()) => format!("Workspace: {repo_alias}:/"),
            Err(error) => format!("Error: failed to save workspace: {error:#}"),
        }
    }

    fn pwd_text(&self, sender_key: &str) -> String {
        match self.session(sender_key) {
            Ok(session) => match session.repo_alias {
                Some(alias) => {
                    format!(
                        "Workspace: {alias}:{}",
                        workspace::display_cwd(&session.cwd)
                    )
                }
                None => "No repo selected. Send /use <repo>.".to_string(),
            },
            Err(error) => format!("Error: workspace failed: {error:#}"),
        }
    }

    fn ls_text(&self, sender_key: &str, path: Option<&str>) -> String {
        match self.resolve_session_path(sender_key, path.unwrap_or(".")) {
            Ok((alias, _session, path)) => render_ls(&alias, &path),
            Err(error) => format!("Error: {error:#}"),
        }
    }

    fn cd_text(&self, sender_key: &str, path: &str) -> String {
        match self.resolve_session_path(sender_key, path) {
            Ok((alias, mut session, path)) => {
                if !path.is_dir() {
                    return format!("Error: not a directory: {}", path.display());
                }
                let Some(root) = self.config.repo_path(&alias) else {
                    return format!("Error: unknown repo alias: {alias}");
                };
                match workspace::relative_cwd(&root, &path) {
                    Ok(cwd) => {
                        session.cwd = cwd;
                        match self.sessions.set(sender_key, session.clone()) {
                            Ok(()) => format!(
                                "Workspace: {alias}:{}",
                                workspace::display_cwd(&session.cwd)
                            ),
                            Err(error) => {
                                format!("Error: failed to save workspace: {error:#}")
                            }
                        }
                    }
                    Err(error) => format!("Error: {error:#}"),
                }
            }
            Err(error) => format!("Error: {error:#}"),
        }
    }

    fn prepare_request(&self, sender_key: &str, mut request: AgentRequest) -> Result<AgentRequest> {
        if request.resume_session.is_some() {
            return Ok(request);
        }

        if request.repo_alias.is_none() {
            if let Some((repo_alias, prompt)) = self.extract_repo_prefix(&request.prompt) {
                request.repo_alias = Some(repo_alias);
                request.prompt = prompt;
                request.cwd = None;
                return Ok(request);
            }

            let session = self.session(sender_key)?;
            let Some(repo_alias) = session.repo_alias else {
                bail!("no repo selected; use /use <repo>");
            };
            request = request.with_workspace(repo_alias, session.cwd);
        }

        Ok(request)
    }

    fn session(&self, sender_key: &str) -> Result<SessionState> {
        self.sessions
            .get_or_default(sender_key, self.config.default_repo_alias())
    }

    fn resolve_session_path(
        &self,
        sender_key: &str,
        path: &str,
    ) -> Result<(String, SessionState, PathBuf)> {
        let session = self.session(sender_key)?;
        let Some(alias) = session.repo_alias.clone() else {
            bail!("no repo selected; use /use <repo>");
        };
        let Some(root) = self.config.repo_path(&alias) else {
            bail!("unknown repo alias: {alias}");
        };
        let path = workspace::resolve_child(&root, &session.cwd, path)?;
        Ok((alias, session, path))
    }

    fn extract_repo_prefix(&self, prompt: &str) -> Option<(String, String)> {
        let (first, rest) = split_first(prompt);
        if first.is_empty() || rest.trim().is_empty() {
            return None;
        }
        self.repo(first)?;
        Some((first.to_string(), rest.trim().to_string()))
    }

    fn repo(&self, alias: &str) -> Option<&RepoConfig> {
        self.config.repos.iter().find(|repo| repo.alias == alias)
    }

    fn jobs_text(&self) -> String {
        let jobs = self.jobs.recent(10);
        if jobs.is_empty() {
            return "No jobs yet.".to_string();
        }

        let lines = jobs
            .iter()
            .map(|job| {
                format!(
                    "- {} {} {} {}",
                    job.id,
                    job.status,
                    job.agent,
                    job.repo_alias.as_deref().unwrap_or("-")
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        format!("Jobs\n{lines}")
    }

    fn tail_text(&self, job_id: &str) -> String {
        match self.jobs.tail(job_id, 2400) {
            Ok(Some(text)) if !text.trim().is_empty() => text,
            Ok(Some(_)) => format!("Job {job_id} has an empty log."),
            Ok(None) => format!("No such job: {job_id}"),
            Err(error) => format!("Error: tail failed: {error:#}"),
        }
    }

    fn cancel_text(&self, job_id: &str) -> String {
        match self.runner.cancel(job_id) {
            Ok(true) => format!("Cancel requested: {job_id}"),
            Ok(false) => format!("No running job: {job_id}"),
            Err(error) => format!("Error: cancel failed: {error:#}"),
        }
    }
}

#[derive(Clone)]
struct AuthState {
    allowed_senders: Arc<Mutex<Vec<String>>>,
    pairing_gate: Option<PairingGate>,
}

impl AuthState {
    fn new(config: &Config, pairing_gate: Option<PairingGate>) -> Self {
        Self {
            allowed_senders: Arc::new(Mutex::new(config.whitenoise.allowed_senders.clone())),
            pairing_gate,
        }
    }

    fn pairing_required(&self) -> bool {
        self.pairing_gate.is_some() && self.allowed_senders().is_empty()
    }

    fn should_ignore_sender(&self, sender: &str) -> bool {
        let allowed = self.allowed_senders();
        if allowed.is_empty() {
            return self.pairing_gate.is_some();
        }
        !allowed.iter().any(|allowed| allowed == sender)
    }

    fn accepts_pairing_pin(&self, sender: &str, text: &str) -> bool {
        if !self.pairing_required() || !self.should_ignore_sender(sender) {
            return false;
        }
        let Some(pairing_gate) = &self.pairing_gate else {
            return false;
        };
        pairing_gate.verify(text)
    }

    fn allow_sender(&self, sender: &str) -> Result<()> {
        {
            let mut allowed = self
                .allowed_senders
                .lock()
                .map_err(|_| anyhow::anyhow!("sender allowlist lock poisoned"))?;
            if !allowed.iter().any(|allowed| allowed == sender) {
                allowed.push(sender.to_string());
            }
        }
        if let Some(pairing_gate) = &self.pairing_gate {
            pairing_gate.mark_complete();
        }
        Ok(())
    }

    fn allowed_senders(&self) -> Vec<String> {
        self.allowed_senders
            .lock()
            .map(|allowed| allowed.clone())
            .unwrap_or_default()
    }
}

fn session_key(group_id: Option<&str>, sender: Option<&str>) -> String {
    if let Some(group_id) = group_id
        .map(str::trim)
        .filter(|group_id| !group_id.is_empty())
    {
        return format!("group:{group_id}");
    }

    sender
        .map(str::trim)
        .filter(|sender| !sender.is_empty())
        .map(|sender| format!("sender:{sender}"))
        .unwrap_or_else(|| "local".to_string())
}

#[derive(Debug, Clone)]
struct SessionEntry {
    key: String,
    group_id: String,
    name: String,
    state: SessionState,
}

impl SessionEntry {
    fn new(key: String, group_id: String, state: SessionState) -> Self {
        let name = session_display_name(&key, &state);
        Self {
            key,
            group_id,
            name,
            state,
        }
    }
}

fn normalize_session_name(name: Option<&str>) -> Option<String> {
    name.map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|name| {
            name.chars()
                .filter_map(|ch| {
                    if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                        Some(ch)
                    } else if ch.is_whitespace() {
                        Some('-')
                    } else {
                        None
                    }
                })
                .collect::<String>()
        })
        .map(|name| name.trim_matches('-').chars().take(48).collect::<String>())
        .filter(|name| !name.is_empty())
}

fn short_group_id(group_id: &str) -> String {
    group_id.chars().take(6).collect()
}

fn generated_session_name() -> String {
    let uuid = Uuid::new_v4().simple().to_string();
    format!("session-{}", &uuid[..6])
}

fn session_display_name(key: &str, session: &SessionState) -> String {
    if let Some(name) = session.name.as_deref() {
        return name.to_string();
    }
    if let Some(group_id) = key.strip_prefix("group:") {
        let short = group_id.chars().take(6).collect::<String>();
        if !short.is_empty() {
            return format!("s-{short}");
        }
    }
    "local".to_string()
}

fn workspace_text(session: &SessionState) -> String {
    session
        .repo_alias
        .as_ref()
        .map(|alias| format!("{}:{}", alias, workspace::display_cwd(&session.cwd)))
        .unwrap_or_else(|| "none".to_string())
}

fn render_ls(alias: &str, path: &Path) -> String {
    if path.is_file() {
        let size = fs::metadata(path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        return format!("Listing {alias}: {}\n{} bytes", path.display(), size);
    }
    if !path.is_dir() {
        return format!("Error: not a directory: {}", path.display());
    }

    let mut entries = match fs::read_dir(path) {
        Ok(entries) => entries
            .filter_map(|entry| entry.ok())
            .map(|entry| {
                let name = entry.file_name().to_string_lossy().to_string();
                let is_dir = entry
                    .file_type()
                    .map(|file_type| file_type.is_dir())
                    .unwrap_or(false);
                if is_dir { format!("{name}/") } else { name }
            })
            .collect::<Vec<_>>(),
        Err(error) => return format!("Error: ls failed: {error:#}"),
    };
    entries.sort_by_key(|entry| entry.to_ascii_lowercase());
    if entries.is_empty() {
        return format!("Listing {alias}: {}\n(empty)", path.display());
    }

    let total = entries.len();
    let mut shown = entries.into_iter().take(60).collect::<Vec<_>>();
    let truncated = total > 60;
    if truncated {
        shown.push("...".to_string());
    }
    format!("Listing {alias}: {}\n{}", path.display(), shown.join("\n"))
}

fn split_first(input: &str) -> (&str, &str) {
    let input = input.trim();
    if input.is_empty() {
        return ("", "");
    }

    match input.find(char::is_whitespace) {
        Some(index) => (&input[..index], input[index..].trim()),
        None => (input, ""),
    }
}

fn help_text() -> String {
    [
        "agentnoise commands",
        "",
        "Workspace",
        "/status",
        "/new [name]",
        "/rename [name]",
        "/list",
        "/resume <number|name|id>",
        "/close",
        "/repos",
        "/use <repo>",
        "/pwd",
        "/ls [path]",
        "/cd <path>",
        "",
        "Agents",
        "/codex <prompt>",
        "/codex <repo> <prompt>",
        "/codex-resume <session> <prompt>",
        "/claude <prompt>",
        "/claude <repo> <prompt>",
        "/claude-resume <session> <prompt>",
        "/wiki <prompt>",
        "/codex-wiki <prompt>",
        "/claude-wiki <prompt>",
        "",
        "Jobs",
        "/jobs",
        "/tail <job>",
        "/cancel <job>",
        "",
        "/help",
    ]
    .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_pin_authorizes_sender_and_persists_allowlist() {
        let temp = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let config_path = temp.path().join("config.toml");
        let mut config = Config::template();
        config.whitenoise.allowed_senders.clear();
        config.runner.data_dir = temp.path().join("data").display().to_string();
        config.runner.log_dir = temp.path().join("logs").display().to_string();
        config.repos[0].path = repo.path().display().to_string();
        config.save(&config_path).unwrap();

        let pairing_gate = PairingGate::new(30);
        let pin = pairing_gate.current_pin().code;
        let app = AgentApp::new_with_auth(config_path.clone(), config, Some(pairing_gate)).unwrap();

        assert!(app.accepts_current_pairing_pin(Some("phone"), &pin));
        assert!(!app.accepts_current_pairing_pin(Some("phone"), "000000"));
        assert!(matches!(
            app.route_message(Some("group-a"), Some("phone"), "000000")
                .unwrap(),
            RouteAction::Reply(reply) if reply.contains("invalid or expired")
        ));
        assert!(matches!(
            app.route_message(Some("group-a"), Some("phone"), &pin)
                .unwrap(),
            RouteAction::Reply(reply) if reply.contains("Paired")
        ));
        assert!(matches!(
            app.route_message(Some("group-a"), Some("phone"), "/status")
                .unwrap(),
            RouteAction::Reply(_)
        ));

        let saved = Config::load(&config_path).unwrap();
        assert_eq!(saved.whitenoise.allowed_senders, vec!["phone"]);
    }

    #[test]
    fn group_id_separates_workspace_state_for_same_sender() {
        let temp = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        fs::create_dir(repo.path().join("src")).unwrap();
        let config_path = temp.path().join("config.toml");
        let mut config = Config::template();
        config.whitenoise.allowed_senders = vec!["phone".to_string()];
        config.runner.data_dir = temp.path().join("data").display().to_string();
        config.runner.log_dir = temp.path().join("logs").display().to_string();
        config.repos[0].path = repo.path().display().to_string();
        config.save(&config_path).unwrap();
        let app = AgentApp::new_with_auth(config_path, config, None).unwrap();

        assert!(matches!(
            app.route_message(Some("group-a"), Some("phone"), "/cd src")
                .unwrap(),
            RouteAction::Reply(reply) if reply.contains(":/src")
        ));
        assert!(matches!(
            app.route_message(Some("group-b"), Some("phone"), "/pwd")
                .unwrap(),
            RouteAction::Reply(reply) if reply.contains(":/")
                && !reply.contains(":/src")
        ));
        assert!(matches!(
            app.route_message(Some("group-a"), Some("phone"), "/pwd")
                .unwrap(),
            RouteAction::Reply(reply) if reply.contains(":/src")
        ));
    }

    #[test]
    fn new_session_clones_current_workspace_state() {
        let temp = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        fs::create_dir(repo.path().join("src")).unwrap();
        let config_path = temp.path().join("config.toml");
        let mut config = Config::template();
        config.whitenoise.allowed_senders = vec!["phone".to_string()];
        config.runner.data_dir = temp.path().join("data").display().to_string();
        config.runner.log_dir = temp.path().join("logs").display().to_string();
        config.repos[0].path = repo.path().display().to_string();
        config.save(&config_path).unwrap();
        let app = AgentApp::new_with_auth(config_path, config, None).unwrap();

        app.route_message(Some("group-a"), Some("phone"), "/cd src")
            .unwrap();
        assert!(matches!(
            app.route_message(Some("group-a"), Some("phone"), "/new bugfix ui")
                .unwrap(),
            RouteAction::NewSession(request)
                if request.name == "bugfix-ui"
                    && request.sender == "phone"
                    && request.state.cwd == "src"
        ));
    }

    #[test]
    fn unsupported_messages_reply_only_to_allowed_senders() {
        let temp = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let config_path = temp.path().join("config.toml");
        let mut config = Config::template();
        config.whitenoise.allowed_senders = vec!["phone".to_string()];
        config.runner.data_dir = temp.path().join("data").display().to_string();
        config.runner.log_dir = temp.path().join("logs").display().to_string();
        config.repos[0].path = repo.path().display().to_string();
        config.save(&config_path).unwrap();
        let app = AgentApp::new_with_auth(config_path, config, None).unwrap();

        assert!(matches!(
            app.route_unsupported_message(Some("phone"), "Attachment received")
                .unwrap(),
            RouteAction::Reply(reply) if reply == "Attachment received"
        ));
        assert!(matches!(
            app.route_unsupported_message(Some("stranger"), "Attachment received")
                .unwrap(),
            RouteAction::Ignore
        ));
    }

    #[test]
    fn rename_names_current_group_session() {
        let temp = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let config_path = temp.path().join("config.toml");
        let mut config = Config::template();
        config.whitenoise.allowed_senders = vec!["phone".to_string()];
        config.runner.data_dir = temp.path().join("data").display().to_string();
        config.runner.log_dir = temp.path().join("logs").display().to_string();
        config.repos[0].path = repo.path().display().to_string();
        config.save(&config_path).unwrap();
        let app = AgentApp::new_with_auth(config_path, config, None).unwrap();

        assert!(matches!(
            app.route_message(Some("group-a"), Some("phone"), "/rename main")
                .unwrap(),
            RouteAction::Reply(reply) if reply.contains("Session: main")
        ));
        assert!(matches!(
            app.route_message(Some("group-a"), Some("phone"), "/sessions")
                .unwrap(),
            RouteAction::Reply(reply) if reply.contains("* main")
        ));
    }

    #[test]
    fn resume_selects_session_from_list() {
        let temp = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let config_path = temp.path().join("config.toml");
        let mut config = Config::template();
        config.whitenoise.allowed_senders = vec!["phone".to_string()];
        config.runner.data_dir = temp.path().join("data").display().to_string();
        config.runner.log_dir = temp.path().join("logs").display().to_string();
        config.repos[0].path = repo.path().display().to_string();
        config.save(&config_path).unwrap();
        let app = AgentApp::new_with_auth(config_path, config, None).unwrap();

        app.route_message(Some("group-a"), Some("phone"), "/rename main")
            .unwrap();
        app.route_message(Some("group-b"), Some("phone"), "/rename bugfix")
            .unwrap();

        assert!(matches!(
            app.route_message(Some("group-a"), Some("phone"), "/list")
                .unwrap(),
            RouteAction::Reply(reply) if reply.contains("1. - bugfix")
                && reply.contains("2. * main")
        ));
        assert!(matches!(
            app.route_message(Some("group-a"), Some("phone"), "/resume bugfix")
                .unwrap(),
            RouteAction::ResumeSession(request) if request.group_id == "group-b"
                && request.reply_text.contains("Resumed session: bugfix")
                && request.target_text.contains("Session resumed: bugfix")
        ));
        assert!(matches!(
            app.route_message(Some("group-a"), Some("phone"), "/resume 2")
                .unwrap(),
            RouteAction::Reply(reply) if reply.contains("Session resumed: main")
        ));
    }
}
