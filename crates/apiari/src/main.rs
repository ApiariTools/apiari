mod buzz;
mod config;
mod daemon;
mod git_safety;
mod init;
mod ui;
mod validate_bash;

use clap::{Parser, Subcommand};
use color_eyre::eyre::Result;

#[derive(Parser)]
#[command(name = "apiari", about = "Unified CLI for apiari workspaces")]
struct Cli {
    #[command(subcommand)]
    command: Command,
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

    /// PreToolUse hook: validate Bash commands (used internally by coordinator)
    #[command(hide = true)]
    ValidateBash,
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "apiari=info,buzz=info".into()),
        )
        .with_target(false)
        .init();

    let cli = Cli::parse();

    match cli.command {
        Command::Init { name } => {
            init::run_init(name.as_deref())?;
        }
        Command::Daemon {
            command,
            background,
        } => match command {
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
        Command::Status { workspace } => {
            daemon::ensure_daemon()?;
            daemon::show_status(workspace.as_deref())?;
        }
        Command::Chat { workspace, message } => {
            daemon::ensure_daemon()?;
            daemon::run_chat(&workspace, message).await?;
        }
        Command::Ui { workspace } => {
            ui::run(workspace.as_deref()).await?;
        }
        Command::ValidateBash => {
            std::process::exit(validate_bash::run());
        }
    }

    Ok(())
}
