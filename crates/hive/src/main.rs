use clap::{Parser, Subcommand};
use color_eyre::Result;
use std::net::SocketAddr;
use std::path::PathBuf;
use tracing::info;

mod config_watcher;
mod db;
mod docs;
mod events;
mod followup;
mod init;
mod pr_feedback;
mod pr_review;
mod publish;
mod remote;
mod research;
mod review;
mod routes;
mod sentry_watcher;
mod setup;
mod simulator;
mod stt;
mod swarm;
mod tick;
mod tts;
mod usage;
mod watcher;

#[derive(Parser)]
#[command(name = "hive", about = "Workspace command hub")]
struct Cli {
    /// Port to serve on
    #[arg(long, default_value = "4200")]
    port: u16,

    /// Config directory (default: ~/.config/hive)
    #[arg(long)]
    config_dir: Option<std::path::PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Initialize a new workspace configuration
    Init {
        /// Workspace name (used for config filename and display)
        name: String,
        /// Root directory path for the workspace
        #[arg(long)]
        root: Option<String>,
    },
    /// Publish a report from a specialty bot
    Publish(publish::PublishArgs),
    /// Manage workspace reference docs
    Docs(docs::DocsArgs),
    /// Install voice dependencies (whisper STT + Kokoro TTS)
    Setup,
    /// Schedule, list, or cancel follow-ups
    Followup(followup::FollowupArgs),
    /// Interact with swarm workers via the daemon IPC
    Swarm {
        /// Workspace root directory where .swarm/ lives
        #[arg(long)]
        dir: Option<std::path::PathBuf>,

        #[command(subcommand)]
        command: swarm::SwarmCommand,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "hive=info".into()),
        )
        .init();

    // Strip sandbox GH_TOKEN if running inside Claude Code
    if std::env::var("CLAUDECODE").is_ok() {
        info!("stripping sandbox GH_TOKEN");
        unsafe {
            std::env::remove_var("GH_TOKEN");
        }
    }

    let cli = Cli::parse();

    let config_dir = cli
        .config_dir
        .clone()
        .unwrap_or_else(|| dirs::home_dir().unwrap().join(".config/hive"));
    std::fs::create_dir_all(&config_dir)?;

    // Handle subcommands before daemon startup
    if let Some(command) = cli.command {
        match command {
            Command::Init { name, root } => {
                return init::run(init::InitArgs { name, root }, &config_dir);
            }
            Command::Publish(args) => {
                let db_path = config_dir.join("hive.db");
                return publish::run(args, &db_path);
            }
            Command::Docs(args) => {
                return docs::run(args, &config_dir);
            }
            Command::Setup => {
                return setup::run();
            }
            Command::Followup(args) => {
                let db_path = config_dir.join("hive.db");
                return followup::run(args, &db_path);
            }
            Command::Swarm { dir, command } => {
                let work_dir = match dir {
                    Some(d) => d,
                    None => resolve_swarm_dir(&config_dir)?,
                };
                return swarm::run(work_dir, command, &config_dir).await;
            }
        }
    }

    let db_path = config_dir.join("hive.db");
    let db = db::Db::open(&db_path)?;
    research::ensure_schema(&db);
    review::ensure_schema(&db);

    // Build unified tick engine
    let watched_bots = load_watched_bots(&config_dir);
    let watched_workspaces = load_watched_workspaces(&config_dir);
    let pr_review_cache: pr_review::PrReviewCache = Default::default();
    let usage_cache: usage::UsageCache = Default::default();
    let ws_roots = load_workspace_roots(&config_dir);

    let mut engine = tick::TickEngine::new(15);

    if !watched_bots.is_empty() {
        info!("starting {} specialty bot watcher(s)", watched_bots.len());
        engine.add_watcher(Box::new(tick::SignalWatcher::new(watched_bots.clone())));

        // Sentry watcher — polls Sentry API for new issues
        sentry_watcher::ensure_schema(&db);
        engine.add_watcher(Box::new(sentry_watcher::SentryWatcher::new(
            watched_bots.clone(),
            db.clone(),
        )));

        engine.add_watcher(Box::new(tick::ScheduleWatcher::new(
            watched_bots,
            db.clone(),
        )));
    }

    if !watched_workspaces.is_empty() {
        info!(
            "[config-watcher] watching {} workspace(s) for prompt changes",
            watched_workspaces.len()
        );
        engine.add_watcher(Box::new(tick::ConfigChangeWatcher::new(watched_workspaces)));
    }

    if !ws_roots.is_empty() {
        info!(
            "starting PR review poller for {} workspace(s)",
            ws_roots.len()
        );
        engine.add_watcher(Box::new(tick::PrReviewWatcher::new(
            pr_review_cache.clone(),
            ws_roots.clone(),
        )));

        let hive_dir = config_dir.join(".hive");
        std::fs::create_dir_all(&hive_dir).ok();
        engine.add_watcher(Box::new(pr_feedback::PrFeedbackWatcher::new(
            ws_roots,
            hive_dir.join("pr_feedback.json"),
            3,
            pr_review_cache.clone(),
        )));
    }

    engine.add_watcher(Box::new(usage::UsageWatcher::new(usage_cache.clone())));

    // Follow-up watcher — checks for due follow-ups every tick
    followup::ensure_schema(&db);
    engine.add_watcher(Box::new(tick::FollowupWatcher::new(db.clone())));

    // Create event hub before starting tick engine so it can emit events
    let event_hub = events::EventHub::new();
    tokio::spawn(engine.run(db.clone(), Some(event_hub.clone())));

    // Auto-start TTS and STT servers if set up
    let _tts_child = tts::start_tts_server().await;
    let _stt_child = stt::start_stt_server().await;

    // Load remote hive instances and spawn discovery + WS bridge tasks
    let remote_entries = remote::load_remotes_config(&config_dir);
    let remote_registry = remote::new_registry();
    if !remote_entries.is_empty() {
        info!(
            "connecting to {} remote hive instance(s)",
            remote_entries.len()
        );
        remote::spawn_discovery(
            remote_registry.clone(),
            remote_entries,
            event_hub.clone(),
            reqwest::Client::new(),
        )
        .await;
    }

    let app = routes::router(
        db,
        &config_dir,
        event_hub,
        pr_review_cache,
        usage_cache,
        remote_registry,
    );

    let addr = SocketAddr::from(([0, 0, 0, 0], cli.port));
    info!("hive listening on http://{addr}");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to listen for ctrl+c");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to listen for SIGTERM")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    info!("shutting down");
}

