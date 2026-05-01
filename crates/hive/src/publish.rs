//! `hive publish` CLI command — specialty bots call this to post clean reports.
//!
//! Usage: hive publish --workspace mgm --bot Customer --report "markdown content"
//! Or:    hive publish --workspace mgm --bot Customer --file /tmp/report.md
//!
//! This writes directly to the DB, bypassing the streaming pipeline.
//! The proactive watcher tells bots about this command in their prompt.

use clap::Args;
use color_eyre::Result;

#[derive(Args)]
pub struct PublishArgs {
    /// Workspace name
    #[arg(long)]
    pub workspace: String,

    /// Bot name
    #[arg(long)]
    pub bot: String,

    /// Report content (inline)
    #[arg(long, conflicts_with = "file")]
    pub report: Option<String>,

    /// Report file path
    #[arg(long, conflicts_with = "report")]
    pub file: Option<String>,
}

pub fn run(args: PublishArgs, db_path: &std::path::Path) -> Result<()> {
    let content = match (args.report, args.file) {
        (Some(text), _) => text,
        (_, Some(path)) => std::fs::read_to_string(&path)?,
        _ => return Err(color_eyre::eyre::eyre!("provide --report or --file")),
    };

    let content = content.trim();
    if content.is_empty() {
        return Err(color_eyre::eyre::eyre!("empty report"));
    }

    let db = crate::db::Db::open(db_path)?;
    db.add_message(&args.workspace, &args.bot, "assistant", content, None)?;

    println!(
        "Published report for {}/{} ({} chars)",
        args.workspace,
        args.bot,
        content.len()
    );
    Ok(())
}
