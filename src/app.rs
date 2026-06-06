use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use nostr::PublicKey;
use uuid::Uuid;

use crate::approvals::{self, ApprovalStore};
use crate::attachments::{self, AttachmentRecord, AttachmentStore};
use crate::auth::{PairingGate, is_pairing_pin_message};
use crate::capabilities;
use crate::chat::{ChatCommand, WorktreeCommand, parse_chat_command};
use crate::config::{Config, RepoConfig};
use crate::jobs::{JobRecord, JobStatus, JobStore};
use crate::progress::{ProgressKind, ProgressRateLimiter, render_progress};
use crate::runner::{AgentRequest, Runner};
use crate::session::{ChatStateStore, SessionState};
use crate::subscriptions::{self, SubscriptionState};
use crate::text::{mobile_digest, short_ref};
use crate::wn::MessageEvent;
use crate::workspace;
use crate::worktrees::{self, WorktreeStore};

#[derive(Debug)]
pub enum RouteAction {
    Ignore,
    Reply(String),
    IngestAttachments(AttachmentIngestAction),
    NewSession(NewSessionRequest),
    ResumeSession(ResumeSessionRequest),
    DownloadMedia(MediaDownloadAction),
    Run(AgentRequest),
}

#[derive(Debug, Clone)]
pub struct NewSessionRequest {
    pub name: String,
    pub group_name: String,
    pub sender: String,
    pub state: SessionState,
}

impl NewSessionRequest {
    pub fn group_name(&self) -> String {
        self.group_name.clone()
    }

    pub fn ready_text(&self) -> String {
        format!(
            "Ready: {}\nWorkspace: {}\nTry: /codex <prompt>",
            self.name,
            workspace_text(&self.state)
        )
    }

    pub fn created_text(&self) -> String {
        format!("Created chat: {}\nUse /list to switch.", self.name)
    }

    pub fn created_text_for_group(&self, group_id: &str) -> String {
        format!(
            "Created chat: {}\nOpen: {}",
            self.name,
            white_noise_chat_uri(group_id)
        )
    }

    pub fn job_started_text_for_group(&self, group_id: &str) -> String {
        format!(
            "Started work chat: {}\nOpen: {}",
            self.name,
            white_noise_chat_uri(group_id)
        )
    }

    pub fn job_ready_text(&self, ack: &str) -> String {
        ack.to_string()
    }
}

#[derive(Debug, Clone)]
pub struct ResumeSessionRequest {
    pub group_id: String,
    pub reply_text: String,
    pub target_text: String,
}

#[derive(Debug, Clone)]
pub struct AttachmentIngestAction {
    pub record: AttachmentRecord,
}

#[derive(Debug, Clone)]
pub struct MediaDownloadAction {
    pub record_id: String,
    pub attachment_index: usize,
    pub original_file_hash: String,
    pub output_path: PathBuf,
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

        let session_key = session_key(group_id, sender);
        let command = match parse_chat_command(text) {
            Ok(command) => command,
            Err(error) => {
                if !text.trim_start().starts_with('/') {
                    return self.route_bare_text(group_id, &session_key, text);
                }
                return Ok(RouteAction::Reply(invalid_command_text(
                    text,
                    &format!("{error:#}"),
                )));
            }
        };

        match command {
            ChatCommand::Help => Ok(RouteAction::Reply(help_text())),
            ChatCommand::Status => Ok(RouteAction::Reply(self.status_text(&session_key))),
            ChatCommand::Agents => Ok(RouteAction::Reply(capabilities::render_capabilities(
                &self.config,
            ))),
            ChatCommand::AgentSessions { limit } => Ok(RouteAction::Reply(
                crate::local_sessions::render_local_sessions(limit.unwrap_or(8).clamp(1, 20)),
            )),
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
            ChatCommand::Download { target, index } => {
                self.download_media_action(group_id, &session_key, &target, index)
            }
            ChatCommand::Worktrees => Ok(RouteAction::Reply(self.worktrees_text(&session_key))),
            ChatCommand::Worktree(command) => Ok(RouteAction::Reply(
                self.worktree_text(&session_key, command),
            )),
            ChatCommand::Run(request) => self.route_run_request(group_id, &session_key, request),
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
        Ok(RouteAction::IngestAttachments(AttachmentIngestAction {
            record,
        }))
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
        Ok(render_job_reply(&record))
    }

    pub fn run_ack_text(&self, request: &AgentRequest) -> String {
        if let Some(session) = request.resume_session.as_deref() {
            return format!(
                "Queued resume.\n{} · session {}\nI'll post the answer here.",
                request.agent,
                short_ref(session)
            );
        }

        let workspace = request_workspace_text(request);
        if workspace == "selected workspace" {
            format!("Queued.\n{}\nI'll post the answer here.", request.agent)
        } else {
            format!(
                "Queued.\n{} · {}\nI'll post the answer here.",
                request.agent, workspace
            )
        }
    }

