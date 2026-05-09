//! Output modes — emit signals as JSONL to various destinations.

use crate::signal::Signal;
use apiari_common::ipc::JsonlWriter;
use color_eyre::Result;
use std::path::PathBuf;

/// Where to send emitted signals.
#[derive(Debug, Clone)]
pub enum OutputMode {
    /// Write JSONL to stdout.
    Stdout,
    /// Append JSONL to a file.
    File(PathBuf),
    /// POST JSON to a webhook URL (stub).
    Webhook(String),
}

impl OutputMode {
    /// Parse from config strings.
    pub fn from_config(mode: &str, path: Option<&PathBuf>, url: Option<&str>) -> Result<Self> {
        match mode {
            "stdout" => Ok(Self::Stdout),
            "file" => {
                let path = path
                    .cloned()
                    .unwrap_or_else(|| PathBuf::from(".buzz/signals.jsonl"));
                Ok(Self::File(path))
            }
            "webhook" => {
                let url =
                    url.ok_or_else(|| color_eyre::eyre::eyre!("webhook output requires a url"))?;
                Ok(Self::Webhook(url.to_string()))
            }
            other => Err(color_eyre::eyre::eyre!("unknown output mode: {other}")),
        }
    }
}

/// Emit a batch of signals to the configured output destination.
pub fn emit(signals: &[Signal], mode: &OutputMode) -> Result<()> {
    if signals.is_empty() {
        return Ok(());
    }

    match mode {
        OutputMode::Stdout => {
            for signal in signals {
                let json = serde_json::to_string(signal)?;
                println!("{json}");
            }
        }
        OutputMode::File(path) => {
            let writer = JsonlWriter::<Signal>::new(path);
            for signal in signals {
                writer.append(signal)?;
            }
        }
        OutputMode::Webhook(url) => {
            // Stub: in a real implementation this would POST to the URL.
            tracing::warn!(url = %url, dropped = signals.len(), "webhook output not yet implemented");
        }
    }

    Ok(())
}
