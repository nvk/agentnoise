//! Fake-phone test harnesses for darkmatter v2.
//!
//! `roundtrip` is a protocol-only smoke test that uses an in-process desktop
//! responder. `live_roundtrip` is the release gate: it starts the real
//! `agentnoise transport run` process against an isolated mock relay, sends a
//! message from a separate fake-phone runtime, and requires the phone to see a
//! real reply from the daemon.
//!
//! Mechanism:
//! 1. Boot an in-process [`nostr_relay_builder::MockRelay`].
//! 2. Build one [`marmot_app::MarmotApp`] pointing at that relay; create two
//!    managed accounts on it: `desktop` and `phone`.
//! 3. `phone.create_group([desktop])`, wait for desktop's `GroupJoined` event.
//! 4. Spawn a desktop responder that subscribes to messages, wraps each reply
//!    in an [`crate::dm_streams::AgentTextStream`] lifecycle so the phone sees
//!    `AgentStreamStarted` / `AgentStreamFinalized` events (smoke test for the
//!    v2 QUIC-live-preview wiring).
//! 5. Phone sends the requested test message and collects replies + stream
//!    events until min_replies are seen, expectations matched, or timeout
//!    fires.

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command as ProcessCommand, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use cgka_traits::TransportEndpoint;
use cgka_traits::agent_text_stream::{
    AGENT_TEXT_STREAM_RECORD_TEXT_DELTA, AgentTextStreamTranscriptV1,
};
use cgka_traits::app_event::STREAM_TAG;
use marmot_app::{
    AccountSetupRequest, AgentTextStreamFinishRequest, AppMessageQuery, MarmotApp, MarmotAppEvent,
    MarmotAppRuntime, RuntimeMessageUpdate,
};
use nostr_relay_builder::MockRelay;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::{Config, RunnerLauncher};
use crate::darkmatter_app::{DarkmatterEngine, keychain_service_for_instance};
use crate::dm_streams::start_event_id_from_summary;
use crate::events::{EventDirection, RuntimeEvent};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FakePhonePlan {
    pub root: PathBuf,
    pub data_dir: PathBuf,
    pub logs_dir: PathBuf,
    pub socket: PathBuf,
    pub nsec_file: PathBuf,
}

#[derive(Debug, Clone)]
pub struct FakePhoneRoundtrip {
    pub root: PathBuf,
    pub pin: Option<String>,
    pub message: String,
    pub group_name: String,
    pub timeout: Duration,
    pub expect: Vec<String>,
    pub min_replies: usize,
    pub require_job_final: bool,
}

#[derive(Debug, Clone)]
pub struct LiveFakePhoneRoundtrip {
    pub root: PathBuf,
    pub message: String,
    pub group_name: String,
    pub timeout: Duration,
    pub expect: Vec<String>,
    pub min_replies: usize,
    pub require_job_final: bool,
    pub start_worker: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FakePhoneResult {
    pub phone_npub: String,
    pub group_id: String,
    pub replies: Vec<String>,
    pub matched: Vec<String>,
    pub saw_job_final: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveFakePhoneResult {
    pub root: PathBuf,
    pub desktop_npub: String,
    pub phone_npub: String,
    pub group_id: String,
    pub relay_url: String,
    pub replies: Vec<String>,
    pub matched: Vec<String>,
    pub saw_job_final: bool,
    pub saw_inbound_journal: bool,
    pub saw_outbound_journal: bool,
    pub transport_stdout: PathBuf,
    pub transport_stderr: PathBuf,
    pub worker_stdout: Option<PathBuf>,
    pub worker_stderr: Option<PathBuf>,
    pub event_log: PathBuf,
}

pub fn plan(config: &Config, root: Option<&Path>) -> FakePhonePlan {
    let root = root
        .map(Path::to_path_buf)
        .unwrap_or_else(|| config.resolved_data_dir().join("fake-phone"));
    let data_dir = root.join("dm-data");
    let logs_dir = root.join("dm-logs");
    let socket = data_dir.join("mock-relay.sock");
    let nsec_file = root.join("fake-phone.nsec");
    FakePhonePlan {
        root,
        data_dir,
        logs_dir,
        socket,
        nsec_file,
    }
}

/// Run the end-to-end fake-phone round-trip. Builds its own tokio runtime so
/// callers don't have to.
pub fn roundtrip(_config: &Config, options: FakePhoneRoundtrip) -> Result<FakePhoneResult> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building tokio runtime for fake-phone roundtrip")?;
    runtime.block_on(run_roundtrip(options))
}

/// Run a real daemon round-trip against a local mock relay.
///
/// This intentionally ignores the caller's configured data/keychain paths and
/// builds an isolated desktop under `options.root`, so it can be run on a
/// developer machine without mutating the real agentnoise identity.
pub fn live_roundtrip(
    base_config: &Config,
    options: LiveFakePhoneRoundtrip,
) -> Result<LiveFakePhoneResult> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building tokio runtime for live fake-phone roundtrip")?;
    runtime.block_on(run_live_roundtrip(base_config, options))
}

