#![cfg(unix)]
use agentnoise::config::{Config, RunnerLauncher};
use agentnoise::jobs::JobStore;
use agentnoise::runner::{AgentKind, AgentRequest, Runner};
use std::os::unix::fs::PermissionsExt;

#[test]
fn private_files_under_permissive_umask() {
    let Some(root) = std::env::var_os("AGENTNOISE_PERMISSION_TEST_ROOT") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let mut config = Config::template();
    config.runner.launcher = RunnerLauncher::Direct;
    config.runner.data_dir = root.join("data").display().to_string();
    config.runner.log_dir = root.join("logs").display().to_string();
    config.agents.codex.bin = "/usr/bin/true".to_string();
    config.repos[0].alias = "work".to_string();
    config.repos[0].path = root.display().to_string();
    let jobs = JobStore::open(&config.resolved_jobs_path(), &config.resolved_log_dir()).unwrap();
    let runner = Runner::new(config.clone(), jobs);
    let job = runner
        .run_blocking(AgentRequest::new(AgentKind::Codex, "work", "test"))
        .unwrap();
    // A second atomic save must retain restrictive mode too.
    let reopened =
        JobStore::open(&config.resolved_jobs_path(), &config.resolved_log_dir()).unwrap();
    reopened
        .create(&AgentRequest::new(AgentKind::Codex, "work", "second"))
        .unwrap();
    for path in [config.resolved_jobs_path(), job.log_path] {
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    for path in [root.join("data"), root.join("logs")] {
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
}

#[test]
fn run_with_permissive_umask_without_changing_test_process_umask() {
    let root = tempfile::tempdir().unwrap();
    let status = std::process::Command::new("/bin/sh")
        .args(["-c", "umask 022; exec \"$@\"", "permission-test"])
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", "private_files_under_permissive_umask"])
        .env("AGENTNOISE_PERMISSION_TEST_ROOT", root.path())
        // The only executable used by this fixture is /usr/bin/true.
        .env("AGENTNOISE_ALLOW_LAUNCHD_CODEX", "1")
        .status()
        .unwrap();
    assert!(status.success());
}
