//! Email watcher — polls IMAP mailboxes and emits signals into the review queue.
//!
//! Each configured mailbox becomes its own watcher instance with source
//! `{name}_email_review_queue` so emails auto-appear in the Reviews panel.
//!
//! Cursor persistence: the highest UID seen per mailbox is stored in the
//! signal store cursor table. The cursor is loaded from the daemon before
//! the first poll, and persisted via the `reconcile()` method (sync) after
//! each poll.

use std::time::Duration;

use async_trait::async_trait;
use color_eyre::Result;
use futures::TryStreamExt;
use tracing::{info, warn};

use super::Watcher;
use crate::buzz::{
    config::EmailMailboxConfig,
    signal::{Severity, SignalStatus, SignalUpdate, store::SignalStore},
};

/// Parsed email data (decoupled from IMAP for testability).
#[derive(Debug, Clone)]
pub struct ParsedEmail {
    pub message_id: String,
    pub subject: String,
    pub from: String,
    pub date: String,
    pub body_preview: Option<String>,
    pub uid: u32,
}

/// Watches an IMAP mailbox for new emails.
pub struct EmailWatcher {
    config: EmailMailboxConfig,
    client: reqwest::Client,
    watcher_name: String,
    source: String,
    cursor_key: String,
    last_uid: u32,
}

impl EmailWatcher {
    pub fn new(config: EmailMailboxConfig) -> Self {
        let watcher_name = format!("{}_email", config.name);
        let source = format!("{}_email_review_queue", config.name);
        let cursor_key = format!("{}_email_imap_cursor", config.name);
        Self {
            config,
            client: reqwest::Client::new(),
            watcher_name,
            source,
            cursor_key,
            last_uid: 0,
        }
    }

    /// Get the cursor key (for loading initial cursor from daemon).
    pub fn cursor_key(&self) -> &str {
        &self.cursor_key
    }

    /// Set the initial UID from a previously persisted cursor.
    pub fn set_initial_uid(&mut self, uid: u32) {
        self.last_uid = uid;
    }

    /// Build the signal source name for a mailbox.
    pub fn source_name(name: &str) -> String {
        format!("{name}_email_review_queue")
    }

    /// Build the cursor key for a mailbox.
    pub fn cursor_key_for(name: &str) -> String {
        format!("{name}_email_imap_cursor")
    }

    /// Infer severity from email subject keywords.
    pub fn infer_severity(subject: &str) -> Severity {
        let lower = subject.to_lowercase();
        let urgent_keywords = [
            "urgent",
            "action required",
            "action needed",
            "deadline",
            "immediate",
            "asap",
            "critical",
            "important",
            "time-sensitive",
            "time sensitive",
        ];
        for keyword in &urgent_keywords {
            if lower.contains(keyword) {
                return Severity::Warning;
            }
        }
        Severity::Info
    }

    /// Build a signal from a parsed email.
    pub fn build_signal(source: &str, email: &ParsedEmail, include_body: bool) -> SignalUpdate {
        let severity = Self::infer_severity(&email.subject);

        let body = if include_body {
            let preview = email.body_preview.as_deref().unwrap_or("[no body]");
            let truncated = if preview.len() > 500 {
                // Truncate at a char boundary to avoid panics on multi-byte UTF-8
                let end = preview
                    .char_indices()
                    .take_while(|(i, _)| *i < 500)
                    .last()
                    .map(|(i, c)| i + c.len_utf8())
                    .unwrap_or(0);
                format!("{}...", &preview[..end])
            } else {
                preview.to_string()
            };
            format!(
                "From: {}\nDate: {}\n\n{}",
                email.from, email.date, truncated
            )
        } else {
            format!("From: {}\nDate: {}", email.from, email.date)
        };

        SignalUpdate::new(source, &email.message_id, &email.subject, severity)
            .with_body(body)
            .with_status(SignalStatus::Open)
    }

