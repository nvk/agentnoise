use std::fs::{self, File, OpenOptions};
use std::io::{self, IsTerminal, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::Command;
use std::thread;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::config::Config;

const LOCK_FILE: &str = "engine.lock";
const STATUS_FILE: &str = "engine.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcquireMode {
    Try,
    Wait,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimePairingInfo {
    pub npub: String,
    pub nprofile: String,
    pub relays: Vec<String>,
    pub pin_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeStatus {
    pub version: u8,
    pub pid: u32,
    pub started_at: String,
    pub config_path: String,
    pub data_dir: String,
    pub log_dir: String,
    pub npub: Option<String>,
    pub groups: Vec<String>,
    pub pairing: Option<RuntimePairingInfo>,
}

pub struct EngineGuard {
    lock: File,
    lock_path: PathBuf,
    status_path: PathBuf,
}

impl EngineGuard {
    pub fn update_status(
        &self,
        config_path: &Path,
        config: &Config,
        pairing: Option<RuntimePairingInfo>,
    ) -> Result<()> {
        write_status(&self.status_path, config_path, config, pairing)
    }
}

impl Drop for EngineGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.status_path);
        let _ = self.lock.sync_all();
        let _ = fs::remove_file(&self.lock_path);
    }
}

pub fn stdio_is_interactive() -> bool {
    io::stdout().is_terminal() || io::stderr().is_terminal()
}

pub fn acquire_engine(
    config_path: &Path,
    config: &Config,
    mode: AcquireMode,
) -> Result<Option<EngineGuard>> {
    fs::create_dir_all(config.resolved_data_dir())
        .with_context(|| format!("creating data dir {}", config.resolved_data_dir().display()))?;
    let lock_path = lock_path(config);
    let lock = match mode {
        AcquireMode::Try => match try_create_engine_lock(&lock_path)? {
            Some(lock) => lock,
            None => return Ok(None),
        },
        AcquireMode::Wait => loop {
            if let Some(lock) = try_create_engine_lock(&lock_path)? {
                break lock;
            }
            thread::sleep(Duration::from_secs(1));
        },
    };

    write_lock_owner(&lock)?;
    let guard = EngineGuard {
        lock,
        lock_path,
        status_path: status_path(config),
    };
    guard.update_status(config_path, config, None)?;
    Ok(Some(guard))
}

fn try_create_engine_lock(path: &Path) -> Result<Option<File>> {
    loop {
        match OpenOptions::new().write(true).create_new(true).open(path) {
            Ok(lock) => return Ok(Some(lock)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if lock_owner_is_active(path)? {
                    return Ok(None);
                }
                match fs::remove_file(path) {
                    Ok(()) => continue,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    Err(error) => {
                        return Err(error)
                            .with_context(|| format!("removing stale lock {}", path.display()));
                    }
                }
            }
            Err(error) => {
                return Err(error).with_context(|| format!("creating lock {}", path.display()));
            }
        }
    }
}

pub fn engine_is_running(config: &Config) -> Result<bool> {
    let data_dir = config.resolved_data_dir();
    if !data_dir.exists() {
        return Ok(false);
    }
    let lock_path = data_dir.join(LOCK_FILE);
    if !lock_path.exists() {
        return Ok(false);
    }
    if lock_owner_is_active(&lock_path)? {
        return Ok(true);
    }
    fs::remove_file(&lock_path).ok();
    Ok(false)
}

pub fn attach_ui(config_path: &Path, config: &Config) -> Result<()> {
    println!("agentnoise already running");
    match read_status(config)? {
        Some(status) => print_status(&status),
        None => {
            println!("status: runtime lock is held, but no status file was found");
            println!("config: {}", config_path.display());
        }
    }

    let logs = existing_log_paths(config);
    if logs.is_empty() {
        println!("logs: none found");
        return Ok(());
    }

    println!("logs:");
    for path in &logs {
        println!("- {}", path.display());
    }
    println!("press Ctrl-C to close this UI; the listener keeps running");
    follow_logs(logs)
}

pub fn read_status(config: &Config) -> Result<Option<RuntimeStatus>> {
    let path = status_path(config);
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path)
        .with_context(|| format!("reading runtime status {}", path.display()))?;
    let status = serde_json::from_str(&text)
        .with_context(|| format!("parsing runtime status {}", path.display()))?;
    Ok(Some(status))
}