async fn run_roundtrip(options: FakePhoneRoundtrip) -> Result<FakePhoneResult> {
    let tmp = tempfile::tempdir().context("creating fake-phone tempdir")?;
    let relay = MockRelay::run()
        .await
        .map_err(|e| anyhow::anyhow!("starting MockRelay: {e}"))?;
    let url = relay.url().await.to_string();
    let endpoints = vec![TransportEndpoint(url.clone())];

    let app = MarmotApp::with_relays(tmp.path(), vec![url.clone()]);
    let runtime = MarmotAppRuntime::new(app.clone());

    let setup = AccountSetupRequest {
        identity: None,
        default_relays: endpoints.clone(),
        bootstrap_relays: endpoints.clone(),
        publish_missing_relay_lists: true,
        publish_initial_key_package: true,
    };
    let desktop = runtime
        .create_identity(setup.clone())
        .await
        .map_err(|err| anyhow::anyhow!("creating desktop identity: {err}"))?;
    let phone = runtime
        .create_identity(setup)
        .await
        .map_err(|err| anyhow::anyhow!("creating phone identity: {err}"))?;

    let desktop_id = desktop.account.account_id_hex.clone();
    let phone_id = phone.account.account_id_hex.clone();
    let phone_npub = npub_from_account_id(&phone_id)?;

    let mut events = runtime.subscribe();

    let group_id = runtime
        .create_group(
            &phone_id,
            &options.group_name,
            std::slice::from_ref(&desktop_id),
            None,
        )
        .await
        .map_err(|err| anyhow::anyhow!("phone create_group: {err}"))?;
    let group_id_hex = hex::encode(group_id.as_slice());

    // Wait for desktop to receive the welcome.
    let desktop_id_match = desktop_id.clone();
    let group_id_match = group_id.clone();
    wait_for_event(&mut events, Duration::from_secs(5), move |event| {
        matches!(
            event,
            MarmotAppEvent::GroupJoined { account_id_hex, group_id: gid, .. }
                if account_id_hex == &desktop_id_match && gid == &group_id_match
        )
    })
    .await
    .context("desktop did not receive GroupJoined event within 5s")?;

    // Spawn the desktop responder: wraps every received message in an agent
    // text stream lifecycle and echoes a synthetic reply.
    let runtime_for_desktop = runtime.clone();
    let desktop_id_for_handler = desktop_id.clone();
    let group_id_for_handler = group_id.clone();
    let group_id_hex_for_handler = group_id_hex.clone();
    let pin_for_handler = options.pin.clone();
    let desktop_task = tokio::spawn(async move {
        let mut subscription = match runtime_for_desktop.subscribe_messages(
            &desktop_id_for_handler,
            AppMessageQuery {
                group_id_hex: Some(group_id_hex_for_handler.clone()),
                limit: None,
            },
        ) {
            Ok(subscription) => subscription,
            Err(error) => {
                eprintln!("fake-phone: desktop subscribe_messages failed: {error:#}");
                return;
            }
        };
        while let Some(update) = subscription.recv().await {
            let RuntimeMessageUpdate::Message(received) = update else {
                continue;
            };
            if received.message.sender == desktop_id_for_handler {
                continue;
            }
            if let Err(error) = handle_desktop_message(
                &runtime_for_desktop,
                &desktop_id_for_handler,
                &group_id_for_handler,
                &received.message.plaintext,
                pin_for_handler.as_deref(),
            )
            .await
            {
                eprintln!("fake-phone: desktop reply failed: {error:#}");
            }
        }
    });

    // Phone subscribes for the reply stream BEFORE sending so it doesn't miss
    // anything.
    let mut phone_messages = runtime
        .subscribe_messages(
            &phone_id,
            AppMessageQuery {
                group_id_hex: Some(group_id_hex.clone()),
                limit: None,
            },
        )
        .map_err(|err| anyhow::anyhow!("phone subscribe_messages: {err}"))?;
    let mut phone_events = runtime.subscribe();

    runtime
        .send_message(&phone_id, &group_id, options.message.as_bytes().to_vec())
        .await
        .map_err(|err| anyhow::anyhow!("phone send_message: {err}"))?;

    let mut replies: Vec<String> = Vec::new();
    let mut saw_start = false;
    let mut saw_finalize = false;
    let deadline = Instant::now() + options.timeout;

    while Instant::now() < deadline {
        let satisfied = replies.len() >= options.min_replies.max(1)
            && options
                .expect
                .iter()
                .all(|needle| replies.iter().any(|reply| reply.contains(needle)))
            && (!options.require_job_final || saw_finalize);
        if satisfied {
            break;
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let tick = std::cmp::min(remaining, Duration::from_millis(250));
        tokio::select! {
            _ = tokio::time::sleep(tick) => {}
            update = phone_messages.recv() => {
                match update {
                    Some(RuntimeMessageUpdate::Message(message))
                        if message.message.sender == desktop_id =>
                    {
                        // The durable stream final is now a kind-9 chat with
                        // a stream tag, so it is both a real reply and the
                        // finalization signal.
                        if message
                            .message
                            .tags
                            .iter()
                            .any(|tag| tag.first().is_some_and(|name| name == STREAM_TAG))
                        {
                            saw_finalize = true;
                        }
                        replies.push(message.message.plaintext);
                    }
                    Some(_) => {}
                    None => break,
                }
            }
            event = phone_events.recv() => {
                match event {
                    Ok(MarmotAppEvent::AgentStreamStarted(stream))
                        if stream.account_id_hex == phone_id =>
                    {
                        saw_start = true;
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    Err(_) => {}
                }
            }
        }
    }

    desktop_task.abort();
    runtime.shutdown().await;
    let _ = saw_start;

    let matched: Vec<String> = options
        .expect
        .iter()
        .filter(|needle| replies.iter().any(|reply| reply.contains(needle.as_str())))
        .cloned()
        .collect();

    Ok(FakePhoneResult {
        phone_npub,
        group_id: group_id_hex,
        replies,
        matched,
        saw_job_final: saw_finalize,
    })
}

async fn run_live_roundtrip(
    base_config: &Config,
    options: LiveFakePhoneRoundtrip,
) -> Result<LiveFakePhoneResult> {
    fs::create_dir_all(&options.root)
        .with_context(|| format!("creating {}", options.root.display()))?;

    let relay = MockRelay::run()
        .await
        .map_err(|e| anyhow::anyhow!("starting MockRelay: {e}"))?;
    let relay_url = relay.url().await.to_string();
    let relays = vec![relay_url.clone()];
    let endpoints = vec![TransportEndpoint(relay_url.clone())];

    let desktop_root = options.root.join("desktop");
    let desktop_config_path = desktop_root.join("config.toml");
    let mut desktop_config = live_desktop_config(base_config, &desktop_root, &relays)?;
    install_fake_codex(&mut desktop_config, &desktop_root)?;
    desktop_config.save(&desktop_config_path)?;

    let desktop_id = ensure_live_desktop_account(&desktop_config, &relays).await?;
    let desktop_npub = npub_from_account_id(&desktop_id)?;
    desktop_config.darkmatter.account = Some(desktop_npub.clone());
    desktop_config.darkmatter.bot_npub = Some(desktop_npub.clone());
    desktop_config.save(&desktop_config_path)?;

    let phone_root = options.root.join("phone");
    let phone = create_live_phone(&phone_root, endpoints).await?;

    desktop_config.darkmatter.allowed_senders = vec![phone.account_id_hex.clone()];
    desktop_config.darkmatter.require_pairing_pin = false;
    desktop_config.save(&desktop_config_path)?;

    let mut transport = LiveChildProcess::start_transport(
        &desktop_config_path,
        &desktop_config
            .resolved_log_dir()
            .join("fake-phone-transport"),
    )?;
    transport.wait_ready(Duration::from_secs(20))?;
    let mut worker = if options.start_worker {
        let mut worker = LiveChildProcess::start_worker(
            &desktop_config_path,
            &desktop_config.resolved_log_dir().join("fake-phone-worker"),
        )?;
        worker.wait_ready(Duration::from_secs(20))?;
        Some(worker)
    } else {
        None
    };

    let group_id = phone
        .runtime
        .create_group(
            &phone.account_id_hex,
            &options.group_name,
            std::slice::from_ref(&desktop_id),
            None,
        )
        .await
        .map_err(|err| anyhow::anyhow!("fake phone create_group: {err}"))?;
    let group_id_hex = hex::encode(group_id.as_slice());

    let event_log = desktop_config.resolved_event_log_path();
    let outcome = send_live_until_replies(
        LiveSendContext {
            runtime: &phone.runtime,
            phone_id: &phone.account_id_hex,
            desktop_id: &desktop_id,
            group_id: &group_id,
            group_id_hex: &group_id_hex,
            event_log: &event_log,
            transport: &mut transport,
            worker: worker.as_mut(),
        },
        &options,
    )
    .await?;

    phone.runtime.shutdown().await;

    if !outcome.satisfied() {
        bail!(
            "{}\n{}",
            outcome.failure_message(),
            live_failure_detail(&transport, &event_log)
        );
    }

    Ok(LiveFakePhoneResult {
        root: options.root,
        desktop_npub,
        phone_npub: phone.npub,
        group_id: group_id_hex,
        relay_url,
        replies: outcome.replies,
        matched: outcome.matched,
        saw_job_final: outcome.saw_job_final,
        saw_inbound_journal: outcome.saw_inbound_journal,
        saw_outbound_journal: outcome.saw_outbound_journal,
        transport_stdout: transport.stdout_path.clone(),
        transport_stderr: transport.stderr_path.clone(),
        worker_stdout: worker.as_ref().map(|worker| worker.stdout_path.clone()),
        worker_stderr: worker.as_ref().map(|worker| worker.stderr_path.clone()),
        event_log,
    })
}

fn live_desktop_config(base: &Config, root: &Path, relays: &[String]) -> Result<Config> {
    let mut config = base.clone();
    config.instance = None;
    config.runner.launcher = RunnerLauncher::Direct;
    config.runner.data_dir = root.join("data").display().to_string();
    config.runner.log_dir = root.join("logs").display().to_string();
    config.runner.worktree_dir = root.join("worktrees").display().to_string();
    config.darkmatter.group_id.clear();
    config.darkmatter.group_ids.clear();
    config.darkmatter.account = None;
    config.darkmatter.bot_sender = None;
    config.darkmatter.bot_npub = None;
    config.darkmatter.allowed_senders.clear();
    config.darkmatter.require_pairing_pin = false;
    config.darkmatter.dev_burner_nsec = true;
    config.darkmatter.message_relays = relays.to_vec();
    config.darkmatter.pairing_relays = relays.to_vec();

    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace)
        .with_context(|| format!("creating fake desktop workspace {}", workspace.display()))?;
    if let Some(repo) = config.repos.first_mut() {
        repo.alias = "sandbox".to_string();
        repo.path = workspace.display().to_string();
    }
    config.validate()?;
    Ok(config)
}

fn install_fake_codex(config: &mut Config, root: &Path) -> Result<()> {
    let bin = root.join("bin").join("fake-codex");
    if let Some(parent) = bin.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    fs::write(
        &bin,
        "#!/bin/sh\nprintf '%s\\n' '{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"agentnoise-darkmatter-live-ok\"}}'\n",
    )
    .with_context(|| format!("writing {}", bin.display()))?;
    set_executable(&bin)?;
    config.agents.codex.bin = bin.display().to_string();
    config.runner.job_timeout_seconds = 30;
    config.runner.startup_silence_timeout_seconds = 5;
    Ok(())
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)
        .with_context(|| format!("reading permissions for {}", path.display()))?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)
        .with_context(|| format!("setting executable bit on {}", path.display()))
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<()> {
    Ok(())
}