/// Scan all workspace configs and collect bots with watch sources.
fn load_watched_bots(config_dir: &std::path::Path) -> Vec<watcher::WatchedBot> {
    let workspaces_dir = config_dir.join("workspaces");
    let mut watched = Vec::new();

    let entries = match std::fs::read_dir(&workspaces_dir) {
        Ok(e) => e,
        Err(_) => return watched,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "toml") {
            let workspace = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();

            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let config: toml::Value = match toml::from_str(&content) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let working_dir = config
                .get("workspace")
                .and_then(|w| w.get("root"))
                .and_then(|r| r.as_str())
                .map(PathBuf::from);

            let ws_response_style = config
                .get("workspace")
                .and_then(|w| w.get("response_style"))
                .and_then(|v| v.as_str())
                .map(String::from);

            if let Some(bots) = config.get("bots").and_then(|b| b.as_array()) {
                for bot in bots {
                    let name = bot
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("")
                        .to_string();
                    let watch: Vec<String> = bot
                        .get("watch")
                        .and_then(|w| w.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default();

                    let schedule = bot
                        .get("schedule")
                        .and_then(|s| s.as_str())
                        .map(String::from);
                    let schedule_hours = bot
                        .get("schedule_hours")
                        .and_then(|s| s.as_integer())
                        .map(|s| s as u64);
                    let proactive_prompt = bot
                        .get("proactive_prompt")
                        .and_then(|p| p.as_str())
                        .map(String::from);

                    let services: Vec<String> = bot
                        .get("services")
                        .and_then(|s| s.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default();

                    let has_watch = !watch.is_empty();
                    let has_schedule = (schedule.is_some() || schedule_hours.is_some())
                        && proactive_prompt.is_some();

                    // bot-level response_style > workspace-level
                    let response_style = bot
                        .get("response_style")
                        .and_then(|v| v.as_str())
                        .map(String::from)
                        .or_else(|| ws_response_style.clone());

                    if has_watch || has_schedule {
                        watched.push(watcher::WatchedBot {
                            workspace: workspace.clone(),
                            name,
                            provider: bot
                                .get("provider")
                                .and_then(|p| p.as_str())
                                .unwrap_or("claude")
                                .to_string(),
                            model: bot.get("model").and_then(|m| m.as_str()).map(String::from),
                            role: bot
                                .get("role")
                                .and_then(|r| r.as_str())
                                .unwrap_or("")
                                .to_string(),
                            watch,
                            working_dir: working_dir.clone(),
                            schedule,
                            schedule_hours,
                            proactive_prompt,
                            services,
                            response_style,
                        });
                    }
                }
            }
        }
    }

    watched
}

fn load_watched_workspaces(config_dir: &std::path::Path) -> Vec<config_watcher::WatchedWorkspace> {
    let workspaces_dir = config_dir.join("workspaces");
    let mut watched = Vec::new();

    let entries = match std::fs::read_dir(&workspaces_dir) {
        Ok(e) => e,
        Err(_) => return watched,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "toml") {
            let ws_name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();

            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let config: toml::Value = match toml::from_str(&content) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let root = config
                .get("workspace")
                .and_then(|w| w.get("root"))
                .and_then(|r| r.as_str())
                .map(PathBuf::from);

            // Collect bot names (always include Main)
            let mut bots = vec!["Main".to_string()];
            if let Some(bot_arr) = config.get("bots").and_then(|b| b.as_array()) {
                for bot in bot_arr {
                    if let Some(name) = bot.get("name").and_then(|n| n.as_str()) {
                        bots.push(name.to_string());
                    }
                }
            }

            watched.push(config_watcher::WatchedWorkspace {
                name: ws_name,
                config_path: path,
                root,
                bots,
            });
        }
    }

    watched
}

