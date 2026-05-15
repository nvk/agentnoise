use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use nostr::PublicKey;
use uuid::Uuid;

use crate::approvals::{self, ApprovalStore};
use crate::attachments::{self, AttachmentStore};
use crate::auth::{PairingGate, is_pairing_pin_message};
use crate::capabilities;
use crate::chat::{ChatCommand, WorktreeCommand, parse_chat_command};
use crate::config::{Config, RepoConfig};
use crate::jobs::JobStore;
use crate::progress::{ProgressRateLimiter, render_progress};
use crate::runner::{AgentRequest, Runner};
use crate::session::{ChatStateStore, SessionState};
use crate::wn::MessageEvent;
use crate::workspace;
use crate::worktrees::{self, WorktreeStore};

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
            "Session: {}\nWorkspace: {}\nReady. Send /help.",
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
    approvals: ApprovalStore,
    attachments: AttachmentStore,
    worktrees: WorktreeStore,
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
        let approvals =
            ApprovalStore::open(&config.resolved_approvals_path()).context("opening approvals")?;
        let attachments = AttachmentStore::open(&config.resolved_attachments_path())
            .context("opening attachment store")?;
        let worktrees = WorktreeStore::open(&config.resolved_worktree_db_path())
            .context("opening worktrees")?;
        let runner = Runner::new(config.clone(), jobs.clone());
        let auth = AuthState::new(&config, pairing_gate);
        Ok(Self {
            config,
            config_path,
            jobs,
            runner,
            sessions,
            approvals,
            attachments,
            worktrees,
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
            return Ok(RouteAction::Reply(self.sender_not_allowed_text()));
        }

        let command = match parse_chat_command(text) {
            Ok(command) => command,
            Err(error) => {
                return Ok(RouteAction::Reply(invalid_command_text(
                    text,
                    &format!("{error:#}"),
                )));
            }
        };

        let session_key = session_key(group_id, sender);
        match command {
            ChatCommand::Help => Ok(RouteAction::Reply(help_text())),
            ChatCommand::Status => Ok(RouteAction::Reply(self.status_text(&session_key))),
            ChatCommand::Agents => Ok(RouteAction::Reply(capabilities::render_capabilities(
                &self.config,
            ))),
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
            ChatCommand::Approvals => Ok(RouteAction::Reply(approvals::render_pending(
                &self.approvals.pending(),
            ))),
            ChatCommand::Approve { approval_id } => {
                match self.approvals.approve(&approval_id, &session_key) {
                    Ok(request) => Ok(RouteAction::Run(request)),
                    Err(error) => Ok(RouteAction::Reply(format!(
                        "Error: approval failed: {error:#}"
                    ))),
                }
            }
            ChatCommand::Deny { approval_id } => {
                match self.approvals.deny(&approval_id, &session_key) {
                    Ok(()) => Ok(RouteAction::Reply(format!(
                        "Denied approval: {approval_id}"
                    ))),
                    Err(error) => Ok(RouteAction::Reply(format!("Error: deny failed: {error:#}"))),
                }
            }
            ChatCommand::Attachments => Ok(RouteAction::Reply(self.attachments_text())),
            ChatCommand::Attach { target } => {
                Ok(RouteAction::Reply(self.attach_text(target.as_deref())))
            }
            ChatCommand::Worktrees => Ok(RouteAction::Reply(self.worktrees_text(&session_key))),
            ChatCommand::Worktree(command) => Ok(RouteAction::Reply(
                self.worktree_text(&session_key, command),
            )),
            ChatCommand::Run(request) => match self.prepare_request(&session_key, request) {
                Ok(request) => {
                    if let Some(reason) = approvals::approval_reason(&self.config, &request) {
                        let approval = self.approvals.create(
                            &session_key,
                            request,
                            reason,
                            self.config.runner.approval_ttl_seconds,
                        )?;
                        Ok(RouteAction::Reply(approvals::render_approval_request(
                            &approval,
                        )))
                    } else {
                        Ok(RouteAction::Run(request))
                    }
                }
                Err(error) => Ok(RouteAction::Reply(format!("Error: {error:#}"))),
            },
        }
    }

    pub fn route_unsupported_message(
        &self,
        sender: Option<&str>,
        message: &str,
    ) -> Result<RouteAction> {
        if self.should_ignore_bot(sender) {
            return Ok(RouteAction::Ignore);
        }
        if self.should_ignore_sender(sender) {
            return Ok(RouteAction::Reply(self.sender_not_allowed_text()));
        }

        Ok(RouteAction::Reply(message.to_string()))
    }

    pub fn route_unsupported_event(&self, event: &MessageEvent) -> Result<RouteAction> {
        if self.should_ignore_bot(event.sender.as_deref()) {
            return Ok(RouteAction::Ignore);
        }
        if self.should_ignore_sender(event.sender.as_deref()) {
            return Ok(RouteAction::Reply(self.sender_not_allowed_text()));
        }

        if event.attachments.is_empty() {
            return Ok(RouteAction::Reply(
                event.unsupported.clone().unwrap_or_else(|| {
                    "Unsupported White Noise message. Send /help for commands.".to_string()
                }),
            ));
        }

        let record = self.attachments.add(
            event.group_id.clone(),
            event.sender.clone(),
            event.id.clone(),
            event.attachments.clone(),
        )?;
        Ok(RouteAction::Reply(format!(
            "Attachment saved: {}\nSend /attach {} for details.",
            attachments::render_record_summary(&record),
            record.id
        )))
    }

    pub fn route_initial_history_event(&self, event: &MessageEvent) -> Result<RouteAction> {
        if self.should_ignore_bot(event.sender.as_deref()) {
            return Ok(RouteAction::Ignore);
        }
        if let Some(reply) = self.try_pair_sender(event.sender.as_deref(), &event.text)? {
            return Ok(RouteAction::Reply(reply));
        }
        if self.should_ignore_sender(event.sender.as_deref()) {
            return Ok(RouteAction::Reply(self.sender_not_allowed_text()));
        }

        let text = event.text.trim();
        if text.is_empty() && event.attachments.is_empty() {
            return Ok(RouteAction::Ignore);
        }

        Ok(RouteAction::Reply(initial_history_text(text)))
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

    pub fn run_request_with_progress(
        &self,
        request: AgentRequest,
        send_progress: impl Fn(String) + Send + Sync + 'static,
    ) -> Result<String> {
        let mut limiter = ProgressRateLimiter::new(self.config.runner.progress_interval_seconds);
        let callback = Arc::new(Mutex::new(move |event: crate::progress::ProgressEvent| {
            if limiter.should_send(&event) {
                send_progress(render_progress(&event));
            }
        }));
        let record = self.runner.run_blocking_with_progress(
            request,
            Arc::new(move |event| {
                if let Ok(mut callback) = callback.lock() {
                    callback(event);
                }
            }),
        )?;
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

        [
            self.config.whitenoise.bot_sender.as_deref(),
            self.config.whitenoise.bot_npub.as_deref(),
            self.config.whitenoise.account.as_deref(),
        ]
        .into_iter()
        .flatten()
        .any(|bot| sender_id_matches(bot, sender))
    }

    fn should_ignore_sender(&self, sender: Option<&str>) -> bool {
        let Some(sender) = sender else {
            return self.auth.pairing_required();
        };

        self.auth.should_ignore_sender(sender)
    }

    fn sender_not_allowed_text(&self) -> String {
        if self.auth.pairing_required() {
            "Pairing required. Send the current desktop/SSH PIN as `/pair 123456`, then send `/help`."
                .to_string()
        } else {
            "This sender is not paired with agentnoise. Pair from the desktop or SSH terminal, then send `/help`."
                .to_string()
        }
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
                .any(|allowed| sender_id_matches(allowed, sender))
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
        session.worktree = None;
        session.worktree_path = None;
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
                let root = session.worktree_path.clone().unwrap_or(root);
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
            if let Some(root) = session.worktree_path {
                request = request.with_workspace_root(root);
            }
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
        let root = session.worktree_path.clone().unwrap_or(root);
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

    fn attachments_text(&self) -> String {
        let records = self.attachments.list_recent(10);
        if records.is_empty() {
            return "No attachments saved yet.".to_string();
        }
        let lines = records
            .iter()
            .enumerate()
            .map(|(index, record)| {
                format!(
                    "{}. {}",
                    index + 1,
                    attachments::render_record_summary(record)
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!("Attachments\n{lines}")
    }

    fn attach_text(&self, target: Option<&str>) -> String {
        let Some(target) = target.map(str::trim).filter(|target| !target.is_empty()) else {
            return "Usage: /attach <number|id>".to_string();
        };
        match self.attachments.get(target) {
            Some(record) => attachments::render_record_details(&record),
            None => format!("No matching attachment: {target}"),
        }
    }

    fn worktrees_text(&self, sender_key: &str) -> String {
        match self.session(sender_key) {
            Ok(session) => {
                let Some(alias) = session.repo_alias.as_deref() else {
                    return "No repo selected. Send /use <repo>.".to_string();
                };
                let records = self.worktrees.list(Some(alias));
                worktrees::render_worktrees(&records, session.worktree.as_deref())
            }
            Err(error) => format!("Error: workspace failed: {error:#}"),
        }
    }

    fn worktree_text(&self, sender_key: &str, command: WorktreeCommand) -> String {
        let session = match self.session(sender_key) {
            Ok(session) => session,
            Err(error) => return format!("Error: workspace failed: {error:#}"),
        };
        let Some(alias) = session.repo_alias.clone() else {
            return "No repo selected. Send /use <repo>.".to_string();
        };

        match command {
            WorktreeCommand::New { name } => {
                match self.worktrees.create(&self.config, &alias, &name) {
                    Ok(record) => self.switch_to_worktree(sender_key, session, &record),
                    Err(error) => format!("Error: worktree create failed: {error:#}"),
                }
            }
            WorktreeCommand::Use { name } => {
                let Some(record) = self.worktrees.find(&alias, &name) else {
                    return format!("Unknown worktree: {name}");
                };
                self.switch_to_worktree(sender_key, session, &record)
            }
            WorktreeCommand::Remove { name, confirm } => {
                if !confirm {
                    return format!("Send /worktree remove {name} confirm to remove it.");
                }
                match self.worktrees.remove(&self.config, &alias, &name) {
                    Ok(record) => {
                        if session.worktree.as_deref() == Some(record.name.as_str()) {
                            let mut session = session;
                            session.worktree = None;
                            session.worktree_path = None;
                            session.cwd = ".".to_string();
                            self.sessions.set(sender_key, session).ok();
                        }
                        format!("Removed worktree: {}", record.name)
                    }
                    Err(error) => format!("Error: worktree remove failed: {error:#}"),
                }
            }
        }
    }

    fn switch_to_worktree(
        &self,
        sender_key: &str,
        mut session: SessionState,
        record: &worktrees::WorktreeRecord,
    ) -> String {
        session.repo_alias = Some(record.repo_alias.clone());
        session.cwd = ".".to_string();
        session.worktree = Some(record.name.clone());
        session.worktree_path = Some(record.path.clone());
        match self.sessions.set(sender_key, session.clone()) {
            Ok(()) => format!(
                "Worktree: {}\nWorkspace: {}",
                record.name,
                workspace_text(&session)
            ),
            Err(error) => format!("Error: failed to save worktree: {error:#}"),
        }
    }
}

fn sender_id_matches(configured: &str, sender: &str) -> bool {
    let configured = configured.trim();
    let sender = sender.trim();
    if configured.is_empty() || sender.is_empty() {
        return false;
    }
    if configured == sender {
        return true;
    }

    match (
        nostr_public_key_hex(configured),
        nostr_public_key_hex(sender),
    ) {
        (Some(configured), Some(sender)) => configured == sender,
        _ => false,
    }
}

fn nostr_public_key_hex(value: &str) -> Option<String> {
    PublicKey::parse(value.trim())
        .ok()
        .map(|public_key| public_key.to_hex())
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
        !allowed
            .iter()
            .any(|allowed| sender_id_matches(allowed, sender))
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
            if !allowed
                .iter()
                .any(|allowed| sender_id_matches(allowed, sender))
            {
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
    let Some(alias) = session.repo_alias.as_ref() else {
        return "none".to_string();
    };
    let suffix = session
        .worktree
        .as_ref()
        .map(|name| format!(" [wt:{name}]"))
        .unwrap_or_default();
    format!(
        "{}:{}{}",
        alias,
        workspace::display_cwd(&session.cwd),
        suffix
    )
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

fn invalid_command_text(text: &str, error: &str) -> String {
    let text = text.trim();
    if text.is_empty() {
        return "I received an empty message. Send /help for commands, or try /codex <prompt>."
            .to_string();
    }

    if error.contains("not a command") {
        return format!(
            "I received: {}\nagentnoise only runs explicit commands. Send /help for commands, or try /codex <prompt>.",
            preview_text(text)
        );
    }

    format!("Command not run: {error}\nSend /help for commands, or try /codex <prompt>.")
}

fn initial_history_text(text: &str) -> String {
    let text = text.trim();
    if text.is_empty() {
        return "I saw an older White Noise item while catching up after startup, so I did not run it. Send it again now, or send /help."
            .to_string();
    }

    format!(
        "I saw this while catching up after startup, so I did not run it:\n{}\nSend it again now, or send /help.",
        preview_text(text)
    )
}

fn preview_text(text: &str) -> String {
    const MAX: usize = 180;
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= MAX {
        return collapsed;
    }

    let mut output = collapsed.chars().take(MAX).collect::<String>();
    output.push_str("...");
    output
}

fn help_text() -> String {
    [
        "agentnoise commands",
        "",
        "Workspace",
        "/status",
        "/agents",
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
        "/hermes <prompt>",
        "/hermes <repo> <prompt>",
        "/hermes-resume <session> <prompt>",
        "/wiki <prompt>",
        "/codex-wiki <prompt>",
        "/claude-wiki <prompt>",
        "",
        "Jobs",
        "/jobs",
        "/tail <job>",
        "/cancel <job>",
        "/approvals",
        "/approve <approval>",
        "/deny <approval>",
        "/attachments",
        "/attach <number|id>",
        "",
        "Worktrees",
        "/worktrees",
        "/worktree new <name>",
        "/worktree use <name>",
        "/worktree remove <name> confirm",
        "",
        "/help",
    ]
    .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr::nips::nip19::ToBech32;

    fn test_public_key_pair() -> (String, String) {
        let hex = "1111111111111111111111111111111111111111111111111111111111111111";
        let npub = PublicKey::from_hex(hex).unwrap().to_bech32().unwrap();
        (hex.to_string(), npub)
    }

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
    fn bot_sender_matches_hex_author_when_configured_as_npub() {
        let temp = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let (bot_hex, bot_npub) = test_public_key_pair();
        let config_path = temp.path().join("config.toml");
        let mut config = Config::template();
        config.whitenoise.bot_npub = Some(bot_npub.clone());
        config.whitenoise.account = Some(bot_npub);
        config.runner.data_dir = temp.path().join("data").display().to_string();
        config.runner.log_dir = temp.path().join("logs").display().to_string();
        config.repos[0].path = repo.path().display().to_string();
        config.save(&config_path).unwrap();
        let app = AgentApp::new_with_auth(config_path, config, None).unwrap();

        assert!(matches!(
            app.route_message(Some("group-a"), Some(&bot_hex), "/help")
                .unwrap(),
            RouteAction::Ignore
        ));
    }

    #[test]
    fn allowed_sender_matches_hex_author_when_configured_as_npub() {
        let temp = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let (phone_hex, phone_npub) = test_public_key_pair();
        let config_path = temp.path().join("config.toml");
        let mut config = Config::template();
        config.whitenoise.allowed_senders = vec![phone_npub];
        config.runner.data_dir = temp.path().join("data").display().to_string();
        config.runner.log_dir = temp.path().join("logs").display().to_string();
        config.repos[0].path = repo.path().display().to_string();
        config.save(&config_path).unwrap();
        let app = AgentApp::new_with_auth(config_path, config, None).unwrap();

        assert!(matches!(
            app.route_message(Some("group-a"), Some(&phone_hex), "/help")
                .unwrap(),
            RouteAction::Reply(reply) if reply.contains("agentnoise commands")
        ));
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
    fn unsupported_messages_reply_with_auth_guidance() {
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
            RouteAction::Reply(reply) if reply.contains("not paired")
        ));
    }

    #[test]
    fn bare_text_gets_helpful_reply_instead_of_ignore() {
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
            app.route_message(Some("group-a"), Some("phone"), "Test")
                .unwrap(),
            RouteAction::Reply(reply) if reply.contains("I received: Test")
                && reply.contains("/codex <prompt>")
        ));
        assert!(matches!(
            app.route_message(Some("group-a"), Some("phone"), "/wat")
                .unwrap(),
            RouteAction::Reply(reply) if reply.contains("unknown command")
                && reply.contains("/help")
        ));
    }

    #[test]
    fn initial_history_gets_catchup_reply_instead_of_silent_drop() {
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
        let event = MessageEvent {
            group_id: Some("group-a".to_string()),
            sender: Some("phone".to_string()),
            text: "/codex run this".to_string(),
            unsupported: None,
            id: Some("event-a".to_string()),
            trigger: None,
            is_initial: true,
            attachments: Vec::new(),
        };

        assert!(matches!(
            app.route_initial_history_event(&event).unwrap(),
            RouteAction::Reply(reply) if reply.contains("catching up after startup")
                && reply.contains("/codex run this")
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
