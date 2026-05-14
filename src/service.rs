use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::ValueEnum;

use crate::config::Config;
use crate::launchd;
use crate::paths::{default_service_path, expand_tilde};

pub const SYSTEMD_UNIT: &str = "agentnoise.service";

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum ServiceTarget {
    Launchd,
    SystemdUser,
    FreebsdRc,
    OpenbsdRc,
}

pub fn default_target() -> ServiceTarget {
    if cfg!(target_os = "macos") {
        ServiceTarget::Launchd
    } else if cfg!(target_os = "linux") {
        ServiceTarget::SystemdUser
    } else if cfg!(target_os = "freebsd") {
        ServiceTarget::FreebsdRc
    } else if cfg!(target_os = "openbsd") {
        ServiceTarget::OpenbsdRc
    } else {
        ServiceTarget::SystemdUser
    }
}

pub fn render(target: ServiceTarget, exe: &Path, config_path: &Path, config: &Config) -> String {
    match target {
        ServiceTarget::Launchd => launchd::render_plist(exe, config_path, config),
        ServiceTarget::SystemdUser => render_systemd_user(exe, config_path, config),
        ServiceTarget::FreebsdRc => render_freebsd_rc(exe, config_path),
        ServiceTarget::OpenbsdRc => render_openbsd_rc(exe, config_path),
    }
}

pub fn install(
    target: ServiceTarget,
    exe: &Path,
    config_path: &Path,
    config: &Config,
    force: bool,
    load: bool,
    path_override: Option<&Path>,
) -> Result<PathBuf> {
    if !config_path.exists() {
        bail!(
            "config does not exist: {}; run `agentnoise up --no-listen` first",
            config_path.display()
        );
    }
    ensure_runtime_dirs(config)?;

    match target {
        ServiceTarget::Launchd => {
            let path = launchd::install(exe, config_path, config, force)?;
            if load {
                launchd::load_plist(&path)?;
            }
            Ok(path)
        }
        ServiceTarget::SystemdUser => {
            let path = path_override
                .map(Path::to_path_buf)
                .unwrap_or_else(systemd_user_path);
            write_service_file(&path, &render_systemd_user(exe, config_path, config), force)?;
            if load {
                systemctl_user(&["daemon-reload"])?;
                systemctl_user(&["enable", "--now", SYSTEMD_UNIT])?;
            }
            Ok(path)
        }
        ServiceTarget::FreebsdRc => {
            let path = path_override
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("/usr/local/etc/rc.d/agentnoise"));
            write_service_file(&path, &render_freebsd_rc(exe, config_path), force)?;
            make_executable(&path)?;
            if load {
                run_command("sysrc", &["agentnoise_enable=YES"])?;
                run_command("service", &["agentnoise", "start"])?;
            }
            Ok(path)
        }
        ServiceTarget::OpenbsdRc => {
            let path = path_override
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("/etc/rc.d/agentnoise"));
            write_service_file(&path, &render_openbsd_rc(exe, config_path), force)?;
            make_executable(&path)?;
            if load {
                run_command("rcctl", &["enable", "agentnoise"])?;
                run_command("rcctl", &["start", "agentnoise"])?;
            }
            Ok(path)
        }
    }
}

pub fn uninstall(
    target: ServiceTarget,
    unload: bool,
    path_override: Option<&Path>,
) -> Result<Option<PathBuf>> {
    match target {
        ServiceTarget::Launchd => {
            let removed = launchd::uninstall(unload)?;
            Ok(removed.then(launchd::plist_path))
        }
        ServiceTarget::SystemdUser => {
            if unload {
                systemctl_user(&["disable", "--now", SYSTEMD_UNIT]).ok();
            }
            let path = path_override
                .map(Path::to_path_buf)
                .unwrap_or_else(systemd_user_path);
            if path.exists() {
                fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
                systemctl_user(&["daemon-reload"]).ok();
                Ok(Some(path))
            } else {
                Ok(None)
            }
        }
        ServiceTarget::FreebsdRc => {
            if unload {
                run_command("service", &["agentnoise", "stop"]).ok();
            }
            let path = path_override
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("/usr/local/etc/rc.d/agentnoise"));
            remove_if_present(path)
        }
        ServiceTarget::OpenbsdRc => {
            if unload {
                run_command("rcctl", &["stop", "agentnoise"]).ok();
                run_command("rcctl", &["disable", "agentnoise"]).ok();
            }
            let path = path_override
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("/etc/rc.d/agentnoise"));
            remove_if_present(path)
        }
    }
}

pub fn systemd_user_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| expand_tilde("~/.config"))
        .join("systemd/user")
        .join(SYSTEMD_UNIT)
}

fn render_systemd_user(exe: &Path, config_path: &Path, config: &Config) -> String {
    format!(
        r#"[Unit]
Description=agentnoise White Noise coding-agent bridge
Documentation=https://agentnoise.com
Wants=network-online.target
After=network-online.target

[Service]
Type=simple
ExecStart={exe} --config {config_path} up
Restart=on-failure
RestartSec=5s
Environment={path_env}
WorkingDirectory={workdir}

[Install]
WantedBy=default.target
"#,
        exe = systemd_quote(&exe.display().to_string()),
        config_path = systemd_quote(&config_path.display().to_string()),
        path_env = systemd_quote(&format!("PATH={}", default_service_path())),
        workdir = systemd_quote(&config.resolved_data_dir().display().to_string()),
    )
}