    /// Build the summarizer request body for the Ollama/OpenAI-compatible API.
    pub fn build_summarizer_request(model: &str, subject: &str, body: &str) -> serde_json::Value {
        serde_json::json!({
            "model": model,
            "messages": [{
                "role": "user",
                "content": format!(
                    "Summarize this email in 1-2 sentences describing what action \
                     (if any) is needed.\n\nSubject: {subject}\n\nBody: {body}"
                )
            }],
            "max_tokens": 150
        })
    }

    /// Call the summarizer API. Returns None on any failure (graceful fallback).
    async fn summarize(&self, subject: &str, body: &str) -> Option<String> {
        let config = self.config.summarizer.as_ref()?;
        let url = format!("{}/v1/chat/completions", config.base_url);
        let payload = Self::build_summarizer_request(&config.model, subject, body);

        let response = self
            .client
            .post(&url)
            .json(&payload)
            .timeout(Duration::from_secs(15))
            .send()
            .await
            .ok()?;

        if !response.status().is_success() {
            warn!(
                "email summarizer returned {}: {}",
                response.status(),
                self.config.name
            );
            return None;
        }

        let json: serde_json::Value = response.json().await.ok()?;
        json["choices"][0]["message"]["content"]
            .as_str()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// Fetch emails via IMAP and return parsed email data.
    async fn fetch_emails(&self, last_uid: u32) -> Result<Vec<ParsedEmail>> {
        use tokio_util::compat::TokioAsyncReadCompatExt;

        let addr = (self.config.host.as_str(), self.config.port);
        let tcp = tokio::net::TcpStream::connect(addr).await?;

        if self.config.tls {
            let tls = async_native_tls::TlsConnector::new();
            let tls_stream = tls.connect(&self.config.host, tcp.compat()).await?;
            let client = async_imap::Client::new(tls_stream);
            let mut session = client
                .login(&self.config.username, &self.config.password)
                .await
                .map_err(|e| color_eyre::eyre::eyre!("IMAP login failed: {}", e.0))?;
            let result = fetch_from_session(
                &mut session,
                &self.config.folder,
                &self.config.filter,
                last_uid,
                self.config.max_fetch,
                self.config.include_body,
            )
            .await;
            let _ = session.logout().await;
            result
        } else {
            let client = async_imap::Client::new(tcp.compat());
            let mut session = client
                .login(&self.config.username, &self.config.password)
                .await
                .map_err(|e| color_eyre::eyre::eyre!("IMAP login failed: {}", e.0))?;
            let result = fetch_from_session(
                &mut session,
                &self.config.folder,
                &self.config.filter,
                last_uid,
                self.config.max_fetch,
                self.config.include_body,
            )
            .await;
            let _ = session.logout().await;
            result
        }
    }
}

/// Search and fetch emails from an IMAP session (generic over stream type).
async fn fetch_from_session<S>(
    session: &mut async_imap::Session<S>,
    folder: &str,
    filter: &str,
    last_uid: u32,
    max_fetch: u32,
    include_body: bool,
) -> Result<Vec<ParsedEmail>>
where
    S: futures::AsyncRead + futures::AsyncWrite + Unpin + Send + std::fmt::Debug,
{
    session.select(folder).await?;

    let search_query = if last_uid > 0 {
        format!("UID {}:* {}", last_uid + 1, filter)
    } else {
        filter.to_string()
    };

    let uids: std::collections::HashSet<u32> = session.uid_search(&search_query).await?;

    if uids.is_empty() {
        return Ok(Vec::new());
    }

    // Sort and limit to max_fetch most recent UIDs
    let mut uid_list: Vec<u32> = uids.into_iter().collect();
    uid_list.sort();
    if uid_list.len() > max_fetch as usize {
        let start = uid_list.len() - max_fetch as usize;
        uid_list = uid_list[start..].to_vec();
    }

    let fetch_query = uid_list
        .iter()
        .map(|u: &u32| u.to_string())
        .collect::<Vec<_>>()
        .join(",");

    let fetch_items = if include_body {
        "UID ENVELOPE BODY.PEEK[]"
    } else {
        "UID ENVELOPE"
    };

    let messages: Vec<async_imap::types::Fetch> = session
        .uid_fetch(&fetch_query, fetch_items)
        .await?
        .try_collect()
        .await?;

    let mut emails = Vec::new();
    for msg in &messages {
        if let Some(parsed) = parse_imap_message(msg, include_body) {
            emails.push(parsed);
        }
    }

    Ok(emails)
}

/// Parse an IMAP fetch result into a ParsedEmail.
fn parse_imap_message(msg: &async_imap::types::Fetch, include_body: bool) -> Option<ParsedEmail> {
    let uid = msg.uid?;

    // Try to parse from full body first (has all headers)
    if let Some(body_bytes) = msg.body() {
        let parsed = mail_parser::MessageParser::default().parse(body_bytes)?;
        let message_id = parsed
            .message_id()
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("uid-{uid}"));
        let subject = parsed.subject().unwrap_or("[no subject]").to_string();
        let from = parsed
            .from()
            .and_then(|addrs| {
                addrs.first().map(|a| {
                    if let Some(name) = a.name() {
                        format!("{} <{}>", name, a.address().unwrap_or(""))
                    } else {
                        a.address().unwrap_or("unknown").to_string()
                    }
                })
            })
            .unwrap_or_else(|| "unknown".to_string());
        let date = parsed
            .date()
            .map(|d| d.to_rfc3339())
            .unwrap_or_else(|| "unknown date".to_string());
        let body_preview = if include_body {
            parsed.body_text(0).map(|t| t.to_string())
        } else {
            None
        };

        return Some(ParsedEmail {
            message_id,
            subject,
            from,
            date,
            body_preview,
            uid,
        });
    }

