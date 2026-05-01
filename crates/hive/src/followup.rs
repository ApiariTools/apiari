//! Follow-up scheduler — bots schedule future check-ins that fire automatically.
//!
//! Usage: hive followup schedule --workspace ws --bot Bot --delay 5m --action "Check PR status"
//! Or:    hive followup list --workspace ws
//! Or:    hive followup cancel --id <followup-id>
//!
//! Delays: 30s, 5m, 1h, 2h30m — parsed into seconds and stored as fires_at timestamp.

use clap::{Args, Subcommand};
use color_eyre::Result;
use serde::{Deserialize, Serialize};

use crate::db::Db;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Followup {
    pub id: String,
    pub workspace: String,
    pub bot: String,
    pub action: String,
    pub created_at: String,
    pub fires_at: String,
    pub status: String, // "pending", "fired", "cancelled"
}

const FOLLOWUP_SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS followups (
        id TEXT PRIMARY KEY,
        workspace TEXT NOT NULL,
        bot TEXT NOT NULL,
        action TEXT NOT NULL,
        created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
        fires_at TEXT NOT NULL,
        status TEXT NOT NULL DEFAULT 'pending'
    );
";

/// Ensure the followups table exists.
pub fn ensure_schema(db: &Db) {
    db.execute_batch(FOLLOWUP_SCHEMA).ok();
}

/// Parse a delay string like "30s", "5m", "1h", "2h30m" into seconds.
pub fn parse_delay(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty delay string".to_string());
    }

    let mut total_secs: u64 = 0;
    let mut current_num = String::new();

    for c in s.chars() {
        if c.is_ascii_digit() {
            current_num.push(c);
        } else {
            if current_num.is_empty() {
                return Err(format!("unexpected '{c}' without a number"));
            }
            let n: u64 = current_num
                .parse()
                .map_err(|_| format!("invalid number: {current_num}"))?;
            current_num.clear();
            match c {
                's' => total_secs += n,
                'm' => total_secs += n * 60,
                'h' => total_secs += n * 3600,
                _ => return Err(format!("unknown unit '{c}', expected s/m/h")),
            }
        }
    }

    if !current_num.is_empty() {
        return Err(format!("trailing number without unit: {current_num}"));
    }

    if total_secs == 0 {
        return Err("delay must be greater than 0".to_string());
    }

    Ok(total_secs)
}

/// Insert a new follow-up into the DB. Returns the generated ID and fires_at timestamp.
pub fn schedule(
    db: &Db,
    workspace: &str,
    bot: &str,
    delay_secs: u64,
    action: &str,
) -> Result<Followup> {
    let id = format!("fu_{}", &generate_id()[..12]);
    let now = chrono::Utc::now();
    let fires_at = now + chrono::Duration::seconds(delay_secs as i64);
    let created_at = now.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let fires_at_str = fires_at.format("%Y-%m-%dT%H:%M:%SZ").to_string();

    db.execute_sql(
        "INSERT INTO followups (id, workspace, bot, action, created_at, fires_at, status) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending')",
        &[&id, workspace, bot, action, &created_at, &fires_at_str],
    )?;

    Ok(Followup {
        id,
        workspace: workspace.to_string(),
        bot: bot.to_string(),
        action: action.to_string(),
        created_at,
        fires_at: fires_at_str,
        status: "pending".to_string(),
    })
}

