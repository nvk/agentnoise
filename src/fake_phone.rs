use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use nostr::Keys;
use nostr::nips::nip19::ToBech32;
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use crate::auth::is_pairing_pin_message;
use crate::config::{Config, WhitenoiseConfig};
use crate::runtime;
use crate::secrets;
use crate::whitenoise_cli;
use crate::wn::WnClient;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FakePhonePlan {
    pub root: PathBuf,
    pub data_dir: PathBuf,
    pub logs_dir: PathBuf,
    pub socket: PathBuf,
    pub nsec_file: PathBuf,
    pub group_file: PathBuf,
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
    pub shared_daemon: bool,
}

#[derive(Debug, Clone)]
pub struct FakePhoneTerminal {
    pub root: PathBuf,
    pub pin: Option<String>,
    pub group_name: String,
    pub shared_daemon: bool,
    pub follow_handoffs: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FakePhoneResult {
    pub phone_npub: String,
    pub group_id: String,
    pub replies: Vec<String>,
    pub matched: Vec<String>,
    pub saw_job_final: bool,
}

pub fn plan(config: &Config, root: Option<&Path>) -> FakePhonePlan {
    let root = root
        .map(Path::to_path_buf)
        .unwrap_or_else(|| config.resolved_data_dir().join("fake-phone"));
    let data_dir = root.join("wnd-data");
    let logs_dir = root.join("wnd-logs");
    let socket = data_dir.join("release").join("wnd.sock");
    let nsec_file = root.join("fake-phone.nsec");
    let group_file = root.join("fake-phone.group.json");
    FakePhonePlan {
        root,
        data_dir,
        logs_dir,
        socket,
        nsec_file,
        group_file,
    }
}

pub fn roundtrip(config: &Config, options: FakePhoneRoundtrip) -> Result<FakePhoneResult> {
    let plan = plan(config, Some(&options.root));
    fs::create_dir_all(&plan.root).with_context(|| format!("creating {}", plan.root.display()))?;
    fs::create_dir_all(&plan.data_dir)
        .with_context(|| format!("creating {}", plan.data_dir.display()))?;
    fs::create_dir_all(&plan.logs_dir)
        .with_context(|| format!("creating {}", plan.logs_dir.display()))?;

    let mut daemon = if options.shared_daemon {
        None
    } else {
        let mut daemon = ChildGuard::new(start_fake_wnd(config, &plan)?);
        wait_for_socket(&plan, &mut daemon.child, Duration::from_secs(10))?;
        Some(daemon)
    };

    let mut fake_config = fake_whitenoise_config(config, &plan, options.shared_daemon);
    let phone_npub = create_or_reuse_identity(&fake_config, &plan)?;
    fake_config.account = Some(phone_npub.clone());
    let agent_npub = config
        .whitenoise
        .bot_npub
        .as_deref()
        .or(config.whitenoise.account.as_deref())
        .map(str::trim)
        .filter(|npub| !npub.is_empty())
        .context("config has no agentnoise npub; run `agentnoise setup` first")?;
    let group_id = create_or_reuse_group(&fake_config, &plan, &options.group_name, agent_npub)?;
    let client = WnClient::new(fake_config.clone_with_group(&group_id));
    if let Some(pin) = options
        .pin
        .as_deref()
        .map(str::trim)
        .filter(|pin| !pin.is_empty())
    {
        client.send_to(&group_id, pin)?;
        thread::sleep(Duration::from_secs(1));
    }

    let outcome = send_until_replies(config, &client, &fake_config, &group_id, &options)?;
    if !outcome.satisfied() {
        bail!("{}", outcome.failure_message());
    }
    drop(daemon.take());
    Ok(FakePhoneResult {
        phone_npub,
        group_id,
        replies: outcome.replies,
        matched: outcome.matched,
        saw_job_final: outcome.saw_job_final,
    })
}

pub fn terminal(config: &Config, options: FakePhoneTerminal) -> Result<()> {
    let plan = plan(config, Some(&options.root));
    fs::create_dir_all(&plan.root).with_context(|| format!("creating {}", plan.root.display()))?;
    fs::create_dir_all(&plan.data_dir)
        .with_context(|| format!("creating {}", plan.data_dir.display()))?;
    fs::create_dir_all(&plan.logs_dir)
        .with_context(|| format!("creating {}", plan.logs_dir.display()))?;

    let _daemon = if options.shared_daemon {
        None
    } else {
        let mut daemon = ChildGuard::new(start_fake_wnd(config, &plan)?);
        wait_for_socket(&plan, &mut daemon.child, Duration::from_secs(10))?;
        Some(daemon)
    };

    let mut fake_config = fake_whitenoise_config(config, &plan, options.shared_daemon);
    let phone_npub = create_or_reuse_identity(&fake_config, &plan)?;
    fake_config.account = Some(phone_npub.clone());
    let agent_npub = config
        .whitenoise
        .bot_npub
        .as_deref()
        .or(config.whitenoise.account.as_deref())
        .map(str::trim)
        .filter(|npub| !npub.is_empty())
        .context("config has no agentnoise npub; run `agentnoise setup` first")?;
    let group_id = create_or_reuse_group(&fake_config, &plan, &options.group_name, agent_npub)?;
    let client = WnClient::new(fake_config.clone_with_group(&group_id));
    if let Some(pin) = options
        .pin
        .as_deref()
        .map(str::trim)
        .filter(|pin| !pin.is_empty())
    {
        client.send_to(&group_id, pin)?;
    }

    run_terminal_loop(
        client,
        fake_config,
        phone_npub,
        group_id,
        options.follow_handoffs,
    )
}

#[derive(Debug)]
enum TerminalEvent {
    Input(String),
    EndInput,
    Message(MessageForTerminal),
    Error(String),
}

#[derive(Debug)]
struct MessageForTerminal {
    group_id: String,
    sender: Option<String>,
    text: String,
    attachments: usize,
}

#[derive(Debug, PartialEq, Eq)]
enum TerminalCommand {
    Help,
    Quit,
    Chats,
    Use(String),
    Attach {
        path: PathBuf,
        caption: Option<String>,
    },
    Pin(String),
    Send(String),
    Empty,
}

fn run_terminal_loop(
    client: WnClient,
    config: WhitenoiseConfig,
    phone_npub: String,
    initial_group_id: String,
    follow_handoffs: bool,
) -> Result<()> {
    let (tx, rx) = mpsc::channel();
    spawn_terminal_stdin(tx.clone());

    let mut active_group = initial_group_id.clone();
    let mut groups = vec![initial_group_id.clone()];
    let mut children = vec![subscribe_terminal_group(
        client.clone(),
        config.clone(),
        initial_group_id.clone(),
        tx.clone(),
    )?];

    print_terminal_intro(&phone_npub, &initial_group_id, follow_handoffs);
    print_terminal_prompt(&active_group);
    for item in rx {
        match item {
            TerminalEvent::Input(line) => {
                let command = parse_terminal_input(&line);
                let should_quit =
                    handle_terminal_command(&client, &mut active_group, &groups, command)?;
                if should_quit {
                    break;
                }
                print_terminal_prompt(&active_group);
            }
            TerminalEvent::EndInput => break,
            TerminalEvent::Message(message) => {
                print_terminal_message(&active_group, &message);
                if follow_handoffs
                    && let Some(next_group_id) = extract_chat_uri_group_id(&message.text)
                    && !groups.iter().any(|group_id| group_id == &next_group_id)
                {
                    match subscribe_terminal_group(
                        client.clone(),
                        config.clone(),
                        next_group_id.clone(),
                        tx.clone(),
                    ) {
                        Ok(child) => {
                            children.push(child);
                            groups.push(next_group_id.clone());
                            active_group = next_group_id.clone();
                            println!(
                                "fake-phone: followed handoff; active chat {}",
                                short_group_id(&active_group)
                            );
                        }
                        Err(error) => {
                            println!(
                                "fake-phone: failed to follow handoff {}: {error:#}",
                                short_group_id(&next_group_id)
                            );
                        }
                    }
                }
                print_terminal_prompt(&active_group);
            }
            TerminalEvent::Error(error) => {
                println!("fake-phone: {error}");
                print_terminal_prompt(&active_group);
            }
        }
    }

    for child in &mut children {
        child.kill().ok();
        child.wait().ok();
    }
    println!();
    println!("fake-phone: bye");
    Ok(())
}

fn spawn_terminal_stdin(tx: mpsc::Sender<TerminalEvent>) {
    thread::spawn(move || {
        let stdin = io::stdin();
        for line in stdin.lock().lines() {
            match line {
                Ok(line) => {
                    if tx.send(TerminalEvent::Input(line)).is_err() {
                        return;
                    }
                }
                Err(error) => {
                    let _ = tx.send(TerminalEvent::Error(format!("stdin failed: {error}")));
                    break;
                }
            }
        }
        let _ = tx.send(TerminalEvent::EndInput);
    });
}

fn subscribe_terminal_group(
    client: WnClient,
    config: WhitenoiseConfig,
    group_id: String,
    tx: mpsc::Sender<TerminalEvent>,
) -> Result<Child> {
    if let Err(error) = whitenoise_cli::accept_group(&config, &group_id) {
        eprintln!("agentnoise fake-phone tui: failed to accept group {group_id}: {error:#}");
    }
    let mut child = client.subscribe_group_with_limit(&group_id, 20)?;
    let stdout = child
        .stdout
        .take()
        .context("fake phone tui subscribe did not expose stdout")?;
    thread::spawn(move || {
        for value in WnClient::parse_events_from_reader(stdout) {
            let value = match value {
                Ok(value) => value,
                Err(error) => {
                    let _ = tx.send(TerminalEvent::Error(format!("{error:#}")));
                    return;
                }
            };
            for event in WnClient::parse_events_for_group(&value, &group_id) {
                let text = if event.text.trim().is_empty() {
                    event.unsupported.unwrap_or_default()
                } else {
                    event.text
                };
                let _ = tx.send(TerminalEvent::Message(MessageForTerminal {
                    group_id: group_id.clone(),
                    sender: event.sender,
                    text,
                    attachments: event.attachments.len(),
                }));
            }
        }
    });
    Ok(child)
}

fn handle_terminal_command(
    client: &WnClient,
    active_group: &mut String,
    groups: &[String],
    command: TerminalCommand,
) -> Result<bool> {
    match command {
        TerminalCommand::Help => print_terminal_help(),
        TerminalCommand::Quit => return Ok(true),
        TerminalCommand::Chats => print_terminal_chats(active_group, groups),
        TerminalCommand::Use(target) => match resolve_terminal_group(groups, &target) {
            Some(group_id) => {
                *active_group = group_id.to_string();
                println!("fake-phone: active chat {}", short_group_id(active_group));
            }
            None => println!("fake-phone: no such chat: {target}"),
        },
        TerminalCommand::Attach { path, caption } => {
            let media = client.upload_media_to(active_group, &path, caption.as_deref())?;
            let hash = media
                .original_file_hash
                .as_deref()
                .or(media.encrypted_file_hash.as_deref())
                .unwrap_or("uploaded");
            println!(
                "fake-phone: sent attachment {} ({})",
                path.display(),
                short_hash(hash)
            );
        }
        TerminalCommand::Pin(pin) => {
            client.send_to(active_group, &pin)?;
        }
        TerminalCommand::Send(text) => {
            client.send_to(active_group, &text)?;
        }
        TerminalCommand::Empty => {}
    }
    Ok(false)
}

fn parse_terminal_input(line: &str) -> TerminalCommand {
    let line = line.trim();
    if line.is_empty() {
        return TerminalCommand::Empty;
    }
    let Some(rest) = line.strip_prefix(':') else {
        return TerminalCommand::Send(line.to_string());
    };
    let (command, rest) = split_first(rest);
    match command.to_ascii_lowercase().as_str() {
        "help" | "h" | "?" => TerminalCommand::Help,
        "quit" | "q" | "exit" => TerminalCommand::Quit,
        "chats" | "groups" | "g" => TerminalCommand::Chats,
        "use" | "chat" | "switch" => {
            if rest.trim().is_empty() {
                TerminalCommand::Chats
            } else {
                TerminalCommand::Use(rest.trim().to_string())
            }
        }
        "attach" | "media" | "image" | "file" => {
            let (path, caption) = split_first(rest);
            if path.is_empty() {
                TerminalCommand::Help
            } else {
                TerminalCommand::Attach {
                    path: PathBuf::from(path),
                    caption: (!caption.trim().is_empty()).then(|| caption.trim().to_string()),
                }
            }
        }
        "pin" => {
            if rest.trim().is_empty() {
                TerminalCommand::Help
            } else {
                TerminalCommand::Pin(rest.trim().to_string())
            }
        }
        _ => TerminalCommand::Send(line.to_string()),
    }
}

fn split_first(input: &str) -> (&str, &str) {
    let input = input.trim();
    match input.find(char::is_whitespace) {
        Some(index) => (&input[..index], input[index..].trim()),
        None => (input, ""),
    }
}

fn resolve_terminal_group<'a>(groups: &'a [String], target: &str) -> Option<&'a str> {
    if let Ok(index) = target.parse::<usize>()
        && index > 0
    {
        return groups.get(index - 1).map(String::as_str);
    }
    groups
        .iter()
        .find(|group| group.as_str() == target || group.starts_with(target))
        .map(String::as_str)
}

