use std::env;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::runner::AgentKind;
use crate::text::{compact_text, compact_timestamp, short_ref};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalAgentSession {
    pub agent: AgentKind,
    pub id: String,
    pub title: Option<String>,
    pub cwd: Option<PathBuf>,
    pub updated_at: Option<String>,
    pub source_path: PathBuf,
}

pub fn discover_local_sessions(limit: usize) -> Result<Vec<LocalAgentSession>> {
    discover_local_sessions_in(&codex_home(), &claude_home(), limit)
}

pub fn discover_all_local_sessions() -> Result<Vec<LocalAgentSession>> {
    discover_local_sessions_in(&codex_home(), &claude_home(), usize::MAX)
}

pub fn discover_local_sessions_in(
    codex_home: &Path,
    claude_home: &Path,
    limit: usize,
) -> Result<Vec<LocalAgentSession>> {
    let mut sessions = Vec::new();
    sessions.extend(discover_codex_sessions(codex_home)?);
    sessions.extend(discover_claude_sessions(claude_home)?);
    sessions.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| right.id.cmp(&left.id))
    });
    sessions.truncate(limit.max(1));
    Ok(sessions)
}

pub fn render_local_sessions(limit: usize) -> String {
    match discover_local_sessions(limit) {
        Ok(sessions) => render_sessions(&sessions),
        Err(error) => format!("Error: failed to inspect local agent sessions: {error:#}"),
    }
}

pub fn local_session_key(session: &LocalAgentSession) -> String {
    format!("{}:{}", session.agent, session.id)
}

pub fn resolve_session_id(agent: AgentKind, target: &str) -> Result<Option<String>> {
    let target = target.trim();
    if target.is_empty() {
        return Ok(None);
    }
    let sessions = discover_all_local_sessions()?;
    if let Some(exact) = sessions
        .iter()
        .find(|session| session.agent == agent && session.id == target)
    {
        return Ok(Some(exact.id.clone()));
    }

    let matches = sessions
        .iter()
        .filter(|session| session.agent == agent && session.id.starts_with(target))
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => Ok(None),
        [session] => Ok(Some(session.id.clone())),
        _ => anyhow::bail!("ambiguous {agent} session id {target}; use more characters"),
    }
}

pub fn render_sessions(sessions: &[LocalAgentSession]) -> String {
    if sessions.is_empty() {
        return "No local Codex or Claude sessions found.".to_string();
    }

    let lines = render_session_list(sessions);

    format!("local sessions\n{lines}\nmetadata only")
}

pub fn render_new_session_notice(sessions: &[LocalAgentSession]) -> String {
    if sessions.is_empty() {
        return "No new local Codex or Claude sessions found.".to_string();
    }

    let noun = if sessions.len() == 1 {
        "session"
    } else {
        "sessions"
    };
    format!(
        "new local {noun}\n{}\nmetadata only",
        render_session_list(sessions)
    )
}

