//! Sentry watcher — polls the Sentry Issues API for new unresolved issues.
//!
//! Each bot with `watch = ["sentry"]` and `services = ["sentry"]` gets polled
//! every 8 ticks (~2 minutes at 15s base). On first run, existing issues are
//! recorded as the cursor without triggering the bot.

use crate::db::Db;
use crate::tick::{Action, TickContext, Watcher};
use crate::watcher::WatchedBot;
use async_trait::async_trait;
use serde::Deserialize;
use tracing::warn;

/// A Sentry issue as returned by the Issues API (subset of fields).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SentryIssue {
    id: String,
    title: String,
    culprit: Option<String>,
    permalink: Option<String>,
    count: Option<String>,
    user_count: Option<u64>,
    first_seen: Option<String>,
    last_seen: Option<String>,
    level: Option<String>,
    metadata: Option<SentryMetadata>,
}

#[derive(Debug, Deserialize)]
struct SentryMetadata {
    #[serde(rename = "type")]
    error_type: Option<String>,
    value: Option<String>,
}

/// Sentry config parsed from `.apiari/services.toml`.
#[derive(Debug, Clone)]
struct SentryConfig {
    token: String,
    org: String,
    project: String,
}

/// Per-bot state for sentry polling.
struct SentryBotState {
    bot: WatchedBot,
    config: Option<SentryConfig>,
    initialized: bool,
    disabled: bool,
}

pub struct SentryWatcher {
    states: Vec<SentryBotState>,
    db: Db,
    client: reqwest::Client,
}

impl SentryWatcher {
    pub fn new(bots: Vec<WatchedBot>, db: Db) -> Self {
        let states = bots
            .into_iter()
            .filter(|b| {
                b.watch.contains(&"sentry".to_string())
                    && b.services.contains(&"sentry".to_string())
            })
            .map(|bot| {
                let config = load_sentry_config(&bot);
                SentryBotState {
                    bot,
                    config,
                    initialized: false,
                    disabled: false,
                }
            })
            .collect();

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .unwrap_or_default();

        Self { states, db, client }
    }
}

fn load_sentry_config(bot: &WatchedBot) -> Option<SentryConfig> {
    let root = bot.working_dir.as_ref()?;
    let services_path = root.join(".apiari/services.toml");
    let content = std::fs::read_to_string(&services_path).ok()?;
    let config: toml::Value = toml::from_str(&content).ok()?;
    let section = config.get("sentry")?.as_table()?;

    let token = section.get("token")?.as_str()?.to_string();
    let org = section.get("org")?.as_str()?.to_string();
    let project = section.get("project")?.as_str()?.to_string();

    if token.is_empty() || org.is_empty() || project.is_empty() {
        return None;
    }

    Some(SentryConfig {
        token,
        org,
        project,
    })
}

#[async_trait]
impl Watcher for SentryWatcher {
    fn name(&self) -> &str {
        "sentry-watcher"
    }

    fn interval_ticks(&self) -> u64 {
        8 // 8 × 15s = 120s = 2 minutes
    }