fn print_terminal_intro(phone_npub: &str, group_id: &str, follow_handoffs: bool) {
    println!("agentnoise fake phone");
    println!("npub: {phone_npub}");
    println!("active chat: {}", short_group_id(group_id));
    println!(
        "handoffs: {}",
        if follow_handoffs {
            "auto-follow"
        } else {
            "off"
        }
    );
    println!("type /status, /help, /wiki ... or any message to send it");
    println!("local commands start with ':'; try :help");
}

fn print_terminal_help() {
    println!("fake-phone commands");
    println!("  plain text or /status       send to active chat");
    println!("  :attach <path> [caption]    send a picture/file with optional caption");
    println!("  :pin <code>                 send a pairing PIN");
    println!("  :chats                      list followed chats");
    println!("  :use <number|group-prefix>  switch active chat");
    println!("  :quit                       exit");
}

fn print_terminal_chats(active_group: &str, groups: &[String]) {
    println!("fake-phone chats");
    for (index, group_id) in groups.iter().enumerate() {
        let marker = if group_id == active_group { "*" } else { " " };
        println!("{marker} {}. {}", index + 1, short_group_id(group_id));
    }
}

fn print_terminal_message(active_group: &str, message: &MessageForTerminal) {
    let active = if message.group_id == active_group {
        "*"
    } else {
        " "
    };
    let sender = message
        .sender
        .as_deref()
        .map(short_sender)
        .unwrap_or_else(|| "unknown".to_string());
    let attachments = if message.attachments == 0 {
        String::new()
    } else {
        format!(" [{} attachment(s)]", message.attachments)
    };
    println!(
        "\n{active}[{}] {sender}: {}{attachments}",
        short_group_id(&message.group_id),
        message.text
    );
}

