use std::fmt;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use anyhow::{Context, Result, bail};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::{Config, RunnerLauncher};
use crate::jobs::{JobRecord, JobStore};
use crate::progress::{self, ProgressEvent};
use crate::text::format_chat_text;
use crate::workspace;

type ProgressCallback = Arc<dyn Fn(ProgressEvent) + Send + Sync + 'static>;
type LineObserver = Arc<dyn Fn(&str) + Send + Sync + 'static>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum AgentKind {
    Codex,
    Claude,
    Hermes,
}

impl fmt::Display for AgentKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Hermes => "hermes",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRequest {
    pub agent: AgentKind,
    #[serde(default)]
    pub profile: Option<String>,
    pub repo_alias: Option<String>,
    pub cwd: Option<String>,
    #[serde(default)]
    pub workspace_root: Option<PathBuf>,
    pub prompt: String,
    pub resume_session: Option<String>,
}

impl AgentRequest {
    pub fn new(agent: AgentKind, repo_alias: impl Into<String>, prompt: impl Into<String>) -> Self {
        Self {
            agent,
            profile: None,
            repo_alias: Some(repo_alias.into()),
            cwd: None,
            workspace_root: None,
            prompt: prompt.into(),
            resume_session: None,
        }
    }

    pub fn prompt(agent: AgentKind, prompt: impl Into<String>) -> Self {
        Self {
            agent,
            profile: None,
            repo_alias: None,
            cwd: None,
            workspace_root: None,
            prompt: prompt.into(),
            resume_session: None,
        }
    }

    pub fn resume(agent: AgentKind, session: impl Into<String>, prompt: impl Into<String>) -> Self {
        Self {
            agent,
            profile: None,
            repo_alias: None,
            cwd: None,
            workspace_root: None,
            prompt: prompt.into(),
            resume_session: Some(session.into()),
        }
    }

    pub fn with_workspace(mut self, repo_alias: impl Into<String>, cwd: impl Into<String>) -> Self {
        self.repo_alias = Some(repo_alias.into());
        self.cwd = Some(cwd.into());
        self
    }

    pub fn with_workspace_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.workspace_root = Some(root.into());
        self
    }

    pub fn with_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.prompt = prompt.into();
        self
    }

    pub fn with_profile(mut self, profile: impl Into<String>) -> Self {
        self.profile = Some(profile.into());
        self
    }
}

#[derive(Debug, Clone)]
pub struct CommandPlan {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
}

#[derive(Clone)]
pub struct Runner {
    config: Config,
    jobs: JobStore,
}

impl Runner {
    pub fn new(config: Config, jobs: JobStore) -> Self {
        Self { config, jobs }
    }

    pub fn run_blocking(&self, request: AgentRequest) -> Result<JobRecord> {
        self.run_blocking_inner(request, None)
    }

    pub fn run_blocking_with_progress(
        &self,
        request: AgentRequest,
        progress: ProgressCallback,
    ) -> Result<JobRecord> {
        self.run_blocking_inner(request, Some(progress))
    }

