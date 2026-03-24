//! Minimal MCP (Model Context Protocol) stdio server for swarm operations.
//!
//! Exposes swarm worker management as MCP tools so the coordinator can
//! dispatch workers directly — no shell commands needed.
//!
//! Protocol: JSON-RPC 2.0 over stdio with `Content-Length` headers (like LSP).

use std::io::{BufRead, Write};
use std::path::PathBuf;

use serde_json::{Value, json};

use crate::buzz::coordinator::swarm_client::SwarmClient;

/// Run the MCP stdio server for the given workspace root.
pub fn run(workspace_root: PathBuf) -> i32 {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut reader = stdin.lock();
    let mut writer = stdout.lock();

    let client = SwarmClient::new(workspace_root);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");

    loop {
        let msg = match read_message(&mut reader) {
            Ok(Some(msg)) => msg,
            Ok(None) => break, // EOF
            Err(e) => {
                eprintln!("mcp-swarm: read error: {e}");
                break;
            }
        };

        let response = handle_message(&client, &rt, &msg);
        if let Some(resp) = response {
            if let Err(e) = write_message(&mut writer, &resp) {
                eprintln!("mcp-swarm: write error: {e}");
                break;
            }
        }

        // notifications (no "id") don't get a response
    }
    0
}

/// Read a JSON-RPC message with Content-Length header.
fn read_message(reader: &mut impl BufRead) -> Result<Option<Value>, String> {
    let mut content_length: Option<usize> = None;

    // Read headers
    loop {
        let mut header = String::new();
        match reader.read_line(&mut header) {
            Ok(0) => return Ok(None), // EOF
            Ok(_) => {}
            Err(e) => return Err(format!("header read: {e}")),
        }
        let trimmed = header.trim();
        if trimmed.is_empty() {
            break; // End of headers
        }
        if let Some(len_str) = trimmed.strip_prefix("Content-Length:") {
            content_length = len_str.trim().parse().ok();
        }
    }

    let len = content_length.ok_or("missing Content-Length header")?;
    let mut body = vec![0u8; len];
    reader
        .read_exact(&mut body)
        .map_err(|e| format!("body read: {e}"))?;

    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|e| format!("json parse: {e}"))
}

/// Write a JSON-RPC response with Content-Length header.
fn write_message(writer: &mut impl Write, msg: &Value) -> Result<(), String> {
    let body = serde_json::to_string(msg).map_err(|e| format!("json serialize: {e}"))?;
    write!(writer, "Content-Length: {}\r\n\r\n{}", body.len(), body)
        .map_err(|e| format!("write: {e}"))?;
    writer.flush().map_err(|e| format!("flush: {e}"))?;
    Ok(())
}

/// Handle a JSON-RPC message. Returns `Some(response)` for requests, `None` for notifications.
fn handle_message(
    client: &SwarmClient,
    rt: &tokio::runtime::Runtime,
    msg: &Value,
) -> Option<Value> {
    let id = msg.get("id")?; // Notifications have no id
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");

    let result = match method {
        "initialize" => handle_initialize(),
        "tools/list" => handle_tools_list(),
        "tools/call" => {
            let params = msg.get("params").cloned().unwrap_or(json!({}));
            handle_tools_call(client, rt, &params)
        }
        "notifications/initialized" | "notifications/cancelled" => return None,
        "ping" => Ok(json!({})),
        _ => Err(json!({
            "code": -32601,
            "message": format!("method not found: {method}")
        })),
    };

    Some(match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(error) => json!({ "jsonrpc": "2.0", "id": id, "error": error }),
    })
}

fn handle_initialize() -> Result<Value, Value> {
    Ok(json!({
        "protocolVersion": "2024-11-05",
        "capabilities": {
            "tools": {}
        },
        "serverInfo": {
            "name": "apiari-swarm",
            "version": env!("CARGO_PKG_VERSION")
        }
    }))
}