    // Fall back to ENVELOPE data
    let envelope = msg.envelope()?;
    let message_id = envelope
        .message_id
        .as_ref()
        .map(|s| String::from_utf8_lossy(s).to_string())
        .unwrap_or_else(|| format!("uid-{uid}"));
    let subject = envelope
        .subject
        .as_ref()
        .map(|s| String::from_utf8_lossy(s).to_string())
        .unwrap_or_else(|| "[no subject]".to_string());
    let from = envelope
        .from
        .as_ref()
        .and_then(|addrs| {
            addrs.first().map(|a| {
                let name = a
                    .name
                    .as_ref()
                    .map(|n| String::from_utf8_lossy(n).to_string());
                let mailbox = a
                    .mailbox
                    .as_ref()
                    .map(|m| String::from_utf8_lossy(m).to_string())
                    .unwrap_or_default();
                let host = a
                    .host
                    .as_ref()
                    .map(|h| String::from_utf8_lossy(h).to_string())
                    .unwrap_or_default();
                let email_addr = format!("{mailbox}@{host}");
                if let Some(name) = name {
                    format!("{name} <{email_addr}>")
                } else {
                    email_addr
                }
            })
        })
        .unwrap_or_else(|| "unknown".to_string());
    let date = envelope
        .date
        .as_ref()
        .map(|d| String::from_utf8_lossy(d).to_string())
        .unwrap_or_else(|| "unknown date".to_string());

    Some(ParsedEmail {
        message_id,
        subject,
        from,
        date,
        body_preview: None,
        uid,
    })
}

#[async_trait]
impl Watcher for EmailWatcher {
    fn name(&self) -> &str {
        &self.watcher_name
    }

    fn signal_source(&self) -> &str {
        &self.source
    }