    pub fn run_request_with_progress(
        &self,
        request: AgentRequest,
        send_progress: impl Fn(String) + Send + Sync + 'static,
    ) -> Result<String> {
        let record = self.run_request_record_with_progress(request, send_progress)?;
        Ok(render_job_reply(&record))
    }

    pub fn run_request_record_with_progress(
        &self,
        request: AgentRequest,
        send_progress: impl Fn(String) + Send + Sync + 'static,
    ) -> Result<JobRecord> {
        let mut limiter = ProgressRateLimiter::new(self.config.runner.progress_interval_seconds);
        let progress_mode = self.config.runner.progress_mode;
        let callback = Arc::new(Mutex::new(move |event: crate::progress::ProgressEvent| {
            if event.kind == ProgressKind::Finished {
                return;
            }
            if let Some(text) = render_progress(&event, progress_mode)
                && limiter.should_send(&event)
            {
                send_progress(text);
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
        Ok(record)
    }

    pub fn render_job_record(&self, record: &JobRecord) -> String {
        render_job_reply(record)
    }

    pub fn create_session_record(&self, group_id: &str, state: SessionState) -> Result<String> {
        let key = session_key(Some(group_id), None);
        self.sessions.set(&key, state)?;
        Ok(key)
    }

    pub fn session_context_text_for_group(&self, group_id: &str) -> Result<Option<String>> {
        let group_id = group_id.trim();
        if group_id.is_empty() || self.is_primary_group(group_id) {
            return Ok(None);
        }

        let key = session_key(Some(group_id), None);
        let session = self.session(&key)?;
        let mut lines = Vec::new();
        lines.push(format!(
            "chat label: {}",
            session_display_name(&key, &session)
        ));
        lines.push(format!("workspace: {}", workspace_text(&session)));
        if let Some(agent) = session.default_agent {
            lines.push(format!("default agent: {agent}"));
        }
        if let Some(prefix) = session.default_prompt_prefix.as_deref() {
            lines.push(format!("default prompt prefix: {prefix}"));
        }
        Ok(Some(lines.join("\n")))
    }

    pub fn record_attachment_downloaded(
        &self,
        record_id: &str,
        attachment_index: usize,
        path: &Path,
        size: Option<u64>,
    ) -> Result<()> {
        self.attachments
            .set_local_path(record_id, attachment_index, path, size)
            .map(|_| ())
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

        Ok(Some("paired\nsend /help".to_string()))
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
        let session = self.session(sender_key).ok();
        let session_name = session
            .as_ref()
            .map(|session| session_display_name(sender_key, session))
            .unwrap_or_else(|| "none".to_string());
        let workspace = session
            .as_ref()
            .map(workspace_text)
            .unwrap_or_else(|| "none".to_string());

        let subscription_status = subscription_status_line(&self.config)
            .map(|line| format!("\n{line}"))
            .unwrap_or_default();

        format!(
            "agentnoise: running\nlauncher: {}\nchat: {}\nworkspace: {}\njobs: {active} active\nchats: {group_count}\nrepos: {}",
            self.config.runner.launcher,
            session_name,
            workspace,
            self.config.repos.len()
        ) + &subscription_status
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
            group_name: format!("agentnoise: {name}"),
            name,
            sender,
            state,
        }))
    }

    pub fn job_session_request(
        &self,
        group_id: Option<&str>,
        sender: Option<&str>,
        request: &AgentRequest,
    ) -> Result<Option<NewSessionRequest>> {
        if request.resume_session.is_some() {
            return Ok(None);
        }
        let Some(group_id) = group_id.map(str::trim).filter(|group| !group.is_empty()) else {
            return Ok(None);
        };
        let primary_group = self.config.whitenoise.group_id.trim();
        if primary_group.is_empty() || group_id != primary_group {
            return Ok(None);
        }
        let Some(sender) = sender
            .map(str::trim)
            .filter(|sender| !sender.is_empty())
            .map(str::to_string)
        else {
            return Ok(None);
        };

        let group_name = job_group_name(&local_hostname(), &request.prompt);
        let name = group_name.clone();
        let mut state = self.session(&session_key(Some(group_id), Some(&sender)))?;
        state.name = Some(name.clone());
        state.closed = false;
        set_session_default_request(&mut state, request);

        Ok(Some(NewSessionRequest {
            name,
            group_name,
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

        Ok(format!("session {name}\n{}", workspace_text(&session)))
    }

    fn sessions_text(&self, sender_key: &str) -> String {
        let sessions = self.session_entries(sender_key);
        if sessions.is_empty() {
            return "No sessions yet. Send /rename <name> to name this chat, or /new <name> to create another.".to_string();
        }

        let lines = sessions
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let status = session_status_label(entry.key == sender_key, entry.state.closed);
                format!(
                    "{}. {}{}\n   g-{} | {}\n   /jump {}",
                    index + 1,
                    entry.name,
                    status,
                    short_group_id(&entry.group_id),
                    workspace_text(&entry.state),
                    index + 1,
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        format!("sessions\n{lines}")
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
                        "Usage: /jump <number|name|id>".to_string(),
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

        let current_group_id = current_group_id.map(str::trim).unwrap_or_default();
        let target_text = if current_group_id.is_empty() || current_group_id == entry.group_id {
            format!(
                "session {}\n{}\nready: /pwd /codex <prompt>",
                entry.name,
                workspace_text(&state)
            )
        } else {
            format!(
                "session {}\n{}\nready: /pwd /codex <prompt>\nback: {}",
                entry.name,
                workspace_text(&state),
                white_noise_chat_uri(current_group_id)
            )
        };
        if current_group_id == entry.group_id {
            return Ok(RouteAction::Reply(target_text));
        }

        Ok(RouteAction::ResumeSession(ResumeSessionRequest {
            group_id: entry.group_id.clone(),
            reply_text: format!(
                "resumed {}\nopen: {}\ncontinue there",
                entry.name,
                white_noise_chat_uri(&entry.group_id)
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
        Ok(format!(
            "closed {name}\n/list to switch\n/jump {name} to reopen"
        ))
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
            Ok(()) => format!("workspace {repo_alias}:/"),
            Err(error) => format!("Error: failed to save workspace: {error:#}"),
        }
    }

    fn pwd_text(&self, sender_key: &str) -> String {
        match self.session(sender_key) {
            Ok(session) => match session.repo_alias {
                Some(alias) => {
                    format!("workspace {alias}:{}", workspace::display_cwd(&session.cwd))
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
                                "workspace {alias}:{}",
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
            if let Some(session) = request.resume_session.clone()
                && let Some(full_session) =
                    crate::local_sessions::resolve_session_id(request.agent, &session)?
            {
                request.resume_session = Some(full_session);
            }
            self.config.effective_agent_profile_for_request(&request)?;
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

        self.config.effective_agent_profile_for_request(&request)?;
        Ok(request)
    }

    fn route_bare_text(
        &self,
        group_id: Option<&str>,
        session_key: &str,
        text: &str,
    ) -> Result<RouteAction> {
        let prompt = text.trim();
        if prompt.is_empty() {
            return Ok(RouteAction::Reply(invalid_command_text(
                text,
                "not a command",
            )));
        }
        let Some(group_id) = group_id
            .map(str::trim)
            .filter(|group_id| !group_id.is_empty())
        else {
            return Ok(RouteAction::Reply(invalid_command_text(
                text,
                "not a command",
            )));
        };
        if self.is_primary_group(group_id) {
            return Ok(RouteAction::Reply(invalid_command_text(
                text,
                "not a command",
            )));
        }

        let session = self.session(session_key)?;
        let Some(agent) = session.default_agent else {
            return Ok(RouteAction::Reply(invalid_command_text(
                text,
                "not a command",
            )));
        };
        let mut request = AgentRequest::prompt(agent, apply_prompt_prefix(&session, prompt));
        if let Some(repo_alias) = session.repo_alias.clone() {
            request = request.with_workspace(repo_alias, session.cwd.clone());
        }
        if let Some(worktree_path) = session.worktree_path.clone() {
            request = request.with_workspace_root(worktree_path);
        }
        if let Some(profile) = session.default_profile.clone() {
            request = request.with_profile(profile);
        }
        self.route_run_request(Some(group_id), session_key, request)
    }

    fn route_run_request(
        &self,
        group_id: Option<&str>,
        session_key: &str,
        request: AgentRequest,
    ) -> Result<RouteAction> {
        match self.prepare_request(session_key, request) {
            Ok(request) => {
                if request.resume_session.is_none() && !self.is_primary_group_opt(group_id) {
                    let mut session = self.session(session_key)?;
                    set_session_default_request(&mut session, &request);
                    self.sessions.set(session_key, session)?;
                }
                if let Some(reason) = approvals::approval_reason(&self.config, &request) {
                    let approval = self.approvals.create(
                        session_key,
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
        }
    }

    fn is_primary_group_opt(&self, group_id: Option<&str>) -> bool {
        group_id
            .map(str::trim)
            .filter(|group| !group.is_empty())
            .is_some_and(|group| self.is_primary_group(group))
    }

    fn is_primary_group(&self, group_id: &str) -> bool {
        let primary_group = self.config.whitenoise.group_id.trim();
        !primary_group.is_empty() && group_id == primary_group
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
                    short_ref(&job.id),
                    job.status,
                    job.agent,
                    job.repo_alias.as_deref().unwrap_or("-")
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        format!("jobs\n{lines}")
    }

    fn tail_text(&self, job_id: &str) -> String {
        match self.jobs.tail(job_id, 2400) {
            Ok(Some(text)) if !text.trim().is_empty() => text,
            Ok(Some(_)) => format!("{} log is empty.", short_ref(job_id)),
            Ok(None) => format!("No such job: {job_id}"),
            Err(error) => format!("Error: tail failed: {error:#}"),
        }
    }

    fn cancel_text(&self, job_id: &str) -> String {
        match self.runner.cancel(job_id) {
            Ok(true) => format!("cancel requested: {}", short_ref(job_id)),
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

    fn download_media_action(
        &self,
        group_id: Option<&str>,
        sender_key: &str,
        target: &str,
        index: Option<usize>,
    ) -> Result<RouteAction> {
        let Some(group_id) = group_id.map(str::trim).filter(|group| !group.is_empty()) else {
            return Ok(RouteAction::Reply(
                "Error: /download only works inside a White Noise chat.".to_string(),
            ));
        };
        let Some(record) = self.attachments.get(target) else {
            return Ok(RouteAction::Reply(format!(
                "No matching attachment: {target}"
            )));
        };
        if record
            .group_id
            .as_deref()
            .map(str::trim)
            .filter(|record_group| !record_group.is_empty())
            .is_some_and(|record_group| record_group != group_id)
        {
            return Ok(RouteAction::Reply(
                "Error: that attachment belongs to a different chat.".to_string(),
            ));
        }
        let attachment_index = index.unwrap_or(1).saturating_sub(1);
        let Some(attachment) = record.attachments.get(attachment_index) else {
            return Ok(RouteAction::Reply(format!(
                "No file {} on attachment {}",
                attachment_index + 1,
                record.id
            )));
        };
        let Some(original_file_hash) = attachment
            .hash
            .as_deref()
            .map(str::trim)
            .filter(|hash| !hash.is_empty())
            .map(str::to_string)
        else {
            return Ok(RouteAction::Reply(format!(
                "Attachment {} file {} does not include a downloadable White Noise media hash.",
                record.id,
                attachment_index + 1
            )));
        };
        Ok(RouteAction::DownloadMedia(MediaDownloadAction {
            output_path: self.attachment_download_path_for_session(
                sender_key,
                &record.id,
                attachment_index,
                attachment,
            ),
            record_id: record.id,
            attachment_index,
            original_file_hash,
        }))
    }

    pub fn attachment_download_path_for_message(
        &self,
        group_id: Option<&str>,
        sender: Option<&str>,
        record_id: &str,
        attachment_index: usize,
        attachment: &attachments::AttachmentInfo,
    ) -> PathBuf {
        self.attachment_download_path_for_session(
            &session_key(group_id, sender),
            record_id,
            attachment_index,
            attachment,
        )
    }

    fn attachment_download_path_for_session(
        &self,
        session_key: &str,
        record_id: &str,
        attachment_index: usize,
        attachment: &attachments::AttachmentInfo,
    ) -> PathBuf {
        self.session_attachment_root(session_key)
            .unwrap_or_else(|| self.config.resolved_data_dir().join("attachments"))
            .join(record_id)
            .join(self.attachment_file_name(attachment_index, attachment))
    }

    fn session_attachment_root(&self, session_key: &str) -> Option<PathBuf> {
        let session = self.session(session_key).ok()?;
        let alias = session.repo_alias.as_deref()?;
        let root = session
            .worktree_path
            .clone()
            .or_else(|| self.config.repo_path(alias))?;
        let workdir = workspace::resolve_cwd(&root, Some(&session.cwd)).ok()?;
        Some(workdir.join(".agentnoise").join("attachments"))
    }

    fn attachment_file_name(
        &self,
        attachment_index: usize,
        attachment: &attachments::AttachmentInfo,
    ) -> String {
        let name = attachment
            .name
            .as_deref()
            .map(attachments::safe_file_name)
            .unwrap_or_else(|| "attachment".to_string());
        format!("{:02}-{name}", attachment_index + 1)
    }

    pub fn attachment_download_path(
        &self,
        record_id: &str,
        attachment_index: usize,
        attachment: &attachments::AttachmentInfo,
    ) -> PathBuf {
        self.config
            .resolved_data_dir()
            .join("attachments")
            .join(record_id)
            .join(self.attachment_file_name(attachment_index, attachment))
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

fn subscription_status_line(config: &Config) -> Option<String> {
    let snapshot = subscriptions::read_snapshot(&config.resolved_subscriptions_path())
        .ok()
        .flatten()?;
    let total = snapshot.groups.len();
    if total == 0 {
        return None;
    }
    let stale = snapshot.groups.iter().filter(|group| group.stale).count();
    let running = snapshot
        .groups
        .iter()
        .filter(|group| group.state == SubscriptionState::Running && !group.stale)
        .count();
    let restarting = snapshot
        .groups
        .iter()
        .filter(|group| {
            matches!(
                group.state,
                SubscriptionState::Restarting
                    | SubscriptionState::Exited
                    | SubscriptionState::Failed
            )
        })
        .count();
    if stale == 0 && restarting == 0 {
        Some(format!("subs: {running}/{total} ok"))
    } else {
        Some(format!(
            "subs: {running}/{total} ok, {stale} stale, {restarting} restarting"
        ))
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
    group_id.chars().take(5).collect()
}

fn white_noise_chat_uri(group_id: &str) -> String {
    format!("whitenoise://chat/{}", group_id.trim())
}

fn session_status_label(current: bool, closed: bool) -> &'static str {
    match (current, closed) {
        (true, true) => " (current, closed)",
        (true, false) => " (current)",
        (false, true) => " (closed)",
        (false, false) => "",
    }
}

fn generated_session_name() -> String {
    let uuid = Uuid::new_v4().simple().to_string();
    format!("session-{}", &uuid[..6])
}

fn local_hostname() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .and_then(|host| normalize_hostname(&host))
        .or_else(|| {
            Command::new("hostname")
                .output()
                .ok()
                .and_then(|output| output.status.success().then_some(output.stdout))
                .and_then(|stdout| String::from_utf8(stdout).ok())
                .and_then(|host| normalize_hostname(&host))
        })
        .unwrap_or_else(|| "desktop".to_string())
}

fn normalize_hostname(host: &str) -> Option<String> {
    let host = host.trim().split('.').next().unwrap_or(host.trim());
    let normalized = host
        .chars()
        .filter_map(|ch| {
            let ch = ch.to_ascii_lowercase();
            if ch.is_ascii_alphanumeric() || ch == '-' {
                Some(ch)
            } else {
                None
            }
        })
        .collect::<String>();
    (!normalized.is_empty()).then_some(normalized)
}

fn job_summary(prompt: &str) -> String {
    let words = prompt
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter_map(|word| {
            let word = word.trim().to_ascii_lowercase();
            if word.len() < 2 || is_summary_stopword(&word) {
                None
            } else {
                Some(word)
            }
        })
        .take(4)
        .collect::<Vec<_>>();

    match words.len() {
        0 => "agent job".to_string(),
        1 => format!("{} job", words[0]),
        _ => words.join(" "),
    }
}

fn job_group_name(host: &str, prompt: &str) -> String {
    let host = normalize_hostname(host).unwrap_or_else(|| "desktop".to_string());
    format!("{host} - {}", job_summary(prompt))
}

fn is_summary_stopword(word: &str) -> bool {
    matches!(
        word,
        "wiki"
            | "the"
            | "and"
            | "for"
            | "with"
            | "that"
            | "this"
            | "from"
            | "into"
            | "about"
            | "please"
            | "what"
            | "whats"
            | "can"
            | "you"
            | "does"
            | "have"
            | "are"
            | "like"
    )
}

fn session_display_name(key: &str, session: &SessionState) -> String {
    if let Some(name) = session.name.as_deref() {
        return name.to_string();
    }
    if let Some(group_id) = key.strip_prefix("group:") {
        let short = short_group_id(group_id);
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

fn set_session_default_request(session: &mut SessionState, request: &AgentRequest) {
    session.default_agent = Some(request.agent);
    session.default_profile = request.profile.clone();
    session.default_prompt_prefix = request_prompt_prefix(request);
}

fn request_prompt_prefix(request: &AgentRequest) -> Option<String> {
    let prompt = request.prompt.trim();
    match request.agent {
        crate::runner::AgentKind::Codex if prompt == "@wiki" || prompt.starts_with("@wiki ") => {
            Some("@wiki".to_string())
        }
        crate::runner::AgentKind::Claude if prompt == "wiki" || prompt.starts_with("wiki ") => {
            Some("wiki".to_string())
        }
        _ => None,
    }
}

fn apply_prompt_prefix(session: &SessionState, prompt: &str) -> String {
    let prompt = prompt.trim();
    let Some(prefix) = session.default_prompt_prefix.as_deref() else {
        return prompt.to_string();
    };
    if prompt == prefix || prompt.starts_with(&format!("{prefix} ")) {
        prompt.to_string()
    } else {
        format!("{prefix} {prompt}")
    }
}

fn render_job_reply(record: &JobRecord) -> String {
    let id = short_ref(&record.id);
    let status = job_status_label(record.status);
    let summary = record
        .summary
        .as_deref()
        .map(str::trim)
        .filter(|summary| !summary.is_empty())
        .unwrap_or("no output captured");

    let digest = mobile_digest(summary, 900, 10);
    let detail_label = if digest.truncated {
        "Full answer"
    } else {
        "Details"
    };

    format!(
        "{status} · {id}\n{}\n\n{detail_label}: /tail {id}",
        digest.text
    )
}

fn job_status_label(status: JobStatus) -> &'static str {
    match status {
        JobStatus::Succeeded => "Done",
        JobStatus::Failed => "Failed",
        JobStatus::Cancelled => "Cancelled",
        JobStatus::Interrupted => "Interrupted",
        JobStatus::Pending => "Pending",
        JobStatus::Running => "Running",
        JobStatus::CancelRequested => "Cancelling",
    }
}

fn request_workspace_text(request: &AgentRequest) -> String {
    if let Some(repo) = request.repo_alias.as_deref() {
        return format!(
            "{}:{}",
            repo,
            workspace::display_cwd(request.cwd.as_deref().unwrap_or_default())
        );
    }
    if let Some(root) = request.workspace_root.as_deref() {
        return root.display().to_string();
    }
    "selected workspace".to_string()
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
        "commands",
        "",
        "chat",
        "/new [name]",
        "/rename [name]",
        "/list",
        "/jump <number|name|id>",
        "/resume <number|name|id>",
        "/close",
        "",
        "workspace",
        "/status",
        "/repos",
        "/use <repo>",
        "/pwd",
        "/ls [path]",
        "/cd <path>",
        "",
        "agents",
        "/agents",
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
        "jobs",
        "/jobs",
        "/tail <job>",
        "/cancel <job>",
        "/approvals",
        "/approve <approval>",
        "/deny <approval>",
        "/attachments",
        "/attach <number|id>",
        "/download <number|id> [file-number]",
        "",
        "sessions",
        "/agent-sessions [limit]",
        "",
        "worktrees",
        "/worktrees",
        "/worktree new <name>",
        "/worktree use <name>",
        "/worktree remove <name> confirm",
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
            RouteAction::Reply(reply) if reply.contains("paired")
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
    fn status_reply_includes_subscription_health_when_available() {
        let temp = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let config_path = temp.path().join("config.toml");
        let mut config = Config::template();
        config.whitenoise.allowed_senders = vec!["phone".to_string()];
        config.whitenoise.group_id = "group-a".to_string();
        config.runner.data_dir = temp.path().join("data").display().to_string();
        config.runner.log_dir = temp.path().join("logs").display().to_string();
        config.repos[0].path = repo.path().display().to_string();
        config.save(&config_path).unwrap();
        subscriptions::write_snapshot(
            &config.resolved_subscriptions_path(),
            &subscriptions::SubscriptionSnapshot {
                version: 1,
                updated_at: "2026-05-21T00:00:00Z".to_string(),
                groups: vec![subscriptions::SubscriptionStatus {
                    group_id: "group-a".to_string(),
                    state: SubscriptionState::Running,
                    pid: Some(42),
                    started_at: Some("2026-05-21T00:00:00Z".to_string()),
                    last_json_at: None,
                    last_event_at: None,
                    last_error_at: None,
                    last_error: None,
                    last_exit_at: None,
                    last_exit_status: None,
                    restart_count: 0,
                    parse_error_count: 0,
                    last_poll_at: None,
                    latest_polled_message_id: None,
                    latest_stream_message_id: None,
                    latest_journaled_message_id: None,
                    recovered_inbound: 0,
                    stale: false,
                }],
            },
        )
        .unwrap();

        let app = AgentApp::new_with_auth(config_path, config, None).unwrap();

        assert!(matches!(
            app.route_message(Some("group-a"), Some("phone"), "/status")
                .unwrap(),
            RouteAction::Reply(reply) if reply.contains("agentnoise: running")
                && reply.contains("workspace: sandbox:/")
                && reply.contains("subs: 1/1 ok")
        ));
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
            RouteAction::Reply(reply) if reply.contains("commands")
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
    fn media_event_is_saved_and_downloadable() {
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
            text: String::new(),
            unsupported: Some("Attachment received".to_string()),
            id: Some("msg1".to_string()),
            trigger: None,
            is_initial: false,
            attachments: vec![attachments::AttachmentInfo {
                kind: "attachments".to_string(),
                name: Some("shot.png".to_string()),
                mime_type: Some("image/png".to_string()),
                url: Some("https://blossom.example/shot".to_string()),
                size: None,
                hash: Some("11".repeat(32)),
                local_path: None,
            }],
        };
        assert!(matches!(
            app.route_unsupported_event(&event).unwrap(),
            RouteAction::IngestAttachments(request)
                if request.record.attachments.len() == 1
                    && request.record.attachments[0]
                        .hash
                        .as_deref()
                        .is_some_and(|hash| hash == "11".repeat(32))
        ));

        let action = app
            .route_message(Some("group-a"), Some("phone"), "/download 1")
            .unwrap();
        assert!(matches!(
            action,
            RouteAction::DownloadMedia(request)
                if request.original_file_hash == "11".repeat(32)
                    && request.output_path == repo
                        .path()
                        .canonicalize()
                        .unwrap()
                        .join(".agentnoise/attachments")
                        .join(request.record_id.clone())
                        .join("01-shot.png")
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
    fn inbox_bare_text_stays_command_only() {
        let temp = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let config_path = temp.path().join("config.toml");
        let mut config = Config::template();
        config.whitenoise.group_id = "inbox".to_string();
        config.whitenoise.group_ids = vec!["inbox".to_string()];
        config.whitenoise.allowed_senders = vec!["phone".to_string()];
        config.runner.data_dir = temp.path().join("data").display().to_string();
        config.runner.log_dir = temp.path().join("logs").display().to_string();
        config.repos[0].path = repo.path().display().to_string();
        config.save(&config_path).unwrap();
        let app = AgentApp::new_with_auth(config_path, config, None).unwrap();

        let mut state = SessionState::new(Some("sandbox".to_string()));
        state.default_agent = Some(crate::runner::AgentKind::Codex);
        app.create_session_record("inbox", state).unwrap();

        assert!(matches!(
            app.route_message(Some("inbox"), Some("phone"), "do the thing")
                .unwrap(),
            RouteAction::Reply(reply) if reply.contains("I received: do the thing")
                && reply.contains("/codex <prompt>")
        ));
    }

    #[test]
    fn run_ack_names_agent_and_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let config_path = temp.path().join("config.toml");
        let mut config = Config::template();
        config.whitenoise.allowed_senders = vec!["phone".to_string()];
        config.runner.data_dir = temp.path().join("data").display().to_string();
        config.runner.log_dir = temp.path().join("logs").display().to_string();
        config.repos[0].alias = "work".to_string();
        config.repos[0].path = repo.path().display().to_string();
        config.save(&config_path).unwrap();
        let app = AgentApp::new_with_auth(config_path, config, None).unwrap();

        let request = match app
            .route_message(Some("group-a"), Some("phone"), "/codex work fix it")
            .unwrap()
        {
            RouteAction::Run(request) => request,
            other => panic!("expected run action, got {other:?}"),
        };
        let ack = app.run_ack_text(&request);

        assert!(ack.contains("Queued."));
        assert!(ack.contains("codex · work:/"));
        assert!(ack.contains("I'll post the answer here."));
    }

    #[test]
    fn primary_chat_job_gets_parallel_session_request() {
        let temp = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let config_path = temp.path().join("config.toml");
        let mut config = Config::template();
        config.whitenoise.group_id = "inbox".to_string();
        config.whitenoise.group_ids = vec!["inbox".to_string()];
        config.whitenoise.allowed_senders = vec!["phone".to_string()];
        config.runner.data_dir = temp.path().join("data").display().to_string();
        config.runner.log_dir = temp.path().join("logs").display().to_string();
        config.repos[0].alias = "work".to_string();
        config.repos[0].path = repo.path().display().to_string();
        config.save(&config_path).unwrap();
        let app = AgentApp::new_with_auth(config_path, config, None).unwrap();

        let request = match app
            .route_message(
                Some("inbox"),
                Some("phone"),
                "/wiki research water filters like Boroux/Berkey",
            )
            .unwrap()
        {
            RouteAction::Run(request) => request,
            other => panic!("expected run action, got {other:?}"),
        };
        let session = app
            .job_session_request(Some("inbox"), Some("phone"), &request)
            .unwrap()
            .expect("session request");

        assert_eq!(session.sender, "phone");
        assert_eq!(session.state.repo_alias.as_deref(), Some("work"));
        assert_eq!(
            session.state.default_agent,
            Some(crate::runner::AgentKind::Codex)
        );
        assert_eq!(
            session.state.default_prompt_prefix.as_deref(),
            Some("@wiki")
        );
        assert!(
            session
                .group_name
                .ends_with(" - research water filters boroux")
        );
        assert!(session.name.ends_with(" - research water filters boroux"));
        assert_eq!(session.state.name, Some(session.name.clone()));
    }

    #[test]
    fn non_primary_work_chat_bare_text_runs_default_agent() {
        let temp = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let config_path = temp.path().join("config.toml");
        let mut config = Config::template();
        config.whitenoise.group_id = "inbox".to_string();
        config.whitenoise.group_ids = vec!["inbox".to_string(), "worker".to_string()];
        config.whitenoise.allowed_senders = vec!["phone".to_string()];
        config.runner.data_dir = temp.path().join("data").display().to_string();
        config.runner.log_dir = temp.path().join("logs").display().to_string();
        config.repos[0].alias = "work".to_string();
        config.repos[0].path = repo.path().display().to_string();
        config.save(&config_path).unwrap();
        let app = AgentApp::new_with_auth(config_path, config, None).unwrap();

        let mut state = SessionState::new(Some("work".to_string()));
        state.default_agent = Some(crate::runner::AgentKind::Codex);
        app.create_session_record("worker", state).unwrap();

        let request = match app
            .route_message(Some("worker"), Some("phone"), "work on the fix")
            .unwrap()
        {
            RouteAction::Run(request) => request,
            other => panic!("expected run action, got {other:?}"),
        };

        assert_eq!(request.agent, crate::runner::AgentKind::Codex);
        assert_eq!(request.prompt, "work on the fix");
        assert_eq!(request.repo_alias.as_deref(), Some("work"));
    }

    #[test]
    fn work_chat_slash_run_sets_bare_text_mode() {
        let temp = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let config_path = temp.path().join("config.toml");
        let mut config = Config::template();
        config.whitenoise.group_id = "inbox".to_string();
        config.whitenoise.group_ids = vec!["inbox".to_string(), "worker".to_string()];
        config.whitenoise.allowed_senders = vec!["phone".to_string()];
        config.runner.data_dir = temp.path().join("data").display().to_string();
        config.runner.log_dir = temp.path().join("logs").display().to_string();
        config.repos[0].alias = "work".to_string();
        config.repos[0].path = repo.path().display().to_string();
        config.save(&config_path).unwrap();
        let app = AgentApp::new_with_auth(config_path, config, None).unwrap();

        assert!(matches!(
            app.route_message(Some("worker"), Some("phone"), "/wiki research relays")
                .unwrap(),
            RouteAction::Run(request) if request.prompt == "@wiki research relays"
        ));

        let request = match app
            .route_message(Some("worker"), Some("phone"), "keep going")
            .unwrap()
        {
            RouteAction::Run(request) => request,
            other => panic!("expected run action, got {other:?}"),
        };

        assert_eq!(request.agent, crate::runner::AgentKind::Codex);
        assert_eq!(request.prompt, "@wiki keep going");
        assert_eq!(request.repo_alias.as_deref(), Some("work"));
    }

    #[test]
    fn non_primary_chat_job_stays_in_current_session() {
        let temp = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let config_path = temp.path().join("config.toml");
        let mut config = Config::template();
        config.whitenoise.group_id = "inbox".to_string();
        config.whitenoise.group_ids = vec!["inbox".to_string(), "worker".to_string()];
        config.whitenoise.allowed_senders = vec!["phone".to_string()];
        config.runner.data_dir = temp.path().join("data").display().to_string();
        config.runner.log_dir = temp.path().join("logs").display().to_string();
        config.repos[0].path = repo.path().display().to_string();
        config.save(&config_path).unwrap();
        let app = AgentApp::new_with_auth(config_path, config, None).unwrap();

        let request = match app
            .route_message(Some("worker"), Some("phone"), "/codex follow up")
            .unwrap()
        {
            RouteAction::Run(request) => request,
            other => panic!("expected run action, got {other:?}"),
        };

        assert!(
            app.job_session_request(Some("worker"), Some("phone"), &request)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn job_group_name_uses_hostname_and_short_prompt_summary() {
        assert_eq!(
            job_group_name(
                "Frontier1-Mini.local",
                "@wiki research water filters like Boroux/Berkey/ClearlyFiltered",
            ),
            "frontier1-mini - research water filters boroux"
        );
        assert_eq!(job_group_name("m5", "test"), "m5 - test job");
    }

    #[cfg(unix)]
    #[test]
    fn run_request_progress_does_not_duplicate_final_reply() {
        let temp = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let bin = temp.path().join("agent");
        std::fs::write(
            &bin,
            r#"#!/bin/sh
printf '%s\n' '{"type":"item.completed","item":{"type":"agent_message","text":"done once"}}'
"#,
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&bin).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&bin, permissions).unwrap();
        }

        let config_path = temp.path().join("config.toml");
        let mut config = Config::template();
        config.whitenoise.allowed_senders = vec!["phone".to_string()];
        config.runner.launcher = crate::config::RunnerLauncher::Direct;
        config.runner.data_dir = temp.path().join("data").display().to_string();
        config.runner.log_dir = temp.path().join("logs").display().to_string();
        config.agents.codex.bin = bin.display().to_string();
        config.repos[0].alias = "work".to_string();
        config.repos[0].path = repo.path().display().to_string();
        config.save(&config_path).unwrap();
        let app = AgentApp::new_with_auth(config_path, config, None).unwrap();

        let progress = Arc::new(Mutex::new(Vec::new()));
        let progress_callback = Arc::clone(&progress);
        let reply = app
            .run_request_with_progress(
                crate::runner::AgentRequest::new(crate::runner::AgentKind::Codex, "work", "hello"),
                move |text| progress_callback.lock().unwrap().push(text),
            )
            .unwrap();

        assert!(reply.starts_with("Done · an-"));
        assert!(reply.contains("done once"));
        assert!(reply.contains("Details: /tail an-"));
        let progress = progress.lock().unwrap();
        assert!(progress.iter().all(|text| !text.contains("started")));
        assert!(!progress.iter().any(|text| text.contains("succeeded")));
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
            RouteAction::Reply(reply) if reply.contains("session main")
        ));
        assert!(matches!(
            app.route_message(Some("group-a"), Some("phone"), "/sessions")
                .unwrap(),
            RouteAction::Reply(reply) if reply.contains("main (current)")
                && reply.contains("g-group")
                && reply.contains("/jump 1")
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
            RouteAction::Reply(reply) if reply.contains("1. bugfix")
                && reply.contains("2. main (current)")
                && reply.contains("g-group")
                && reply.contains("/jump 1")
        ));
        assert!(matches!(
            app.route_message(Some("group-a"), Some("phone"), "/jump bugfix")
                .unwrap(),
            RouteAction::ResumeSession(request) if request.group_id == "group-b"
                && request.reply_text.contains("resumed bugfix")
                && request.reply_text.contains("open: whitenoise://chat/group-b")
                && request.reply_text.contains("continue there")
                && request.target_text.contains("session bugfix")
                && request.target_text.contains("back: whitenoise://chat/group-a")
        ));
        assert!(matches!(
            app.route_message(Some("group-a"), Some("phone"), "/resume 2")
                .unwrap(),
            RouteAction::Reply(reply) if reply.contains("session main")
                && reply.contains("ready: /pwd /codex <prompt>")
        ));
    }
}