    fn run_blocking_inner(
        &self,
        request: AgentRequest,
        progress_callback: Option<ProgressCallback>,
    ) -> Result<JobRecord> {
        if request.prompt.chars().count() > self.config.runner.max_prompt_chars {
            bail!(
                "prompt is too long: max {} chars",
                self.config.runner.max_prompt_chars
            );
        }

        let plan = self.build_command(&request)?;
        let job = self.jobs.create(&request)?;
        if let Some(progress_callback) = &progress_callback {
            progress_callback(progress::started(request.agent, &job.id));
        }

        let mut command = Command::new(&plan.program);
        command.args(&plan.args);
        if let Some(cwd) = &plan.cwd {
            command.current_dir(cwd);
        }
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        #[cfg(unix)]
        command.process_group(0);

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                let summary = format!("failed to spawn {}: {error}", plan.program);
                self.write_log(&job, &plan, "", &summary)?;
                if let Some(progress_callback) = &progress_callback {
                    progress_callback(ProgressEvent {
                        kind: progress::ProgressKind::Error,
                        agent: request.agent,
                        job_id: Some(job.id.clone()),
                        label: "spawn failed".to_string(),
                        detail: Some(summary.clone()),
                        final_event: true,
                    });
                }
                return self.jobs.mark_failed(&job.id, summary);
            }
        };

        self.jobs.mark_running(&job.id, child.id())?;
        let log_file = self.create_log(&job, &plan)?;
        let capture_limit = capture_max_bytes(self.config.runner.max_output_chars);
        let stdout_bytes = Arc::new(Mutex::new(CaptureBuffer::new(capture_limit)));
        let stderr_bytes = Arc::new(Mutex::new(CaptureBuffer::new(capture_limit)));
        let progress_observer = progress_callback.clone().map(|progress_callback| {
            let agent = request.agent;
            let job_id = job.id.clone();
            Arc::new(move |line: &str| {
                if let Some(mut event) = progress::parse_progress_line(agent, line) {
                    event.job_id = Some(job_id.clone());
                    progress_callback(event);
                }
            }) as LineObserver
        });

        let stdout_thread = child.stdout.take().map(|stdout| {
            copy_stream_to_log(
                "stdout",
                stdout,
                Arc::clone(&log_file),
                Arc::clone(&stdout_bytes),
                progress_observer.clone(),
            )
        });
        let stderr_thread = child.stderr.take().map(|stderr| {
            copy_stream_to_log(
                "stderr",
                stderr,
                Arc::clone(&log_file),
                Arc::clone(&stderr_bytes),
                None,
            )
        });

        let status = child
            .wait()
            .with_context(|| format!("waiting for job {}", job.id))?;
        join_log_thread(stdout_thread)?;
        join_log_thread(stderr_thread)?;

        let stdout = stdout_bytes
            .lock()
            .map_err(|_| anyhow::anyhow!("stdout lock poisoned"))?
            .to_lossy_string();
        let stderr = stderr_bytes
            .lock()
            .map_err(|_| anyhow::anyhow!("stderr lock poisoned"))?
            .to_lossy_string();
        let summary = summarize(
            request.agent,
            &stdout,
            &stderr,
            status.success(),
            self.config.runner.max_output_chars,
        );
        let record = self.jobs.finish(&job.id, status.code(), summary)?;
        if let Some(progress_callback) = &progress_callback {
            progress_callback(progress::finished(
                request.agent,
                &job.id,
                &record.status.to_string(),
            ));
        }
        Ok(record)
    }

    pub fn cancel(&self, job_id: &str) -> Result<bool> {
        let Some(pid) = self.jobs.request_cancel(job_id)? else {
            return Ok(false);
        };

        #[cfg(unix)]
        let status = Command::new("kill")
            .arg("-TERM")
            .arg(format!("-{pid}"))
            .status()
            .with_context(|| format!("sending TERM to process group {pid}"))?;

        #[cfg(not(unix))]
        let status = Command::new("kill")
            .arg("-TERM")
            .arg(pid.to_string())
            .status()
            .with_context(|| format!("sending TERM to pid {pid}"))?;

        Ok(status.success())
    }

    pub fn build_command(&self, request: &AgentRequest) -> Result<CommandPlan> {
        let agent = self.config.agent(request.agent);
        if !agent.enabled {
            bail!("{} is disabled in config", request.agent);
        }
        let permission_mode = self.config.effective_permission_mode_for_request(request)?;

        let prompt = agentnoise_prompt(request);
        let mut args = Vec::new();
        let cwd = match (request.agent, request.resume_session.as_deref()) {
            (AgentKind::Codex, Some(session)) => {
                args.extend([
                    "exec".to_string(),
                    "resume".to_string(),
                    "--json".to_string(),
                    session.to_string(),
                    prompt,
                ]);
                None
            }
            (AgentKind::Codex, None) => {
                let (_repo_root, workdir) = self.workspace_paths(request)?;
                args.extend([
                    "exec".to_string(),
                    "--json".to_string(),
                    "-C".to_string(),
                    workdir.display().to_string(),
                    prompt,
                ]);
                Some(workdir)
            }
            (AgentKind::Claude, Some(session)) => {
                args.extend([
                    "-p".to_string(),
                    "--output-format".to_string(),
                    "stream-json".to_string(),
                    "--resume".to_string(),
                    session.to_string(),
                    prompt,
                ]);
                None
            }
            (AgentKind::Claude, None) => {
                let (repo_root, workdir) = self.workspace_paths(request)?;
                args.extend([
                    "-p".to_string(),
                    "--output-format".to_string(),
                    "stream-json".to_string(),
                ]);
                if let Some(permission_mode) = &permission_mode
                    && !permission_mode.trim().is_empty()
                {
                    args.extend(["--permission-mode".to_string(), permission_mode.clone()]);
                }
                args.extend([
                    "--add-dir".to_string(),
                    repo_root.display().to_string(),
                    prompt,
                ]);
                Some(workdir)
            }
            (AgentKind::Hermes, Some(session)) => {
                args.extend([
                    "chat".to_string(),
                    "--quiet".to_string(),
                    "--source".to_string(),
                    "agentnoise".to_string(),
                    "--toolsets".to_string(),
                    "skills".to_string(),
                    "--resume".to_string(),
                    session.to_string(),
                    "-q".to_string(),
                    prompt,
                ]);
                None
            }
            (AgentKind::Hermes, None) => {
                let (_repo_root, workdir) = self.workspace_paths(request)?;
                args.extend([
                    "chat".to_string(),
                    "--quiet".to_string(),
                    "--source".to_string(),
                    "agentnoise".to_string(),
                    "--toolsets".to_string(),
                    "skills".to_string(),
                    "-q".to_string(),
                    prompt,
                ]);
                Some(workdir)
            }
        };

        if self.config.runner.launcher == RunnerLauncher::Direct {
            return Ok(CommandPlan {
                program: agent.bin.clone(),
                args,
                cwd,
            });
        }

        let bondage_conf = self.config.resolved_bondage_conf().display().to_string();
        let agent_profile = self.config.effective_agent_profile_for_request(request)?;
        let mut wrapped_args = vec![
            "exec".to_string(),
            agent_profile,
            bondage_conf,
            "--".to_string(),
            agent.bin.clone(),
        ];
        wrapped_args.extend(args);

        Ok(CommandPlan {
            program: self.config.runner.bondage_bin.clone(),
            args: wrapped_args,
            cwd,
        })
    }

    fn required_repo_root(&self, request: &AgentRequest) -> Result<PathBuf> {
        if let Some(root) = &request.workspace_root {
            if !root.is_dir() {
                bail!("workspace root is not a directory: {}", root.display());
            }
            return root
                .canonicalize()
                .with_context(|| format!("canonicalizing {}", root.display()));
        }

        let Some(alias) = &request.repo_alias else {
            bail!("repo alias is required");
        };
        let Some(path) = self.config.repo_path(alias) else {
            bail!("unknown repo alias: {alias}");
        };
        if !path.is_dir() {
            bail!("repo path is not a directory: {}", path.display());
        }
        Ok(path)
    }

    fn workspace_paths(&self, request: &AgentRequest) -> Result<(PathBuf, PathBuf)> {
        let root = self.required_repo_root(request)?;
        let workdir = workspace::resolve_cwd(&root, request.cwd.as_deref())?;
        Ok((root, workdir))
    }

    fn create_log(&self, job: &JobRecord, plan: &CommandPlan) -> Result<Arc<Mutex<File>>> {
        if let Some(parent) = job.log_path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        let mut file = File::create(&job.log_path)
            .with_context(|| format!("creating {}", job.log_path.display()))?;
        writeln!(file, "job: {}", job.id)?;
        writeln!(file, "agent: {}", job.agent)?;
        writeln!(file, "repo: {}", job.repo_alias.as_deref().unwrap_or("-"))?;
        if let Some(cwd) = &plan.cwd {
            writeln!(file, "cwd: {}", cwd.display())?;
        }
        writeln!(file, "program: {}", plan.program)?;
        writeln!(file, "args: {:?}", plan.args)?;
        writeln!(file)?;
        Ok(Arc::new(Mutex::new(file)))
    }

    fn write_log(
        &self,
        job: &JobRecord,
        plan: &CommandPlan,
        stdout: &str,
        stderr: &str,
    ) -> Result<()> {
        let mut text = String::new();
        text.push_str(&format!("job: {}\n", job.id));
        text.push_str(&format!("agent: {}\n", job.agent));
        text.push_str(&format!(
            "repo: {}\n",
            job.repo_alias.as_deref().unwrap_or("-")
        ));
        if let Some(cwd) = &plan.cwd {
            text.push_str(&format!("cwd: {}\n", cwd.display()));
        }
        text.push_str(&format!("program: {}\n", plan.program));
        text.push_str(&format!("args: {:?}\n", plan.args));
        text.push_str("\n--- stdout ---\n");
        text.push_str(stdout);
        text.push_str("\n--- stderr ---\n");
        text.push_str(stderr);

        if let Some(parent) = job.log_path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        fs::write(&job.log_path, text)
            .with_context(|| format!("writing {}", job.log_path.display()))?;
        Ok(())
    }
}

