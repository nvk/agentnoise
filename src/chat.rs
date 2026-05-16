use anyhow::{Result, bail};

use crate::runner::{AgentKind, AgentRequest};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatCommand {
    Help,
    Status,
    Agents,
    AgentSessions { limit: Option<usize> },
    New { name: Option<String> },
    Rename { name: Option<String> },
    Sessions,
    Resume { target: Option<String> },
    Close,
    Repos,
    Use { repo_alias: String },
    Pwd,
    Ls { path: Option<String> },
    Cd { path: String },
    Jobs,
    Tail { job_id: String },
    Cancel { job_id: String },
    Approvals,
    Approve { approval_id: String },
    Deny { approval_id: String },
    Attachments,
    Attach { target: Option<String> },
    Worktrees,
    Worktree(WorktreeCommand),
    Run(AgentRequest),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeCommand {
    New { name: String },
    Use { name: String },
    Remove { name: String, confirm: bool },
}

pub fn parse_chat_command(message: &str) -> Result<ChatCommand> {
    let message = message.trim();
    if !message.starts_with('/') {
        bail!("not a command");
    }

    let (command, rest) = split_first(message.trim_start_matches('/'));
    let command = command.to_ascii_lowercase();

    match command.as_str() {
        "help" => Ok(ChatCommand::Help),
        "status" => Ok(ChatCommand::Status),
        "agents" => Ok(ChatCommand::Agents),
        "agent-sessions" | "local-sessions" => Ok(ChatCommand::AgentSessions {
            limit: optional_limit(rest)?,
        }),
        "new" => Ok(ChatCommand::New {
            name: optional(rest),
        }),
        "rename" | "here" => Ok(ChatCommand::Rename {
            name: optional(rest),
        }),
        "sessions" | "list" => Ok(ChatCommand::Sessions),
        "resume" => Ok(ChatCommand::Resume {
            target: optional(rest),
        }),
        "close" => Ok(ChatCommand::Close),
        "repos" => Ok(ChatCommand::Repos),
        "use" => {
            let repo_alias = required(rest, "usage: /use <repo>")?;
            Ok(ChatCommand::Use { repo_alias })
        }
        "pwd" => Ok(ChatCommand::Pwd),
        "ls" => Ok(ChatCommand::Ls {
            path: optional(rest),
        }),
        "cd" => {
            let path = required(rest, "usage: /cd <path>")?;
            Ok(ChatCommand::Cd { path })
        }
        "jobs" => Ok(ChatCommand::Jobs),
        "tail" => {
            let job_id = required(rest, "usage: /tail <job>")?;
            Ok(ChatCommand::Tail { job_id })
        }
        "cancel" => {
            let job_id = required(rest, "usage: /cancel <job>")?;
            Ok(ChatCommand::Cancel { job_id })
        }
        "approvals" => Ok(ChatCommand::Approvals),
        "approve" => {
            let approval_id = required(rest, "usage: /approve <approval>")?;
            Ok(ChatCommand::Approve { approval_id })
        }
        "deny" => {
            let approval_id = required(rest, "usage: /deny <approval>")?;
            Ok(ChatCommand::Deny { approval_id })
        }
        "attachments" => Ok(ChatCommand::Attachments),
        "attach" => Ok(ChatCommand::Attach {
            target: optional(rest),
        }),
        "worktrees" => Ok(ChatCommand::Worktrees),
        "worktree" => parse_worktree(rest),
        "codex" => parse_run(AgentKind::Codex, rest),
        "claude" => parse_run(AgentKind::Claude, rest),
        "hermes" => parse_run(AgentKind::Hermes, rest),
        "wiki" | "codex-wiki" => parse_wiki_run(AgentKind::Codex, "@wiki", rest),
        "claude-wiki" => parse_wiki_run(AgentKind::Claude, "wiki", rest),
        "codex-resume" => parse_resume(AgentKind::Codex, rest),
        "claude-resume" => parse_resume(AgentKind::Claude, rest),
        "hermes-resume" => parse_resume(AgentKind::Hermes, rest),
        _ => parse_profile_command(&command, rest),
    }
}

fn parse_profile_command(command: &str, rest: &str) -> Result<ChatCommand> {
    for (prefix, agent) in [
        ("codex-", AgentKind::Codex),
        ("claude-", AgentKind::Claude),
        ("hermes-", AgentKind::Hermes),
    ] {
        let Some(name) = command.strip_prefix(prefix) else {
            continue;
        };
        if let Some(name) = name.strip_suffix("-resume") {
            let (session, prompt) = split_first(rest);
            if name.is_empty() || session.is_empty() || prompt.trim().is_empty() {
                bail!("usage: /{}{}-resume <session> <prompt>", prefix, name);
            }
            return Ok(ChatCommand::Run(
                AgentRequest::resume(agent, session.to_string(), prompt.trim().to_string())
                    .with_profile(name),
            ));
        }
        if name.is_empty() {
            break;
        }
        return parse_run(agent, rest).map(|command| match command {
            ChatCommand::Run(request) => ChatCommand::Run(request.with_profile(name)),
            command => command,
        });
    }
    bail!("unknown command: /{command}")
}

fn parse_worktree(rest: &str) -> Result<ChatCommand> {
    let (subcommand, rest) = split_first(rest);
    match subcommand.to_ascii_lowercase().as_str() {
        "new" => Ok(ChatCommand::Worktree(WorktreeCommand::New {
            name: required(rest, "usage: /worktree new <name>")?,
        })),
        "use" => Ok(ChatCommand::Worktree(WorktreeCommand::Use {
            name: required(rest, "usage: /worktree use <name>")?,
        })),
        "remove" => {
            let (name, maybe_confirm) = split_first(rest);
            if name.is_empty() {
                bail!("usage: /worktree remove <name> confirm");
            }
            Ok(ChatCommand::Worktree(WorktreeCommand::Remove {
                name: name.to_string(),
                confirm: maybe_confirm.eq_ignore_ascii_case("confirm"),
            }))
        }
        _ => bail!("usage: /worktree <new|use|remove> ..."),
    }
}

fn parse_run(agent: AgentKind, rest: &str) -> Result<ChatCommand> {
    let prompt = rest.trim();
    if prompt.is_empty() {
        bail!("usage: /{} <prompt>", agent);
    }

    Ok(ChatCommand::Run(AgentRequest::prompt(agent, prompt)))
}

fn parse_wiki_run(agent: AgentKind, prefix: &str, rest: &str) -> Result<ChatCommand> {
    let prompt = rest.trim();
    if prompt.is_empty() {
        bail!("usage: /wiki <prompt>");
    }

    Ok(ChatCommand::Run(AgentRequest::prompt(
        agent,
        prefixed_prompt(prefix, prompt),
    )))
}

fn parse_resume(agent: AgentKind, rest: &str) -> Result<ChatCommand> {
    let (session, prompt) = split_first(rest);
    if session.is_empty() || prompt.trim().is_empty() {
        bail!("usage: /{}-resume <session> <prompt>", agent);
    }

    Ok(ChatCommand::Run(AgentRequest::resume(
        agent,
        session.to_string(),
        prompt.trim().to_string(),
    )))
}

fn required(rest: &str, usage: &str) -> Result<String> {
    let value = rest.trim();
    if value.is_empty() {
        bail!("{usage}");
    }
    Ok(value.to_string())
}

fn optional(rest: &str) -> Option<String> {
    let value = rest.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn optional_limit(rest: &str) -> Result<Option<usize>> {
    let value = rest.trim();
    if value.is_empty() {
        return Ok(None);
    }
    let limit = value
        .parse::<usize>()
        .map_err(|_| anyhow::anyhow!("usage: /agent-sessions [limit]"))?;
    if limit == 0 {
        bail!("usage: /agent-sessions [limit]");
    }
    Ok(Some(limit))
}

fn prefixed_prompt(prefix: &str, prompt: &str) -> String {
    let prompt = prompt.trim();
    if prompt == prefix || prompt.starts_with(&format!("{prefix} ")) {
        prompt.to_string()
    } else {
        format!("{prefix} {prompt}")
    }
}

fn split_first(input: &str) -> (&str, &str) {
    let input = input.trim();
    if input.is_empty() {
        return ("", "");
    }

    match input.find(char::is_whitespace) {
        Some(index) => (&input[..index], input[index..].trim()),
        None => (input, ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_status() {
        assert_eq!(parse_chat_command("/status").unwrap(), ChatCommand::Status);
        assert_eq!(parse_chat_command("/agents").unwrap(), ChatCommand::Agents);
        assert_eq!(
            parse_chat_command("/agent-sessions 12").unwrap(),
            ChatCommand::AgentSessions { limit: Some(12) }
        );
    }

    #[test]
    fn parses_codex_run() {
        assert_eq!(
            parse_chat_command("/codex explain the repo").unwrap(),
            ChatCommand::Run(AgentRequest::prompt(AgentKind::Codex, "explain the repo"))
        );
        assert_eq!(
            parse_chat_command("/codex-fix repair the test").unwrap(),
            ChatCommand::Run(
                AgentRequest::prompt(AgentKind::Codex, "repair the test").with_profile("fix")
            )
        );
        assert_eq!(
            parse_chat_command("/codex-unsafe-resume abc123 continue").unwrap(),
            ChatCommand::Run(
                AgentRequest::resume(AgentKind::Codex, "abc123", "continue").with_profile("unsafe")
            )
        );
    }

    #[test]
    fn parses_workspace_commands() {
        assert_eq!(
            parse_chat_command("/use sandbox").unwrap(),
            ChatCommand::Use {
                repo_alias: "sandbox".to_string()
            }
        );
        assert_eq!(
            parse_chat_command("/cd ..").unwrap(),
            ChatCommand::Cd {
                path: "..".to_string()
            }
        );
        assert_eq!(
            parse_chat_command("/ls src").unwrap(),
            ChatCommand::Ls {
                path: Some("src".to_string())
            }
        );
    }

    #[test]
    fn parses_session_commands() {
        assert_eq!(
            parse_chat_command("/new bugfix-ui").unwrap(),
            ChatCommand::New {
                name: Some("bugfix-ui".to_string())
            }
        );
        assert_eq!(
            parse_chat_command("/rename").unwrap(),
            ChatCommand::Rename { name: None }
        );
        assert_eq!(
            parse_chat_command("/here").unwrap(),
            ChatCommand::Rename { name: None }
        );
        assert_eq!(
            parse_chat_command("/sessions").unwrap(),
            ChatCommand::Sessions
        );
        assert_eq!(parse_chat_command("/list").unwrap(), ChatCommand::Sessions);
        assert_eq!(
            parse_chat_command("/resume 2").unwrap(),
            ChatCommand::Resume {
                target: Some("2".to_string())
            }
        );
        assert_eq!(parse_chat_command("/close").unwrap(), ChatCommand::Close);
    }

    #[test]
    fn parses_wiki_run() {
        assert_eq!(
            parse_chat_command("/wiki research agent chat ux").unwrap(),
            ChatCommand::Run(AgentRequest::prompt(
                AgentKind::Codex,
                "@wiki research agent chat ux"
            ))
        );
        assert_eq!(
            parse_chat_command("/claude-wiki research agent chat ux").unwrap(),
            ChatCommand::Run(AgentRequest::prompt(
                AgentKind::Claude,
                "wiki research agent chat ux"
            ))
        );
    }

    #[test]
    fn parses_claude_resume() {
        assert_eq!(
            parse_chat_command("/claude-resume abc123 keep going").unwrap(),
            ChatCommand::Run(AgentRequest::resume(
                AgentKind::Claude,
                "abc123",
                "keep going"
            ))
        );
    }

    #[test]
    fn parses_hermes_commands() {
        assert_eq!(
            parse_chat_command("/hermes explain the repo").unwrap(),
            ChatCommand::Run(AgentRequest::prompt(AgentKind::Hermes, "explain the repo"))
        );
        assert_eq!(
            parse_chat_command("/hermes-resume h123 keep going").unwrap(),
            ChatCommand::Run(AgentRequest::resume(
                AgentKind::Hermes,
                "h123",
                "keep going"
            ))
        );
    }

    #[test]
    fn parses_approval_attachment_and_worktree_commands() {
        assert_eq!(
            parse_chat_command("/approvals").unwrap(),
            ChatCommand::Approvals
        );
        assert_eq!(
            parse_chat_command("/approve apr-123").unwrap(),
            ChatCommand::Approve {
                approval_id: "apr-123".to_string()
            }
        );
        assert_eq!(
            parse_chat_command("/attach 1").unwrap(),
            ChatCommand::Attach {
                target: Some("1".to_string())
            }
        );
        assert_eq!(
            parse_chat_command("/worktree new fix ui").unwrap(),
            ChatCommand::Worktree(WorktreeCommand::New {
                name: "fix ui".to_string()
            })
        );
        assert_eq!(
            parse_chat_command("/worktree remove fix-ui confirm").unwrap(),
            ChatCommand::Worktree(WorktreeCommand::Remove {
                name: "fix-ui".to_string(),
                confirm: true
            })
        );
    }

    #[test]
    fn rejects_plain_text() {
        assert!(parse_chat_command("hello").is_err());
    }
}