/// Query pending follow-ups that are due (fires_at <= now).
pub fn query_due(db: &Db) -> Vec<Followup> {
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    // Use the generic query approach via reader
    let conn = match db.reader() {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let mut stmt = match conn.prepare(
        "SELECT id, workspace, bot, action, created_at, fires_at, status FROM followups WHERE status = 'pending' AND fires_at <= ?1",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    stmt.query_map(rusqlite::params![now], |row| {
        Ok(Followup {
            id: row.get(0)?,
            workspace: row.get(1)?,
            bot: row.get(2)?,
            action: row.get(3)?,
            created_at: row.get(4)?,
            fires_at: row.get(5)?,
            status: row.get(6)?,
        })
    })
    .map(|rows| rows.filter_map(|r| r.ok()).collect())
    .unwrap_or_default()
}

/// Query all follow-ups for a workspace (any status).
pub fn query_workspace(db: &Db, workspace: &str) -> Vec<Followup> {
    let conn = match db.reader() {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let mut stmt = match conn.prepare(
        "SELECT id, workspace, bot, action, created_at, fires_at, status FROM followups WHERE workspace = ?1 ORDER BY fires_at ASC",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    stmt.query_map(rusqlite::params![workspace], |row| {
        Ok(Followup {
            id: row.get(0)?,
            workspace: row.get(1)?,
            bot: row.get(2)?,
            action: row.get(3)?,
            created_at: row.get(4)?,
            fires_at: row.get(5)?,
            status: row.get(6)?,
        })
    })
    .map(|rows| rows.filter_map(|r| r.ok()).collect())
    .unwrap_or_default()
}

/// Mark a follow-up as fired, but only if it's still pending.
/// Returns true if the status was changed, false if it was already cancelled/fired.
pub fn mark_fired_if_pending(db: &Db, id: &str) -> Result<bool> {
    // Check current status first
    let conn = db.reader()?;
    let status: Option<String> = conn
        .query_row(
            "SELECT status FROM followups WHERE id = ?1",
            rusqlite::params![id],
            |row| row.get(0),
        )
        .ok();
    drop(conn);

    match status.as_deref() {
        Some("pending") => {
            db.execute_sql(
                "UPDATE followups SET status = 'fired' WHERE id = ?1 AND status = 'pending'",
                &[id],
            )?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// Cancel a follow-up. Validates that it belongs to the given workspace.
/// Returns true if a pending follow-up was cancelled.
pub fn cancel(db: &Db, id: &str, workspace: &str) -> Result<bool> {
    // Check current status and workspace ownership
    let conn = db.reader()?;
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT status, workspace FROM followups WHERE id = ?1",
            rusqlite::params![id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ok();
    drop(conn);

    match row {
        Some((status, ws)) if status == "pending" && ws == workspace => {
            db.execute_sql(
                "UPDATE followups SET status = 'cancelled' WHERE id = ?1 AND status = 'pending'",
                &[id],
            )?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// Generate a short unique ID from timestamp + pid + counter.
/// Not a true UUID — just needs to be unique within this process.
fn generate_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    let pid = std::process::id();
    format!("{:08x}{:04x}{:04x}", nanos, pid & 0xFFFF, rand_u16())
}

fn rand_u16() -> u16 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    (t & 0xFFFF) as u16
}

// ── CLI subcommands ──

#[derive(Args)]
pub struct FollowupArgs {
    #[command(subcommand)]
    pub command: FollowupCommand,
}

#[derive(Subcommand)]
pub enum FollowupCommand {
    /// Schedule a new follow-up
    Schedule {
        #[arg(long)]
        workspace: String,
        #[arg(long)]
        bot: String,
        #[arg(long)]
        delay: String,
        #[arg(long)]
        action: String,
    },
    /// List follow-ups for a workspace
    List {
        #[arg(long)]
        workspace: String,
    },
    /// Cancel a pending follow-up
    Cancel {
        #[arg(long)]
        workspace: String,
        #[arg(long)]
        id: String,
    },
}

pub fn run(args: FollowupArgs, db_path: &std::path::Path) -> Result<()> {
    let db = Db::open(db_path)?;
    ensure_schema(&db);

    match args.command {
        FollowupCommand::Schedule {
            workspace,
            bot,
            delay,
            action,
        } => {
            let delay_secs = parse_delay(&delay).map_err(|e| color_eyre::eyre::eyre!("{e}"))?;
            let followup = schedule(&db, &workspace, &bot, delay_secs, &action)?;
            println!(
                "Scheduled follow-up {} for {}/{}: \"{}\" (fires at {})",
                followup.id, workspace, bot, action, followup.fires_at
            );
        }
        FollowupCommand::List { workspace } => {
            let followups = query_workspace(&db, &workspace);
            if followups.is_empty() {
                println!("No follow-ups for workspace '{workspace}'");
            } else {
                for f in followups {
                    println!(
                        "{} [{}] {}/{}: \"{}\" (fires {})",
                        f.id, f.status, f.workspace, f.bot, f.action, f.fires_at
                    );
                }
            }
        }
        FollowupCommand::Cancel { workspace, id } => {
            let cancelled = cancel(&db, &id, &workspace)?;
            if cancelled {
                println!("Cancelled follow-up {id}");
            } else {
                println!("Follow-up {id} not found or already processed");
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_delay_seconds() {
        assert_eq!(parse_delay("30s").unwrap(), 30);
    }

    #[test]
    fn test_parse_delay_minutes() {
        assert_eq!(parse_delay("5m").unwrap(), 300);
    }

    #[test]
    fn test_parse_delay_hours() {
        assert_eq!(parse_delay("1h").unwrap(), 3600);
    }

    #[test]
    fn test_parse_delay_compound() {
        assert_eq!(parse_delay("2h30m").unwrap(), 9000);
    }

    #[test]
    fn test_parse_delay_all_units() {
        assert_eq!(parse_delay("1h30m45s").unwrap(), 5445);
    }

    #[test]
    fn test_parse_delay_empty() {
        assert!(parse_delay("").is_err());
    }

    #[test]
    fn test_parse_delay_no_unit() {
        assert!(parse_delay("30").is_err());
    }

    #[test]
    fn test_parse_delay_invalid_unit() {
        assert!(parse_delay("5x").is_err());
    }

    #[test]
    fn test_schedule_and_query() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("test.db")).unwrap();
        ensure_schema(&db);

        let followup = schedule(&db, "ws", "Bot", 300, "Check PR").unwrap();
        assert_eq!(followup.workspace, "ws");
        assert_eq!(followup.bot, "Bot");
        assert_eq!(followup.action, "Check PR");
        assert_eq!(followup.status, "pending");
        assert!(followup.id.starts_with("fu_"));

        let all = query_workspace(&db, "ws");
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, followup.id);
    }

    #[test]
    fn test_cancel_followup() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("test.db")).unwrap();
        ensure_schema(&db);

        let followup = schedule(&db, "ws", "Bot", 300, "Check PR").unwrap();

        let cancelled = cancel(&db, &followup.id, "ws").unwrap();
        assert!(cancelled);

        let all = query_workspace(&db, "ws");
        assert_eq!(all[0].status, "cancelled");

        // Cancel again should return false
        let cancelled = cancel(&db, &followup.id, "ws").unwrap();
        assert!(!cancelled);

        // Cancel with wrong workspace should return false
        let followup2 = schedule(&db, "ws", "Bot", 300, "Check PR 2").unwrap();
        let cancelled = cancel(&db, &followup2.id, "other_ws").unwrap();
        assert!(!cancelled);
    }

    #[test]
    fn test_query_due_none_due() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("test.db")).unwrap();
        ensure_schema(&db);

        // Schedule 5 minutes from now — not due yet
        schedule(&db, "ws", "Bot", 300, "Check PR").unwrap();

        let due = query_due(&db);
        assert!(due.is_empty());
    }

    #[test]
    fn test_query_due_with_past_followup() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("test.db")).unwrap();
        ensure_schema(&db);

        // Insert a followup with fires_at in the past
        db.execute_sql(
            "INSERT INTO followups (id, workspace, bot, action, fires_at, status) VALUES (?1, ?2, ?3, ?4, ?5, 'pending')",
            &["fu_test", "ws", "Bot", "Check PR", "2020-01-01T00:00:00Z"],
        ).unwrap();

        let due = query_due(&db);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].id, "fu_test");
    }

    #[test]
    fn test_mark_fired() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("test.db")).unwrap();
        ensure_schema(&db);

        db.execute_sql(
            "INSERT INTO followups (id, workspace, bot, action, fires_at, status) VALUES (?1, ?2, ?3, ?4, ?5, 'pending')",
            &["fu_test", "ws", "Bot", "Check PR", "2020-01-01T00:00:00Z"],
        ).unwrap();

        let fired = mark_fired_if_pending(&db, "fu_test").unwrap();
        assert!(fired);

        let all = query_workspace(&db, "ws");
        assert_eq!(all[0].status, "fired");

        // Already fired — should return false
        let fired_again = mark_fired_if_pending(&db, "fu_test").unwrap();
        assert!(!fired_again);

        // Should no longer appear as due
        let due = query_due(&db);
        assert!(due.is_empty());
    }
}