fn print_terminal_prompt(active_group: &str) {
    print!("fake-phone[{}]> ", short_group_id(active_group));
    let _ = io::stdout().flush();
}

fn short_group_id(group_id: &str) -> String {
    group_id.chars().take(8).collect()
}

fn short_sender(sender: &str) -> String {
    if sender.chars().count() <= 12 {
        sender.to_string()
    } else {
        let head = sender.chars().take(8).collect::<String>();
        format!("{head}…")
    }
}

fn short_hash(hash: &str) -> String {
    if hash.chars().count() <= 12 {
        hash.to_string()
    } else {
        hash.chars().take(12).collect()
    }
}

struct ChildGuard {
    child: Child,
}

impl ChildGuard {
    fn new(child: Child) -> Self {
        Self { child }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.child.kill().ok();
        self.child.wait().ok();
    }
}

fn start_fake_wnd(config: &Config, plan: &FakePhonePlan) -> Result<Child> {
    let wnd = whitenoise_cli::resolve_wnd_for_config(&config.whitenoise);
    let relays = config.whitenoise.pairing_relays.join(",");
    let stdout_path = fake_wnd_stdout_path(plan);
    let stderr_path = fake_wnd_stderr_path(plan);
    if plan.socket.exists() {
        fs::remove_file(&plan.socket).with_context(|| {
            format!("removing stale fake phone socket {}", plan.socket.display())
        })?;
    }
    fs::write(&stdout_path, "")
        .with_context(|| format!("creating fake phone stdout log {}", stdout_path.display()))?;
    fs::write(&stderr_path, "")
        .with_context(|| format!("creating fake phone stderr log {}", stderr_path.display()))?;
    let stdout = fs::File::create(&stdout_path)
        .with_context(|| format!("opening fake phone stdout log {}", stdout_path.display()))?;
    let stderr = fs::File::create(&stderr_path)
        .with_context(|| format!("opening fake phone stderr log {}", stderr_path.display()))?;
    Command::new(&wnd)
        .arg("--data-dir")
        .arg(&plan.data_dir)
        .arg("--logs-dir")
        .arg(&plan.logs_dir)
        .arg("--discovery-relays")
        .arg(relays)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .with_context(|| format!("starting fake phone {}", wnd.display()))
}

