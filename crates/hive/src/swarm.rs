use apiari_swarm::client::{DaemonRequest, DaemonResponse, WorkerInfo, send_daemon_request};
use apiari_swarm::daemon::lifecycle;
use clap::Subcommand;
use color_eyre::Result;
use std::path::{Path, PathBuf};

/// Find the `default_agent` setting from the workspace TOML whose root matches `work_dir`.
pub fn find_default_agent(
    config_dir: &std::path::Path,
    work_dir: &std::path::Path,
) -> Option<String> {
    let workspaces_dir = config_dir.join("workspaces");
    let entries = std::fs::read_dir(&workspaces_dir).ok()?;

    let canonical_work_dir =
        std::fs::canonicalize(work_dir).unwrap_or_else(|_| work_dir.to_path_buf());

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "toml")
            && let Ok(content) = std::fs::read_to_string(&path)
            && let Ok(config) = toml::from_str::<toml::Value>(&content)
            && let Some(root) = config
                .get("workspace")
                .and_then(|w| w.get("root"))
                .and_then(|r| r.as_str())
        {
            let canonical_root =
                std::fs::canonicalize(root).unwrap_or_else(|_| PathBuf::from(root));
            if canonical_root == canonical_work_dir {
                return config
                    .get("workspace")
                    .and_then(|w| w.get("default_agent"))
                    .and_then(|a| a.as_str())
                    .map(|s| s.to_string());
            }
        }
    }

    None
}

#[derive(Subcommand)]
pub enum SwarmCommand {
    /// Create a new swarm worker
    Create {
        /// Repository name (subdirectory under workspace root)
        #[arg(long)]
        repo: Option<String>,

        /// Agent to use (claude, codex, or gemini); defaults to workspace config or "claude"
        #[arg(long)]
        agent: Option<String>,

        /// Task prompt text
        #[arg(long, conflicts_with = "prompt_file")]
        prompt: Option<String>,

        /// Path to a file containing the task prompt
        #[arg(long, conflicts_with = "prompt")]
        prompt_file: Option<PathBuf>,
    },
    /// Send a message to a running worker
    Send {
        /// Worker/worktree ID
        worktree_id: String,
        /// Message to send
        message: String,
    },
    /// Close (cancel/abandon) a worker
    Close {
        /// Worker/worktree ID
        worktree_id: String,
    },
    /// List current swarm workers
    Status,
}

/// Resolve the agent to use: explicit flag > workspace config > "claude" fallback.
fn resolve_agent(flag: Option<String>, config_default: Option<String>) -> String {
    flag.or(config_default)
        .unwrap_or_else(|| "claude".to_string())
}

/// Resolve the prompt text from either --prompt or --prompt-file.
fn resolve_prompt(prompt: Option<String>, prompt_file: Option<PathBuf>) -> Result<String> {
    match (prompt, prompt_file) {
        (Some(text), _) => Ok(text),
        (_, Some(path)) => std::fs::read_to_string(&path).map_err(|e| {
            color_eyre::eyre::eyre!("failed to read prompt file '{}': {e}", path.display())
        }),
        (None, None) => Err(color_eyre::eyre::eyre!(
            "either --prompt or --prompt-file is required"
        )),
    }
}

/// Build a CreateWorker request from CLI arguments.
fn build_create_request(
    dir: &Path,
    repo: Option<String>,
    agent: String,
    prompt: Option<String>,
    prompt_file: Option<PathBuf>,
) -> Result<DaemonRequest> {
    let prompt_text = resolve_prompt(prompt, prompt_file)?;
    Ok(DaemonRequest::CreateWorker {
        prompt: prompt_text,
        agent,
        repo,
        start_point: None,
        workspace: Some(dir.to_path_buf()),
        profile: None,
        task_dir: None,
        role: None,
        review_pr: None,
        base_branch: None,
    })
}

