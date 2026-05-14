use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::paths::{expand_tilde, find_on_path};
use crate::whitenoise_cli;

#[derive(Debug)]
enum Level {
    Ok,
    Warn,
}

#[derive(Debug)]
struct Check {
    level: Level,
    name: String,
    detail: String,
}

pub fn render_doctor(config_path: &Path, config: &Config) -> String {
    let mut checks = vec![
        path_check("config", config_path),
        wn_command_check(&config.whitenoise.wn_bin),
        command_check("bondage", &config.runner.bondage_bin),
        command_check("codex", &config.agents.codex.bin),
        command_check("claude", &config.agents.claude.bin),
    ];
    if config.agents.hermes.enabled {
        checks.push(command_check("hermes", &config.agents.hermes.bin));
    }
    checks.extend([
        path_check("bondage conf", &config.resolved_bondage_conf()),
        path_check("data dir", &config.resolved_data_dir()),
        path_check("log dir", &config.resolved_log_dir()),
    ]);

    let group_count = config.whitenoise.control_group_ids().len();
    if group_count == 0 {
        checks.push(Check {
            level: Level::Warn,
            name: "White Noise groups".to_string(),
            detail: "not configured".to_string(),
        });
    } else {
        checks.push(Check {
            level: Level::Ok,
            name: "White Noise groups".to_string(),
            detail: format!("{group_count} configured"),
        });
    }

    if config.whitenoise.allowed_senders.is_empty() {
        if config.whitenoise.require_pairing_pin {
            checks.push(Check {
                level: Level::Warn,
                name: "sender allowlist".to_string(),
                detail: "empty; pairing PIN required before commands are accepted".to_string(),
            });
        } else {
            checks.push(Check {
                level: Level::Warn,
                name: "sender allowlist".to_string(),
                detail: "empty; any sender in the group can command the helper".to_string(),
            });
        }
    } else {
        checks.push(Check {
            level: Level::Ok,
            name: "sender allowlist".to_string(),
            detail: format!("{} sender(s)", config.whitenoise.allowed_senders.len()),
        });
    }

    if config.whitenoise.require_pairing_pin {
        checks.push(Check {
            level: Level::Ok,
            name: "pairing PIN".to_string(),
            detail: format!("{}s window", config.whitenoise.pairing_pin_seconds),
        });
    }

    if config.whitenoise.use_keychain_nsec {
        let store = config.secret_store();
        match store.nsec_status() {
            Ok(true) => checks.push(Check {
                level: Level::Ok,
                name: "OS keychain nsec".to_string(),
                detail: format!("present: {}", store.label()),
            }),
            Ok(false) => checks.push(Check {
                level: Level::Warn,
                name: "OS keychain nsec".to_string(),
                detail: format!("missing: {}", store.label()),
            }),
            Err(error) => checks.push(Check {
                level: Level::Warn,
                name: "OS keychain nsec".to_string(),
                detail: format!("unavailable: {error:#}"),
            }),
        }
    } else {
        checks.push(Check {
            level: Level::Ok,
            name: "OS keychain nsec".to_string(),
            detail: "disabled".to_string(),
        });
    }

    for repo in &config.repos {
        checks.push(path_check(
            &format!("repo {}", repo.alias),
            &expand_tilde(&repo.path),
        ));
    }

    let mut output = String::from("agentnoise doctor\n\n");
    for check in checks {
        let marker = match check.level {
            Level::Ok => "ok",
            Level::Warn => "warn",
        };
        output.push_str(&format!("[{marker}] {}: {}\n", check.name, check.detail));
    }
    output
}

fn command_check(name: &str, command: &str) -> Check {
    match find_executable(command) {
        Some(path) => Check {
            level: Level::Ok,
            name: name.to_string(),
            detail: path.display().to_string(),
        },
        None => Check {
            level: Level::Warn,
            name: name.to_string(),
            detail: format!("{command} not found on PATH"),
        },
    }
}

fn wn_command_check(command: &str) -> Check {
    let resolved = whitenoise_cli::resolve_wn(command);
    if resolved.is_file() {
        return Check {
            level: Level::Ok,
            name: "wn".to_string(),
            detail: resolved.display().to_string(),
        };
    }

    Check {
        level: Level::Warn,
        name: "wn".to_string(),
        detail: format!("{} not found", resolved.display()),
    }
}

fn path_check(name: &str, path: &Path) -> Check {
    if path.exists() {
        Check {
            level: Level::Ok,
            name: name.to_string(),
            detail: path.display().to_string(),
        }
    } else {
        Check {
            level: Level::Warn,
            name: name.to_string(),
            detail: format!("missing: {}", path.display()),
        }
    }
}

fn find_executable(command: &str) -> Option<PathBuf> {
    let command_path = PathBuf::from(command);
    if command_path.components().count() > 1 {
        return command_path.is_file().then_some(command_path);
    }

    find_on_path(command)
}