    async fn poll(&mut self, _store: &SignalStore) -> Result<Vec<SignalUpdate>> {
        let last_uid = self.last_uid;

        // Timeout the entire IMAP fetch to avoid blocking the daemon poll loop
        let fetch_result =
            tokio::time::timeout(Duration::from_secs(60), self.fetch_emails(last_uid)).await;
        let emails = match fetch_result {
            Ok(Ok(emails)) => emails,
            Ok(Err(e)) => {
                warn!(
                    "[{}] IMAP fetch failed for {}: {e}",
                    self.watcher_name, self.config.host
                );
                return Ok(Vec::new());
            }
            Err(_) => {
                warn!(
                    "[{}] IMAP fetch timed out for {}",
                    self.watcher_name, self.config.host
                );
                return Ok(Vec::new());
            }
        };

        if emails.is_empty() {
            return Ok(Vec::new());
        }

        // Track highest UID for cursor update
        let max_uid = emails.iter().map(|e| e.uid).max().unwrap_or(last_uid);

        let mut signals = Vec::new();
        for email in &emails {
            let mut signal = Self::build_signal(&self.source, email, self.config.include_body);

            // Try summarization if configured and body content is available
            if self.config.summarizer.is_some()
                && let Some(body_text) = email.body_preview.as_deref()
                && !body_text.is_empty()
                && let Some(summary) = self.summarize(&email.subject, body_text).await
            {
                signal.body = Some(format!("From: {}\n\n{summary}", email.from));
            }

            signals.push(signal);
        }

        if !signals.is_empty() {
            info!(
                "[{}] fetched {} email(s) from {}",
                self.watcher_name,
                signals.len(),
                self.config.host
            );
        }

        // Update local cursor state (persisted by reconcile)
        if max_uid > self.last_uid {
            self.last_uid = max_uid;
        }

        Ok(signals)
    }