fn wait_for_socket(plan: &FakePhonePlan, child: &mut Child, timeout: Duration) -> Result<()> {
    let started = Instant::now();
    while started.elapsed() < timeout {
        if plan.socket.exists() {
            return Ok(());
        }
        if let Some(status) = child
            .try_wait()
            .context("checking fake phone White Noise daemon")?
        {
            bail!(
                "fake phone daemon exited before socket appeared: {}\n{}",
                status,
                fake_wnd_failure_detail(plan)
            );
        }
        thread::sleep(Duration::from_millis(250));
    }
    bail!(
        "fake phone socket did not appear: {}\n{}",
        plan.socket.display(),
        fake_wnd_failure_detail(plan)
    )
}

fn fake_wnd_stdout_path(plan: &FakePhonePlan) -> PathBuf {
    plan.logs_dir.join("fake-wnd.stdout.log")
}

fn fake_wnd_stderr_path(plan: &FakePhonePlan) -> PathBuf {
    plan.logs_dir.join("fake-wnd.stderr.log")
}

fn fake_wnd_failure_detail(plan: &FakePhonePlan) -> String {
    let mut lines = vec![
        format!("stdout: {}", fake_wnd_stdout_path(plan).display()),
        format!("stderr: {}", fake_wnd_stderr_path(plan).display()),
        format!("logs: {}", plan.logs_dir.display()),
    ];
    if let Some(stderr) = log_excerpt(&fake_wnd_stderr_path(plan)) {
        lines.push(format!("stderr excerpt:\n{stderr}"));
    }
    if let Some(stdout) = log_excerpt(&fake_wnd_stdout_path(plan)) {
        lines.push(format!("stdout excerpt:\n{stdout}"));
    }
    lines.join("\n")
}

fn log_excerpt(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let lines = text.lines().rev().take(12).collect::<Vec<_>>();
    Some(lines.into_iter().rev().collect::<Vec<_>>().join("\n"))
}

