//! Task entity — the core unit of work in the task lifecycle system.
//!
//! Tasks move through stages on a kanban board. This module defines the
//! data types; `store` provides the SQLite-backed persistence layer.

pub mod engine;
pub mod rules;
pub mod store;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The stages a task moves through. These are the default stages.
/// In the future, stages will be configurable per workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStage {
    Triage,
    InProgress,
    InAiReview,
    HumanReview,
    Merged,
    Dismissed,
}

impl TaskStage {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Triage => "Triage",
            Self::InProgress => "In Progress",
            Self::InAiReview => "In AI Review",
            Self::HumanReview => "Human Review",
            Self::Merged => "Merged",
            Self::Dismissed => "Dismissed",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "Triage" => Some(Self::Triage),
            "In Progress" => Some(Self::InProgress),
            "In AI Review" => Some(Self::InAiReview),
            "Human Review" => Some(Self::HumanReview),
            "Merge Ready" => Some(Self::HumanReview), // migration: old serialized value
            "Merged" => Some(Self::Merged),
            "Dismissed" => Some(Self::Dismissed),
            _ => None,
        }
    }

    /// Ordered list of active (non-terminal) stages for kanban display.
    pub fn kanban_stages() -> &'static [TaskStage] {
        &[
            Self::Triage,
            Self::InProgress,
            Self::InAiReview,
            Self::HumanReview,
        ]
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Merged | Self::Dismissed)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String, // uuid
    pub workspace: String,
    pub title: String,
    pub stage: TaskStage,
    pub source: Option<String>, // "sentry", "github_issue", "manual", etc.
    pub source_url: Option<String>,
    pub worker_id: Option<String>,
    pub pr_url: Option<String>,
    pub pr_number: Option<i64>,
    pub repo: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskEvent {
    pub id: i64,
    pub task_id: String,
    pub event_type: String, // "stage_change", "signal", "user_action"
    pub from_stage: Option<String>,
    pub to_stage: Option<String>,
    pub signal_id: Option<i64>,
    pub detail: Option<String>,
    pub created_at: DateTime<Utc>,
}