    fn reconcile(&self, _source: &str, _poll_ids: &[String], store: &SignalStore) -> Result<usize> {
        // Persist IMAP cursor
        if self.last_uid > 0
            && let Err(e) = store.set_cursor(&self.cursor_key, &self.last_uid.to_string())
        {
            warn!("[{}] failed to persist IMAP cursor: {e}", self.watcher_name);
        }
        // Email signals are NOT auto-reconciled: poll() only fetches NEW emails
        // (UID > cursor), so poll_ids would only contain new message IDs, not the
        // full set of active emails. Auto-reconcile with partial IDs would
        // incorrectly resolve still-relevant signals. Emails are resolved when
        // they no longer appear in the IMAP search results on a future poll.
        //
        // Return 1 to signal custom reconciliation was handled (prevents the
        // framework from falling back to auto-reconcile with poll_ids).
        Ok(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buzz::config::EmailMailboxConfig;

    fn test_config() -> EmailMailboxConfig {
        EmailMailboxConfig {
            name: "fastmail".to_string(),
            host: "imap.fastmail.com".to_string(),
            port: 993,
            tls: true,
            username: "user@example.com".to_string(),
            password: "app-password".to_string(),
            folder: "INBOX".to_string(),
            filter: "UNSEEN".to_string(),
            include_body: false,
            max_fetch: 20,
            interval_secs: 300,
            summarizer: None,
        }
    }

    fn test_email() -> ParsedEmail {
        ParsedEmail {
            message_id: "<abc123@mail.example.com>".to_string(),
            subject: "Meeting tomorrow at 3pm".to_string(),
            from: "Alice <alice@example.com>".to_string(),
            date: "2026-03-17T10:00:00Z".to_string(),
            body_preview: Some("Hi, let's meet tomorrow to discuss the project.".to_string()),
            uid: 42,
        }
    }

    // --- Source name format ---

    #[test]
    fn test_source_name_format() {
        assert_eq!(
            EmailWatcher::source_name("fastmail"),
            "fastmail_email_review_queue"
        );
    }

    #[test]
    fn test_source_name_different_names() {
        assert_eq!(
            EmailWatcher::source_name("gmail"),
            "gmail_email_review_queue"
        );
        assert_eq!(EmailWatcher::source_name("work"), "work_email_review_queue");
    }

    #[test]
    fn test_cursor_key_format() {
        assert_eq!(
            EmailWatcher::cursor_key_for("fastmail"),
            "fastmail_email_imap_cursor"
        );
    }

    // --- Watcher name and source ---

    #[test]
    fn test_watcher_name_and_source() {
        let watcher = EmailWatcher::new(test_config());
        assert_eq!(watcher.name(), "fastmail_email");
        assert_eq!(watcher.signal_source(), "fastmail_email_review_queue");
    }

    // --- Severity inference ---

    #[test]
    fn test_severity_default_info() {
        assert_eq!(
            EmailWatcher::infer_severity("Meeting tomorrow"),
            Severity::Info
        );
    }

    #[test]
    fn test_severity_urgent() {
        assert_eq!(
            EmailWatcher::infer_severity("URGENT: Server down"),
            Severity::Warning
        );
    }

    #[test]
    fn test_severity_action_required() {
        assert_eq!(
            EmailWatcher::infer_severity("Action Required: Review PR"),
            Severity::Warning
        );
    }

    #[test]
    fn test_severity_deadline() {
        assert_eq!(
            EmailWatcher::infer_severity("Deadline approaching for Q1 report"),
            Severity::Warning
        );
    }

    #[test]
    fn test_severity_case_insensitive() {
        assert_eq!(
            EmailWatcher::infer_severity("This is CRITICAL information"),
            Severity::Warning
        );
        assert_eq!(
            EmailWatcher::infer_severity("immediate attention needed"),
            Severity::Warning
        );
    }

    #[test]
    fn test_severity_asap() {
        assert_eq!(
            EmailWatcher::infer_severity("Need this ASAP"),
            Severity::Warning
        );
    }

    #[test]
    fn test_severity_time_sensitive() {
        assert_eq!(
            EmailWatcher::infer_severity("Time-sensitive: contract expires"),
            Severity::Warning
        );
        assert_eq!(
            EmailWatcher::infer_severity("Time sensitive offer"),
            Severity::Warning
        );
    }

    // --- Signal construction ---

    #[test]
    fn test_build_signal_without_body() {
        let email = test_email();
        let signal = EmailWatcher::build_signal("fastmail_email_review_queue", &email, false);

        assert_eq!(signal.source, "fastmail_email_review_queue");
        assert_eq!(signal.external_id, "<abc123@mail.example.com>");
        assert_eq!(signal.title, "Meeting tomorrow at 3pm");
        assert_eq!(signal.severity, Severity::Info);
        assert_eq!(signal.status, SignalStatus::Open);

        let body = signal.body.unwrap();
        assert!(body.contains("From: Alice <alice@example.com>"));
        assert!(body.contains("Date: 2026-03-17T10:00:00Z"));
        assert!(!body.contains("discuss the project"));
    }

    #[test]
    fn test_build_signal_with_body() {
        let email = test_email();
        let signal = EmailWatcher::build_signal("fastmail_email_review_queue", &email, true);

        let body = signal.body.unwrap();
        assert!(body.contains("From: Alice <alice@example.com>"));
        assert!(body.contains("discuss the project"));
    }

    #[test]
    fn test_build_signal_urgent_subject() {
        let email = ParsedEmail {
            subject: "URGENT: Production is down".to_string(),
            ..test_email()
        };
        let signal = EmailWatcher::build_signal("test_email_review_queue", &email, false);
        assert_eq!(signal.severity, Severity::Warning);
    }

    #[test]
    fn test_build_signal_body_truncation() {
        let long_body = "x".repeat(600);
        let email = ParsedEmail {
            body_preview: Some(long_body),
            ..test_email()
        };
        let signal = EmailWatcher::build_signal("test_email_review_queue", &email, true);
        let body = signal.body.unwrap();
        assert!(body.contains("..."));
        assert!(body.len() < 600);
    }

    #[test]
    fn test_build_signal_no_body_preview() {
        let email = ParsedEmail {
            body_preview: None,
            ..test_email()
        };
        let signal = EmailWatcher::build_signal("test_email_review_queue", &email, true);
        let body = signal.body.unwrap();
        assert!(body.contains("[no body]"));
    }

    // --- Config parsing ---

    #[test]
    fn test_config_parsing_single_mailbox() {
        let toml_str = r#"
[[email]]
name = "fastmail"
host = "imap.fastmail.com"
port = 993
tls = true
username = "user@example.com"
password = "app-password"
"#;
        let config: crate::buzz::config::WatchersConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.email.len(), 1);
        assert_eq!(config.email[0].name, "fastmail");
        assert_eq!(config.email[0].host, "imap.fastmail.com");
        assert_eq!(config.email[0].port, 993);
        assert!(config.email[0].tls);
        assert_eq!(config.email[0].folder, "INBOX"); // default
        assert_eq!(config.email[0].filter, "UNSEEN"); // default
        assert!(!config.email[0].include_body); // default
        assert_eq!(config.email[0].max_fetch, 20); // default
    }

    #[test]
    fn test_config_parsing_multiple_mailboxes() {
        let toml_str = r#"
[[email]]
name = "fastmail"
host = "imap.fastmail.com"
username = "user@fastmail.com"
password = "pass1"

[[email]]
name = "gmail"
host = "imap.gmail.com"
username = "user@gmail.com"
password = "pass2"
include_body = true
max_fetch = 50
"#;
        let config: crate::buzz::config::WatchersConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.email.len(), 2);
        assert_eq!(config.email[0].name, "fastmail");
        assert_eq!(config.email[1].name, "gmail");
        assert!(config.email[1].include_body);
        assert_eq!(config.email[1].max_fetch, 50);
    }