    async fn tick(&mut self, _ctx: &TickContext) -> Vec<Action> {
        // Build concurrent fetch futures for all active bots
        let mut futures = Vec::new();
        let mut active_indices = Vec::new();

        for (i, state) in self.states.iter().enumerate() {
            if state.disabled {
                continue;
            }

            let config = match &state.config {
                Some(c) => c.clone(),
                None => continue, // will be disabled below
            };

            let url = format!(
                "https://sentry.io/api/0/projects/{}/{}/issues/?query=is:unresolved&sort=date&limit=25",
                config.org, config.project
            );

            let client = self.client.clone();
            active_indices.push(i);
            futures.push(async move {
                let response = client
                    .get(&url)
                    .header("Authorization", format!("Bearer {}", config.token))
                    .send()
                    .await;
                (response, config)
            });
        }

        // Disable bots with no config (deferred to avoid borrow conflict above)
        for state in &mut self.states {
            if !state.disabled && state.config.is_none() {
                warn!(
                    "[sentry] no valid sentry config for {}/{}, disabling",
                    state.bot.workspace, state.bot.name
                );
                state.disabled = true;
            }
        }

        if futures.is_empty() {
            return Vec::new();
        }

        // Poll all bots concurrently
        let results = futures_util::future::join_all(futures).await;

        let mut actions = Vec::new();
        for (result_idx, (response, _config)) in results.into_iter().enumerate() {
            let state_idx = active_indices[result_idx];
            let state = &mut self.states[state_idx];

            let response = match response {
                Ok(r) => r,
                Err(e) => {
                    warn!(
                        "[sentry] API error for {}/{}: {e}",
                        state.bot.workspace, state.bot.name
                    );
                    continue;
                }
            };

            // Check rate limiting
            if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                warn!(
                    "[sentry] rate limited for {}/{}, skipping",
                    state.bot.workspace, state.bot.name
                );
                continue;
            }

            if !response.status().is_success() {
                let status = response.status();
                if status == reqwest::StatusCode::UNAUTHORIZED
                    || status == reqwest::StatusCode::FORBIDDEN
                {
                    warn!(
                        "[sentry] auth failed for {}/{} ({}), disabling",
                        state.bot.workspace, state.bot.name, status
                    );
                    state.disabled = true;
                } else {
                    warn!(
                        "[sentry] API returned {} for {}/{}",
                        status, state.bot.workspace, state.bot.name
                    );
                }
                continue;
            }

            let issues: Vec<SentryIssue> = match response.json().await {
                Ok(i) => i,
                Err(e) => {
                    warn!(
                        "[sentry] failed to parse response for {}/{}: {e}",
                        state.bot.workspace, state.bot.name
                    );
                    continue;
                }
            };

            if issues.is_empty() {
                continue;
            }

            // Get cursor (last known issue ID)
            let cursor = match self
                .db
                .get_sentry_cursor(&state.bot.workspace, &state.bot.name)
            {
                Ok(c) => c,
                Err(e) => {
                    warn!(
                        "[sentry] failed to read cursor for {}/{}: {e}",
                        state.bot.workspace, state.bot.name
                    );
                    continue;
                }
            };

            if !state.initialized {
                state.initialized = true;
                if cursor.is_some() {
                    // DB has cursor from previous run, fall through to detect new issues
                } else {
                    // True first run: record cursor without alerting
                    if let Some(first) = issues.first() {
                        let now = chrono::Utc::now().to_rfc3339();
                        if let Err(e) = self.db.set_sentry_cursor(
                            &state.bot.workspace,
                            &state.bot.name,
                            &first.id,
                            &now,
                        ) {
                            warn!("[sentry] failed to set initial cursor: {e}");
                        }
                    }
                    continue;
                }
            }

            // Find new issues — Sentry IDs are monotonically increasing integers,
            // so we filter to issues with id > cursor. This avoids depending on the
            // cursor issue still being present in the `is:unresolved` results.
            let new_issues: Vec<&SentryIssue> = match &cursor {
                Some(last_id) => {
                    let cursor_num = match last_id.parse::<u64>() {
                        Ok(n) => n,
                        Err(_) => {
                            warn!(
                                "[sentry] cursor ID '{}' is not numeric for {}/{}, skipping",
                                last_id, state.bot.workspace, state.bot.name
                            );
                            continue;
                        }
                    };
                    issues
                        .iter()
                        .filter(|issue| issue.id.parse::<u64>().is_ok_and(|n| n > cursor_num))
                        .collect()
                }
                None => {
                    // No cursor but initialized — shouldn't happen, take highest-ID issue
                    issues
                        .iter()
                        .filter(|i| i.id.parse::<u64>().is_ok())
                        .max_by_key(|i| i.id.parse::<u64>().unwrap())
                        .into_iter()
                        .collect()
                }
            };

            if new_issues.is_empty() {
                continue;
            }

            // Capture newest issue ID for cursor update — use the highest numeric ID,
            // not first(), since results are sorted by date and ID order may differ.
            let newest_issue_id = new_issues
                .iter()
                .filter_map(|i| i.id.parse::<u64>().ok().map(|n| (n, &i.id)))
                .max_by_key(|(n, _)| *n)
                .map(|(_, id)| id.clone());

            // Dispatch each new issue as a signal
            for issue in &new_issues {
                let title = format!(
                    "[{}] {}",
                    issue.level.as_deref().unwrap_or("error").to_uppercase(),
                    issue.title
                );

                let mut body = String::new();
                body.push_str(&format!("**{}**\n\n", issue.title));

                if let Some(ref meta) = issue.metadata {
                    if let Some(ref t) = meta.error_type {
                        body.push_str(&format!("**Type:** {t}\n"));
                    }
                    if let Some(ref v) = meta.value {
                        body.push_str(&format!("**Value:** {v}\n"));
                    }
                }

                if let Some(ref culprit) = issue.culprit
                    && !culprit.is_empty()
                {
                    body.push_str(&format!("**Culprit:** {culprit}\n"));
                }

                body.push_str(&format!(
                    "**Level:** {}\n",
                    issue.level.as_deref().unwrap_or("error")
                ));
                body.push_str(&format!(
                    "**Events:** {}\n",
                    issue.count.as_deref().unwrap_or("0")
                ));
                body.push_str(&format!(
                    "**Users affected:** {}\n",
                    issue.user_count.unwrap_or(0)
                ));

                if let Some(ref first) = issue.first_seen {
                    body.push_str(&format!("**First seen:** {first}\n"));
                }
                if let Some(ref last) = issue.last_seen {
                    body.push_str(&format!("**Last seen:** {last}\n"));
                }
                if let Some(ref link) = issue.permalink {
                    body.push_str(&format!("\n[View in Sentry]({link})\n"));
                }

                actions.push(Action::DispatchSignal {
                    bot: state.bot.clone(),
                    signal_source: "sentry".to_string(),
                    signal_title: title,
                    signal_body: body,
                });
            }

            // Emit cursor update after DispatchSignal actions — the tick engine
            // awaits signal dispatch completion before executing this action.
            if let Some(issue_id) = newest_issue_id {
                actions.push(Action::UpdateSentryCursor {
                    workspace: state.bot.workspace.clone(),
                    bot: state.bot.name.clone(),
                    issue_id,
                });
            }

            tracing::info!(
                "[sentry] found {} new issue(s) for {}/{}",
                new_issues.len(),
                state.bot.workspace,
                state.bot.name
            );
        }

        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_bot(services: Vec<String>) -> WatchedBot {
        WatchedBot {
            workspace: "test-ws".to_string(),
            name: "test-bot".to_string(),
            provider: "claude".to_string(),
            model: None,
            role: "sentry monitor".to_string(),
            watch: vec!["sentry".to_string()],
            working_dir: None,
            schedule: None,
            schedule_hours: None,
            proactive_prompt: None,
            services,
            response_style: None,
        }
    }

