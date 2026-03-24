mod buzz;
mod config;
mod config_set;
mod daemon;
mod git_safety;
mod mcp_swarm;
pub mod shells;
mod ui;
mod validate_bash;

use clap::{CommandFactory, Parser, Subcommand};
use color_eyre::eyre::Result;
use std::fs;
use tracing_subscriber::fmt::writer::BoxMakeWriter;

#[derive(Parser)]
#[command(name = "apiari", about = "Unified CLI for apiari workspaces")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum DaemonCommand {
    /// Start the daemon (background by default)
    Start {
        /// Run in foreground (for debugging)
        #[arg(long)]
        foreground: bool,
    },
    /// Stop the running daemon
    Stop,
    /// Restart the daemon
    Restart {
        /// Run in foreground (for debugging)
        #[arg(long)]
        foreground: bool,
    },
}

#[derive(Subcommand)]
enum Command {
    /// Initialize a workspace config from the current directory
    #[command(hide = true)]
    Init {
        /// Override the workspace name (default: directory name)
        #[arg(long)]
        name: Option<String>,
    },

    /// Manage the daemon (watches all workspaces)
    Daemon {
        #[command(subcommand)]
        command: Option<DaemonCommand>,

        /// Deprecated: use `apiari daemon start` instead
        #[arg(long, hide = true)]
        background: bool,
    },

    /// Show open signals
    Status {
        /// Show signals for a specific workspace only
        workspace: Option<String>,
    },

    /// Chat with a workspace's coordinator
    Chat {
        /// Workspace name
        workspace: String,

        /// Message to send (omit for interactive mode)
        message: Option<String>,
    },

    /// Launch the TUI dashboard
    Ui {
        /// Focus a specific workspace tab
        #[arg(long)]
        workspace: Option<String>,
    },

    /// Read or update workspace configuration
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },

    /// PreToolUse hook: validate Bash commands (used internally by coordinator)
    #[command(hide = true)]
    ValidateBash,

    /// MCP server for swarm worker management (used internally by coordinator)
    #[command(hide = true)]
    McpSwarm {
        /// Workspace root directory
        #[arg(long)]
        workspace_root: std::path::PathBuf,
    },
}

#[derive(Subcommand)]
enum ConfigCommand {
    /// Set a config value (dot-separated key path)
    Set {
        /// Dot-separated key (e.g. telegram.bot_token, watchers.github.interval_secs)
        key: String,

        /// Value to set (auto-detects type: integer, boolean, or string)
        value: String,

        /// Workspace name (default: auto-detect from current directory)
        #[arg(long)]
        workspace: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse();

    // TUI uses stdout for rendering (crossterm alternate screen), so tracing
    // must not write to stdout OR stderr (both point to the same TTY).
    // Route tracing to a log file for TUI paths, stderr for everything else.
    let is_tui = matches!(
        cli.command,
        None | Some(Command::Ui { .. }) | Some(Command::Init { .. })
    );
    let writer: BoxMakeWriter = if is_tui {
        let log_dir = config::config_dir();
        fs::create_dir_all(&log_dir)?;
        let file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_dir.join("tui.log"))?;
        BoxMakeWriter::new(file)
    } else {
        BoxMakeWriter::new(std::io::stderr)
    };

    tracing_subscriber::fmt()
        .with_writer(writer)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "apiari=info,buzz=info".into()),
        )
        .with_target(false)
        .init();

    match cli.command {
        None => {
            if std::io::IsTerminal::is_terminal(&std::io::stdout()) {
                ui::run(None, None, None).await?;
            } else {
                Cli::command().print_help()?;
            }
        }
        Some(Command::Init { name }) => {
            // Launch TUI directly in add-workspace mode for cwd
            if std::io::IsTerminal::is_terminal(&std::io::stdout())
                && std::io::IsTerminal::is_terminal(&std::io::stdin())
            {
                let cwd = std::env::current_dir()?;
                ui::run(None, Some(cwd), name.as_deref()).await?;
            } else {
                eprintln!("error: `apiari init` requires an interactive terminal");
                std::process::exit(1);
            }
        }
        Some(Command::Daemon {
            command,
            background,
        }) => match command {
            Some(DaemonCommand::Start { foreground }) => {
                if foreground {
                    daemon::run_foreground().await?;
                } else {
                    daemon::spawn_background()?;
                }
            }
            Some(DaemonCommand::Stop) => {
                daemon::stop_daemon()?;
            }
            Some(DaemonCommand::Restart { foreground }) => {
                daemon::stop_daemon()?;
                if foreground {
                    daemon::run_foreground().await?;
                } else {
                    daemon::spawn_background()?;
                }
            }
            None => {
                if background {
                    eprintln!(
                        "Note: `--background` is deprecated. Use `apiari daemon start` instead."
                    );
                    daemon::spawn_background()?;
                } else {
                    daemon::run_foreground().await?;
                }
            }
        },
        Some(Command::Status { workspace }) => {
            daemon::ensure_daemon()?;
            daemon::show_status(workspace.as_deref())?;
        }
        Some(Command::Chat { workspace, message }) => {
            daemon::ensure_daemon()?;
            daemon::run_chat(&workspace, message).await?;
        }
        Some(Command::Ui { workspace }) => {
            ui::run(workspace.as_deref(), None, None).await?;
        }
        Some(Command::Config { command }) => match command {
            ConfigCommand::Set {
                key,
                value,
                workspace,
            } => {
                config_set::run(workspace.as_deref(), &key, &value)?;
            }
        },
        Some(Command::ValidateBash) => {
            std::process::exit(validate_bash::run());
        }
        Some(Command::McpSwarm { workspace_root }) => {
            std::process::exit(mcp_swarm::run(workspace_root));
        }
    }

    Ok(())
}
