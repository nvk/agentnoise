use std::fs;
use std::path::{Path, PathBuf};

use crate::config::{Config, RunnerLauncher};
use crate::paths::{expand_tilde, find_on_path, is_gui_backed_workspace_path};
use crate::runner::{AgentKind, AgentRequest};
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
    ];
    match config.runner.launcher {
        RunnerLauncher::Bondage => {
            checks.push(command_check("bondage", &config.runner.bondage_bin))
        }
        RunnerLauncher::Direct => checks.push(Check {
            level: Level::Warn,
            name: "agent launcher".to_string(),
            detail: "direct; bondage local policy boundary is disabled".to_string(),
        }),
    }
    checks.extend([
        command_check("codex", &config.agents.codex.bin),
        command_check("claude", &config.agents.claude.bin),
    ]);
    if config.agents.hermes.enabled {
        checks.push(command_check("hermes", &config.agents.hermes.bin));
    }
    for warning in config.agent_profile_warnings() {
        checks.push(Check {
            level: Level::Warn,
            name: "agent profile".to_string(),
            detail: warning,
        });
    }
    if config.runner.launcher == RunnerLauncher::Bondage {
        let bondage_conf = config.resolved_bondage_conf();
        checks.push(path_check("bondage conf", &bondage_conf));
        checks.extend(bondage_profile_checks(config, &bondage_conf));
    }
    checks.extend([
        path_check("data dir", &config.resolved_data_dir()),
        path_check("log dir", &config.resolved_log_dir()),
    ]);

    let group_count = config.whitenoise.control_group_ids().len();
    checks.push(Check {
        level: Level::Ok,
        name: "White Noise transport".to_string(),
        detail: format!("{:?}", config.whitenoise.transport).to_ascii_lowercase(),
    });
    if let Some(socket) = config.whitenoise.resolved_socket() {
        checks.push(path_check("White Noise socket", &socket));
    }

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

    if config.whitenoise.dev_burner_nsec {
        checks.push(Check {
            level: Level::Warn,
            name: "dev burner nsec".to_string(),
            detail: identity_dev_burner_status(config),
        });
    } else if config.whitenoise.use_keychain_nsec {
        let store = config.secret_store();
        checks.push(Check {
            level: Level::Ok,
            name: "OS keychain nsec".to_string(),
            detail: format!(
                "configured: {}; run `agentnoise keychain status` for a live keychain check",
                store.label()
            ),
        });
    } else {
        checks.push(Check {
            level: Level::Ok,
            name: "OS keychain nsec".to_string(),
            detail: "disabled".to_string(),
        });
    }

    for repo in &config.repos {
        let repo_path = expand_tilde(&repo.path);
        checks.push(path_check(&format!("repo {}", repo.alias), &repo_path));
        if is_gui_backed_workspace_path(&repo_path) {
            checks.push(Check {
                level: Level::Warn,
                name: format!("repo {} service path", repo.alias),
                detail:
                    "under iCloud Drive/CloudDocs; Codex may hang when run from launchd/brew services. Move the repo outside iCloud or run `agentnoise up` interactively."
                        .to_string(),
            });
        }
    }

    checks.push(path_check("event log", &config.resolved_event_log_path()));
    checks.push(path_check(
        "approvals db",
        &config.resolved_approvals_path(),
    ));
    checks.push(path_check(
        "attachments db",
        &config.resolved_attachments_path(),
    ));

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

fn bondage_profile_checks(config: &Config, config_path: &Path) -> Vec<Check> {
    let Ok(text) = fs::read_to_string(config_path) else {
        return vec![Check {
            level: Level::Warn,
            name: "bondage profiles".to_string(),
            detail: format!(
                "cannot read {}; see https://github.com/nvk/agentnoise/blob/main/docs/configuration.md#agent-launcher",
                config_path.display()
            ),
        }];
    };

    let mut checks = Vec::new();
    for agent in [AgentKind::Codex, AgentKind::Claude, AgentKind::Hermes] {
        let agent_config = config.agent(agent);
        if !agent_config.enabled {
            continue;
        }

        let default_profile = config.effective_agent_profile(agent);
        checks.push(bondage_profile_check(
            &format!("{agent} profile"),
            &default_profile,
            &text,
        ));

        for variant in &agent_config.profiles {
            let mut request = AgentRequest::prompt(agent, "");
            request.profile = Some(variant.name.clone());
            match config.effective_agent_profile_for_request(&request) {
                Ok(profile) => checks.push(bondage_profile_check(
                    &format!("{agent}-{} profile", variant.name),
                    &profile,
                    &text,
                )),
                Err(error) => checks.push(Check {
                    level: Level::Warn,
                    name: format!("{agent}-{} profile", variant.name),
                    detail: error.to_string(),
                }),
            }
        }
    }
    checks
}

