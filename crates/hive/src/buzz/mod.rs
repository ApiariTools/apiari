//! Buzz — pluggable signal aggregator.
//!
//! Polls external sources (Sentry, GitHub, webhooks, reminders), normalizes
//! events into `Signal` structs, deduplicates them, and outputs JSONL.

pub mod config;
pub mod output;
pub mod reminder;
pub mod signal;
pub mod watcher;

use std::path::Path;

use color_eyre::Result;
use tokio::time::{Duration, sleep};

use self::config::BuzzConfig;
use self::output::OutputMode;
use self::reminder::Reminder;
use self::signal::{deduplicate, prioritize};
use self::watcher::{Watcher, create_watchers, save_cursors};

/// Run buzz with the given options.
///
/// - `config_path`: optional explicit path to config file (overrides workspace.yaml)
/// - `base`: workspace root directory for state persistence
/// - `daemon`: run continuously, polling on interval
/// - `once`: single poll then exit (also the default)
/// - `sweep`: run a single sweep (re-evaluate all unresolved issues) then exit
/// - `output_override`: CLI override for output mode (None = use config)
pub async fn run(
    config_path: Option<&Path>,
    base: &Path,
    daemon: bool,
    once: bool,
    sweep: bool,
    output_override: Option<&str>,
) -> Result<()> {
    let workspace = crate::workspace::load_workspace(base)?;
    let config = BuzzConfig::resolve(config_path, &workspace)?;

    let output_mode = if let Some(mode_str) = output_override
        && mode_str != "stdout"
    {
        OutputMode::from_config(
            mode_str,
            config.output.path.as_ref(),
            config.output.url.as_deref(),
        )?
    } else {
        OutputMode::from_config(
            &config.output.mode,
            config.output.path.as_ref(),
            config.output.url.as_deref(),
        )?
    };

    let mut watchers = create_watchers(&config, base);
    let mut reminders: Vec<Reminder> = config.reminders.iter().map(Reminder::from_config).collect();

    if sweep {
        // Sweep mode — run a single sweep across all watchers that support it.
        run_sweep(&mut watchers, &output_mode, base).await?;
    } else if once || !daemon {
        // Single poll mode (also the default when neither flag is given).
        run_once(&mut watchers, &mut reminders, &output_mode, base).await?;
    } else {
        // Daemon mode — poll on interval.
        let interval = Duration::from_secs(config.poll_interval_secs);
        tracing::info!(
            interval_secs = config.poll_interval_secs,
            watchers = watchers.len(),
            "daemon mode — polling"
        );

        loop {
            run_once(&mut watchers, &mut reminders, &output_mode, base).await?;
            sleep(interval).await;
        }
    }

    Ok(())
}

/// Execute a single sweep across all watchers that support it.
async fn run_sweep(
    watchers: &mut [Box<dyn Watcher>],
    output_mode: &OutputMode,
    base: &Path,
) -> Result<()> {
    let mut all_signals = Vec::new();

    for watcher in watchers.iter_mut() {
        if !watcher.has_sweep() {
            continue;
        }
        match watcher.sweep().await {
            Ok(signals) => {
                tracing::info!(
                    name = watcher.name(),
                    count = signals.len(),
                    "sweep returned signal(s)"
                );
                all_signals.extend(signals);
            }
            Err(e) => {
                tracing::error!(name = watcher.name(), error = %e, "sweep error");
            }
        }
    }

    // Deduplicate and prioritize.
    let mut signals = deduplicate(&all_signals);
    prioritize(&mut signals);

    // Emit.
    output::emit(&signals, output_mode)?;

    // Persist watcher state (cursors + seen_issues).
    save_cursors(watchers, base);

    Ok(())
}

/// Execute a single poll cycle across all watchers and reminders.
async fn run_once(
    watchers: &mut [Box<dyn Watcher>],
    reminders: &mut [Reminder],
    output_mode: &OutputMode,
    base: &Path,
) -> Result<()> {
    let mut all_signals = Vec::new();

    // Poll each watcher.
    for watcher in watchers.iter_mut() {
        match watcher.poll().await {
            Ok(signals) => {
                tracing::info!(
                    name = watcher.name(),
                    count = signals.len(),
                    "returned signal(s)"
                );
                all_signals.extend(signals);
            }
            Err(e) => {
                tracing::error!(name = watcher.name(), error = %e, "poll error");
            }
        }
    }

    // Check reminders.
    let reminder_signals = reminder::check_reminders(reminders);
    if !reminder_signals.is_empty() {
        tracing::info!(count = reminder_signals.len(), "reminder(s) fired");
        all_signals.extend(reminder_signals);
    }

    // Deduplicate and prioritize.
    let mut signals = deduplicate(&all_signals);
    prioritize(&mut signals);

    // Emit.
    output::emit(&signals, output_mode)?;

    // Persist watcher cursors so the next run picks up where we left off.
    save_cursors(watchers, base);

    Ok(())
}