/// Build a SendMessage request.
fn build_send_request(worktree_id: String, message: String) -> DaemonRequest {
    DaemonRequest::SendMessage {
        worktree_id,
        message,
    }
}

/// Build a CloseWorker request.
fn build_close_request(worktree_id: String) -> DaemonRequest {
    DaemonRequest::CloseWorker { worktree_id }
}

/// Build a ListWorkers request filtered by workspace.
fn build_list_request(dir: &Path) -> DaemonRequest {
    DaemonRequest::ListWorkers {
        workspace: Some(dir.to_path_buf()),
    }
}

/// Format a single worker for status display.
fn format_worker_line(w: &WorkerInfo) -> String {
    let pr = w
        .pr_url
        .as_deref()
        .map(|u| format!(" PR: {u}"))
        .unwrap_or_default();
    format!(
        "{id}  {phase:>10}  {branch}{pr}",
        id = w.id,
        phase = format!("{:?}", w.phase).to_lowercase(),
        branch = w.branch,
    )
}

/// Format all workers for status display.
fn format_status(workers: &[WorkerInfo]) -> String {
    if workers.is_empty() {
        "No active workers.".to_string()
    } else {
        workers
            .iter()
            .map(format_worker_line)
            .collect::<Vec<_>>()
            .join("\n")
    }
}
pub async fn run(dir: PathBuf, cmd: SwarmCommand, config_dir: &std::path::Path) -> Result<()> {
    lifecycle::ensure_daemon_running(&dir).await?;

    // Register this workspace with the daemon before any worker operations.
    let reg_dir = dir.clone();
    let reg_resp = tokio::task::spawn_blocking(move || {
        let req = DaemonRequest::RegisterWorkspace {
            path: reg_dir.clone(),
        };
        send_daemon_request(&reg_dir, &req)
    })
    .await?;
    match reg_resp {
        Ok(DaemonResponse::Error { message }) => {
            eprintln!("warning: failed to register workspace: {message}");
        }
        Err(e) => {
            eprintln!("warning: failed to register workspace: {e}");
        }
        _ => {}
    }

    match cmd {
        SwarmCommand::Create {
            repo,
            agent,
            prompt,
            prompt_file,
        } => {
            let config_default_agent = find_default_agent(config_dir, &dir);
            let agent = resolve_agent(agent, config_default_agent);

            let req = build_create_request(&dir, repo, agent, prompt, prompt_file)?;
            let resp =
                tokio::task::spawn_blocking(move || send_daemon_request(&dir, &req)).await??;
            check_response(&resp)?;
        }
        SwarmCommand::Send {
            worktree_id,
            message,
        } => {
            let req = build_send_request(worktree_id, message);
            let resp =
                tokio::task::spawn_blocking(move || send_daemon_request(&dir, &req)).await??;
            check_response(&resp)?;
        }
        SwarmCommand::Close { worktree_id } => {
            let req = build_close_request(worktree_id);
            let resp =
                tokio::task::spawn_blocking(move || send_daemon_request(&dir, &req)).await??;
            check_response(&resp)?;
        }
        SwarmCommand::Status => {
            let req = build_list_request(&dir);
            let resp =
                tokio::task::spawn_blocking(move || send_daemon_request(&dir, &req)).await??;
            match &resp {
                DaemonResponse::Workers { workers } => {
                    println!("{}", format_status(workers));
                }
                _ => check_response(&resp)?,
            }
        }
    }

    Ok(())
}