fn handle_tools_list() -> Result<Value, Value> {
    Ok(json!({
        "tools": [
            {
                "name": "swarm_create_worker",
                "description": "Dispatch a new swarm worker to implement a task. The worker runs in its own git worktree with an LLM agent that writes code, commits, and opens a PR.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "prompt": {
                            "type": "string",
                            "description": "The task prompt for the worker. Be detailed and self-contained."
                        },
                        "agent": {
                            "type": "string",
                            "description": "Agent to use: claude, codex, gemini, claude-tui, codex-tui, gemini-tui",
                            "default": "claude"
                        },
                        "repo": {
                            "type": "string",
                            "description": "Repository name to work in (the short name, e.g. 'myrepo')"
                        }
                    },
                    "required": ["prompt"]
                }
            },
            {
                "name": "swarm_send_message",
                "description": "Send a message to a waiting swarm worker. Use this when a worker is in the 'waiting' phase and needs input.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "worktree_id": {
                            "type": "string",
                            "description": "The worktree ID of the worker (e.g. 'hive-1a2b')"
                        },
                        "message": {
                            "type": "string",
                            "description": "The message to send to the worker"
                        }
                    },
                    "required": ["worktree_id", "message"]
                }
            },
            {
                "name": "swarm_close_worker",
                "description": "Close and clean up a swarm worker. Terminates the agent and removes the worktree.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "worktree_id": {
                            "type": "string",
                            "description": "The worktree ID of the worker to close"
                        }
                    },
                    "required": ["worktree_id"]
                }
            },
            {
                "name": "swarm_list_workers",
                "description": "List all swarm workers in this workspace with their current status, phase, and PR info.",
                "inputSchema": {
                    "type": "object",
                    "properties": {}
                }
            }
        ]
    }))
}

fn handle_tools_call(
    client: &SwarmClient,
    rt: &tokio::runtime::Runtime,
    params: &Value,
) -> Result<Value, Value> {
    let tool_name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    match tool_name {
        "swarm_create_worker" => {
            let prompt = args.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
            let agent = args
                .get("agent")
                .and_then(|v| v.as_str())
                .unwrap_or("claude");
            let repo = args.get("repo").and_then(|v| v.as_str());

            if prompt.is_empty() {
                return Ok(tool_error("prompt is required"));
            }

            Ok(
                match rt.block_on(client.create_worker(prompt, agent, repo)) {
                    Ok(id) if id.is_empty() => tool_ok("Worker created"),
                    Ok(id) => tool_ok(&format!("Worker created: {id}")),
                    Err(e) => tool_error(&format!("{e}")),
                },
            )
        }
        "swarm_send_message" => {
            let worktree_id = args
                .get("worktree_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let message = args.get("message").and_then(|v| v.as_str()).unwrap_or("");

            if worktree_id.is_empty() || message.is_empty() {
                return Ok(tool_error("worktree_id and message are required"));
            }

            Ok(
                match rt.block_on(client.send_message(worktree_id, message)) {
                    Ok(()) => tool_ok("Message sent"),
                    Err(e) => tool_error(&format!("{e}")),
                },
            )
        }
        "swarm_close_worker" => {
            let worktree_id = args
                .get("worktree_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            if worktree_id.is_empty() {
                return Ok(tool_error("worktree_id is required"));
            }

            Ok(match rt.block_on(client.close_worker(worktree_id)) {
                Ok(()) => tool_ok("Worker closed"),
                Err(e) => tool_error(&format!("{e}")),
            })
        }
        "swarm_list_workers" => Ok(match rt.block_on(client.list_workers()) {
            Ok(workers) => {
                let summary: Vec<Value> = workers
                    .iter()
                    .map(|w| {
                        json!({
                            "id": w.id,
                            "branch": w.branch,
                            "phase": format!("{:?}", w.phase).to_lowercase(),
                            "agent": w.agent,
                            "pr_url": w.pr_url,
                            "pr_title": w.pr_title,
                        })
                    })
                    .collect();
                tool_ok(&serde_json::to_string_pretty(&summary).unwrap_or_default())
            }
            Err(e) => tool_error(&format!("failed to list workers: {e}")),
        }),
        _ => Err(json!({
            "code": -32602,
            "message": format!("unknown tool: {tool_name}")
        })),
    }
}

fn tool_ok(text: &str) -> Value {
    json!({
        "content": [{ "type": "text", "text": text }]
    })
}

fn tool_error(text: &str) -> Value {
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": true
    })
}