fn copy_stream_to_log(
    label: &'static str,
    mut reader: impl Read + Send + 'static,
    log_file: Arc<Mutex<File>>,
    captured: Arc<Mutex<CaptureBuffer>>,
    line_observer: Option<LineObserver>,
) -> thread::JoinHandle<Result<()>> {
    thread::spawn(move || {
        let mut buffer = [0_u8; 8192];
        let mut line_buffer = String::new();
        loop {
            let n = reader
                .read(&mut buffer)
                .with_context(|| format!("reading {label}"))?;
            if n == 0 {
                break;
            }

            {
                let mut captured = captured
                    .lock()
                    .map_err(|_| anyhow::anyhow!("{label} capture lock poisoned"))?;
                captured.extend_from_slice(&buffer[..n]);
            }

            {
                let mut log_file = log_file
                    .lock()
                    .map_err(|_| anyhow::anyhow!("{label} log lock poisoned"))?;
                log_file
                    .write_all(&buffer[..n])
                    .with_context(|| format!("writing {label} to log"))?;
                log_file.flush().ok();
            }
            if let Some(observer) = &line_observer {
                observe_lines(
                    &mut line_buffer,
                    &String::from_utf8_lossy(&buffer[..n]),
                    observer,
                );
            }
        }
        if let Some(observer) = &line_observer
            && !line_buffer.trim().is_empty()
        {
            observer(line_buffer.trim());
        }
        Ok(())
    })
}