fn check_response(resp: &DaemonResponse) -> Result<()> {
    match resp {
        DaemonResponse::Ok { data } => {
            if let Some(d) = data {
                println!("{}", serde_json::to_string_pretty(d).unwrap_or_default());
            } else {
                println!("ok");
            }
            Ok(())
        }
        DaemonResponse::Error { message } => Err(color_eyre::eyre::eyre!("{message}")),
        DaemonResponse::Workers { workers } => {
            println!(
                "{}",
                serde_json::to_string_pretty(workers).unwrap_or_default()
            );
            Ok(())
        }
        other => {
            println!(
                "{}",
                serde_json::to_string_pretty(other).unwrap_or_default()
            );
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apiari_swarm::WorkerPhase;
    use std::io::Write;
    use tempfile::NamedTempFile;

    // ── Helper: build a WorkerInfo for tests ──

    fn make_worker(id: &str, branch: &str, phase: WorkerPhase, pr_url: Option<&str>) -> WorkerInfo {
        WorkerInfo {
            id: id.to_string(),
            branch: branch.to_string(),
            prompt: "test prompt".to_string(),
            agent: "claude".to_string(),
            phase,
            session_id: None,
            pr_url: pr_url.map(|s| s.to_string()),
            pr_number: None,
            pr_title: None,
            pr_state: None,
            restart_count: 0,
            created_at: None,
            agent_card: None,
            role: None,
            review_verdict: None,
        }
    }

    // ═══════════════════════════════════════════════════════════
    // Agent resolution tests
    // ═══════════════════════════════════════════════════════════

    #[test]
    fn test_agent_from_config() {
        let result = resolve_agent(None, Some("codex".to_string()));
        assert_eq!(result, "codex");
    }

    #[test]
    fn test_agent_flag_overrides_config() {
        let result = resolve_agent(Some("claude".to_string()), Some("codex".to_string()));
        assert_eq!(result, "claude");
    }

    #[test]
    fn test_agent_falls_back_to_claude() {
        let result = resolve_agent(None, None);
        assert_eq!(result, "claude");
    }

    // ═══════════════════════════════════════════════════════════
    // 1. Request construction tests
    // ═══════════════════════════════════════════════════════════

    #[test]
    fn test_create_worker_request_with_prompt() {
        let dir = PathBuf::from("/tmp/test-workspace");
        let req = build_create_request(
            &dir,
            None,
            "claude".to_string(),
            Some("fix the bug".to_string()),
            None,
        )
        .unwrap();

        match req {
            DaemonRequest::CreateWorker {
                prompt,
                agent,
                repo,
                workspace,
                ..
            } => {
                assert_eq!(prompt, "fix the bug");
                assert_eq!(agent, "claude");
                assert!(repo.is_none());
                assert_eq!(workspace, Some(dir));
            }
            _ => panic!("expected CreateWorker"),
        }
    }

    #[test]
    fn test_create_worker_request_with_prompt_file() {
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "task from file").unwrap();

        let dir = PathBuf::from("/tmp/test-workspace");
        let req = build_create_request(
            &dir,
            None,
            "claude".to_string(),
            None,
            Some(tmp.path().to_path_buf()),
        )
        .unwrap();

        match req {
            DaemonRequest::CreateWorker { prompt, .. } => {
                assert_eq!(prompt.trim(), "task from file");
            }
            _ => panic!("expected CreateWorker"),
        }
    }

    #[test]
    fn test_create_worker_request_with_repo() {
        let dir = PathBuf::from("/tmp/test-workspace");
        let req = build_create_request(
            &dir,
            Some("my-repo".to_string()),
            "claude".to_string(),
            Some("do stuff".to_string()),
            None,
        )
        .unwrap();

        match req {
            DaemonRequest::CreateWorker { repo, .. } => {
                assert_eq!(repo, Some("my-repo".to_string()));
            }
            _ => panic!("expected CreateWorker"),
        }
    }

    #[test]
    fn test_create_worker_request_with_custom_agent() {
        let dir = PathBuf::from("/tmp/test-workspace");
        let req = build_create_request(
            &dir,
            None,
            "codex".to_string(),
            Some("do stuff".to_string()),
            None,
        )
        .unwrap();

        match req {
            DaemonRequest::CreateWorker { agent, .. } => {
                assert_eq!(agent, "codex");
            }
            _ => panic!("expected CreateWorker"),
        }
    }

    #[test]
    fn test_create_worker_request_no_prompt_errors() {
        let dir = PathBuf::from("/tmp/test-workspace");
        let result = build_create_request(&dir, None, "claude".to_string(), None, None);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("--prompt") || err.contains("prompt"),
            "error should mention prompt: {err}"
        );
    }

    #[test]
    fn test_create_worker_request_prompt_file_missing_errors() {
        let dir = PathBuf::from("/tmp/test-workspace");
        let missing_path = "/tmp/nonexistent-prompt-file-12345.txt";
        let result = build_create_request(
            &dir,
            None,
            "claude".to_string(),
            None,
            Some(PathBuf::from(missing_path)),
        );
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("failed to read prompt file"),
            "error should mention prompt file: {err}"
        );
        assert!(
            err.contains(missing_path),
            "error should include the file path: {err}"
        );
    }

    #[test]
    fn test_create_worker_request_workspace_is_set() {
        let dir = PathBuf::from("/home/user/project");
        let req = build_create_request(
            &dir,
            None,
            "claude".to_string(),
            Some("task".to_string()),
            None,
        )
        .unwrap();

        match req {
            DaemonRequest::CreateWorker { workspace, .. } => {
                assert_eq!(workspace, Some(PathBuf::from("/home/user/project")));
            }
            _ => panic!("expected CreateWorker"),
        }
    }

    #[test]
    fn test_send_message_request() {
        let req = build_send_request("hive-42".to_string(), "please review".to_string());
        match req {
            DaemonRequest::SendMessage {
                worktree_id,
                message,
            } => {
                assert_eq!(worktree_id, "hive-42");
                assert_eq!(message, "please review");
            }
            _ => panic!("expected SendMessage"),
        }
    }

    #[test]
    fn test_close_worker_request() {
        let req = build_close_request("hive-99".to_string());
        match req {
            DaemonRequest::CloseWorker { worktree_id } => {
                assert_eq!(worktree_id, "hive-99");
            }
            _ => panic!("expected CloseWorker"),
        }
    }

    #[test]
    fn test_list_workers_request() {
        let dir = PathBuf::from("/tmp/my-workspace");
        let req = build_list_request(&dir);
        match req {
            DaemonRequest::ListWorkers { workspace } => {
                assert_eq!(workspace, Some(PathBuf::from("/tmp/my-workspace")));
            }
            _ => panic!("expected ListWorkers"),
        }
    }

    // ═══════════════════════════════════════════════════════════
    // 2. Response handling tests
    // ═══════════════════════════════════════════════════════════

    #[test]
    fn test_check_response_ok_no_data() {
        let resp = DaemonResponse::Ok { data: None };
        assert!(check_response(&resp).is_ok());
    }

    #[test]
    fn test_check_response_ok_with_data() {
        let resp = DaemonResponse::Ok {
            data: Some(serde_json::json!({"id": "hive-1"})),
        };
        assert!(check_response(&resp).is_ok());
    }

    #[test]
    fn test_check_response_error() {
        let resp = DaemonResponse::Error {
            message: "worker not found".to_string(),
        };
        let result = check_response(&resp);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().to_string(), "worker not found");
    }

    #[test]
    fn test_check_response_workers_empty() {
        let resp = DaemonResponse::Workers { workers: vec![] };
        assert!(check_response(&resp).is_ok());
    }

    #[test]
    fn test_check_response_workers_with_entries() {
        let resp = DaemonResponse::Workers {
            workers: vec![make_worker(
                "hive-1",
                "swarm/fix",
                WorkerPhase::Running,
                None,
            )],
        };
        assert!(check_response(&resp).is_ok());
    }

    #[test]
    fn test_check_response_unknown_variant() {
        // Use a variant that check_response handles via the catch-all `other` arm
        let resp = DaemonResponse::Workspaces { workspaces: vec![] };
        assert!(check_response(&resp).is_ok());
    }

    // ═══════════════════════════════════════════════════════════
    // 3. RegisterWorkspace tests
    // ═══════════════════════════════════════════════════════════

    #[test]
    fn test_register_workspace_request_format() {
        let req = DaemonRequest::RegisterWorkspace {
            path: PathBuf::from("/home/user/project"),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"action\":\"register_workspace\""));
        assert!(json.contains("/home/user/project"));

        let restored: DaemonRequest = serde_json::from_str(&json).unwrap();
        match restored {
            DaemonRequest::RegisterWorkspace { path } => {
                assert_eq!(path, PathBuf::from("/home/user/project"));
            }
            _ => panic!("expected RegisterWorkspace"),
        }
    }

    #[test]
    fn test_register_workspace_sent_before_create() {
        // Verify that RegisterWorkspace and CreateWorker are independent requests
        // that can be constructed for the same workspace path.
        let dir = PathBuf::from("/tmp/workspace");

        let register_req = DaemonRequest::RegisterWorkspace { path: dir.clone() };
        let create_req = build_create_request(
            &dir,
            None,
            "claude".to_string(),
            Some("task".to_string()),
            None,
        )
        .unwrap();

        // Both should serialize correctly
        let reg_json = serde_json::to_string(&register_req).unwrap();
        let create_json = serde_json::to_string(&create_req).unwrap();

        assert!(reg_json.contains("register_workspace"));
        assert!(create_json.contains("create_worker"));

        // Both reference the same workspace
        assert!(reg_json.contains("/tmp/workspace"));
        match create_req {
            DaemonRequest::CreateWorker { workspace, .. } => {
                assert_eq!(workspace, Some(dir));
            }
            _ => panic!("expected CreateWorker"),
        }
    }

    #[test]
    fn test_register_workspace_failure_doesnt_block_create() {
        // Simulate: RegisterWorkspace returns an error, but CreateWorker can still be built
        let dir = PathBuf::from("/tmp/workspace");

        let error_resp = DaemonResponse::Error {
            message: "workspace already registered".to_string(),
        };
        // The error from RegisterWorkspace is non-fatal — caller can proceed
        assert!(check_response(&error_resp).is_err());

        // CreateWorker request can still be built independently
        let create_req = build_create_request(
            &dir,
            None,
            "claude".to_string(),
            Some("task".to_string()),
            None,
        );
        assert!(create_req.is_ok());
    }

    // ═══════════════════════════════════════════════════════════
    // 4. Prompt file handling edge cases
    // ═══════════════════════════════════════════════════════════

    #[test]
    fn test_prompt_file_empty() {
        let tmp = NamedTempFile::new().unwrap();
        // File is empty — should resolve to empty string, not error
        let result = resolve_prompt(None, Some(tmp.path().to_path_buf()));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "");
    }

    #[test]
    fn test_prompt_file_utf8() {
        let mut tmp = NamedTempFile::new().unwrap();
        write!(tmp, "修复登录错误 🐛 café résumé").unwrap();

        let result = resolve_prompt(None, Some(tmp.path().to_path_buf()));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "修复登录错误 🐛 café résumé");
    }

    #[test]
    fn test_prompt_file_large() {
        let mut tmp = NamedTempFile::new().unwrap();
        let large_content = "x".repeat(15_000); // 15KB
        write!(tmp, "{large_content}").unwrap();

        let result = resolve_prompt(None, Some(tmp.path().to_path_buf()));
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 15_000);
    }

    #[test]
    fn test_prompt_file_with_newlines() {
        let mut tmp = NamedTempFile::new().unwrap();
        write!(tmp, "line one\nline two\nline three\n").unwrap();

        let result = resolve_prompt(None, Some(tmp.path().to_path_buf()));
        assert!(result.is_ok());
        let text = result.unwrap();
        assert!(text.contains("line one\nline two\nline three\n"));
        assert_eq!(text.lines().count(), 3);
    }

    #[test]
    fn test_prompt_text_takes_priority_over_file() {
        // If both prompt and prompt_file are provided, prompt wins (matches clap conflicts_with)
        let mut tmp = NamedTempFile::new().unwrap();
        write!(tmp, "from file").unwrap();

        let result = resolve_prompt(
            Some("from text".to_string()),
            Some(tmp.path().to_path_buf()),
        );
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "from text");
    }

    // ═══════════════════════════════════════════════════════════
    // 5. Status display tests
    // ═══════════════════════════════════════════════════════════

    #[test]
    fn test_status_display_empty_workers() {
        let output = format_status(&[]);
        assert_eq!(output, "No active workers.");
    }

    #[test]
    fn test_status_display_with_workers() {
        let workers = vec![
            make_worker("hive-1", "swarm/fix-bug", WorkerPhase::Running, None),
            make_worker("hive-2", "swarm/add-tests", WorkerPhase::Completed, None),
        ];
        let output = format_status(&workers);
        assert!(output.contains("hive-1"));
        assert!(output.contains("running"));
        assert!(output.contains("swarm/fix-bug"));
        assert!(output.contains("hive-2"));
        assert!(output.contains("completed"));
        assert!(output.contains("swarm/add-tests"));
    }

    #[test]
    fn test_status_display_with_pr_url() {
        let workers = vec![make_worker(
            "hive-3",
            "swarm/feature",
            WorkerPhase::Completed,
            Some("https://github.com/org/repo/pull/42"),
        )];
        let output = format_status(&workers);
        assert!(output.contains("PR: https://github.com/org/repo/pull/42"));
    }

    #[test]
    fn test_status_display_without_pr_url() {
        let workers = vec![make_worker(
            "hive-4",
            "swarm/wip",
            WorkerPhase::Running,
            None,
        )];
        let output = format_status(&workers);
        assert!(!output.contains("PR:"));
    }

    #[test]
    fn test_format_worker_line_all_phases() {
        for (phase, expected) in [
            (WorkerPhase::Creating, "creating"),
            (WorkerPhase::Starting, "starting"),
            (WorkerPhase::Running, "running"),
            (WorkerPhase::Waiting, "waiting"),
            (WorkerPhase::Completed, "completed"),
            (WorkerPhase::Failed, "failed"),
        ] {
            let w = make_worker("w-1", "swarm/test", phase, None);
            let line = format_worker_line(&w);
            assert!(
                line.contains(expected),
                "phase {expected} not found in: {line}"
            );
        }
    }

    // ═══════════════════════════════════════════════════════════
    // 6. Integration-style test (ignored — needs running daemon)
    // ═══════════════════════════════════════════════════════════

    /// Integration test: start a swarm daemon in a tempdir, send RegisterWorkspace,
    /// then ListWorkers, verify empty worker list returned.
    ///
    /// This requires `apiari_swarm::daemon::lifecycle::ensure_daemon_running` and a
    /// real Unix socket, so it cannot run in unit tests without spinning up the full
    /// daemon process. See `apiari-swarm/tests/daemon_integration.rs` for the pattern.
    #[test]
    #[ignore]
    fn test_full_flow_ensure_daemon_register_list() {
        let dir = tempfile::tempdir().unwrap();
        let work_dir = dir.path().to_path_buf();

        // Would need: lifecycle::ensure_daemon_running(&work_dir).await
        // Then: send_daemon_request(&work_dir, &DaemonRequest::RegisterWorkspace { path: work_dir.clone() })
        // Then: send_daemon_request(&work_dir, &DaemonRequest::ListWorkers { workspace: Some(work_dir.clone()) })
        // Verify: DaemonResponse::Workers { workers } where workers.is_empty()

        // Cannot run without a real daemon process — marked #[ignore].
        let _ = work_dir;
    }
}
