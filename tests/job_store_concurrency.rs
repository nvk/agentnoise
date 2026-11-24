use agentnoise::jobs::{JobStatus, JobStore};
use agentnoise::runner::{AgentKind, AgentRequest};

#[test]
fn independent_stores_preserve_jobs_and_observe_updates() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jobs.json");
    let logs = dir.path().join("logs");
    let a = JobStore::open(&path, &logs).unwrap();
    let b = JobStore::open(&path, &logs).unwrap();
    let first = a
        .create(&AgentRequest::new(AgentKind::Codex, "work", "first"))
        .unwrap();
    let second = b
        .create(&AgentRequest::new(AgentKind::Codex, "work", "second"))
        .unwrap();
    assert!(a.get(&second.id).is_some());
    assert!(b.get(&first.id).is_some());
    b.mark_running(&first.id, 1234).unwrap();
    assert_eq!(a.get(&first.id).unwrap().status, JobStatus::Running);
    assert_eq!(a.request_cancel(&first.id).unwrap(), Some(1234));
    assert_eq!(b.get(&first.id).unwrap().status, JobStatus::CancelRequested);
    assert_eq!(a.recent(10).len(), 2);
}

#[test]
fn subprocess_writer() {
    let Some(path) = std::env::var_os("AGENTNOISE_TEST_JOB_STORE") else {
        return;
    };
    let path = std::path::PathBuf::from(path);
    let store = JobStore::open(&path, &path.parent().unwrap().join("logs")).unwrap();
    for _ in 0..20 {
        store
            .create(&AgentRequest::new(AgentKind::Codex, "work", "test"))
            .unwrap();
    }
}

#[test]
fn concurrent_processes_do_not_lose_jobs() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jobs.json");
    let mut children = Vec::new();
    for _ in 0..4 {
        children.push(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "subprocess_writer"])
                .env("AGENTNOISE_TEST_JOB_STORE", &path)
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        );
    }
    for mut child in children {
        assert!(child.wait().unwrap().success());
    }
    let store = JobStore::open(&path, &dir.path().join("logs")).unwrap();
    assert_eq!(store.recent(100).len(), 80);
}

#[test]
fn corrupt_store_is_not_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jobs.json");
    std::fs::write(&path, "broken json").unwrap();
    assert!(JobStore::open(&path, &dir.path().join("logs")).is_err());
    assert_eq!(std::fs::read_to_string(path).unwrap(), "broken json");
}