async fn ensure_live_desktop_account(config: &Config, relays: &[String]) -> Result<String> {
    let keychain_service = keychain_service_for_instance(config.instance.as_deref());
    let engine = DarkmatterEngine::open(
        config.resolved_data_dir().join("darkmatter"),
        relays.to_vec(),
        &keychain_service,
        config.darkmatter.dev_burner_nsec,
    )?;
    engine.start().await?;
    let account_id_hex = engine
        .ensure_account(config.darkmatter.account.as_deref(), relays)
        .await?;
    engine
        .publish_discovery(&account_id_hex, &config.darkmatter)
        .await
        .context("publishing fake desktop discovery")?;
    engine.shutdown().await;
    Ok(account_id_hex)
}

struct LivePhone {
    runtime: MarmotAppRuntime,
    account_id_hex: String,
    npub: String,
}

async fn create_live_phone(root: &Path, endpoints: Vec<TransportEndpoint>) -> Result<LivePhone> {
    fs::create_dir_all(root).with_context(|| format!("creating {}", root.display()))?;
    let relays = endpoints
        .iter()
        .map(|endpoint| endpoint.0.clone())
        .collect::<Vec<_>>();
    let app = MarmotApp::with_relays(root, relays);
    let runtime = MarmotAppRuntime::new(app);
    runtime
        .start()
        .await
        .map_err(|err| anyhow::anyhow!("starting fake phone runtime: {err}"))?;
    let setup = AccountSetupRequest {
        identity: None,
        default_relays: endpoints.clone(),
        bootstrap_relays: endpoints,
        publish_missing_relay_lists: true,
        publish_initial_key_package: true,
    };
    let phone = runtime
        .create_identity(setup)
        .await
        .map_err(|err| anyhow::anyhow!("creating fake phone identity: {err}"))?;
    let account_id_hex = phone.account.account_id_hex;
    let npub = npub_from_account_id(&account_id_hex)?;
    Ok(LivePhone {
        runtime,
        account_id_hex,
        npub,
    })
}

