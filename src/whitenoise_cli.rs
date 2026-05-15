use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::Value;
use zeroize::Zeroize;

use crate::config::WhitenoiseConfig;
use crate::identity;
use crate::paths::{
    executable_next_to_agentnoise, expand_tilde, find_on_path, local_checkout_whitenoise_bin,
    managed_whitenoise_root, managed_wn_path, managed_wnd_path,
};
use crate::secrets::SecretStore;

pub const REPO_URL: &str = "https://github.com/marmot-protocol/whitenoise-rs.git";
pub const PACKAGE: &str = "whitenoise-cli";

#[derive(Debug, Clone)]
pub struct WhitenoiseInstall {
    pub root: PathBuf,
    pub force: bool,
}

#[derive(Debug, Clone)]
pub struct CreatedGroup {
    pub group_id: Option<String>,
    pub output: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisibleGroup {
    pub group_id: String,
    pub peer_pubkey: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayStatus {
    pub url: String,
    pub types: Vec<String>,
    pub status: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayEnsureSummary {
    pub configured_relays: usize,
    pub added_entries: usize,
    pub already_present_entries: usize,
}

const MESSAGE_RELAY_TYPES: &[&str] = &["nip65", "inbox", "key_package"];

impl Default for WhitenoiseInstall {
    fn default() -> Self {
        Self {
            root: managed_whitenoise_root(),
            force: false,
        }
    }
}

pub fn resolve_wn(configured: &str) -> PathBuf {
    let resolved = resolve_binary(configured, "wn");
    if let Some(packaged) = executable_next_to_agentnoise("wn")
        && should_prefer_packaged_wn(&resolved, &packaged)
    {
        return packaged;
    }
    resolved
}

pub fn resolve_wnd() -> PathBuf {
    resolve_binary("wnd", "wnd")
}

pub fn resolve_wnd_for_config(config: &WhitenoiseConfig) -> PathBuf {
    let wn = resolve_wn(&config.wn_bin);
    if let Some(parent) = wn.parent() {
        let wnd = parent.join("wnd");
        if wnd.is_file() {
            return wnd;
        }
    }
    resolve_wnd()
}

pub fn resolve_binary(configured: &str, name: &str) -> PathBuf {
    let configured_path = expand_tilde(configured);
    if configured != name || configured_path.components().count() > 1 {
        return configured_path;
    }

    executable_next_to_agentnoise(name)
        .or_else(|| {
            let managed = if name == "wn" {
                managed_wn_path()
            } else {
                managed_wnd_path()
            };
            managed.is_file().then_some(managed)
        })
        .or_else(|| local_checkout_whitenoise_bin(name))
        .or_else(|| find_on_path(name))
        .unwrap_or_else(|| PathBuf::from(name))
}

pub fn should_reset_wn_bin_to_default(configured: &str) -> bool {
    executable_next_to_agentnoise("wn")
        .is_some_and(|packaged| should_prefer_packaged_wn(&expand_tilde(configured), &packaged))
}

fn should_prefer_packaged_wn(configured: &Path, packaged: &Path) -> bool {
    !same_path(configured, packaged) && is_agentnoise_managed_wn_path(configured)
}

fn same_path(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn is_agentnoise_managed_wn_path(path: &Path) -> bool {
    path == managed_wn_path()
        || path_components_end_with(path, &[".local-whitenoise", "bin", "wn"])
        || path_components_contain(path, &["Cellar", "agentnoise", "*", "bin", "wn"])
}

fn path_components_end_with(path: &Path, suffix: &[&str]) -> bool {
    let components = path
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect::<Vec<_>>();
    components.ends_with(suffix)
}

fn path_components_contain(path: &Path, pattern: &[&str]) -> bool {
    let components = path
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect::<Vec<_>>();
    components.windows(pattern.len()).any(|window| {
        window
            .iter()
            .zip(pattern)
            .all(|(component, expected)| *expected == "*" || component == expected)
    })
}

pub fn install(options: &WhitenoiseInstall) -> Result<()> {
    let mut command = Command::new("cargo");
    command
        .arg("install")
        .arg("--git")
        .arg(REPO_URL)
        .arg(PACKAGE)
        .arg("--bin")
        .arg("wn")
        .arg("--bin")
        .arg("wnd")
        .arg("--root")
        .arg(&options.root);
    if options.force {
        command.arg("--force");
    }

    let status = command
        .status()
        .context("running cargo install for whitenoise-cli")?;
    if !status.success() {
        bail!("cargo install whitenoise-cli exited with {status}");
    }
    Ok(())
}

pub fn version(path: &Path) -> Result<String> {
    let output = Command::new(path)
        .arg("--version")
        .output()
        .with_context(|| format!("running {} --version", path.display()))?;
    if !output.status.success() {
        bail!("{} --version exited with {}", path.display(), output.status);
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn daemon_status(wn_path: &Path) -> Result<String> {
    daemon_status_with_socket(wn_path, None)
}

pub fn daemon_running(config: &WhitenoiseConfig) -> Result<bool> {
    let wn = resolve_wn(&config.wn_bin);
    let mut command = Command::new(&wn);
    add_socket_arg(&mut command, config.resolved_socket().as_deref());
    let output = command
        .arg("daemon")
        .arg("status")
        .output()
        .with_context(|| format!("running {} daemon status", wn.display()))?;

    if !output.status.success() {
        return Ok(false);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let text = format!("{stdout}\n{stderr}").to_ascii_lowercase();
    Ok(!text.contains("not running"))
}

pub fn start_daemon(config: &WhitenoiseConfig) -> Result<Child> {
    if config.resolved_socket().is_none() {
        let wnd = resolve_wnd_for_config(config);
        return Command::new(&wnd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("starting {} directly", wnd.display()));
    }

    let wn = resolve_wn(&config.wn_bin);
    let mut command = Command::new(&wn);
    add_socket_arg(&mut command, config.resolved_socket().as_deref());
    command
        .arg("daemon")
        .arg("start")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("starting {} daemon start", wn.display()))
}

pub fn ensure_daemon(config: &WhitenoiseConfig) -> Result<Option<Child>> {
    if daemon_running(config)? {
        return Ok(None);
    }

    let mut child = start_daemon(config)?;
    for _ in 0..20 {
        if daemon_running(config)? {
            return Ok(Some(child));
        }
        if let Some(status) = child.try_wait().context("checking White Noise daemon")? {
            if !status.success() {
                bail!("wn daemon start exited before it was ready: {status}");
            }
            for _ in 0..20 {
                if daemon_running(config)? {
                    return Ok(None);
                }
                thread::sleep(Duration::from_millis(250));
            }
            bail!("wn daemon start exited successfully, but daemon did not become ready");
        }
        thread::sleep(Duration::from_millis(250));
    }

    bail!("wn daemon did not become ready within 5 seconds");
}

pub fn daemon_status_with_socket(wn_path: &Path, socket: Option<&Path>) -> Result<String> {
    let mut command = Command::new(wn_path);
    add_socket_arg(&mut command, socket);
    let output = command
        .arg("daemon")
        .arg("status")
        .output()
        .with_context(|| format!("running {} daemon status", wn_path.display()))?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let text = if stdout.is_empty() { stderr } else { stdout };
    if !output.status.success() {
        bail!(
            "{} daemon status exited with {}: {}",
            wn_path.display(),
            output.status,
            text
        );
    }
    Ok(text)
}

pub fn create_group(
    config: &WhitenoiseConfig,
    name: &str,
    members: &[String],
) -> Result<CreatedGroup> {
    let name = name.trim();
    if name.is_empty() {
        bail!("group name cannot be empty");
    }
    if members.is_empty() {
        bail!("at least one group member is required");
    }

    let wn = resolve_wn(&config.wn_bin);
    let mut command = Command::new(&wn);
    add_socket_arg(&mut command, config.resolved_socket().as_deref());
    command.arg("groups").arg("create").arg("--json");
    add_account_arg(&mut command, config.account.as_deref());
    command.arg(name);
    for member in members {
        command.arg(member);
    }

    let output = command
        .output()
        .with_context(|| format!("running {} groups create", wn.display()))?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let text = if stdout.is_empty() { stderr } else { stdout };

    if !output.status.success() {
        bail!(
            "{} groups create exited with {}: {}",
            wn.display(),
            output.status,
            text
        );
    }

    Ok(CreatedGroup {
        group_id: group_id_from_output(&text),
        output: text,
    })
}

pub fn list_groups(config: &WhitenoiseConfig) -> Result<Vec<VisibleGroup>> {
    let wn = resolve_wn(&config.wn_bin);
    let mut command = Command::new(&wn);
    add_socket_arg(&mut command, config.resolved_socket().as_deref());
    command.arg("groups").arg("list").arg("--json");
    add_account_arg(&mut command, config.account.as_deref());

    let text = checked_output(command, &wn, "groups list")?;
    parse_groups_output(&text)
}

pub fn list_relays(config: &WhitenoiseConfig) -> Result<Vec<RelayStatus>> {
    let wn = resolve_wn(&config.wn_bin);
    let mut command = Command::new(&wn);
    add_socket_arg(&mut command, config.resolved_socket().as_deref());
    command.arg("relays").arg("list").arg("--json");
    add_account_arg(&mut command, config.account.as_deref());

    let text = checked_output(command, &wn, "relays list")?;
    parse_relays_output(&text)
}

pub fn ensure_message_relays(config: &WhitenoiseConfig) -> Result<RelayEnsureSummary> {
    let relays = dedupe_urls(config.message_relays.clone());
    if relays.is_empty() {
        return Ok(RelayEnsureSummary {
            configured_relays: 0,
            added_entries: 0,
            already_present_entries: 0,
        });
    }

    let mut current = list_relays(config)?;
    let mut added_entries = 0;
    let mut already_present_entries = 0;
    for relay in &relays {
        for relay_type in MESSAGE_RELAY_TYPES {
            if has_relay_type(&current, relay, relay_type) {
                already_present_entries += 1;
                continue;
            }
            add_relay(config, relay, relay_type)?;
            added_entries += 1;
            add_relay_status(&mut current, relay, relay_type);
        }
    }

    Ok(RelayEnsureSummary {
        configured_relays: relays.len(),
        added_entries,
        already_present_entries,
    })
}

fn add_relay(config: &WhitenoiseConfig, relay: &str, relay_type: &str) -> Result<String> {
    let wn = resolve_wn(&config.wn_bin);
    let mut command = Command::new(&wn);
    add_socket_arg(&mut command, config.resolved_socket().as_deref());
    command
        .arg("relays")
        .arg("add")
        .arg("--json")
        .arg("--type")
        .arg(relay_type)
        .arg(relay);
    add_account_arg(&mut command, config.account.as_deref());

    checked_output(command, &wn, "relays add")
}

pub fn update_profile(
    config: &WhitenoiseConfig,
    name: &str,
    display_name: &str,
    about: &str,
) -> Result<String> {
    let wn = resolve_wn(&config.wn_bin);
    let mut command = Command::new(&wn);
    add_socket_arg(&mut command, config.resolved_socket().as_deref());
    command
        .arg("profile")
        .arg("update")
        .arg("--json")
        .arg("--name")
        .arg(name)
        .arg("--display-name")
        .arg(display_name)
        .arg("--about")
        .arg(about);
    add_account_arg(&mut command, config.account.as_deref());

    checked_output(command, &wn, "profile update")
}

pub fn publish_key_package(config: &WhitenoiseConfig) -> Result<String> {
    let wn = resolve_wn(&config.wn_bin);
    let mut command = Command::new(&wn);
    add_socket_arg(&mut command, config.resolved_socket().as_deref());
    command.arg("keys").arg("publish").arg("--json");
    add_account_arg(&mut command, config.account.as_deref());

    checked_output(command, &wn, "keys publish")
}

pub fn login_from_configured_nsec(
    config: &WhitenoiseConfig,
    relay_override: Option<&str>,
) -> Result<String> {
    let wn = resolve_wn(&config.wn_bin);
    let mut nsec = if config.dev_burner_nsec {
        identity::load_default_nsec(config)?
    } else {
        let store = SecretStore::new(&config.keychain_service, &config.keychain_item);
        store.load_nsec().with_context(|| {
            format!(
                "loading configured nsec from {}; non-interactive services cannot reliably show OS keychain prompts, so run `agentnoise keychain status` or `agentnoise up` once from Terminal to authorize access",
                store.label()
            )
        })?
    };
    let output = run_login(&wn, config, relay_override, &nsec);
    nsec.zeroize();

    finish_login_output(&wn, output?, config.dev_burner_nsec)
}

pub fn login_from_keychain(
    config: &WhitenoiseConfig,
    relay_override: Option<&str>,
) -> Result<String> {
    login_from_configured_nsec(config, relay_override)
}

pub fn login_with_nsec(
    config: &WhitenoiseConfig,
    nsec: &str,
    relay_override: Option<&str>,
) -> Result<String> {
    crate::secrets::validate_nsec(nsec)?;
    let wn = resolve_wn(&config.wn_bin);
    let output = run_login(&wn, config, relay_override, nsec)?;
    finish_login_output(&wn, output, true)
}

fn finish_login_output(wn: &Path, output: Output, direct_nsec: bool) -> Result<String> {
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let text = if stdout.is_empty() { stderr } else { stdout };

    if !output.status.success() {
        let direct_note = if direct_nsec {
            " (agentnoise passed an nsec directly instead of using its OS keychain; current White Noise daemons may still use their own platform secret store for account login)"
        } else {
            ""
        };
        bail!(
            "{} login exited with {}{}: {}",
            wn.display(),
            output.status,
            direct_note,
            text
        );
    }

    Ok(text)
}

pub fn ensure_login_from_configured_nsec(config: &WhitenoiseConfig) -> Result<bool> {
    if !config.use_keychain_nsec && !config.dev_burner_nsec {
        return Ok(false);
    }
    if account_logged_in(config)? {
        return Ok(false);
    }

    login_from_configured_nsec(config, None)?;
    Ok(true)
}

pub fn ensure_login_from_keychain(config: &WhitenoiseConfig) -> Result<bool> {
    ensure_login_from_configured_nsec(config)
}

pub fn account_logged_in(config: &WhitenoiseConfig) -> Result<bool> {
    let wn = resolve_wn(&config.wn_bin);
    let mut command = Command::new(&wn);
    add_socket_arg(&mut command, config.resolved_socket().as_deref());
    command.arg("whoami").arg("--json");
    add_account_arg(&mut command, config.account.as_deref());
    let output = command
        .output()
        .with_context(|| format!("running {} whoami", wn.display()))?;
    if !output.status.success() {
        return Ok(false);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stdout = stdout.trim();
    if stdout.is_empty() {
        return Ok(false);
    }

    if let Some(account) = config
        .account
        .as_deref()
        .filter(|account| !account.trim().is_empty())
    {
        return Ok(stdout.contains(account));
    }

    let value: Value = serde_json::from_str(stdout).context("parsing wn whoami JSON")?;
    Ok(json_has_account(&value))
}

pub fn render_status(config: &WhitenoiseConfig) -> String {
    let wn = resolve_wn(&config.wn_bin);
    let wnd = resolve_wnd_for_config(config);
    let mut output = String::new();
    output.push_str("agentnoise whitenoise\n\n");
    output.push_str(&format!("wn: {}\n", wn.display()));
    if let Some(socket) = config.resolved_socket() {
        output.push_str(&format!("socket: {}\n", socket.display()));
    }
    match version(&wn) {
        Ok(version) => output.push_str(&format!("wn version: {version}\n")),
        Err(error) => output.push_str(&format!("wn version: unavailable ({error:#})\n")),
    }
    output.push_str(&format!("wnd: {}\n", wnd.display()));
    output.push_str(&format!(
        "managed root: {}\n",
        managed_whitenoise_root().display()
    ));
    match daemon_status_with_socket(&wn, config.resolved_socket().as_deref()) {
        Ok(status) => output.push_str(&format!("daemon: {status}\n")),
        Err(error) => output.push_str(&format!("daemon: unavailable ({error:#})\n")),
    }
    output
}

fn run_login(
    wn: &Path,
    config: &WhitenoiseConfig,
    relay_override: Option<&str>,
    nsec: &str,
) -> Result<Output> {
    let mut command = Command::new(wn);
    add_socket_arg(&mut command, config.resolved_socket().as_deref());
    command
        .arg("login")
        .arg("--json")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    add_account_arg(&mut command, config.account.as_deref());

    let relay = relay_override
        .or(config.login_relay.as_deref())
        .map(str::trim)
        .filter(|relay| !relay.is_empty());
    if let Some(relay) = relay {
        command.arg("--relay").arg(relay);
    }

    let mut child = command
        .spawn()
        .with_context(|| format!("starting {} login", wn.display()))?;
    {
        let mut stdin = child
            .stdin
            .take()
            .context("wn login did not expose stdin")?;
        use std::io::Write;
        stdin
            .write_all(nsec.as_bytes())
            .context("writing nsec to wn login")?;
        stdin.write_all(b"\n").context("finishing nsec input")?;
    }

    child
        .wait_with_output()
        .with_context(|| format!("waiting for {} login", wn.display()))
}

fn add_account_arg(command: &mut Command, account: Option<&str>) {
    if let Some(account) = account.map(str::trim).filter(|account| !account.is_empty()) {
        command.arg("--account").arg(account);
    }
}

fn add_socket_arg(command: &mut Command, socket: Option<&Path>) {
    if let Some(socket) = socket {
        command.arg("--socket").arg(socket);
    }
}

fn checked_output(mut command: Command, wn: &Path, label: &str) -> Result<String> {
    let output = command
        .output()
        .with_context(|| format!("running {} {label}", wn.display()))?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let text = if stdout.is_empty() { stderr } else { stdout };

    if !output.status.success() {
        bail!(
            "{} {label} exited with {}: {}",
            wn.display(),
            output.status,
            text
        );
    }

    Ok(text)
}

fn json_has_account(value: &Value) -> bool {
    let value = value.get("result").unwrap_or(value);
    match value {
        Value::Array(values) => !values.is_empty(),
        Value::Object(object) => !object.is_empty(),
        Value::String(value) => !value.trim().is_empty(),
        _ => false,
    }
}

pub fn group_id_from_output(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if looks_like_group_id(text) {
        return Some(text.to_string());
    }
    let value: Value = serde_json::from_str(text).ok()?;
    find_group_id(&value)
}

fn parse_groups_output(text: &str) -> Result<Vec<VisibleGroup>> {
    let value: Value = serde_json::from_str(text.trim()).context("parsing wn groups list JSON")?;
    let mut groups = Vec::new();
    collect_visible_groups(&value, &mut groups);

    let mut seen = HashSet::new();
    groups.retain(|group| seen.insert(group.group_id.clone()));
    Ok(groups)
}

fn parse_relays_output(text: &str) -> Result<Vec<RelayStatus>> {
    let value: Value = serde_json::from_str(text.trim()).context("parsing wn relays list JSON")?;
    let mut relays = Vec::new();
    collect_relays(&value, &mut relays);

    let mut merged: Vec<RelayStatus> = Vec::new();
    for relay in relays {
        if let Some(existing) = merged.iter_mut().find(|existing| existing.url == relay.url) {
            for relay_type in relay.types {
                if !existing
                    .types
                    .iter()
                    .any(|existing| existing == &relay_type)
                {
                    existing.types.push(relay_type);
                }
            }
            if existing.status.is_none() {
                existing.status = relay.status;
            }
        } else {
            merged.push(relay);
        }
    }
    for relay in &mut merged {
        relay.types.sort();
    }
    merged.sort_by(|left, right| left.url.cmp(&right.url));
    Ok(merged)
}

fn collect_relays(value: &Value, relays: &mut Vec<RelayStatus>) {
    match value {
        Value::Object(object) => {
            if let Some(relay) = relay_status(value) {
                relays.push(relay);
                return;
            }
            for value in object.values() {
                collect_relays(value, relays);
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_relays(value, relays);
            }
        }
        _ => {}
    }
}

fn relay_status(value: &Value) -> Option<RelayStatus> {
    let Value::Object(object) = value else {
        return None;
    };
    let url = object.get("url").and_then(Value::as_str)?.trim();
    if url.is_empty() {
        return None;
    }
    let types = object
        .get("types")
        .and_then(Value::as_array)
        .map(|types| {
            types
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|relay_type| !relay_type.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let status = object
        .get("status")
        .and_then(Value::as_str)
        .map(str::to_string);
    Some(RelayStatus {
        url: url.to_string(),
        types,
        status,
    })
}

fn has_relay_type(relays: &[RelayStatus], relay: &str, relay_type: &str) -> bool {
    relays.iter().any(|status| {
        status.url == relay
            && status
                .types
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(relay_type))
    })
}

fn add_relay_status(relays: &mut Vec<RelayStatus>, relay: &str, relay_type: &str) {
    if let Some(status) = relays.iter_mut().find(|status| status.url == relay) {
        if !status
            .types
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(relay_type))
        {
            status.types.push(relay_type.to_string());
        }
        return;
    }

    relays.push(RelayStatus {
        url: relay.to_string(),
        types: vec![relay_type.to_string()],
        status: None,
    });
}

fn dedupe_urls(urls: Vec<String>) -> Vec<String> {
    let mut output = Vec::new();
    for url in urls {
        let url = url.trim();
        if url.is_empty() {
            continue;
        }
        if !output.iter().any(|existing| existing == url) {
            output.push(url.to_string());
        }
    }
    output
}

fn collect_visible_groups(value: &Value, groups: &mut Vec<VisibleGroup>) {
    match value {
        Value::Object(object) => {
            if let Some(group_id) = direct_group_id(value) {
                groups.push(VisibleGroup {
                    group_id,
                    peer_pubkey: find_string(
                        value,
                        &[
                            "dm_peer_pubkey",
                            "peer_pubkey",
                            "welcomer_pubkey",
                            "sender_npub",
                            "sender",
                            "author",
                            "pubkey",
                            "from",
                        ],
                    ),
                });
                return;
            }
            for value in object.values() {
                collect_visible_groups(value, groups);
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_visible_groups(value, groups);
            }
        }
        _ => {}
    }
}

fn find_group_id(value: &Value) -> Option<String> {
    match value {
        Value::Object(object) => {
            if let Some(group_id) = direct_group_id(value) {
                return Some(group_id);
            }
            object.values().find_map(find_group_id)
        }
        Value::Array(values) => values.iter().find_map(find_group_id),
        Value::String(value) if looks_like_group_id(value) => Some(value.clone()),
        _ => None,
    }
}

fn direct_group_id(value: &Value) -> Option<String> {
    let Value::Object(object) = value else {
        return None;
    };
    for key in ["group_id", "groupId", "mls_group_id", "mlsGroupId", "id"] {
        if let Some(value) = object.get(key)
            && let Some(group_id) = group_id_value(value)
        {
            return Some(group_id);
        }
    }
    None
}

fn group_id_value(value: &Value) -> Option<String> {
    match value {
        Value::String(value) if looks_like_group_id(value) => Some(value.clone()),
        Value::Array(values) => bytes_array_to_hex(values),
        Value::Object(object) => {
            if let Some(Value::Array(values)) = value.pointer("/value/vec") {
                return bytes_array_to_hex(values);
            }
            if let Some(Value::Array(values)) = object.get("vec") {
                return bytes_array_to_hex(values);
            }
            object.values().find_map(group_id_value)
        }
        _ => None,
    }
}

fn bytes_array_to_hex(values: &[Value]) -> Option<String> {
    if values.is_empty() {
        return None;
    }
    let mut output = String::with_capacity(values.len() * 2);
    for value in values {
        let byte = value.as_u64()?;
        if byte > u8::MAX as u64 {
            return None;
        }
        output.push_str(&format!("{:02x}", byte));
    }
    looks_like_group_id(&output).then_some(output)
}

fn find_string(value: &Value, keys: &[&str]) -> Option<String> {
    match value {
        Value::Object(object) => {
            for key in keys {
                if let Some(Value::String(value)) = object.get(*key)
                    && !value.trim().is_empty()
                {
                    return Some(value.clone());
                }
            }
            object.values().find_map(|value| find_string(value, keys))
        }
        Value::Array(values) => values.iter().find_map(|value| find_string(value, keys)),
        _ => None,
    }
}

fn looks_like_group_id(value: &str) -> bool {
    let value = value.trim();
    (32..=512).contains(&value.len()) && value.chars().all(|ch| ch.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_path_is_preserved() {
        assert_eq!(
            resolve_binary("/tmp/custom/wn", "wn"),
            PathBuf::from("/tmp/custom/wn")
        );
    }

    #[test]
    fn detects_agentnoise_managed_wn_paths() {
        assert!(is_agentnoise_managed_wn_path(
            &managed_whitenoise_root().join("bin/wn")
        ));
        assert!(is_agentnoise_managed_wn_path(&PathBuf::from(
            "/tmp/agentnoise/.local-whitenoise/bin/wn"
        )));
        assert!(is_agentnoise_managed_wn_path(&PathBuf::from(
            "/opt/homebrew/Cellar/agentnoise/0.1.2/bin/wn"
        )));
        assert!(!is_agentnoise_managed_wn_path(&PathBuf::from(
            "/tmp/custom/bin/wn"
        )));
    }

    #[test]
    fn detects_accounts_in_whoami_json() {
        let value: Value = serde_json::from_str(r#"{"result":[{"pubkey":"abc"}]}"#).unwrap();
        assert!(json_has_account(&value));

        let value: Value = serde_json::from_str(r#"{"result":[]}"#).unwrap();
        assert!(!json_has_account(&value));
    }

    #[test]
    fn extracts_group_id_from_nested_json() {
        let output = r#"{"result":{"group":{"mls_group_id":"0123456789abcdef0123456789abcdef"}}}"#;
        assert_eq!(
            group_id_from_output(output).as_deref(),
            Some("0123456789abcdef0123456789abcdef")
        );
    }

    #[test]
    fn extracts_group_id_from_whitenoise_vec_shape() {
        let output = r#"{
          "result": [
            {
              "dm_peer_pubkey": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
              "mls_group_id": {
                "value": {
                  "vec": [1,35,69,103,137,171,205,239,1,35,69,103,137,171,205,239]
                }
              }
            }
          ]
        }"#;
        let groups = parse_groups_output(output).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].group_id, "0123456789abcdef0123456789abcdef");
        assert_eq!(
            groups[0].peer_pubkey.as_deref(),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
    }

    #[test]
    fn parses_relay_list_and_merges_types() {
        let output = r#"{
          "result": [
            {
              "status": "Connected",
              "types": ["nip65", "inbox"],
              "url": "wss://relay.example"
            },
            {
              "status": "Connected",
              "types": ["key_package"],
              "url": "wss://relay.example"
            }
          ]
        }"#;

        let relays = parse_relays_output(output).unwrap();
        assert_eq!(relays.len(), 1);
        assert_eq!(relays[0].url, "wss://relay.example");
        assert_eq!(
            relays[0].types,
            vec![
                "inbox".to_string(),
                "key_package".to_string(),
                "nip65".to_string()
            ]
        );
        assert!(has_relay_type(&relays, "wss://relay.example", "inbox"));
        assert!(has_relay_type(
            &relays,
            "wss://relay.example",
            "key_package"
        ));
    }
}