fn fake_whitenoise_config(
    config: &Config,
    plan: &FakePhonePlan,
    shared_daemon: bool,
) -> WhitenoiseConfig {
    let mut fake = config.whitenoise.clone();
    fake.group_id.clear();
    fake.group_ids.clear();
    fake.account = None;
    if !shared_daemon {
        fake.socket = Some(plan.socket.display().to_string());
    }
    fake.allowed_senders.clear();
    fake.use_keychain_nsec = false;
    fake.dev_burner_nsec = false;
    fake
}

trait WithGroup {
    fn clone_with_group(&self, group_id: &str) -> Self;
}

impl WithGroup for WhitenoiseConfig {
    fn clone_with_group(&self, group_id: &str) -> Self {
        let mut config = self.clone();
        config.group_id = group_id.to_string();
        config.group_ids = vec![group_id.to_string()];
        config
    }
}

fn create_or_reuse_identity(config: &WhitenoiseConfig, plan: &FakePhonePlan) -> Result<String> {
    let mut nsec = load_or_create_burner_nsec(plan)?;
    let phone_npub = match npub_from_nsec(&nsec) {
        Ok(npub) => npub,
        Err(error) => {
            nsec.zeroize();
            return Err(error);
        }
    };
    let login = whitenoise_cli::login_with_nsec(config, &nsec, None)
        .with_context(|| format!("logging fake phone identity into {}", plan.socket.display()));
    nsec.zeroize();
    login?;
    Ok(phone_npub)
}

fn load_or_create_burner_nsec(plan: &FakePhonePlan) -> Result<String> {
    match fs::read_to_string(&plan.nsec_file) {
        Ok(secret) => {
            let nsec = secret.trim().to_string();
            secrets::validate_nsec(&nsec)
                .with_context(|| format!("validating {}", plan.nsec_file.display()))?;
            return Ok(nsec);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("reading {}", plan.nsec_file.display()));
        }
    }

    let keys = Keys::generate();
    let nsec = keys.secret_key().to_bech32().expect("nsec bech32");
    write_burner_nsec(&plan.nsec_file, &nsec)?;
    Ok(nsec)
}

fn npub_from_nsec(nsec: &str) -> Result<String> {
    let keys = Keys::parse(nsec).context("parsing fake phone burner nsec")?;
    keys.public_key()
        .to_bech32()
        .context("encoding fake phone npub")
}

fn write_burner_nsec(path: &Path, nsec: &str) -> Result<()> {
    secrets::validate_nsec(nsec)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    fs::write(path, format!("{nsec}\n")).with_context(|| format!("writing {}", path.display()))?;
    set_burner_file_permissions(path)
}

#[cfg(unix)]
fn set_burner_file_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)
        .with_context(|| format!("reading permissions for {}", path.display()))?
        .permissions();
    permissions.set_mode(0o600);
    fs::set_permissions(path, permissions)
        .with_context(|| format!("setting permissions on {}", path.display()))
}

#[cfg(not(unix))]
fn set_burner_file_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct StoredFakePhoneGroup {
    agent_npub: String,
    group_id: String,
}

fn create_or_reuse_group(
    config: &WhitenoiseConfig,
    plan: &FakePhonePlan,
    name: &str,
    agent_npub: &str,
) -> Result<String> {
    if let Some(group) = load_stored_group(plan, agent_npub)? {
        return Ok(group.group_id);
    }
    let group_id = create_group(config, name, agent_npub)?;
    store_group(plan, agent_npub, &group_id)?;
    Ok(group_id)
}

fn load_stored_group(
    plan: &FakePhonePlan,
    agent_npub: &str,
) -> Result<Option<StoredFakePhoneGroup>> {
    let text = match fs::read_to_string(&plan.group_file) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("reading {}", plan.group_file.display()));
        }
    };
    let group: StoredFakePhoneGroup = serde_json::from_str(&text)
        .with_context(|| format!("parsing {}", plan.group_file.display()))?;
    if group.agent_npub == agent_npub && !group.group_id.trim().is_empty() {
        Ok(Some(group))
    } else {
        Ok(None)
    }
}

fn store_group(plan: &FakePhonePlan, agent_npub: &str, group_id: &str) -> Result<()> {
    let group = StoredFakePhoneGroup {
        agent_npub: agent_npub.to_string(),
        group_id: group_id.to_string(),
    };
    let text = serde_json::to_string_pretty(&group).context("serializing fake phone group")?;
    fs::write(&plan.group_file, text)
        .with_context(|| format!("writing {}", plan.group_file.display()))
}

fn create_group(config: &WhitenoiseConfig, name: &str, agent_npub: &str) -> Result<String> {
    let created = whitenoise_cli::create_group(config, name, &[agent_npub.to_string()])?;
    created
        .group_id
        .or_else(|| whitenoise_cli::group_id_from_output(&created.output))
        .context("White Noise did not return a group id")
}