struct TransportLogLine {
    text: String,
}

struct LiveChildProcess {
    label: String,
    child: Child,
    line_rx: Receiver<TransportLogLine>,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
}

impl LiveChildProcess {
    fn start_transport(config_path: &Path, log_dir: &Path) -> Result<Self> {
        Self::start(
            config_path,
            log_dir,
            "transport",
            &["transport", "run", "--ssh", "--no-daemon"],
        )
    }

    fn start_worker(config_path: &Path, log_dir: &Path) -> Result<Self> {
        Self::start(
            config_path,
            log_dir,
            "worker",
            &["worker", "start", "--poll-seconds", "1"],
        )
    }

    fn start(config_path: &Path, log_dir: &Path, label: &str, args: &[&str]) -> Result<Self> {
        fs::create_dir_all(log_dir)
            .with_context(|| format!("creating {label} log dir {}", log_dir.display()))?;
        let stdout_path = log_dir.join(format!("{label}.stdout.log"));
        let stderr_path = log_dir.join(format!("{label}.stderr.log"));
        fs::write(&stdout_path, "")
            .with_context(|| format!("creating {}", stdout_path.display()))?;
        fs::write(&stderr_path, "")
            .with_context(|| format!("creating {}", stderr_path.display()))?;

        let exe = std::env::current_exe().context("resolving current agentnoise executable")?;
        let mut child = ProcessCommand::new(exe)
            .arg("--config")
            .arg(config_path)
            .args(args)
            .env("AGENTNOISE_ALLOW_LAUNCHD_CODEX", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("starting live fake-phone {label}"))?;

        let stdout = child
            .stdout
            .take()
            .context("transport stdout pipe unavailable")?;
        let stderr = child
            .stderr
            .take()
            .context("transport stderr pipe unavailable")?;
        let (tx, rx) = mpsc::channel();
        spawn_log_reader(stdout, stdout_path.clone(), tx.clone());
        spawn_log_reader(stderr, stderr_path.clone(), tx);

        Ok(Self {
            label: label.to_string(),
            child,
            line_rx: rx,
            stdout_path,
            stderr_path,
        })
    }

