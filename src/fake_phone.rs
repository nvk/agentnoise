use std::fs;
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

use crate::config::{Config, WhitenoiseConfig};
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
}

#[derive(Debug, Clone)]
pub struct FakePhoneRoundtrip {
    pub root: PathBuf,
    pub pin: Option<String>,
    pub message: String,
    pub group_name: String,
    pub timeout: Duration,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FakePhoneResult {
    pub phone_npub: String,
    pub group_id: String,
    pub replies: Vec<String>,
}

pub fn plan(config: &Config, root: Option<&Path>) -> FakePhonePlan {
    let root = root
        .map(Path::to_path_buf)
        .unwrap_or_else(|| config.resolved_data_dir().join("fake-phone"));
    let data_dir = root.join("wnd-data");
    let logs_dir = root.join("wnd-logs");
    let socket = data_dir.join("release").join("wnd.sock");
    let nsec_file = root.join("fake-phone.nsec");
    FakePhonePlan {
        root,
        data_dir,
        logs_dir,
        socket,
        nsec_file,
    }
}

pub fn roundtrip(config: &Config, options: FakePhoneRoundtrip) -> Result<FakePhoneResult> {
    let plan = plan(config, Some(&options.root));
    fs::create_dir_all(&plan.root).with_context(|| format!("creating {}", plan.root.display()))?;
    fs::create_dir_all(&plan.data_dir)
        .with_context(|| format!("creating {}", plan.data_dir.display()))?;
    fs::create_dir_all(&plan.logs_dir)
        .with_context(|| format!("creating {}", plan.logs_dir.display()))?;

    let _daemon = ChildGuard::new(start_fake_wnd(config, &plan)?);
    wait_for_socket(&plan.socket, Duration::from_secs(10))?;

    let fake_config = fake_whitenoise_config(config, &plan);
    let phone_npub = create_or_reuse_identity(&fake_config, &plan)?;
    let agent_npub = config
        .whitenoise
        .bot_npub
        .as_deref()
        .or(config.whitenoise.account.as_deref())
        .map(str::trim)
        .filter(|npub| !npub.is_empty())
        .context("config has no agentnoise npub; run `agentnoise setup` first")?;
    let group_id = create_group(&fake_config, &options.group_name, agent_npub)?;
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

    let replies = send_until_reply(&client, &group_id, &options.message, options.timeout)?;
    Ok(FakePhoneResult {
        phone_npub,
        group_id,
        replies,
    })
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
    if plan.socket.exists() {
        fs::remove_file(&plan.socket).with_context(|| {
            format!("removing stale fake phone socket {}", plan.socket.display())
        })?;
    }
    Command::new(&wnd)
        .arg("--data-dir")
        .arg(&plan.data_dir)
        .arg("--logs-dir")
        .arg(&plan.logs_dir)
        .arg("--discovery-relays")
        .arg(relays)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("starting fake phone {}", wnd.display()))
}

fn wait_for_socket(socket: &Path, timeout: Duration) -> Result<()> {
    let started = Instant::now();
    while started.elapsed() < timeout {
        if socket.exists() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(250));
    }
    bail!("fake phone socket did not appear: {}", socket.display())
}

fn fake_whitenoise_config(config: &Config, plan: &FakePhonePlan) -> WhitenoiseConfig {
    let mut fake = config.whitenoise.clone();
    fake.group_id.clear();
    fake.group_ids.clear();
    fake.account = None;
    fake.socket = Some(plan.socket.display().to_string());
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

fn create_group(config: &WhitenoiseConfig, name: &str, agent_npub: &str) -> Result<String> {
    let created = whitenoise_cli::create_group(config, name, &[agent_npub.to_string()])?;
    created
        .group_id
        .or_else(|| whitenoise_cli::group_id_from_output(&created.output))
        .context("White Noise did not return a group id")
}

fn send_until_reply(
    client: &WnClient,
    group_id: &str,
    message: &str,
    timeout: Duration,
) -> Result<Vec<String>> {
    let mut child = client.subscribe_group_with_limit(group_id, 0)?;
    let stdout = child
        .stdout
        .take()
        .context("fake phone subscribe did not expose stdout")?;
    let group_id = group_id.to_string();
    let reader_group_id = group_id.clone();
    let sent_message = message.to_string();
    let (tx, rx) = mpsc::channel();
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
                    return;
                }
            }
        }
    });

    let started = Instant::now();
    let mut last_send = Instant::now() - Duration::from_secs(10);
    let mut replies = Vec::new();
    while started.elapsed() < timeout {
        if last_send.elapsed() >= Duration::from_secs(5) {
            client.send_to(&group_id, message)?;
            last_send = Instant::now();
        }
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(Ok(reply)) => {
                replies.push(reply);
                break;
            }
            Ok(Err(error)) => {
                child.kill().ok();
                return Err(error);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    child.kill().ok();
    Ok(replies)
}

fn is_command_reply(text: &str, sent_message: &str) -> bool {
    let text = text.trim();
    !text.is_empty()
        && text != sent_message.trim()
        && text != "Paired. Send /help for commands."
        && !text.starts_with("I saw this while catching up after startup,")
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
    fn fake_phone_reply_filter_ignores_harness_noise() {
        assert!(!is_command_reply("", "/status"));
        assert!(!is_command_reply("/status", "/status"));
        assert!(!is_command_reply(
            " Paired. Send /help for commands. ",
            "/status"
        ));
        assert!(!is_command_reply(
            "I saw this while catching up after startup, so I did not run it:\n/status\nSend it again now, or send /help.",
            "/status"
        ));
        assert!(is_command_reply(
            "agentnoise\nStatus: OK\nSession: default",
            "/status"
        ));
    }
}