    #[test]
    fn test_config_parsing_with_summarizer() {
        let toml_str = r#"
[[watchers.email]]
name = "work"
host = "imap.work.com"
username = "me@work.com"
password = "secret"
summarizer = { base_url = "http://localhost:11434", model = "llama3.2:3b" }
"#;
        let config: crate::buzz::config::BuzzConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.watchers.email.len(), 1);
        let summarizer = config.watchers.email[0].summarizer.as_ref().unwrap();
        assert_eq!(summarizer.base_url, "http://localhost:11434");
        assert_eq!(summarizer.model, "llama3.2:3b");
    }

    #[test]
    fn test_config_parsing_no_email_section() {
        let toml_str = r#"
[watchers.github]
enabled = true
interval_secs = 60
repos = ["org/repo"]
"#;
        let config: crate::buzz::config::BuzzConfig = toml::from_str(toml_str).unwrap();
        assert!(config.watchers.email.is_empty());
    }

    // --- Summarizer request construction ---

    #[test]
    fn test_build_summarizer_request() {
        let req = EmailWatcher::build_summarizer_request(
            "llama3.2:3b",
            "Meeting tomorrow",
            "Let's discuss the roadmap.",
        );

        assert_eq!(req["model"], "llama3.2:3b");
        assert_eq!(req["max_tokens"], 150);
        let content = req["messages"][0]["content"].as_str().unwrap();
        assert!(content.contains("Meeting tomorrow"));
        assert!(content.contains("discuss the roadmap"));
        assert!(content.contains("1-2 sentences"));
    }

    #[test]
    fn test_build_summarizer_request_format() {
        let req = EmailWatcher::build_summarizer_request("model", "subj", "body");
        assert_eq!(req["messages"][0]["role"], "user");
        assert!(req["messages"].as_array().unwrap().len() == 1);
    }

    // --- Cursor persistence ---

    #[test]
    fn test_set_initial_uid() {
        let mut watcher = EmailWatcher::new(test_config());
        assert_eq!(watcher.last_uid, 0);
        watcher.set_initial_uid(42);
        assert_eq!(watcher.last_uid, 42);
    }

    #[test]
    fn test_cursor_key_getter() {
        let watcher = EmailWatcher::new(test_config());
        assert_eq!(watcher.cursor_key(), "fastmail_email_imap_cursor");
    }
}