    fn wait_ready(&mut self, timeout: Duration) -> Result<()> {
        let ready_transport = "agentnoise listening";
        let ready_worker = "agentnoise worker running";
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            self.ensure_alive()?;
            match self.line_rx.recv_timeout(Duration::from_millis(250)) {
                Ok(line)
                    if line.text.contains(ready_transport) || line.text.contains(ready_worker) =>
                {
                    return Ok(());
                }
                Ok(_) => {}
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.ensure_alive()?;
                    bail!("{} log stream closed before readiness", self.label);
                }
            }
        }
        bail!(
            "{} did not become ready within {}s\n{}",
            self.label,
            timeout.as_secs(),
            live_failure_detail(self, &PathBuf::new())
        )
    }

    fn ensure_alive(&mut self) -> Result<()> {
        if let Some(status) = self.child.try_wait().context("checking transport child")? {
            bail!(
                "{} exited before fake-phone roundtrip completed: {status}\n{}",
                self.label,
                live_failure_detail(self, &PathBuf::new())
            );
        }
        Ok(())
    }
}

impl Drop for LiveChildProcess {
    fn drop(&mut self) {
        self.child.kill().ok();
        self.child.wait().ok();
    }
}

fn spawn_log_reader<R>(reader: R, path: PathBuf, tx: mpsc::Sender<TransportLogLine>)
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let file = OpenOptions::new().create(true).append(true).open(&path);
        let mut file = match file {
            Ok(file) => file,
            Err(_) => return,
        };
        for line in BufReader::new(reader).lines() {
            let text = match line {
                Ok(text) => text,
                Err(error) => format!("log read error: {error}"),
            };
            let _ = writeln!(file, "{text}");
            let _ = tx.send(TransportLogLine { text });
        }
    });
}