fn observe_lines(line_buffer: &mut String, chunk: &str, observer: &LineObserver) {
    line_buffer.push_str(chunk);
    while let Some(index) = line_buffer.find('\n') {
        let line = line_buffer[..index].trim().to_string();
        line_buffer.drain(..=index);
        if !line.is_empty() {
            observer(&line);
        }
    }
    if line_buffer.len() > 16 * 1024 {
        line_buffer.clear();
    }
}

fn join_log_thread(handle: Option<thread::JoinHandle<Result<()>>>) -> Result<()> {
    let Some(handle) = handle else {
        return Ok(());
    };
    handle
        .join()
        .map_err(|_| anyhow::anyhow!("log thread panicked"))?
}

#[derive(Debug)]
struct CaptureBuffer {
    bytes: Vec<u8>,
    max_bytes: usize,
}

impl CaptureBuffer {
    fn new(max_bytes: usize) -> Self {
        Self {
            bytes: Vec::new(),
            max_bytes: max_bytes.max(1),
        }
    }

    fn extend_from_slice(&mut self, bytes: &[u8]) {
        if bytes.len() >= self.max_bytes {
            self.bytes.clear();
            self.bytes
                .extend_from_slice(&bytes[bytes.len() - self.max_bytes..]);
            return;
        }

        self.bytes.extend_from_slice(bytes);
        let excess = self.bytes.len().saturating_sub(self.max_bytes);
        if excess > 0 {
            self.bytes.drain(..excess);
        }
    }

    fn to_lossy_string(&self) -> String {
        String::from_utf8_lossy(&self.bytes).to_string()
    }
}

fn capture_max_bytes(max_output_chars: usize) -> usize {
    max_output_chars
        .saturating_mul(8)
        .clamp(8 * 1024, 1024 * 1024)
}

