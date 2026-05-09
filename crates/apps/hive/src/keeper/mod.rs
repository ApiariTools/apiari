//! Keeper — read-only dashboard for monitoring swarm sessions.

pub mod discovery;
pub mod tui;

use color_eyre::Result;

/// Run the keeper dashboard.
///
/// - `once`: print status summary to stdout and exit (no TUI)
pub async fn run(once: bool) -> Result<()> {
    let result = discovery::discover_all()?;

    if once {
        print_status(&result);
        return Ok(());
    }

    tui::run(result).await
}

/// Print a one-shot status summary to stdout and exit.
fn print_status(result: &discovery::DiscoveryResult) {
    if result.sessions.is_empty() {
        println!("No active swarm sessions found.");
    } else {
        for session in &result.sessions {
            let alive = session.worktrees.iter().filter(|w| w.agent_alive).count();
            let total = session.worktrees.len();
            println!(
                "{} ({}) [{}/{}]",
                session.session_name,
                session.project_dir.display(),
                alive,
                total,
            );

            for wt in &session.worktrees {
                let agent_status = if wt.agent_alive { "running" } else { "done" };
                println!(
                    "  {} [{}] agent={} -- {}",
                    wt.branch, wt.agent_kind, agent_status, wt.prompt,
                );
                if let Some(ref summary) = wt.summary {
                    println!("    => {}", summary);
                }
                if let Some(ref pr) = wt.pr {
                    println!("    PR #{} {} -- {}", pr.number, pr.state, pr.url);
                }
            }

            println!();
        }
    }

    // Print buzz summary if available.
    if let Some(ref buzz) = result.buzz {
        println!(
            "Buzz signals: {} critical, {} warning, {} info",
            buzz.critical_count, buzz.warning_count, buzz.info_count,
        );
        for signal in buzz.signals.iter().take(5) {
            println!(
                "  [{}] {} -- {}",
                signal.severity, signal.source, signal.title,
            );
        }
    }
}
