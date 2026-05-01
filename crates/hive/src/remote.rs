//! Remote hive discovery, WebSocket bridging, and API proxying.
//!
//! When `~/.config/hive/remotes.toml` exists, hive connects to remote hive
//! instances, discovers their workspaces, bridges their WebSocket events into
//! the local EventHub, and exposes proxy routes so the frontend can talk to
//! remote workspaces transparently.

use crate::events::{EventHub, HiveEvent};
use serde::{Deserialize, Serialize};
use std::path::Path as StdPath;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

// ── Config ──

#[derive(Debug, Clone, Deserialize)]
pub struct RemotesConfig {
    #[serde(default)]
    pub remotes: Vec<RemoteEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RemoteEntry {
    pub name: String,
    pub url: String,
}

/// Validate that a remote name is safe for use in URL path segments and JSON.
fn is_valid_remote_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

pub fn load_remotes_config(config_dir: &StdPath) -> Vec<RemoteEntry> {
    let path = config_dir.join("remotes.toml");
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let config: RemotesConfig = match toml::from_str(&content) {
        Ok(c) => c,
        Err(e) => {
            warn!("failed to parse remotes.toml: {e}");
            return Vec::new();
        }
    };
    config
        .remotes
        .into_iter()
        .filter(|r| {
            if !is_valid_remote_name(&r.name) {
                warn!(
                    "skipping remote with invalid name {:?} (must be alphanumeric, hyphens, underscores, max 64 chars)",
                    r.name
                );
                return false;
            }
            true
        })
        .collect()
}

// ── Shared state ──

#[derive(Debug, Clone, Serialize)]
pub struct RemoteState {
    pub name: String,
    pub url: String,
    pub online: bool,
    pub workspaces: Vec<String>,
}

pub type RemoteRegistry = Arc<RwLock<Vec<RemoteState>>>;

pub fn new_registry() -> RemoteRegistry {
    Arc::new(RwLock::new(Vec::new()))
}

// ── Discovery ──

pub async fn spawn_discovery(
    registry: RemoteRegistry,
    remotes: Vec<RemoteEntry>,
    events: EventHub,
    http_client: reqwest::Client,
) {
    // Initialize all remotes before spawning discovery tasks
    {
        let mut reg = registry.write().await;
        for remote in &remotes {
            reg.push(RemoteState {
                name: remote.name.clone(),
                url: remote.url.clone(),
                online: false,
                workspaces: Vec::new(),
            });
        }
    }

    for remote in remotes {
        let registry = registry.clone();
        let events = events.clone();
        let client = http_client.clone();

        // Spawn discovery poller
        let disc_registry = registry.clone();
        let disc_client = client.clone();
        let disc_remote = remote.clone();
        tokio::spawn(async move {
            loop {
                let result = discover_workspaces(&disc_client, &disc_remote.url).await;
                let (online, workspaces) = match result {
                    Ok(ws) => {
                        tracing::debug!(
                            "[remote] discovered {} workspace(s) on {}",
                            ws.len(),
                            disc_remote.name
                        );
                        (true, ws)
                    }
                    Err(e) => {
                        tracing::debug!("[remote] discovery failed for {}: {e}", disc_remote.name);
                        (false, Vec::new())
                    }
                };

                {
                    let mut reg = disc_registry.write().await;
                    if let Some(state) = reg.iter_mut().find(|s| s.name == disc_remote.name) {
                        let was_online = state.online;
                        state.online = online;
                        state.workspaces = workspaces;
                        if online && !was_online {
                            info!("[remote] {} came online", disc_remote.name);
                        } else if !online && was_online {
                            warn!("[remote] {} went offline", disc_remote.name);
                        }
                    }
                }

                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            }
        });

        // Spawn event bridge
        tokio::spawn(event_bridge(remote, events, client));
    }
}

fn parse_curl_response(raw: Vec<u8>) -> (u16, String, Vec<u8>) {
    let header_end = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap_or(0);
    let headers = String::from_utf8_lossy(&raw[..header_end]).to_string();
    let body_bytes = if header_end > 0 {
        raw[header_end + 4..].to_vec()
    } else {
        raw
    };

    let status = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(500);

    let content_type = headers
        .lines()
        .find(|line| line.to_ascii_lowercase().starts_with("content-type:"))
        .and_then(|line| line.split_once(':'))
        .map(|(_, value)| value.trim().to_string())
        .unwrap_or_default();

    (status, content_type, body_bytes)
}

pub async fn curl_request(
    method: &str,
    url: &str,
    body: Option<&[u8]>,
    content_type: Option<&str>,
) -> Result<(u16, String, Vec<u8>), String> {
    let mut cmd = tokio::process::Command::new("/usr/bin/curl");
    cmd.args(["-s", "-m", "30", "-X", method, "-D", "-", url]);
    if let Some(ct) = content_type {
        cmd.args(["-H", &format!("Content-Type: {ct}")]);
    }
    if body.is_some() {
        cmd.arg("--data-binary").arg("@-");
        cmd.stdin(std::process::Stdio::piped());
    }
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| e.to_string())?;

