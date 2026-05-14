use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::Value;
use zeroize::Zeroize;

use crate::config::WhitenoiseConfig;
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

impl Default for WhitenoiseInstall {
    fn default() -> Self {
        Self {
            root: managed_whitenoise_root(),
            force: false,
        }
    }
}

pub fn resolve_wn(configured: &str) -> PathBuf {
    resolve_binary(configured, "wn")
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

pub fn login_from_keychain(
    config: &WhitenoiseConfig,
    relay_override: Option<&str>,
) -> Result<String> {
    let wn = resolve_wn(&config.wn_bin);
    let store = SecretStore::new(&config.keychain_service, &config.keychain_item);
    let mut nsec = store.load_nsec()?;
    let output = run_login(&wn, config, relay_override, &nsec);
    nsec.zeroize();

    let output = output?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let text = if stdout.is_empty() { stderr } else { stdout };

    if !output.status.success() {
        bail!(
            "{} login exited with {}: {}",
            wn.display(),
            output.status,
            text
        );
    }

    Ok(text)
}

pub fn ensure_login_from_keychain(config: &WhitenoiseConfig) -> Result<bool> {
    if !config.use_keychain_nsec {
        return Ok(false);
    }
    if account_logged_in(config)? {
        return Ok(false);
    }

    login_from_keychain(config, None)?;
    Ok(true)
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

fn group_id_from_output(text: &str) -> Option<String> {
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
}