fn render_session_list(sessions: &[LocalAgentSession]) -> String {
    sessions
        .iter()
        .enumerate()
        .map(|(index, session)| render_session_entry(index + 1, session))
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_session_entry(index: usize, session: &LocalAgentSession) -> String {
    let title = session
        .title
        .as_deref()
        .filter(|title| !title.trim().is_empty())
        .unwrap_or("untitled");
    let cwd = session
        .cwd
        .as_ref()
        .map(|path| compact_path(path, 36))
        .unwrap_or_else(|| "unknown".to_string());
    let updated = session
        .updated_at
        .as_deref()
        .map(compact_timestamp)
        .unwrap_or_else(|| "unknown".to_string());
    let resume = match session.agent {
        AgentKind::Codex => "/codex-resume",
        AgentKind::Claude => "/claude-resume",
        AgentKind::Hermes => "/hermes-resume",
    };
    let id = short_ref(&session.id);
    format!(
        "{index}. {} {id} - {}\n   {updated} | {cwd}\n   {resume} {id} <prompt>",
        session.agent,
        compact_text(title, 54),
    )
}

fn discover_codex_sessions(codex_home: &Path) -> Result<Vec<LocalAgentSession>> {
    let path = codex_home.join("session_index.jsonl");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = File::open(&path).with_context(|| format!("opening {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut sessions = Vec::new();
    for line in reader.lines() {
        let line = line.with_context(|| format!("reading {}", path.display()))?;
        let Ok(record) = serde_json::from_str::<CodexIndexRecord>(&line) else {
            continue;
        };
        if record.id.trim().is_empty() {
            continue;
        }
        sessions.push(LocalAgentSession {
            agent: AgentKind::Codex,
            id: record.id.trim().to_string(),
            title: record.thread_name.map(|value| value.trim().to_string()),
            cwd: None,
            updated_at: record.updated_at,
            source_path: path.clone(),
        });
    }
    Ok(sessions)
}

fn discover_claude_sessions(claude_home: &Path) -> Result<Vec<LocalAgentSession>> {
    let projects_dir = claude_home.join("projects");
    if !projects_dir.exists() {
        return Ok(Vec::new());
    }

    let mut sessions = Vec::new();
    for project in read_dirs(&projects_dir)? {
        for entry in read_files(&project)? {
            if entry.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(id) = entry.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            let metadata = claude_metadata(&entry).unwrap_or_default();
            let updated_at = metadata
                .updated_at
                .or_else(|| modified_time(&entry).and_then(format_system_time));
            let title = metadata
                .cwd
                .as_ref()
                .and_then(|cwd| cwd.file_name())
                .and_then(|name| name.to_str())
                .map(str::to_string)
                .or_else(|| claude_project_title(&entry))
                .or_else(|| Some(short_id(id).to_string()));

            sessions.push(LocalAgentSession {
                agent: AgentKind::Claude,
                id: id.to_string(),
                title,
                cwd: metadata.cwd,
                updated_at,
                source_path: entry,
            });
        }
    }
    Ok(sessions)
}

fn read_dirs(path: &Path) -> Result<Vec<PathBuf>> {
    let mut dirs = Vec::new();
    for entry in fs::read_dir(path).with_context(|| format!("reading {}", path.display()))? {
        let entry = entry.with_context(|| format!("reading {}", path.display()))?;
        let path = entry.path();
        if path.is_dir() {
            dirs.push(path);
        }
    }
    Ok(dirs)
}

fn read_files(path: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(path).with_context(|| format!("reading {}", path.display()))? {
        let entry = entry.with_context(|| format!("reading {}", path.display()))?;
        let path = entry.path();
        if path.is_file() {
            files.push(path);
        }
    }
    Ok(files)
}

#[derive(Debug, Default)]
struct ClaudeMetadata {
    cwd: Option<PathBuf>,
    updated_at: Option<String>,
}

fn claude_metadata(path: &Path) -> Result<ClaudeMetadata> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut metadata = ClaudeMetadata::default();
    for line in reader.lines().take(200) {
        let line = line.with_context(|| format!("reading {}", path.display()))?;
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if metadata.cwd.is_none()
            && let Some(cwd) = value.get("cwd").and_then(|value| value.as_str())
            && !cwd.trim().is_empty()
        {
            metadata.cwd = Some(PathBuf::from(cwd));
        }
        if metadata.updated_at.is_none()
            && let Some(timestamp) = value.get("timestamp").and_then(|value| value.as_str())
            && !timestamp.trim().is_empty()
        {
            metadata.updated_at = Some(timestamp.to_string());
        }
        if metadata.cwd.is_some() && metadata.updated_at.is_some() {
            break;
        }
    }
    Ok(metadata)
}

#[derive(Debug, Deserialize)]
struct CodexIndexRecord {
    id: String,
    #[serde(default)]
    thread_name: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
}

fn codex_home() -> PathBuf {
    env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".codex")))
        .unwrap_or_else(|| PathBuf::from(".codex"))
}