#[derive(Debug)]
struct ReplyOutcome {
    replies: Vec<String>,
    matched: Vec<String>,
    expected: Vec<String>,
    min_replies: usize,
    require_job_final: bool,
    saw_job_final: bool,
}

impl ReplyOutcome {
    fn new(options: &FakePhoneRoundtrip) -> Self {
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
        }
    }

    fn record(&mut self, reply: String) {
        if is_job_final_reply(&reply) {
            self.saw_job_final = true;
        }
        for expected in &self.expected {
            if reply.contains(expected) && !self.matched.iter().any(|value| value == expected) {
                self.matched.push(expected.clone());
            }
        }
        self.replies.push(reply);
    }

    fn satisfied(&self) -> bool {
        self.replies.len() >= self.min_replies
            && self.matched.len() == self.expected.len()
            && (!self.require_job_final || self.saw_job_final)
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
        let detail = if self.replies.is_empty() {
            "no replies received".to_string()
        } else {
            format!("last reply: {}", self.replies.last().unwrap())
        };
        format!(
            "fake phone roundtrip timed out: {}; {}",
            missing.join("; "),
            detail
        )
    }
}

fn send_until_replies(
    agent_config: &Config,
    client: &WnClient,
    config: &WhitenoiseConfig,
    group_id: &str,
    options: &FakePhoneRoundtrip,
) -> Result<ReplyOutcome> {
    let group_id = group_id.to_string();
    let sent_message = options.message.clone();
    let (tx, rx) = mpsc::channel();
    let mut children = vec![subscribe_replies(
        client,
        config,
        &group_id,
        &sent_message,
        tx.clone(),
    )?];
    let mut subscribed_groups = vec![group_id.clone()];

    let started = Instant::now();
    let mut last_send = Instant::now() - Duration::from_secs(10);
    let mut last_pairing_pin_sent = options.pin.as_ref().map(|pin| pin.trim().to_string());
    let mut sent_after_reply = false;
    let mut outcome = ReplyOutcome::new(options);
    while started.elapsed() < options.timeout {
        if !sent_after_reply && last_send.elapsed() >= Duration::from_secs(5) {
            client.send_to(&group_id, &options.message)?;
            last_send = Instant::now();
        }
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(Ok(reply)) => {
                if reply_should_stop_resending(&reply, &outcome.expected) {
                    sent_after_reply = true;
                }
                if let Some(next_group_id) = extract_chat_uri_group_id(&reply)
                    && !subscribed_groups
                        .iter()
                        .any(|group_id| group_id == &next_group_id)
                {
                    children.push(subscribe_replies(
                        client,
                        config,
                        &next_group_id,
                        &sent_message,
                        tx.clone(),
                    )?);
                    subscribed_groups.push(next_group_id);
                }
                if reply_requests_pairing_pin(&reply)
                    && let Some(pin) = current_runtime_pairing_pin(agent_config)?
                    && last_pairing_pin_sent.as_deref() != Some(pin.as_str())
                {
                    client.send_to(&group_id, &pin)?;
                    last_pairing_pin_sent = Some(pin);
                    last_send = Instant::now() - Duration::from_secs(10);
                }
                outcome.record(reply);
                if outcome.satisfied() {
                    break;
                }
            }
            Ok(Err(error)) => {
                for child in &mut children {
                    child.kill().ok();
                }
                return Err(error);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    for child in &mut children {
        child.kill().ok();
    }
    Ok(outcome)
}

fn current_runtime_pairing_pin(config: &Config) -> Result<Option<String>> {
    let Some(status) = runtime::read_status(config)? else {
        return Ok(None);
    };
    let Some(pin) = status
        .pairing
        .and_then(|pairing| pairing.current_pin)
        .filter(|pin| pin.remaining_seconds().unwrap_or(0) > 1)
    else {
        return Ok(None);
    };
    Ok(Some(pin.code))
}

fn subscribe_replies(
    client: &WnClient,
    config: &WhitenoiseConfig,
    group_id: &str,
    sent_message: &str,
    tx: mpsc::Sender<Result<String>>,
) -> Result<Child> {
    if let Err(error) = whitenoise_cli::accept_group(config, group_id) {
        eprintln!("agentnoise fake-phone: failed to accept group {group_id}: {error:#}");
    }
    let mut child = client.subscribe_group_with_limit(group_id, 0)?;
    let stdout = child
        .stdout
        .take()
        .context("fake phone subscribe did not expose stdout")?;
    let reader_group_id = group_id.to_string();
    let sent_message = sent_message.to_string();
    thread::spawn(move || {
        for value in WnClient::parse_events_from_reader(stdout) {
            let value = match value {
                Ok(value) => value,
                Err(error) => {
                    let _ = tx.send(Err(error));
                    return;
                }
            };
            for event in WnClient::parse_events_for_group(&value, &reader_group_id) {
                if is_command_reply(&event.text, &sent_message) {
                    let _ = tx.send(Ok(event.text));
                }
            }
        }
    });
    Ok(child)
}

fn is_command_reply(text: &str, sent_message: &str) -> bool {
    let text = text.trim();
    !text.is_empty()
        && text != sent_message.trim()
        && text != "Paired. Send /help for commands."
        && text != "paired\nsend /help"
        && !text.starts_with("agentnoise up ")
        && !is_pairing_pin_message(text)
        && !text.starts_with("I saw this while catching up after startup,")
}

fn reply_should_stop_resending(reply: &str, expected: &[String]) -> bool {
    if expected.is_empty() {
        return true;
    }
    expected.iter().any(|expected| reply.contains(expected))
        || is_job_session_started_reply(reply)
        || is_job_ack_reply(reply)
        || is_job_final_reply(reply)
}

fn reply_requests_pairing_pin(reply: &str) -> bool {
    let reply = reply.trim();
    reply.starts_with("Pairing required.")
        || reply.starts_with("Pairing PIN invalid or expired.")
        || reply.contains("current desktop/SSH PIN")
}

fn is_job_session_started_reply(text: &str) -> bool {
    let text = text.trim();
    (text.starts_with("Started session: ")
        || text.starts_with("Started work chat: ")
        || text.starts_with("started "))
        && text.contains("whitenoise://chat/")
}

fn is_job_ack_reply(text: &str) -> bool {
    let text = text.trim();
    ((text.starts_with("Got it: ") || text.contains("\nGot it: ")) && text.contains(" job queued"))
        || text.starts_with("Queued.")
        || text.starts_with("Queued resume.")
        || (text.starts_with("Job ") && text.contains(": started"))
        || text
            .lines()
            .next()
            .is_some_and(|line| line.ends_with(" queued") || line.ends_with(" started"))
}

fn extract_chat_uri_group_id(text: &str) -> Option<String> {
    let (_, rest) = text.split_once("whitenoise://chat/")?;
    let group_id = rest
        .chars()
        .take_while(|ch| ch.is_ascii_hexdigit())
        .collect::<String>();
    (!group_id.is_empty()).then_some(group_id)
}

fn is_job_final_reply(text: &str) -> bool {
    let text = text.trim();
    let Some(first) = text.lines().next().map(str::trim) else {
        return false;
    };
    let old_style = (first.starts_with("Job ") || first.starts_with("an-"))
        && (first.contains(" done")
            || first.contains(" succeeded")
            || first.contains(" failed")
            || first.contains(" cancelled")
            || first.contains(" interrupted"));
    let new_style = ["Done", "Failed", "Cancelled", "Interrupted"]
        .iter()
        .any(|status| first.starts_with(&format!("{status} · an-")));

    (old_style || new_style) && (text.contains("\n/tail ") || text.contains(": /tail "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_phone_plan_is_isolated_under_data_dir() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = Config::template();
        config.runner.data_dir = temp.path().join("data").display().to_string();
        let plan = plan(&config, None);
        assert!(plan.root.ends_with("fake-phone"));
        assert!(plan.socket.ends_with("wnd.sock"));
    }

    #[test]
    fn fake_phone_identity_uses_reused_burner_nsec_file() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = Config::template();
        config.runner.data_dir = temp.path().join("data").display().to_string();
        let plan = plan(&config, Some(temp.path()));

        let nsec = load_or_create_burner_nsec(&plan).unwrap();
        let npub = npub_from_nsec(&nsec).unwrap();
        assert!(nsec.starts_with("nsec1"));
        assert!(npub.starts_with("npub1"));
        assert!(plan.nsec_file.is_file());

        let reused = load_or_create_burner_nsec(&plan).unwrap();
        assert_eq!(reused, nsec);
        assert_eq!(npub_from_nsec(&reused).unwrap(), npub);
    }

    #[test]
    fn fake_phone_group_store_is_scoped_to_agent_npub() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = Config::template();
        config.runner.data_dir = temp.path().join("data").display().to_string();
        let plan = plan(&config, Some(temp.path()));

        store_group(&plan, "npub1agent", "abcdef").unwrap();
        assert_eq!(
            load_stored_group(&plan, "npub1agent")
                .unwrap()
                .map(|group| group.group_id),
            Some("abcdef".to_string())
        );
        assert!(load_stored_group(&plan, "npub1other").unwrap().is_none());
    }

    #[test]
    fn fake_phone_tui_parses_local_commands_without_stealing_slash_commands() {
        assert_eq!(
            parse_terminal_input("/status"),
            TerminalCommand::Send("/status".to_string())
        );
        assert_eq!(
            parse_terminal_input(":pin 123456"),
            TerminalCommand::Pin("123456".to_string())
        );
        assert_eq!(
            parse_terminal_input(":use abc123"),
            TerminalCommand::Use("abc123".to_string())
        );
        assert_eq!(parse_terminal_input(":quit"), TerminalCommand::Quit);
    }

    #[test]
    fn fake_phone_tui_parses_attachment_command() {
        assert_eq!(
            parse_terminal_input(":attach /tmp/shot.png /wiki inspect this"),
            TerminalCommand::Attach {
                path: PathBuf::from("/tmp/shot.png"),
                caption: Some("/wiki inspect this".to_string()),
            }
        );
    }

    #[test]
    fn fake_phone_tui_resolves_group_by_number_or_prefix() {
        let groups = vec![
            "abcdef0123456789".to_string(),
            "feedface01234567".to_string(),
        ];

        assert_eq!(
            resolve_terminal_group(&groups, "2"),
            Some("feedface01234567")
        );
        assert_eq!(
            resolve_terminal_group(&groups, "abcdef"),
            Some("abcdef0123456789")
        );
        assert_eq!(resolve_terminal_group(&groups, "3"), None);
    }

    #[test]
    fn fake_phone_reply_filter_ignores_harness_noise() {
        assert!(!is_command_reply("", "/status"));
        assert!(!is_command_reply("/status", "/status"));
        assert!(!is_command_reply("123456", "/status"));
        assert!(!is_command_reply("/pair 123-456", "/status"));
        assert!(!is_command_reply(
            " Paired. Send /help for commands. ",
            "/status"
        ));
        assert!(!is_command_reply("paired\nsend /help", "/status"));
        assert!(!is_command_reply(
            "I saw this while catching up after startup, so I did not run it:\n/status\nSend it again now, or send /help.",
            "/status"
        ));
        assert!(!is_command_reply(
            "agentnoise up 19:31Z\nfrontier\nsandbox:/\n/status /help",
            "/status"
        ));
        assert!(is_command_reply(
            "agentnoise: running\nlauncher: bondage\nchat: main\nworkspace: sandbox:/",
            "/status"
        ));
    }

    #[test]
    fn fake_phone_only_stops_resending_on_useful_expected_replies() {
        let expected = vec!["done".to_string()];
        assert!(!reply_should_stop_resending(
            "This sender is not paired with agentnoise.",
            &expected
        ));
        assert!(reply_should_stop_resending(
            "Queued.\ncodex · sandbox:/\nI'll post the answer here.",
            &expected
        ));
        assert!(reply_should_stop_resending(
            "an-12345 started\ncodex",
            &expected
        ));
        assert!(reply_should_stop_resending(
            "Started work chat: m5-research\nOpen: whitenoise://chat/abcdef0123456789",
            &expected
        ));
        assert!(reply_should_stop_resending("all done", &expected));
        assert!(reply_should_stop_resending(
            "This sender is not paired with agentnoise.",
            &[]
        ));
    }

    #[test]
    fn fake_phone_detects_pairing_pin_requests() {
        assert!(reply_requests_pairing_pin(
            "Pairing required. Send the current desktop/SSH PIN as `/pair 123456`, then send `/help`."
        ));
        assert!(reply_requests_pairing_pin(
            "Pairing PIN invalid or expired. Check the desktop log for the current PIN."
        ));
        assert!(!reply_requests_pairing_pin(
            "agentnoise: running\nlauncher: direct\nchat: main\nworkspace: sandbox:/"
        ));
    }

    #[test]
    fn fake_phone_detects_final_job_reply() {
        assert!(!is_job_final_reply("Job an-123 codex: started"));
        assert!(!is_job_final_reply("Job an-123 codex: succeeded"));
        assert!(is_job_final_reply("an-12345 done\nok\n\n/tail an-12345"));
        assert!(is_job_final_reply(
            "an-12345 failed\nboom\n\n/tail an-12345"
        ));
        assert!(is_job_final_reply(
            "Done · an-12345\nok\n\nDetails: /tail an-12345"
        ));
    }

    #[test]
    fn fake_phone_extracts_job_session_group_link() {
        assert_eq!(
            extract_chat_uri_group_id(
                "Started session: m5\nOpen: whitenoise://chat/abcdef0123456789\nnext"
            )
            .as_deref(),
            Some("abcdef0123456789")
        );
        assert!(extract_chat_uri_group_id("Open: https://example.com").is_none());
    }

    #[test]
    fn fake_phone_outcome_requires_expected_text_and_final_job() {
        let options = FakePhoneRoundtrip {
            root: PathBuf::from("/tmp/fake"),
            pin: None,
            message: "/codex hi".to_string(),
            group_name: "test".to_string(),
            timeout: Duration::from_secs(1),
            expect: vec!["ok".to_string()],
            min_replies: 2,
            require_job_final: true,
            shared_daemon: false,
        };
        let mut outcome = ReplyOutcome::new(&options);
        outcome.record("Queued.\ncodex · sandbox:/\nI'll post the answer here.".to_string());
        assert!(!outcome.satisfied());
        outcome.record("Done · an-12345\nok\n\nDetails: /tail an-12345".to_string());
        assert!(outcome.satisfied());
    }
}