fn write_lock_owner(mut lock: &File) -> Result<()> {
    lock.set_len(0).context("truncating runtime lock")?;
    lock.seek(SeekFrom::Start(0))
        .context("seeking runtime lock")?;
    writeln!(lock, "pid={}", std::process::id()).context("writing runtime lock")?;
    lock.sync_all().context("syncing runtime lock")
}

fn lock_owner_is_active(path: &Path) -> Result<bool> {
    let metadata =
        fs::metadata(path).with_context(|| format!("reading lock metadata {}", path.display()))?;
    let text = fs::read_to_string(path).unwrap_or_default();
    if let Some(pid) = parse_lock_pid(&text) {
        return Ok(pid == std::process::id() || process_is_alive(pid));
    }

    let recent = metadata
        .modified()
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age < Duration::from_secs(10));
    Ok(recent)
}

fn parse_lock_pid(text: &str) -> Option<u32> {
    text.lines()
        .find_map(|line| line.trim().strip_prefix("pid=")?.parse().ok())
}

#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(not(unix))]
fn process_is_alive(_pid: u32) -> bool {
    true
}

fn write_status(
    path: &Path,
    config_path: &Path,
    config: &Config,
    pairing: Option<RuntimePairingInfo>,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let status = RuntimeStatus {
        version: 1,
        pid: std::process::id(),
        started_at: OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .unwrap_or_else(|_| "unknown".to_string()),
        config_path: config_path.display().to_string(),
        data_dir: config.resolved_data_dir().display().to_string(),
        log_dir: config.resolved_log_dir().display().to_string(),
        npub: config
            .whitenoise
            .bot_npub
            .clone()
            .or_else(|| config.whitenoise.account.clone()),
        groups: config.whitenoise.control_group_ids(),
        pairing,
    };
    let text = serde_json::to_string_pretty(&status).context("serializing runtime status")?;
    fs::write(path, text).with_context(|| format!("writing runtime status {}", path.display()))
}

fn print_status(status: &RuntimeStatus) {
    println!("pid: {}", status.pid);
    println!("started: {}", status.started_at);
    if let Some(npub) = &status.npub {
        println!("npub: {npub}");
    }
    println!("groups: {}", status.groups.len());
    for group in status.groups.iter().take(5) {
        println!("- {group}");
    }
    if status.groups.len() > 5 {
        println!("- ...");
    }
    if let Some(pairing) = &status.pairing {
        println!("pairing: required");
        println!("pairing npub: {}", pairing.npub);
        println!("pairing nprofile: {}", pairing.nprofile);
        println!("pairing relays:");
        for relay in &pairing.relays {
            println!("- {relay}");
        }
        println!("pin window: {}s", pairing.pin_seconds);
        if let Ok(qr) = crate::identity::render_qr(&pairing.nprofile) {
            println!();
            println!("{qr}");
        }
    } else {
        println!("pairing: not required");
    }
}

fn existing_log_paths(config: &Config) -> Vec<PathBuf> {
    candidate_log_paths(config)
        .into_iter()
        .filter(|path| path.is_file())
        .collect()
}

fn candidate_log_paths(config: &Config) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let log_dir = config.resolved_log_dir();
    push_unique(&mut paths, log_dir.join("launchd.out.log"));
    push_unique(&mut paths, log_dir.join("launchd.err.log"));
    push_unique(&mut paths, log_dir.join("agentnoise.log"));
    push_unique(&mut paths, log_dir.join("agentnoise.err.log"));

    for prefix in homebrew_prefixes() {
        push_unique(&mut paths, prefix.join("var/log/agentnoise.log"));
        push_unique(&mut paths, prefix.join("var/log/agentnoise.err.log"));
    }

    paths
}