struct LiveSendContext<'a> {
    runtime: &'a MarmotAppRuntime,
    phone_id: &'a str,
    desktop_id: &'a str,
    group_id: &'a cgka_traits::GroupId,
    group_id_hex: &'a str,
    event_log: &'a Path,
    transport: &'a mut LiveChildProcess,
    worker: Option<&'a mut LiveChildProcess>,
}

async fn send_live_until_replies(
    mut context: LiveSendContext<'_>,
    options: &LiveFakePhoneRoundtrip,
) -> Result<LiveReplyOutcome> {
    let mut messages = context
        .runtime
        .subscribe_messages(
            context.phone_id,
            AppMessageQuery {
                group_id_hex: Some(context.group_id_hex.to_string()),
                limit: None,
            },
        )
        .map_err(|err| anyhow::anyhow!("fake phone subscribe_messages: {err}"))?;
    let mut events = context.runtime.subscribe();
    let mut outcome = LiveReplyOutcome::new(options);
    let deadline = Instant::now() + options.timeout;
    let mut last_send = Instant::now() - Duration::from_secs(5);

    while Instant::now() < deadline {
        context.transport.ensure_alive()?;
        if let Some(worker) = context.worker.as_deref_mut() {
            worker.ensure_alive()?;
        }
        outcome.refresh_journal_flags(context.event_log, context.group_id_hex, &options.message);
        if outcome.satisfied() {
            break;
        }

        if last_send.elapsed() >= Duration::from_secs(5) {
            context
                .runtime
                .send_message(
                    context.phone_id,
                    context.group_id,
                    options.message.as_bytes().to_vec(),
                )
                .await
                .map_err(|err| anyhow::anyhow!("fake phone send_message: {err}"))?;
            last_send = Instant::now();
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let tick = std::cmp::min(remaining, Duration::from_millis(250));
        tokio::select! {
            _ = tokio::time::sleep(tick) => {}
            update = messages.recv() => {
                match update {
                    Some(RuntimeMessageUpdate::Message(message))
                        if message.message.sender == context.desktop_id =>
                    {
                        if message
                            .message
                            .tags
                            .iter()
                            .any(|tag| tag.first().is_some_and(|name| name == STREAM_TAG))
                        {
                            outcome.saw_job_final = true;
                        }
                        outcome.record(message.message.plaintext);
                    }
                    Some(_) => {}
                    None => break,
                }
            }
            event = events.recv() => {
                match event {
                    Ok(MarmotAppEvent::AgentStreamStarted(stream))
                        if stream.account_id_hex == context.phone_id =>
                    {
                        let _ = stream;
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                }
            }
        }
    }

    outcome.refresh_journal_flags(context.event_log, context.group_id_hex, &options.message);
    Ok(outcome)
}

#[derive(Debug)]
struct LiveReplyOutcome {
    replies: Vec<String>,
    matched: Vec<String>,
    expected: Vec<String>,
    min_replies: usize,
    require_job_final: bool,
    saw_job_final: bool,
    saw_inbound_journal: bool,
    saw_outbound_journal: bool,
}

impl LiveReplyOutcome {
    fn new(options: &LiveFakePhoneRoundtrip) -> Self {
        Self {
            replies: Vec::new(),
            matched: Vec::new(),
            expected: options
                .expect
                .iter()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .collect(),
            min_replies: options.min_replies.max(1),
            require_job_final: options.require_job_final,
            saw_job_final: false,
            saw_inbound_journal: false,
            saw_outbound_journal: false,
        }
    }

    fn record(&mut self, reply: String) {
        if live_is_job_final_reply(&reply) {
            self.saw_job_final = true;
        }
        for expected in &self.expected {
            if reply.contains(expected) && !self.matched.iter().any(|value| value == expected) {
                self.matched.push(expected.clone());
            }
        }
        self.replies.push(reply);
    }

    fn refresh_journal_flags(&mut self, event_log: &Path, group_id: &str, sent_message: &str) {
        let flags = journal_flags(event_log, group_id, sent_message);
        self.saw_inbound_journal |= flags.0;
        self.saw_outbound_journal |= flags.1;
    }

    fn satisfied(&self) -> bool {
        self.replies.len() >= self.min_replies
            && self.matched.len() == self.expected.len()
            && (!self.require_job_final || self.saw_job_final)
            && self.saw_inbound_journal
            && self.saw_outbound_journal
    }

    fn failure_message(&self) -> String {
        let mut missing = Vec::new();
        if self.replies.len() < self.min_replies {
            missing.push(format!(
                "received {} reply/replies, need {}",
                self.replies.len(),
                self.min_replies
            ));
        }
        let unmatched = self
            .expected
            .iter()
            .filter(|expected| !self.matched.iter().any(|matched| matched == *expected))
            .cloned()
            .collect::<Vec<_>>();
        if !unmatched.is_empty() {
            missing.push(format!("missing expected text: {}", unmatched.join(", ")));
        }
        if self.require_job_final && !self.saw_job_final {
            missing.push("missing final job reply".to_string());
        }
        if !self.saw_inbound_journal {
            missing.push("missing inbound event journal entry".to_string());
        }
        if !self.saw_outbound_journal {
            missing.push("missing successful outbound event journal entry".to_string());
        }
        let detail = self
            .replies
            .last()
            .map(|reply| format!("last reply: {reply}"))
            .unwrap_or_else(|| "no replies received".to_string());
        format!(
            "live fake-phone roundtrip timed out: {}; {}",
            missing.join("; "),
            detail
        )
    }
}

fn journal_flags(event_log: &Path, group_id: &str, sent_message: &str) -> (bool, bool) {
    let Ok(text) = fs::read_to_string(event_log) else {
        return (false, false);
    };
    let mut inbound = false;
    let mut outbound = false;
    let sent = sent_message.trim();
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let Ok(event) = serde_json::from_str::<RuntimeEvent>(line) else {
            continue;
        };
        if event.group_id != group_id {
            continue;
        }
        match event.direction {
            EventDirection::Inbound => {
                if event
                    .preview
                    .as_deref()
                    .is_some_and(|preview| preview.contains(sent))
                {
                    inbound = true;
                }
            }
            EventDirection::Outbound => {
                if event.kind == "reply-sent" && event.ok {
                    outbound = true;
                }
            }
        }
    }
    (inbound, outbound)
}

fn live_failure_detail(transport: &LiveChildProcess, event_log: &Path) -> String {
    let mut lines = vec![
        format!(
            "{} stdout: {}",
            transport.label,
            transport.stdout_path.display()
        ),
        format!(
            "{} stderr: {}",
            transport.label,
            transport.stderr_path.display()
        ),
    ];
    if !event_log.as_os_str().is_empty() {
        lines.push(format!("event log: {}", event_log.display()));
    }
    if let Some(stderr) = log_excerpt(&transport.stderr_path) {
        lines.push(format!("stderr excerpt:\n{stderr}"));
    }
    if let Some(stdout) = log_excerpt(&transport.stdout_path) {
        lines.push(format!("stdout excerpt:\n{stdout}"));
    }
    if !event_log.as_os_str().is_empty()
        && let Some(events) = log_excerpt(event_log)
    {
        lines.push(format!("event log excerpt:\n{events}"));
    }
    lines.join("\n")
}

fn log_excerpt(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let lines = text.lines().rev().take(20).collect::<Vec<_>>();
    Some(lines.into_iter().rev().collect::<Vec<_>>().join("\n"))
}

fn live_is_job_final_reply(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    text.lines()
        .next()
        .is_some_and(|line| line.contains(" done"))
        || text.contains("completed")
        || text.contains("failed")
        || text.contains("cancelled")
        || text.contains("job did not succeed")
}

async fn handle_desktop_message(
    runtime: &MarmotAppRuntime,
    account_id_hex: &str,
    group_id: &cgka_traits::GroupId,
    text: &str,
    pin: Option<&str>,
) -> Result<()> {
    let stream_id = stream_id_for_message(text);
    let started_at = current_unix_seconds();

    // The brokered-QUIC route requires at least one candidate even if the
    // harness never opens a real QUIC channel — the start/finish envelopes
    // alone exercise the protocol-layer wiring.
    let quic_candidates = vec!["quic://127.0.0.1:0".to_string()];
    let (_envelope, summary) = runtime
        .start_agent_text_stream(
            account_id_hex,
            group_id,
            &stream_id,
            started_at,
            quic_candidates,
        )
        .await
        .map_err(|err| anyhow::anyhow!("start_agent_text_stream: {err}"))?;
    let (start_event_id, start_event_id_hex) = start_event_id_from_summary(&summary)?;

    let reply = render_fake_desktop_reply(text, pin);
    runtime
        .send_message(account_id_hex, group_id, reply.as_bytes().to_vec())
        .await
        .map_err(|err| anyhow::anyhow!("desktop reply send_message: {err}"))?;

    let mut transcript =
        AgentTextStreamTranscriptV1::new(stream_id.to_vec(), start_event_id.clone());
    transcript.append(1, AGENT_TEXT_STREAM_RECORD_TEXT_DELTA, reply.as_bytes());

    let finish_request = AgentTextStreamFinishRequest {
        stream_id: stream_id.to_vec(),
        start_event_id: start_event_id_hex,
        final_text_or_reference: reply,
        transcript_hash: transcript.hash(),
        chunk_count: transcript.chunk_count(),
        finished_at: current_unix_seconds(),
    };
    runtime
        .finish_agent_text_stream(account_id_hex, group_id, finish_request)
        .await
        .map_err(|err| anyhow::anyhow!("finish_agent_text_stream: {err}"))?;
    Ok(())
}

fn render_fake_desktop_reply(prompt: &str, pin: Option<&str>) -> String {
    let trimmed = prompt.trim();
    if let Some(pin) = pin
        && trimmed == pin
    {
        return format!("paired (PIN {pin})");
    }
    if let Some(rest) = trimmed.strip_prefix("/help") {
        let _ = rest;
        return "agentnoise (fake-phone) commands: /help /status /codex <prompt>".to_string();
    }
    if let Some(rest) = trimmed.strip_prefix("/codex") {
        let prompt = rest.trim();
        return format!("codex queued: {prompt}\ncompleted in 0s (synthetic)");
    }
    format!("agentnoise (fake-phone) received: {trimmed}")
}

fn stream_id_for_message(message: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"agentnoise.fake-phone.stream:");
    hasher.update(message.as_bytes());
    hasher.finalize().into()
}

fn current_unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn npub_from_account_id(account_id_hex: &str) -> Result<String> {
    use nostr::PublicKey;
    use nostr::nips::nip19::ToBech32;
    let pk = PublicKey::from_hex(account_id_hex).context("decoding account id hex")?;
    pk.to_bech32().context("encoding npub bech32")
}

async fn wait_for_event<F>(
    events: &mut tokio::sync::broadcast::Receiver<MarmotAppEvent>,
    timeout: Duration,
    mut matches_event: F,
) -> Result<MarmotAppEvent>
where
    F: FnMut(&MarmotAppEvent) -> bool + Send,
{
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            anyhow::bail!("timed out waiting for event");
        }
        let received = tokio::time::timeout(remaining, events.recv())
            .await
            .map_err(|_| anyhow::anyhow!("timed out waiting for event"))?;
        match received {
            Ok(event) => {
                if matches_event(&event) {
                    return Ok(event);
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                anyhow::bail!("event broadcast closed before match");
            }
        }
    }
}