/// Parse all workspace TOML configs and return their root paths.
fn all_workspace_roots(config_dir: &std::path::Path) -> Vec<PathBuf> {
    let workspaces_dir = config_dir.join("workspaces");
    let mut roots = Vec::new();

    let entries = match std::fs::read_dir(&workspaces_dir) {
        Ok(e) => e,
        Err(_) => return roots,
    };

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
            let root_path = PathBuf::from(root);
            if !roots.contains(&root_path) {
                roots.push(root_path);
            }
        }
    }

    roots
}

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

/// Resolve the workspace root directory for `hive swarm` when `--dir` is not provided.
/// Finds the most specific workspace root that is a parent of the cwd.
/// Canonicalizes both paths to handle symlinks correctly.
/// Falls back to the current directory if no matching workspace is found.
fn resolve_swarm_dir(config_dir: &std::path::Path) -> Result<PathBuf> {
    let cwd = std::fs::canonicalize(std::env::current_dir()?)?;
    let mut best: Option<PathBuf> = None;

    for root in all_workspace_roots(config_dir) {
        if let Ok(canonical) = std::fs::canonicalize(&root)
            && cwd.starts_with(&canonical)
            && best
                .as_ref()
                .is_none_or(|b| canonical.as_os_str().len() > b.as_os_str().len())
        {
            best = Some(root);
        }
    }

    Ok(best.unwrap_or(std::env::current_dir()?))
}

fn load_workspace_roots(config_dir: &std::path::Path) -> Vec<PathBuf> {
    all_workspace_roots(config_dir)
        .into_iter()
        .filter(|r| r.join(".swarm").exists())
        .collect()
}

fn dirs_home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(std::path::PathBuf::from)
}

mod dirs {
    pub fn home_dir() -> Option<std::path::PathBuf> {
        super::dirs_home_dir()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Escape backslashes in a path for embedding in TOML strings (Windows compat).
    fn toml_escape_path(p: &std::path::Path) -> String {
        p.display().to_string().replace('\\', "\\\\")
    }

    #[test]
    fn test_find_default_agent_from_workspace_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let config_dir = tmp.path();
        let ws_dir = config_dir.join("workspaces");
        fs::create_dir_all(&ws_dir).unwrap();

        let work_dir = tmp.path().join("myproject");
        fs::create_dir_all(&work_dir).unwrap();

        let toml_content = format!(
            "[workspace]\nroot = \"{}\"\ndefault_agent = \"codex\"\n",
            toml_escape_path(&work_dir)
        );
        fs::write(ws_dir.join("test.toml"), toml_content).unwrap();

        let result = find_default_agent(config_dir, &work_dir);
        assert_eq!(result, Some("codex".to_string()));
    }

    #[test]
    fn test_find_default_agent_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let config_dir = tmp.path();
        let ws_dir = config_dir.join("workspaces");
        fs::create_dir_all(&ws_dir).unwrap();

        let work_dir = tmp.path().join("myproject");
        fs::create_dir_all(&work_dir).unwrap();

        let toml_content = format!("[workspace]\nroot = \"{}\"\n", toml_escape_path(&work_dir));
        fs::write(ws_dir.join("test.toml"), toml_content).unwrap();

        let result = find_default_agent(config_dir, &work_dir);
        assert_eq!(result, None);
    }

    #[test]
    fn test_find_default_agent_no_matching_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let config_dir = tmp.path();
        let ws_dir = config_dir.join("workspaces");
        fs::create_dir_all(&ws_dir).unwrap();

        let work_dir = tmp.path().join("myproject");
        fs::create_dir_all(&work_dir).unwrap();

        let other_dir = tmp.path().join("other");
        fs::create_dir_all(&other_dir).unwrap();

        let toml_content = format!(
            "[workspace]\nroot = \"{}\"\ndefault_agent = \"codex\"\n",
            toml_escape_path(&other_dir)
        );
        fs::write(ws_dir.join("test.toml"), toml_content).unwrap();

        let result = find_default_agent(config_dir, &work_dir);
        assert_eq!(result, None);
    }
}
