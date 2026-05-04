use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskAttemptRole {
    Implementation,
    Reviewer,
    Investigator,
}

impl TaskAttemptRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Implementation => "implementation",
            Self::Reviewer => "reviewer",
            Self::Investigator => "investigator",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "implementation" => Some(Self::Implementation),
            "reviewer" => Some(Self::Reviewer),
            "investigator" => Some(Self::Investigator),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskAttemptState {
    Created,
    Preparing,
    Running,
    Waiting,
    Succeeded,
    Blocked,
    Failed,
    Cancelled,
}

impl TaskAttemptState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Created => "Created",
            Self::Preparing => "Preparing",
            Self::Running => "Running",
            Self::Waiting => "Waiting",
            Self::Succeeded => "Succeeded",
            Self::Blocked => "Blocked",
            Self::Failed => "Failed",
            Self::Cancelled => "Cancelled",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "Created" => Some(Self::Created),
            "Preparing" => Some(Self::Preparing),
            "Running" => Some(Self::Running),
            "Waiting" => Some(Self::Waiting),
            "Succeeded" => Some(Self::Succeeded),
            "Blocked" => Some(Self::Blocked),
            "Failed" => Some(Self::Failed),
            "Cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Blocked | Self::Failed | Self::Cancelled
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskAttempt {
    pub id: String,
    pub task_id: String,
    pub workspace: String,
    pub worker_id: String,
    pub role: TaskAttemptRole,
    pub state: TaskAttemptState,
    pub branch: Option<String>,
    pub pr_url: Option<String>,
    pub pr_number: Option<i64>,
    pub detail: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub metadata: serde_json::Value,
}
