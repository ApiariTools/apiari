//! Daemon-specific configuration helpers.

use crate::buzz::config::BuzzConfig;

/// Check if Telegram is configured.
pub fn has_telegram(config: &BuzzConfig) -> bool {
    config.telegram.is_some()
}

/// Check if any watchers are enabled.
pub fn has_watchers(config: &BuzzConfig) -> bool {
    let w = &config.watchers;
    w.github.as_ref().is_some_and(|g| g.enabled)
        || w.sentry.as_ref().is_some_and(|s| s.enabled)
        || w.swarm.as_ref().is_some_and(|s| s.enabled)
        || !w.email.is_empty()
}

/// Get the watcher poll interval (minimum across enabled watchers).
pub fn min_watcher_interval(config: &BuzzConfig) -> u64 {
    let w = &config.watchers;
    let mut intervals = Vec::new();

    if let Some(g) = &w.github
        && g.enabled
    {
        intervals.push(g.interval_secs);
    }
    if let Some(s) = &w.sentry
        && s.enabled
    {
        intervals.push(s.interval_secs);
    }
    if let Some(s) = &w.swarm
        && s.enabled
    {
        intervals.push(s.interval_secs);
    }

    for e in &w.email {
        intervals.push(e.interval_secs);
    }

    intervals.into_iter().min().unwrap_or(60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buzz::config::*;

    #[test]
    fn test_has_telegram() {
        let mut config = BuzzConfig::default();
        assert!(!has_telegram(&config));

        config.telegram = Some(TelegramConfig {
            bot_token: "tok".into(),
            chat_id: 123,
            topic_id: None,
            allowed_user_ids: vec![],
        });
        assert!(has_telegram(&config));
    }

    #[test]
    fn test_has_watchers_none() {
        let config = BuzzConfig::default();
        assert!(!has_watchers(&config));
    }

    #[test]
    fn test_has_watchers_github() {
        let mut config = BuzzConfig::default();
        config.watchers.github = Some(GithubWatcherConfig {
            enabled: true,
            interval_secs: 60,
            repos: vec!["org/repo".into()],
            watch_labels: vec![],
            review_queue: vec![],
            filters: std::collections::HashMap::new(),
        });
        assert!(has_watchers(&config));
    }

    #[test]
    fn test_has_watchers_disabled() {
        let mut config = BuzzConfig::default();
        config.watchers.github = Some(GithubWatcherConfig {
            enabled: false,
            interval_secs: 60,
            repos: vec![],
            watch_labels: vec![],
            review_queue: vec![],
            filters: std::collections::HashMap::new(),
        });
        assert!(!has_watchers(&config));
    }

    #[test]
    fn test_min_watcher_interval() {
        let mut config = BuzzConfig::default();
        config.watchers.github = Some(GithubWatcherConfig {
            enabled: true,
            interval_secs: 120,
            repos: vec![],
            watch_labels: vec![],
            review_queue: vec![],
            filters: std::collections::HashMap::new(),
        });
        config.watchers.swarm = Some(SwarmWatcherConfig {
            enabled: true,
            interval_secs: 15,
            state_path: "/tmp/state.json".into(),
        });
        assert_eq!(min_watcher_interval(&config), 15);
    }

    #[test]
    fn test_min_watcher_interval_default() {
        let config = BuzzConfig::default();
        assert_eq!(min_watcher_interval(&config), 60);
    }
}