fn homebrew_prefixes() -> Vec<PathBuf> {
    let mut prefixes = Vec::new();
    if let Some(prefix) = std::env::var_os("HOMEBREW_PREFIX") {
        push_unique(&mut prefixes, PathBuf::from(prefix));
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(prefix) = homebrew_prefix_from_path(&exe)
    {
        push_unique(&mut prefixes, prefix);
    }
    push_unique(&mut prefixes, PathBuf::from("/opt/homebrew"));
    push_unique(&mut prefixes, PathBuf::from("/usr/local"));
    push_unique(&mut prefixes, PathBuf::from("/home/linuxbrew/.linuxbrew"));
    prefixes
}

fn homebrew_prefix_from_path(path: &Path) -> Option<PathBuf> {
    let mut prefix = PathBuf::new();
    for component in path.components() {
        let text = component.as_os_str().to_string_lossy();
        if text == "Cellar" {
            return Some(prefix);
        }
        prefix.push(component.as_os_str());
    }
    None
}

fn push_unique(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.iter().any(|existing| existing == &path) {
        paths.push(path);
    }
}

fn follow_logs(paths: Vec<PathBuf>) -> Result<()> {
    let multiple = paths.len() > 1;
    let mut files = paths
        .into_iter()
        .map(|path| {
            let offset = fs::metadata(&path)
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            FollowedLog { path, offset }
        })
        .collect::<Vec<_>>();

    loop {
        for file in &mut files {
            file.print_new_content(multiple)?;
        }
        io::stdout().flush().ok();
        thread::sleep(Duration::from_secs(1));
    }
}

struct FollowedLog {
    path: PathBuf,
    offset: u64,
}

impl FollowedLog {
    fn print_new_content(&mut self, multiple: bool) -> Result<()> {
        let Ok(metadata) = fs::metadata(&self.path) else {
            self.offset = 0;
            return Ok(());
        };
        if metadata.len() < self.offset {
            self.offset = 0;
        }
        if metadata.len() == self.offset {
            return Ok(());
        }

        let mut file = File::open(&self.path)
            .with_context(|| format!("opening log {}", self.path.display()))?;
        file.seek(SeekFrom::Start(self.offset))
            .with_context(|| format!("seeking log {}", self.path.display()))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .with_context(|| format!("reading log {}", self.path.display()))?;
        self.offset += bytes.len() as u64;

        let text = String::from_utf8_lossy(&bytes);
        if multiple {
            let label = self
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("log");
            for line in text.lines() {
                println!("[{label}] {line}");
            }
            if !text.ends_with('\n') {
                println!();
            }
        } else {
            print!("{text}");
        }
        Ok(())
    }
}

fn lock_path(config: &Config) -> PathBuf {
    config.resolved_data_dir().join(LOCK_FILE)
}

fn status_path(config: &Config) -> PathBuf {
    config.resolved_data_dir().join(STATUS_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config(root: &Path) -> Config {
        let mut config = Config::template();
        config.runner.data_dir = root.join("data").display().to_string();
        config.runner.log_dir = root.join("logs").display().to_string();
        config.whitenoise.account = Some("npub-test".to_string());
        config.whitenoise.group_id = "group-a".to_string();
        config
    }

    #[test]
    fn engine_lock_is_exclusive() {
        let temp = tempfile::tempdir().unwrap();
        let config = test_config(temp.path());
        let config_path = temp.path().join("config.toml");

        let first = acquire_engine(&config_path, &config, AcquireMode::Try)
            .unwrap()
            .unwrap();
        assert!(engine_is_running(&config).unwrap());
        assert!(
            acquire_engine(&config_path, &config, AcquireMode::Try)
                .unwrap()
                .is_none()
        );

        drop(first);
        assert!(!engine_is_running(&config).unwrap());
    }

    #[test]
    fn runtime_status_round_trips() {
        let temp = tempfile::tempdir().unwrap();
        let config = test_config(temp.path());
        let config_path = temp.path().join("config.toml");
        let guard = acquire_engine(&config_path, &config, AcquireMode::Try)
            .unwrap()
            .unwrap();
        guard
            .update_status(
                &config_path,
                &config,
                Some(RuntimePairingInfo {
                    npub: "npub-pair".to_string(),
                    nprofile: "nprofile-pair".to_string(),
                    relays: vec!["wss://relay.example".to_string()],
                    pin_seconds: 30,
                }),
            )
            .unwrap();

        let status = read_status(&config).unwrap().unwrap();
        assert_eq!(status.npub.as_deref(), Some("npub-test"));
        assert_eq!(status.groups, vec!["group-a"]);
        assert_eq!(status.pairing.unwrap().pin_seconds, 30);
    }
}