fn summarize(
    agent: AgentKind,
    stdout: &str,
    stderr: &str,
    success: bool,
    max_chars: usize,
) -> String {
    let decoded_stdout = decode_agent_stdout(agent, stdout);
    let mut combined = String::new();
    if let Some(stdout) = decoded_stdout.as_deref() {
        combined.push_str(stdout.trim());
    } else if !stdout.trim().is_empty() {
        combined.push_str(stdout.trim());
    }

    let stderr = if success && decoded_stdout.is_some() {
        String::new()
    } else {
        stderr.trim().to_string()
    };
    if !stderr.is_empty() {
        if !combined.is_empty() {
            combined.push_str("\n\nstderr:\n");
        }
        combined.push_str(&stderr);
    }
    if combined.is_empty() {
        return "job produced no output".to_string();
    }

    let formatted = format_chat_text(&combined);
    let chars = formatted.chars().collect::<Vec<_>>();
    let start = chars.len().saturating_sub(max_chars);
    chars[start..].iter().collect()
}

fn decode_agent_stdout(agent: AgentKind, stdout: &str) -> Option<String> {
    if agent == AgentKind::Hermes {
        return None;
    }

    let mut saw_json = false;
    let mut final_result = None;
    let mut assistant_messages = Vec::new();

    for line in stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        saw_json = true;

        if let Some(result) = final_result_text(agent, &value) {
            final_result = Some(result);
        }
        if let Some(message) = assistant_message_text(agent, &value) {
            assistant_messages.push(message);
        }
    }

    if let Some(result) = final_result.filter(|result| !result.trim().is_empty()) {
        return Some(result);
    }

    assistant_messages
        .into_iter()
        .rev()
        .find(|message| !message.trim().is_empty())
        .or_else(|| saw_json.then(|| "agent produced no assistant message".to_string()))
}

fn final_result_text(agent: AgentKind, value: &Value) -> Option<String> {
    match agent {
        AgentKind::Claude => value
            .get("result")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|result| !result.is_empty())
            .map(str::to_string),
        AgentKind::Codex | AgentKind::Hermes => None,
    }
}

fn assistant_message_text(agent: AgentKind, value: &Value) -> Option<String> {
    match agent {
        AgentKind::Codex => codex_message_text(value),
        AgentKind::Claude => claude_message_text(value),
        AgentKind::Hermes => None,
    }
    .map(|text| format_chat_text(&text))
    .filter(|text| !text.trim().is_empty())
}

fn codex_message_text(value: &Value) -> Option<String> {
    let item = value.get("item").unwrap_or(value);
    let item_type = item.get("type").and_then(Value::as_str);

    if item_type == Some("agent_message") {
        return item.get("text").and_then(Value::as_str).map(str::to_string);
    }

    let role = item.get("role").and_then(Value::as_str);
    if role == Some("assistant") || item_type == Some("message") {
        return content_text(item);
    }

    None
}

fn claude_message_text(value: &Value) -> Option<String> {
    if value.get("type").and_then(Value::as_str) != Some("assistant") {
        return None;
    }

    let message = value.get("message").unwrap_or(value);
    content_text(message)
}