    if let Some(b) = body
        && let Some(mut stdin) = child.stdin.take()
    {
        use tokio::io::AsyncWriteExt;
        stdin.write_all(b).await.map_err(|e| e.to_string())?;
    }

    let output = child.wait_with_output().await.map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "curl failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    Ok(parse_curl_response(output.stdout))
}

pub async fn reqwest_request(
    client: &reqwest::Client,
    method: reqwest::Method,
    url: &str,
    body: Option<&[u8]>,
    content_type: Option<&str>,
) -> Result<(u16, String, Vec<u8>), String> {
    let mut request = client.request(method, url);
    if let Some(ct) = content_type {
        request = request.header("content-type", ct);
    }
    if let Some(bytes) = body {
        request = request.body(bytes.to_vec());
    }

    let response = request.send().await.map_err(|e| e.to_string())?;
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let body = response.bytes().await.map_err(|e| e.to_string())?.to_vec();
    Ok((status, content_type, body))
}

pub async fn request_with_fallback(
    client: &reqwest::Client,
    method: reqwest::Method,
    url: &str,
    body: Option<&[u8]>,
    content_type: Option<&str>,
) -> Result<(u16, String, Vec<u8>), String> {
    match reqwest_request(client, method.clone(), url, body, content_type).await {
        Ok(response) => Ok(response),
        Err(reqwest_error) => {
            tracing::debug!(
                "[remote] native HTTP failed for {method} {url}: {reqwest_error}; falling back to /usr/bin/curl"
            );
            curl_request(method.as_str(), url, body, content_type)
                .await
                .map_err(|curl_error| {
                    format!(
                        "native HTTP failed: {reqwest_error}; curl fallback failed: {curl_error}"
                    )
                })
        }
    }
}

async fn discover_workspaces(
    client: &reqwest::Client,
    base_url: &str,
) -> Result<Vec<String>, String> {
    #[derive(Deserialize)]
    struct WsInfo {
        name: String,
    }

    let url = format!("{base_url}/api/workspaces");
    let (_, _, body) =
        request_with_fallback(client, reqwest::Method::GET, &url, None, None).await?;
    let workspaces: Vec<WsInfo> = serde_json::from_slice(&body).map_err(|e| e.to_string())?;
    Ok(workspaces.into_iter().map(|w| w.name).collect())
}

// ── Backoff helper ──

const EVENT_BRIDGE_BASE_INTERVAL_SECS: u64 = 3;
const EVENT_BRIDGE_MAX_INTERVAL_SECS: u64 = 300;

fn next_backoff_interval(current: u64) -> u64 {
    (current * 2).min(EVENT_BRIDGE_MAX_INTERVAL_SECS)
}

// ── Remote event bridge ──
// Poll bot status for all remote workspaces. Starts at 3s interval,
// backs off exponentially (up to 5min) when remote is unreachable.