fn render_freebsd_rc(exe: &Path, config_path: &Path) -> String {
    format!(
        r#"#!/bin/sh
#
# PROVIDE: agentnoise
# REQUIRE: NETWORKING LOGIN
# KEYWORD: shutdown
#
# Add this to /etc/rc.conf:
# agentnoise_enable="YES"
# agentnoise_user="your-user"

. /etc/rc.subr

name="agentnoise"
rcvar="agentnoise_enable"

load_rc_config "$name"

: ${{agentnoise_enable:="NO"}}
: ${{agentnoise_user:="agentnoise"}}
: ${{agentnoise_command:={exe}}}
: ${{agentnoise_config:={config_path}}}
: ${{agentnoise_path:="{path_env}"}}

pidfile="/var/run/${{name}}.pid"
procname="${{agentnoise_command}}"
command="/usr/sbin/daemon"
command_args="-f -p ${{pidfile}} -u ${{agentnoise_user}} /usr/bin/env PATH=${{agentnoise_path}} \"${{agentnoise_command}}\" --config \"${{agentnoise_config}}\" up"

run_rc_command "$1"
"#,
        exe = shell_quote(&exe.display().to_string()),
        config_path = shell_quote(&config_path.display().to_string()),
        path_env = default_service_path(),
    )
}

fn render_openbsd_rc(exe: &Path, config_path: &Path) -> String {
    format!(
        r#"#!/bin/ksh

daemon={exe}
daemon_flags="--config {config_path} up"
daemon_user="_agentnoise"

export PATH="{path_env}"

. /etc/rc.d/rc.subr

rc_reload=NO

rc_cmd "$1"
"#,
        exe = shell_quote(&exe.display().to_string()),
        config_path = shell_quote(&config_path.display().to_string()),
        path_env = default_service_path(),
    )
}

fn write_service_file(path: &Path, text: &str, force: bool) -> Result<()> {
    if path.exists() && !force {
        bail!(
            "{} already exists; use --force to overwrite",
            path.display()
        );
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    fs::write(path, text).with_context(|| format!("writing {}", path.display()))
}

fn ensure_runtime_dirs(config: &Config) -> Result<()> {
    let data_dir = config.resolved_data_dir();
    fs::create_dir_all(&data_dir)
        .with_context(|| format!("creating data dir {}", data_dir.display()))?;
    let log_dir = config.resolved_log_dir();
    fs::create_dir_all(&log_dir).with_context(|| format!("creating log dir {}", log_dir.display()))
}

fn remove_if_present(path: PathBuf) -> Result<Option<PathBuf>> {
    if path.exists() {
        fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
        Ok(Some(path))
    } else {
        Ok(None)
    }
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)
        .with_context(|| format!("reading metadata for {}", path.display()))?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)
        .with_context(|| format!("setting executable mode on {}", path.display()))
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<()> {
    Ok(())
}

fn systemctl_user(args: &[&str]) -> Result<()> {
    let status = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .status()
        .with_context(|| format!("running systemctl --user {}", args.join(" ")))?;
    if !status.success() {
        bail!("systemctl --user {} exited with {status}", args.join(" "));
    }
    Ok(())
}

fn run_command(program: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .status()
        .with_context(|| format!("running {} {}", program, args.join(" ")))?;
    if !status.success() {
        bail!("{} {} exited with {status}", program, args.join(" "));
    }
    Ok(())
}

fn systemd_quote(value: &str) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%");
    format!("\"{escaped}\"")
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn systemd_unit_runs_agentnoise_up() {
        let mut config = Config::template();
        config.runner.data_dir = "/tmp/agentnoise data".to_string();
        let unit = render_systemd_user(
            Path::new("/usr/local/bin/agentnoise"),
            Path::new("/home/me/.local/share/agentnoise/config.toml"),
            &config,
        );
        assert!(unit.contains("ExecStart=\"/usr/local/bin/agentnoise\" --config"));
        assert!(unit.contains(" up\n"));
        assert!(unit.contains("Restart=on-failure"));
        assert!(unit.contains("Environment=\"PATH="));
    }

    #[test]
    fn install_creates_runtime_dirs_before_writing_service() {
        let temp = tempfile::tempdir().unwrap();
        let config_path = temp.path().join("config.toml");
        let service_path = temp.path().join("agentnoise.service");
        let mut config = Config::template();
        config.runner.data_dir = temp.path().join("data").display().to_string();
        config.runner.log_dir = temp.path().join("logs").display().to_string();
        config.save(&config_path).unwrap();

        install(
            ServiceTarget::SystemdUser,
            Path::new("/usr/local/bin/agentnoise"),
            &config_path,
            &config,
            false,
            false,
            Some(&service_path),
        )
        .unwrap();

        assert!(config.resolved_data_dir().is_dir());
        assert!(config.resolved_log_dir().is_dir());
        assert!(service_path.is_file());
    }

    #[test]
    fn freebsd_rc_uses_daemon_supervisor() {
        let rc = render_freebsd_rc(
            Path::new("/usr/local/bin/agentnoise"),
            Path::new("/usr/local/etc/agentnoise/config.toml"),
        );
        assert!(rc.contains("PROVIDE: agentnoise"));
        assert!(rc.contains("/usr/sbin/daemon"));
        assert!(rc.contains(" up"));
    }

    #[test]
    fn openbsd_rc_uses_rc_subr() {
        let rc = render_openbsd_rc(
            Path::new("/usr/local/bin/agentnoise"),
            Path::new("/etc/agentnoise/config.toml"),
        );
        assert!(rc.contains("/etc/rc.d/rc.subr"));
        assert!(rc.contains("daemon_flags=\"--config"));
    }
}