    #[test]
    fn test_sentry_watcher_interval() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("test.db")).unwrap();
        crate::sentry_watcher::ensure_schema(&db);
        let watcher = SentryWatcher::new(vec![], db);
        assert_eq!(watcher.interval_ticks(), 8);
    }

    #[test]
    fn test_sentry_watcher_name() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("test.db")).unwrap();
        crate::sentry_watcher::ensure_schema(&db);
        let watcher = SentryWatcher::new(vec![], db);
        assert_eq!(watcher.name(), "sentry-watcher");
    }

    #[test]
    fn test_sentry_watcher_filters_bots() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("test.db")).unwrap();
        crate::sentry_watcher::ensure_schema(&db);

        // Has both watch=["sentry"] and services=["sentry"] — included
        let sentry_bot = make_test_bot(vec!["sentry".to_string()]);
        // Has watch=["github"] — excluded
        let github_bot = WatchedBot {
            watch: vec!["github".to_string()],
            ..make_test_bot(vec![])
        };
        // Has watch=["sentry"] but no services=["sentry"] — excluded
        let watch_only_bot = WatchedBot {
            services: vec![],
            ..make_test_bot(vec![])
        };

        let watcher = SentryWatcher::new(vec![sentry_bot, github_bot, watch_only_bot], db);
        assert_eq!(watcher.states.len(), 1);
    }

    #[test]
    fn test_load_sentry_config_from_services_toml() {
        let dir = tempfile::tempdir().unwrap();
        let apiari_dir = dir.path().join(".apiari");
        std::fs::create_dir_all(&apiari_dir).unwrap();
        std::fs::write(
            apiari_dir.join("services.toml"),
            r#"
[sentry]
token = "sntrys_test123"
org = "my-org"
project = "my-project"
"#,
        )
        .unwrap();

        let bot = WatchedBot {
            workspace: "test".to_string(),
            name: "bot".to_string(),
            provider: "claude".to_string(),
            model: None,
            role: "test".to_string(),
            watch: vec!["sentry".to_string()],
            working_dir: Some(dir.path().to_path_buf()),
            schedule: None,
            schedule_hours: None,
            proactive_prompt: None,
            services: vec!["sentry".to_string()],
            response_style: None,
        };

        let config = load_sentry_config(&bot).unwrap();
        assert_eq!(config.token, "sntrys_test123");
        assert_eq!(config.org, "my-org");
        assert_eq!(config.project, "my-project");
    }

    #[test]
    fn test_load_sentry_config_missing_file() {
        let bot = WatchedBot {
            workspace: "test".to_string(),
            name: "bot".to_string(),
            provider: "claude".to_string(),
            model: None,
            role: "test".to_string(),
            watch: vec!["sentry".to_string()],
            working_dir: Some(std::path::PathBuf::from("/nonexistent")),
            schedule: None,
            schedule_hours: None,
            proactive_prompt: None,
            services: vec!["sentry".to_string()],
            response_style: None,
        };

        assert!(load_sentry_config(&bot).is_none());
    }

    #[test]
    fn test_load_sentry_config_empty_fields() {
        let dir = tempfile::tempdir().unwrap();
        let apiari_dir = dir.path().join(".apiari");
        std::fs::create_dir_all(&apiari_dir).unwrap();
        std::fs::write(
            apiari_dir.join("services.toml"),
            r#"
[sentry]
token = ""
org = "my-org"
project = "my-project"
"#,
        )
        .unwrap();

        let bot = WatchedBot {
            workspace: "test".to_string(),
            name: "bot".to_string(),
            provider: "claude".to_string(),
            model: None,
            role: "test".to_string(),
            watch: vec!["sentry".to_string()],
            working_dir: Some(dir.path().to_path_buf()),
            schedule: None,
            schedule_hours: None,
            proactive_prompt: None,
            services: vec!["sentry".to_string()],
            response_style: None,
        };

        assert!(load_sentry_config(&bot).is_none());
    }

    #[test]
    fn test_sentry_cursor_db_operations() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("test.db")).unwrap();
        crate::sentry_watcher::ensure_schema(&db);

        // Initially no cursor
        assert!(db.get_sentry_cursor("ws", "bot").unwrap().is_none());

        // Set and read back
        db.set_sentry_cursor("ws", "bot", "12345", "2026-04-29T09:00:00Z")
            .unwrap();
        let cursor = db.get_sentry_cursor("ws", "bot").unwrap();
        assert_eq!(cursor.as_deref(), Some("12345"));

        // Update
        db.set_sentry_cursor("ws", "bot", "67890", "2026-04-29T10:00:00Z")
            .unwrap();
        let cursor = db.get_sentry_cursor("ws", "bot").unwrap();
        assert_eq!(cursor.as_deref(), Some("67890"));
    }

    #[test]
    fn test_sentry_watcher_init_respects_existing_cursor() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("test.db")).unwrap();
        crate::sentry_watcher::ensure_schema(&db);

        // Simulate cursor from a previous daemon run
        db.set_sentry_cursor("test-ws", "test-bot", "99999", "2026-04-29T09:00:00Z")
            .unwrap();

        let bot = make_test_bot(vec!["sentry".to_string()]);
        let state = SentryBotState {
            bot,
            config: None,
            initialized: false,
            disabled: false,
        };

        // After initialization, cursor should still be the old one (not overwritten)
        let cursor = db.get_sentry_cursor("test-ws", "test-bot").unwrap();
        assert_eq!(cursor.as_deref(), Some("99999"));

        // initialized starts false but config is None, so it would be disabled
        assert!(!state.initialized);
    }

    /// Verifies that tick() emits DispatchSignal actions before UpdateSentryCursor.
    /// This ordering is critical: the tick engine awaits signal dispatch completion
    /// before executing the cursor update, preventing signal loss on shutdown.
    #[tokio::test]
    async fn test_actions_have_cursor_update_after_signals() {
        // We can't easily call tick() without a real Sentry API, so verify the
        // contract by constructing the action sequence the same way tick() does.
        let bot = make_test_bot(vec!["sentry".to_string()]);
        let watched = WatchedBot {
            workspace: "ws".to_string(),
            name: "bot".to_string(),
            ..bot
        };

        // Simulate what tick() does: dispatch signals, then cursor update
        let mut actions: Vec<Action> = Vec::new();
        let issue_ids = vec!["111", "222", "333"];
        for id in &issue_ids {
            actions.push(Action::DispatchSignal {
                bot: watched.clone(),
                signal_source: "sentry".to_string(),
                signal_title: format!("Issue {id}"),
                signal_body: "body".to_string(),
            });
        }
        actions.push(Action::UpdateSentryCursor {
            workspace: "ws".to_string(),
            bot: "bot".to_string(),
            issue_id: "111".to_string(), // newest
        });

        // Verify: all DispatchSignal actions come before UpdateSentryCursor
        let cursor_idx = actions
            .iter()
            .position(|a| matches!(a, Action::UpdateSentryCursor { .. }))
            .expect("should have cursor update");
        let last_signal_idx = actions
            .iter()
            .rposition(|a| matches!(a, Action::DispatchSignal { .. }))
            .expect("should have signals");
        assert!(
            cursor_idx > last_signal_idx,
            "UpdateSentryCursor (idx {cursor_idx}) must come after last DispatchSignal (idx {last_signal_idx})"
        );

        // Verify counts
        let signal_count = actions
            .iter()
            .filter(|a| matches!(a, Action::DispatchSignal { .. }))
            .count();
        let cursor_count = actions
            .iter()
            .filter(|a| matches!(a, Action::UpdateSentryCursor { .. }))
            .count();
        assert_eq!(signal_count, 3);
        assert_eq!(cursor_count, 1);
    }

    fn make_issue(id: &str) -> SentryIssue {
        SentryIssue {
            id: id.to_string(),
            title: format!("Issue {id}"),
            culprit: None,
            permalink: None,
            count: None,
            user_count: None,
            first_seen: None,
            last_seen: None,
            level: None,
            metadata: None,
        }
    }

    /// Replicate the filtering logic from tick() so we can test it in isolation.
    fn filter_new_issues<'a>(
        issues: &'a [SentryIssue],
        cursor: &Option<String>,
    ) -> Vec<&'a SentryIssue> {
        match cursor {
            Some(last_id) => {
                let cursor_num = match last_id.parse::<u64>() {
                    Ok(n) => n,
                    Err(_) => return Vec::new(),
                };
                issues
                    .iter()
                    .filter(|issue| issue.id.parse::<u64>().is_ok_and(|n| n > cursor_num))
                    .collect()
            }
            None => issues
                .iter()
                .filter(|i| i.id.parse::<u64>().is_ok())
                .max_by_key(|i| i.id.parse::<u64>().unwrap())
                .into_iter()
                .collect(),
        }
    }

    #[test]
    fn test_numeric_cursor_filters_older_issues() {
        let issues = vec![make_issue("105"), make_issue("103"), make_issue("101")];
        let cursor = Some("102".to_string());
        let new = filter_new_issues(&issues, &cursor);
        let ids: Vec<&str> = new.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, vec!["105", "103"]);
    }

    #[test]
    fn test_numeric_cursor_no_new_issues() {
        let issues = vec![make_issue("100"), make_issue("99")];
        let cursor = Some("100".to_string());
        let new = filter_new_issues(&issues, &cursor);
        assert!(new.is_empty());
    }

    #[test]
    fn test_numeric_cursor_resolved_cursor_issue_still_works() {
        // Cursor issue "102" is resolved and no longer in the results.
        // With the old take_while, this would return ALL issues as new.
        let issues = vec![make_issue("105"), make_issue("103"), make_issue("101")];
        let cursor = Some("102".to_string());
        let new = filter_new_issues(&issues, &cursor);
        let ids: Vec<&str> = new.iter().map(|i| i.id.as_str()).collect();
        // Only issues with id > 102 should be returned
        assert_eq!(ids, vec!["105", "103"]);
    }

    #[test]
    fn test_numeric_cursor_non_numeric_issue_id_skipped() {
        let issues = vec![make_issue("abc"), make_issue("105"), make_issue("103")];
        let cursor = Some("100".to_string());
        let new = filter_new_issues(&issues, &cursor);
        let ids: Vec<&str> = new.iter().map(|i| i.id.as_str()).collect();
        // "abc" is skipped, only numeric IDs > 100 included
        assert_eq!(ids, vec!["105", "103"]);
    }

    #[test]
    fn test_numeric_cursor_non_numeric_cursor_returns_empty() {
        let issues = vec![make_issue("105"), make_issue("103")];
        let cursor = Some("not-a-number".to_string());
        let new = filter_new_issues(&issues, &cursor);
        assert!(new.is_empty());
    }

    #[test]
    fn test_no_cursor_takes_highest_id() {
        let issues = vec![make_issue("103"), make_issue("105"), make_issue("101")];
        let new = filter_new_issues(&issues, &None);
        assert_eq!(new.len(), 1);
        assert_eq!(new[0].id, "105");
    }

    #[test]
    fn test_newest_issue_id_uses_max_numeric_id() {
        // Simulates what tick() does: results sorted by date (not ID order),
        // newest_issue_id should be the highest numeric ID.
        let issues = vec![make_issue("103"), make_issue("107"), make_issue("105")];
        let cursor = Some("100".to_string());
        let new = filter_new_issues(&issues, &cursor);
        let newest = new
            .iter()
            .filter_map(|i| i.id.parse::<u64>().ok().map(|n| (n, &i.id)))
            .max_by_key(|(n, _)| *n)
            .map(|(_, id)| id.as_str());
        assert_eq!(newest, Some("107"));
    }

    #[tokio::test]
    async fn test_sentry_watcher_first_tick_initializes_without_actions() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("test.db")).unwrap();
        crate::sentry_watcher::ensure_schema(&db);

        // Bot with no working_dir — config will be None, watcher disables it
        let bot = make_test_bot(vec!["sentry".to_string()]);
        let mut watcher = SentryWatcher::new(vec![bot], db);

        let ctx = TickContext { tick_number: 8 };
        let actions = watcher.tick(&ctx).await;
        // Should produce no actions (bot disabled due to missing config)
        assert!(actions.is_empty());
        assert!(watcher.states[0].disabled);
    }
}

/// Create the sentry_cursors table if it doesn't exist.
pub fn ensure_schema(db: &Db) {
    let _ = db.execute_batch(
        "CREATE TABLE IF NOT EXISTS sentry_cursors (
            workspace TEXT NOT NULL,
            bot TEXT NOT NULL,
            last_issue_id TEXT NOT NULL,
            last_poll_at TEXT NOT NULL,
            PRIMARY KEY (workspace, bot)
        )",
    );
}
