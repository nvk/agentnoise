use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::config::Config;
use crate::paths::{default_service_path, expand_tilde};

pub const LABEL: &str = "com.agentnoise.agentnoise";

pub fn plist_path() -> PathBuf {
    expand_tilde("~/Library/LaunchAgents").join(format!("{LABEL}.plist"))
}

pub fn render_plist(exe: &Path, config_path: &Path, config: &Config) -> String {
    let log_dir = config.resolved_log_dir();
    let stdout = log_dir.join("launchd.out.log");
    let stderr = log_dir.join("launchd.err.log");

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{exe}</string>
    <string>--config</string>
    <string>{config}</string>
    <string>up</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key>
    <string>{path_env}</string>
  </dict>
  <key>StandardOutPath</key>
  <string>{stdout}</string>
  <key>StandardErrorPath</key>
  <string>{stderr}</string>
</dict>
</plist>
"#,
        label = LABEL,
        exe = xml_escape(&exe.display().to_string()),
        config = xml_escape(&config_path.display().to_string()),
        path_env = xml_escape(&default_service_path()),
        stdout = xml_escape(&stdout.display().to_string()),
        stderr = xml_escape(&stderr.display().to_string()),
    )
}

pub fn install(exe: &Path, config_path: &Path, config: &Config, force: bool) -> Result<PathBuf> {
    if !config_path.exists() {
        bail!(
            "config does not exist: {}; run `agentnoise init` first",
            config_path.display()
        );
    }

    let path = plist_path();
    if path.exists() && !force {
        bail!(
            "{} already exists; use --force to overwrite",
            path.display()
        );
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    fs::create_dir_all(config.resolved_log_dir())
        .with_context(|| format!("creating {}", config.resolved_log_dir().display()))?;
    fs::write(&path, render_plist(exe, config_path, config))
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

pub fn uninstall(unload: bool) -> Result<bool> {
    let path = plist_path();
    if unload && path.exists() {
        unload_plist(&path).ok();
    }
    if path.exists() {
        fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
        Ok(true)
    } else {
        Ok(false)
    }
}

pub fn load_plist(path: &Path) -> Result<()> {
    let uid = current_uid()?;
    let status = Command::new("launchctl")
        .arg("bootstrap")
        .arg(format!("gui/{uid}"))
        .arg(path)
        .status()
        .context("running launchctl bootstrap")?;
    if !status.success() {
        bail!("launchctl bootstrap exited with {status}");
    }
    Ok(())
}

pub fn unload_plist(path: &Path) -> Result<()> {
    let uid = current_uid()?;
    let status = Command::new("launchctl")
        .arg("bootout")
        .arg(format!("gui/{uid}"))
        .arg(path)
        .status()
        .context("running launchctl bootout")?;
    if !status.success() {
        bail!("launchctl bootout exited with {status}");
    }
    Ok(())
}

fn current_uid() -> Result<String> {
    let output = Command::new("id")
        .arg("-u")
        .output()
        .context("running id -u")?;
    if !output.status.success() {
        bail!("id -u exited with {}", output.status);
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn xml_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_contains_agentnoise_listener() {
        let config = Config::template();
        let plist = render_plist(
            Path::new("/opt/homebrew/bin/agentnoise"),
            Path::new("/tmp/config.toml"),
            &config,
        );
        assert!(plist.contains("<string>com.agentnoise.agentnoise</string>"));
        assert!(plist.contains("<string>up</string>"));
        assert!(plist.contains("<key>EnvironmentVariables</key>"));
        assert!(plist.contains("/opt/homebrew/bin/agentnoise"));
    }
}