fn bondage_profile_check(name: &str, profile: &str, text: &str) -> Check {
    let profile_header = format!("[profile \"{profile}\"]");
    if text.lines().any(|line| line.trim() == profile_header) {
        return Check {
            level: Level::Ok,
            name: name.to_string(),
            detail: profile.to_string(),
        };
    }

    Check {
        level: Level::Warn,
        name: name.to_string(),
        detail: format!(
            "{profile} missing from bondage.conf; use `agentnoise config launcher direct` for raw CLIs or see https://github.com/nvk/agentnoise/blob/main/docs/configuration.md#agent-launcher"
        ),
    }
}

fn identity_dev_burner_status(config: &Config) -> String {
    let Some(path) = crate::identity::dev_burner_nsec_path(
        &config.whitenoise,
        crate::identity::DEFAULT_IDENTITY_NAME,
    ) else {
        return "enabled but no file path configured".to_string();
    };

    if path.is_file() {
        format!(
            "development-only plaintext secret present: {}",
            path.display()
        )
    } else {
        format!(
            "development-only plaintext secret missing: {}",
            path.display()
        )
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doctor_warns_about_generic_agent_profiles() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = Config::template();
        config.runner.data_dir = temp.path().join("data").display().to_string();
        config.runner.log_dir = temp.path().join("logs").display().to_string();
        config.agents.codex.profile = "codex".to_string();

        let output = render_doctor(&temp.path().join("config.toml"), &config);

        assert!(output.contains("generic profile `codex`"));
        assert!(output.contains("codex-agentnoise"));
    }

    #[test]
    fn doctor_skips_bondage_checks_in_direct_mode() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = Config::template();
        config.runner.launcher = RunnerLauncher::Direct;
        config.runner.data_dir = temp.path().join("data").display().to_string();
        config.runner.log_dir = temp.path().join("logs").display().to_string();

        let output = render_doctor(&temp.path().join("config.toml"), &config);

        assert!(output.contains("[warn] agent launcher: direct"));
        assert!(!output.contains("bondage conf"));
    }

    #[test]
    fn doctor_does_not_query_keychain_status() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = Config::template();
        config.whitenoise.use_keychain_nsec = true;
        config.runner.data_dir = temp.path().join("data").display().to_string();
        config.runner.log_dir = temp.path().join("logs").display().to_string();

        let output = render_doctor(&temp.path().join("config.toml"), &config);

        assert!(output.contains("OS keychain nsec"));
        assert!(output.contains("configured: agentnoise / whitenoise-nsec"));
        assert!(output.contains("agentnoise keychain status"));
    }

    #[test]
    fn doctor_warns_about_missing_bondage_profiles_with_manual_link() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = Config::template();
        config.runner.data_dir = temp.path().join("data").display().to_string();
        config.runner.log_dir = temp.path().join("logs").display().to_string();
        config.runner.bondage_conf = temp.path().join("bondage.conf").display().to_string();
        std::fs::write(
            &config.runner.bondage_conf,
            "[profile \"claude-agentnoise\"]\n",
        )
        .unwrap();

        let output = render_doctor(&temp.path().join("config.toml"), &config);

        assert!(output.contains("[warn] codex profile"));
        assert!(output.contains("codex-agentnoise missing from bondage.conf"));
        assert!(output.contains("agentnoise config launcher direct"));
        assert!(output.contains("docs/configuration.md#agent-launcher"));
    }

    #[test]
    fn doctor_warns_about_icloud_repos_for_services() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = Config::template();
        config.runner.data_dir = temp.path().join("data").display().to_string();
        config.runner.log_dir = temp.path().join("logs").display().to_string();
        config.repos[0].path =
            "/Users/user/Library/Mobile Documents/com~apple~CloudDocs/project".to_string();

        let output = render_doctor(&temp.path().join("config.toml"), &config);

        assert!(output.contains("[warn] repo sandbox service path"));
        assert!(output.contains("Codex may hang"));
        assert!(output.contains("agentnoise up"));
    }
}