fn claude_home() -> PathBuf {
    env::var_os("CLAUDE_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".claude")))
        .unwrap_or_else(|| PathBuf::from(".claude"))
}

fn modified_time(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).ok()?.modified().ok()
}

fn format_system_time(time: SystemTime) -> Option<String> {
    OffsetDateTime::from(time).format(&Rfc3339).ok()
}

fn short_id(id: &str) -> String {
    short_ref(id)
}

fn claude_project_title(path: &Path) -> Option<String> {
    let project = path.parent()?.file_name()?.to_str()?;
    let parts = project
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if parts.len() >= 2 {
        return Some(format!(
            "{}-{}",
            parts[parts.len() - 2],
            parts[parts.len() - 1]
        ));
    }
    (!project.is_empty()).then(|| project.to_string())
}

fn compact_path(path: &Path, max_chars: usize) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| compact_text(name, max_chars))
        .unwrap_or_else(|| compact_text(&path.display().to_string(), max_chars))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn discovers_codex_and_claude_metadata_without_transcript_output() {
        let temp = tempfile::tempdir().unwrap();
        let codex_home = temp.path().join("codex");
        let claude_home = temp.path().join("claude");
        fs::create_dir_all(&codex_home).unwrap();
        fs::create_dir_all(claude_home.join("projects/project-a")).unwrap();

        fs::write(
            codex_home.join("session_index.jsonl"),
            "{\"id\":\"c-1\",\"thread_name\":\"fix tests\",\"updated_at\":\"2026-05-16T10:00:00Z\"}\n",
        )
        .unwrap();
        let claude_path = claude_home.join("projects/project-a/claude-1.jsonl");
        let mut claude_file = File::create(&claude_path).unwrap();
        writeln!(
            claude_file,
            "{{\"type\":\"user\",\"message\":{{\"content\":\"private prompt\"}},\"cwd\":\"/tmp/work\",\"timestamp\":\"2026-05-16T11:00:00Z\",\"sessionId\":\"claude-1\"}}"
        )
        .unwrap();

        let sessions = discover_local_sessions_in(&codex_home, &claude_home, 10).unwrap();

        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].agent, AgentKind::Claude);
        assert_eq!(sessions[0].id, "claude-1");
        assert_eq!(sessions[0].title.as_deref(), Some("work"));
        let rendered = render_sessions(&sessions);
        assert!(rendered.contains("/claude-resume claude-1 <prompt>"));
        assert!(rendered.contains("/codex-resume c-1 <prompt>"));
        assert!(rendered.contains("11:00Z | work"));
        assert!(!rendered.contains("private prompt"));
    }

    #[test]
    fn render_empty_sessions_is_helpful() {
        assert_eq!(
            render_sessions(&[]),
            "No local Codex or Claude sessions found."
        );
    }

    #[test]
    fn new_session_notice_is_metadata_only() {
        let session = LocalAgentSession {
            agent: AgentKind::Codex,
            id: "c-123456".to_string(),
            title: Some("private title".to_string()),
            cwd: Some(PathBuf::from("/tmp/work")),
            updated_at: Some("2026-05-17T12:00:00Z".to_string()),
            source_path: PathBuf::from("/tmp/index.jsonl"),
        };

        let rendered = render_new_session_notice(&[session]);

        assert!(rendered.contains("new local session"));
        assert!(rendered.contains("/codex-resume c-12345 <prompt>"));
        assert!(rendered.contains("metadata only"));
        assert!(!rendered.contains("private prompt"));
    }

    #[test]
    fn local_session_key_includes_agent_and_id() {
        let session = LocalAgentSession {
            agent: AgentKind::Claude,
            id: "abc".to_string(),
            title: None,
            cwd: None,
            updated_at: None,
            source_path: PathBuf::from("/tmp/abc.jsonl"),
        };

        assert_eq!(local_session_key(&session), "claude:abc");
    }
}