async fn event_bridge(remote: RemoteEntry, events: EventHub, client: reqwest::Client) {
    // Wait for discovery to find workspaces first
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;

    let mut last_statuses: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

    let mut interval_secs = EVENT_BRIDGE_BASE_INTERVAL_SECS;

    loop {
        // Get remote workspaces
        let ws_url = format!("{}/api/workspaces", remote.url);
        if let Ok((_, _, body)) =
            request_with_fallback(&client, reqwest::Method::GET, &ws_url, None, None).await
            && let Ok(workspaces) = serde_json::from_slice::<Vec<serde_json::Value>>(&body)
        {
            // Remote is reachable — reset backoff
            interval_secs = EVENT_BRIDGE_BASE_INTERVAL_SECS;
            for ws in &workspaces {
                let ws_name = ws["name"].as_str().unwrap_or_default();
                if ws_name.is_empty() {
                    continue;
                }

                // Poll bot statuses
                let bots_url = format!("{}/api/workspaces/{ws_name}/bots", remote.url);
                if let Ok((_, _, bots_body)) =
                    request_with_fallback(&client, reqwest::Method::GET, &bots_url, None, None)
                        .await
                    && let Ok(bots) = serde_json::from_slice::<Vec<serde_json::Value>>(&bots_body)
                {
                    for bot in &bots {
                        let bot_name = bot["name"].as_str().unwrap_or_default();
                        if bot_name.is_empty() {
                            continue;
                        }

                        let status_url = format!(
                            "{}/api/workspaces/{ws_name}/bots/{bot_name}/status",
                            remote.url
                        );
                        if let Ok((_, _, status_body)) = request_with_fallback(
                            &client,
                            reqwest::Method::GET,
                            &status_url,
                            None,
                            None,
                        )
                        .await
                        {
                            let status_body = String::from_utf8_lossy(&status_body).to_string();
                            let key = format!("{ws_name}/{bot_name}");
                            let changed = last_statuses.get(&key) != Some(&status_body);
                            if changed {
                                last_statuses.insert(key, status_body.clone());
                                if let Ok(status) =
                                    serde_json::from_str::<serde_json::Value>(&status_body)
                                {
                                    let event_type = "bot_status".to_string();
                                    let mut value = status;
                                    if let Some(obj) = value.as_object_mut() {
                                        obj.insert(
                                            "type".to_string(),
                                            serde_json::Value::String(event_type.clone()),
                                        );
                                        obj.insert(
                                            "workspace".to_string(),
                                            serde_json::Value::String(ws_name.to_string()),
                                        );
                                        obj.insert(
                                            "bot".to_string(),
                                            serde_json::Value::String(bot_name.to_string()),
                                        );
                                        obj.insert(
                                            "remote".to_string(),
                                            serde_json::Value::String(remote.name.clone()),
                                        );
                                    }
                                    events.send(HiveEvent::RemoteEvent {
                                        remote: remote.name.clone(),
                                        workspace: ws_name.to_string(),
                                        bot: bot_name.to_string(),
                                        event_type,
                                        raw_json: serde_json::to_string(&value).unwrap_or_default(),
                                    });
                                }
                            }
                        }
                    }
                }
            }
        } else {
            // Remote unreachable — sleep current interval, then increase for next time
            tokio::time::sleep(std::time::Duration::from_secs(interval_secs)).await;
            interval_secs = next_backoff_interval(interval_secs);
            continue;
        }

        tokio::time::sleep(std::time::Duration::from_secs(interval_secs)).await;
    }
}

// ── Helper to get remote workspaces for the unified workspace list ──