fn content_text(value: &Value) -> Option<String> {
    match value.get("content") {
        Some(Value::String(text)) => Some(text.clone()),
        Some(Value::Array(items)) => {
            let text = items
                .iter()
                .filter_map(content_item_text)
                .collect::<Vec<_>>()
                .join("");
            (!text.trim().is_empty()).then_some(text)
        }
        _ => value
            .get("text")
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

fn content_item_text(value: &Value) -> Option<String> {
    let object = value.as_object()?;
    let item_type = object.get("type").and_then(Value::as_str);
    if matches!(item_type, Some("text" | "output_text") | None) {
        return object
            .get("text")
            .and_then(Value::as_str)
            .map(str::to_string);
    }
    None
}

fn agentnoise_prompt(request: &AgentRequest) -> String {
    let mut context = vec![
        "Agentnoise context:".to_string(),
        "- You are running under agentnoise, a White Noise phone-to-desktop control bridge.".to_string(),
        "- The user is chatting from a phone; reply concise, outcome-first, with no Markdown tables or raw logs unless asked.".to_string(),
        "- Full logs stay local; mention /tail <job> when extra detail is useful.".to_string(),
        "- The selected repo, cwd, and session come from agentnoise. Do not ask the user to SSH into this machine.".to_string(),
        "- If this task touches agentnoise, consider pairing, service startup, relay/message reliability, and phone UX.".to_string(),
    ];

    if looks_like_wiki_prompt(&request.prompt) {
        context.push(
            "- LLM-Wiki instructions or plugins may be available; return a compact digest with paths and sources, not a pasted article.".to_string(),
        );
    }
    if let Some(repo) = request.repo_alias.as_deref() {
        context.push(format!("- Agentnoise repo alias: {repo}."));
    }
    if let Some(cwd) = request.cwd.as_deref() {
        context.push(format!("- Agentnoise cwd: {cwd}."));
    }
    if let Some(session) = request.resume_session.as_deref() {
        context.push(format!("- Resuming agent session: {session}."));
    }

    format!(
        "{}\n\nUser request:\n{}",
        context.join("\n"),
        request.prompt
    )
}

fn looks_like_wiki_prompt(prompt: &str) -> bool {
    let prompt = prompt.trim_start();
    prompt == "@wiki"
        || prompt.starts_with("@wiki ")
        || prompt == "wiki"
        || prompt.starts_with("wiki ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AgentProfileConfig, Config};
    use crate::jobs::JobStore;

    #[test]
    fn codex_command_uses_bondage_and_repo_alias() {
        let temp = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let mut config = Config::template();
        config.runner.data_dir = temp.path().join("data").display().to_string();
        config.runner.log_dir = temp.path().join("logs").display().to_string();
        config.runner.bondage_conf = temp.path().join("bondage.conf").display().to_string();
        std::fs::write(&config.runner.bondage_conf, "").unwrap();
        config.repos[0].alias = "work".to_string();
        config.repos[0].path = repo.path().display().to_string();

        let jobs =
            JobStore::open(&config.resolved_jobs_path(), &config.resolved_log_dir()).unwrap();
        let runner = Runner::new(config, jobs);
        let plan = runner
            .build_command(&AgentRequest::new(AgentKind::Codex, "work", "hello"))
            .unwrap();

        assert_eq!(plan.program, "bondage");
        assert_eq!(plan.args[0], "exec");
        assert_eq!(plan.args[1], "codex-agentnoise");
        assert!(plan.args.contains(&"codex".to_string()));
        assert!(plan.args.contains(&"--json".to_string()));
        assert!(plan.args.contains(&"-C".to_string()));
        assert!(plan.args.last().unwrap().contains("Agentnoise context:"));
        assert!(plan.args.last().unwrap().contains("User request:\nhello"));
        let repo_path = repo.path().canonicalize().unwrap();
        assert!(plan.args.contains(&repo_path.display().to_string()));
        assert_eq!(plan.cwd.as_deref(), Some(repo_path.as_path()));
    }

    #[test]
    fn codex_command_uses_selected_cwd() {
        let temp = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir(repo.path().join("src")).unwrap();
        let mut config = Config::template();
        config.runner.data_dir = temp.path().join("data").display().to_string();
        config.runner.log_dir = temp.path().join("logs").display().to_string();
        config.runner.bondage_conf = temp.path().join("bondage.conf").display().to_string();
        std::fs::write(&config.runner.bondage_conf, "").unwrap();
        config.repos[0].alias = "work".to_string();
        config.repos[0].path = repo.path().display().to_string();

        let jobs =
            JobStore::open(&config.resolved_jobs_path(), &config.resolved_log_dir()).unwrap();
        let runner = Runner::new(config, jobs);
        let plan = runner
            .build_command(
                &AgentRequest::new(AgentKind::Codex, "work", "hello").with_workspace("work", "src"),
            )
            .unwrap();

        let workdir = repo.path().join("src").canonicalize().unwrap();
        assert!(plan.args.contains(&"-C".to_string()));
        assert!(plan.args.contains(&workdir.display().to_string()));
        assert_eq!(plan.cwd.as_deref(), Some(workdir.as_path()));
    }

    #[test]
    fn codex_command_uses_configured_profile_variant() {
        let temp = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let mut config = Config::template();
        config.runner.data_dir = temp.path().join("data").display().to_string();
        config.runner.log_dir = temp.path().join("logs").display().to_string();
        config.runner.bondage_conf = temp.path().join("bondage.conf").display().to_string();
        std::fs::write(&config.runner.bondage_conf, "").unwrap();
        config.agents.codex.profiles.push(AgentProfileConfig {
            name: "fix".to_string(),
            profile: "codex-fix".to_string(),
            permission_mode: None,
        });
        config.repos[0].alias = "work".to_string();
        config.repos[0].path = repo.path().display().to_string();

        let jobs =
            JobStore::open(&config.resolved_jobs_path(), &config.resolved_log_dir()).unwrap();
        let runner = Runner::new(config, jobs);
        let plan = runner
            .build_command(
                &AgentRequest::new(AgentKind::Codex, "work", "hello").with_profile("fix"),
            )
            .unwrap();

        assert_eq!(plan.program, "bondage");
        assert_eq!(plan.args[1], "codex-fix");
    }

    #[test]
    fn direct_launcher_runs_raw_codex_without_bondage() {
        let temp = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let mut config = Config::template();
        config.runner.launcher = RunnerLauncher::Direct;
        config.runner.data_dir = temp.path().join("data").display().to_string();
        config.runner.log_dir = temp.path().join("logs").display().to_string();
        config.repos[0].alias = "work".to_string();
        config.repos[0].path = repo.path().display().to_string();

        let jobs =
            JobStore::open(&config.resolved_jobs_path(), &config.resolved_log_dir()).unwrap();
        let runner = Runner::new(config, jobs);
        let plan = runner
            .build_command(&AgentRequest::new(AgentKind::Codex, "work", "hello"))
            .unwrap();

        assert_eq!(plan.program, "codex");
        assert_eq!(plan.args[0], "exec");
        assert!(!plan.args.contains(&"codex-agentnoise".to_string()));
        assert!(!plan.args.contains(&"bondage".to_string()));
        assert!(plan.args.contains(&"--json".to_string()));
        assert!(plan.args.last().unwrap().contains("Agentnoise context:"));
        let repo_path = repo.path().canonicalize().unwrap();
        assert_eq!(plan.cwd.as_deref(), Some(repo_path.as_path()));
    }

    #[test]
    fn claude_resume_does_not_require_repo() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = Config::template();
        config.runner.data_dir = temp.path().join("data").display().to_string();
        config.runner.log_dir = temp.path().join("logs").display().to_string();
        config.runner.bondage_conf = temp.path().join("bondage.conf").display().to_string();
        std::fs::write(&config.runner.bondage_conf, "").unwrap();

        let jobs =
            JobStore::open(&config.resolved_jobs_path(), &config.resolved_log_dir()).unwrap();
        let runner = Runner::new(config, jobs);
        let request = AgentRequest::resume(AgentKind::Claude, "session-1", "continue");
        let plan = runner.build_command(&request).unwrap();

        assert!(plan.args.contains(&"--resume".to_string()));
        assert!(plan.args.contains(&"session-1".to_string()));
        assert_eq!(plan.args.last().unwrap(), &agentnoise_prompt(&request));
    }

    #[test]
    fn hermes_is_disabled_by_default() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = Config::template();
        config.runner.data_dir = temp.path().join("data").display().to_string();
        config.runner.log_dir = temp.path().join("logs").display().to_string();

        let jobs =
            JobStore::open(&config.resolved_jobs_path(), &config.resolved_log_dir()).unwrap();
        let runner = Runner::new(config, jobs);
        let error = runner
            .build_command(&AgentRequest::new(AgentKind::Hermes, "work", "hello"))
            .unwrap_err()
            .to_string();

        assert!(error.contains("hermes is disabled"));
    }

    #[test]
    fn hermes_command_uses_bondage_and_restricted_toolset() {
        let temp = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let mut config = Config::template();
        config.runner.data_dir = temp.path().join("data").display().to_string();
        config.runner.log_dir = temp.path().join("logs").display().to_string();
        config.runner.bondage_conf = temp.path().join("bondage.conf").display().to_string();
        std::fs::write(&config.runner.bondage_conf, "").unwrap();
        config.agents.hermes.enabled = true;
        config.repos[0].alias = "work".to_string();
        config.repos[0].path = repo.path().display().to_string();

        let bondage_conf = config.resolved_bondage_conf().display().to_string();
        let jobs =
            JobStore::open(&config.resolved_jobs_path(), &config.resolved_log_dir()).unwrap();
        let runner = Runner::new(config, jobs);
        let request = AgentRequest::new(AgentKind::Hermes, "work", "hello");
        let plan = runner.build_command(&request).unwrap();

        assert_eq!(plan.program, "bondage");
        assert_eq!(
            plan.args,
            vec![
                "exec".to_string(),
                "hermes-agentnoise".to_string(),
                bondage_conf,
                "--".to_string(),
                "hermes".to_string(),
                "chat".to_string(),
                "--quiet".to_string(),
                "--source".to_string(),
                "agentnoise".to_string(),
                "--toolsets".to_string(),
                "skills".to_string(),
                "-q".to_string(),
                agentnoise_prompt(&request),
            ]
        );
        let repo_path = repo.path().canonicalize().unwrap();
        assert_eq!(plan.cwd.as_deref(), Some(repo_path.as_path()));
    }

    #[test]
    fn hermes_resume_does_not_require_repo() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = Config::template();
        config.runner.data_dir = temp.path().join("data").display().to_string();
        config.runner.log_dir = temp.path().join("logs").display().to_string();
        config.runner.bondage_conf = temp.path().join("bondage.conf").display().to_string();
        std::fs::write(&config.runner.bondage_conf, "").unwrap();
        config.agents.hermes.enabled = true;

        let jobs =
            JobStore::open(&config.resolved_jobs_path(), &config.resolved_log_dir()).unwrap();
        let runner = Runner::new(config, jobs);
        let plan = runner
            .build_command(&AgentRequest::resume(AgentKind::Hermes, "h123", "continue"))
            .unwrap();

        assert_eq!(plan.cwd, None);
        assert!(plan.args.contains(&"--resume".to_string()));
        assert!(plan.args.contains(&"h123".to_string()));
        assert!(plan.args.last().unwrap().contains("Agentnoise context:"));
    }

    #[test]
    fn wiki_prompt_gets_llm_wiki_context() {
        let request = AgentRequest::prompt(AgentKind::Codex, "@wiki research chat UX");
        let prompt = agentnoise_prompt(&request);

        assert!(prompt.contains("LLM-Wiki"));
        assert!(prompt.contains("User request:\n@wiki research chat UX"));
    }

    #[test]
    fn summarizes_codex_json_as_last_agent_message() {
        let stdout = r#"{"type":"thread.started","thread_id":"abc"}
{"type":"item.completed","item":{"type":"agent_message","text":"Working on it."}}
{"type":"item.completed","item":{"type":"agent_message","text":"Final answer."}}
{"type":"turn.completed"}"#;
        let stderr = "bondage: launch chain\nwarning: noisy";

        assert_eq!(
            summarize(AgentKind::Codex, stdout, stderr, true, 1000),
            "Final answer."
        );
    }

    #[test]
    fn summarizes_claude_stream_json_result() {
        let stdout = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Intermediate"}]}}
{"type":"result","subtype":"success","result":"Done."}"#;

        assert_eq!(
            summarize(AgentKind::Claude, stdout, "", true, 1000),
            "Done."
        );
    }

    #[test]
    fn capture_buffer_keeps_only_tail_bytes() {
        let mut buffer = CaptureBuffer::new(5);
        buffer.extend_from_slice(b"abc");
        buffer.extend_from_slice(b"defgh");
        assert_eq!(buffer.to_lossy_string(), "defgh");

        buffer.extend_from_slice(b"ijklmnop");
        assert_eq!(buffer.to_lossy_string(), "lmnop");
    }
}