pub async fn get_remote_workspaces(registry: &RemoteRegistry) -> Vec<(String, String)> {
    let reg = registry.read().await;
    let mut result = Vec::new();
    for remote in reg.iter() {
        if remote.online {
            for ws in &remote.workspaces {
                result.push((ws.clone(), remote.name.clone()));
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_empty_config() {
        let dir = tempfile::tempdir().unwrap();
        let remotes = load_remotes_config(dir.path());
        assert!(remotes.is_empty());
    }

    #[test]
    fn test_load_valid_config() {
        let dir = tempfile::tempdir().unwrap();
        let config = r#"
[[remotes]]
name = "mini-office"
url = "http://100.64.0.2:4200"

[[remotes]]
name = "mini-home"
url = "http://100.64.0.3:4200"
"#;
        std::fs::write(dir.path().join("remotes.toml"), config).unwrap();
        let remotes = load_remotes_config(dir.path());
        assert_eq!(remotes.len(), 2);
        assert_eq!(remotes[0].name, "mini-office");
        assert_eq!(remotes[1].url, "http://100.64.0.3:4200");
    }

    #[test]
    fn test_load_invalid_config() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("remotes.toml"), "not valid toml {{{{").unwrap();
        let remotes = load_remotes_config(dir.path());
        assert!(remotes.is_empty());
    }

    #[test]
    fn test_valid_remote_names() {
        assert!(is_valid_remote_name("mini-office"));
        assert!(is_valid_remote_name("mini_home"));
        assert!(is_valid_remote_name("server1"));
        assert!(!is_valid_remote_name(""));
        assert!(!is_valid_remote_name("has spaces"));
        assert!(!is_valid_remote_name("path/traversal"));
        assert!(!is_valid_remote_name("special!chars"));
        assert!(!is_valid_remote_name(&"a".repeat(65)));
    }

    #[test]
    fn test_load_config_skips_invalid_names() {
        let dir = tempfile::tempdir().unwrap();
        let config = r#"
[[remotes]]
name = "valid-name"
url = "http://10.0.0.1:4200"

[[remotes]]
name = "has spaces"
url = "http://10.0.0.2:4200"

[[remotes]]
name = "also/invalid"
url = "http://10.0.0.3:4200"
"#;
        std::fs::write(dir.path().join("remotes.toml"), config).unwrap();
        let remotes = load_remotes_config(dir.path());
        assert_eq!(remotes.len(), 1);
        assert_eq!(remotes[0].name, "valid-name");
    }

    #[test]
    fn test_new_registry() {
        let reg = new_registry();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let r = reg.read().await;
            assert!(r.is_empty());
        });
    }

    #[test]
    fn test_get_remote_workspaces() {
        let registry = new_registry();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            {
                let mut reg = registry.write().await;
                reg.push(RemoteState {
                    name: "mini".to_string(),
                    url: "http://10.0.0.1:4200".to_string(),
                    online: true,
                    workspaces: vec!["proj-a".to_string(), "proj-b".to_string()],
                });
                reg.push(RemoteState {
                    name: "offline".to_string(),
                    url: "http://10.0.0.2:4200".to_string(),
                    online: false,
                    workspaces: vec!["proj-c".to_string()],
                });
            }
            let ws = get_remote_workspaces(&registry).await;
            assert_eq!(ws.len(), 2);
            assert_eq!(ws[0], ("proj-a".to_string(), "mini".to_string()));
            assert_eq!(ws[1], ("proj-b".to_string(), "mini".to_string()));
        });
    }

    #[test]
    fn test_parse_curl_response() {
        let raw =
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nX-Test: 1\r\n\r\n{\"ok\":true}"
                .to_vec();
        let (status, content_type, body) = parse_curl_response(raw);
        assert_eq!(status, 200);
        assert_eq!(content_type, "application/json");
        assert_eq!(body, br#"{"ok":true}"#);
    }

    #[test]
    fn test_event_bridge_backoff() {
        // Starts at base interval
        let mut interval = EVENT_BRIDGE_BASE_INTERVAL_SECS;
        assert_eq!(interval, 3);

        // Doubles on each failure
        interval = next_backoff_interval(interval);
        assert_eq!(interval, 6);
        interval = next_backoff_interval(interval);
        assert_eq!(interval, 12);
        interval = next_backoff_interval(interval);
        assert_eq!(interval, 24);

        // Caps at max
        interval = 192;
        interval = next_backoff_interval(interval);
        assert_eq!(interval, EVENT_BRIDGE_MAX_INTERVAL_SECS); // 300, not 384
    }
}
