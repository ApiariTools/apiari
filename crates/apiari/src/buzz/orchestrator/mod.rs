pub mod action;
pub mod notify;

use serde::{Deserialize, Serialize};

use crate::buzz::signal::SignalRecord;

use self::{
    action::{CoordinatorActionConfig, TriggeredAction, actions_for_signal},
    notify::{Notification, NotificationTierOverrides, notification_for_signal},
};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OrchestratorConfig {
    #[serde(default)]
    pub notification_tiers: NotificationTierOverrides,
    #[serde(default)]
    pub actions: Vec<CoordinatorActionConfig>,
}

pub struct Orchestrator {
    config: OrchestratorConfig,
    pending_actions: Vec<TriggeredAction>,
}

impl Orchestrator {
    pub fn new(config: OrchestratorConfig) -> Self {
        Self {
            config,
            pending_actions: Vec::new(),
        }
    }

    pub fn process_signal(&mut self, signal: &SignalRecord) -> Vec<Notification> {
        let notification = notification_for_signal(signal, &self.config.notification_tiers);
        self.pending_actions
            .extend(actions_for_signal(signal, &self.config.actions));
        vec![notification]
    }

    pub fn drain_actions(&mut self) -> Vec<TriggeredAction> {
        std::mem::take(&mut self.pending_actions)
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::buzz::{
        orchestrator::notify::NotificationTier,
        signal::{Severity, SignalStatus},
    };

    fn make_signal(source: &str, external_id: &str, severity: Severity) -> SignalRecord {
        SignalRecord {
            id: 1,
            source: source.into(),
            external_id: external_id.into(),
            title: "Signal".into(),
            body: None,
            severity,
            status: SignalStatus::Open,
            url: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            resolved_at: None,
            metadata: None,
            snoozed_until: None,
        }
    }

    #[test]
    fn processes_notification_and_action_together() {
        let mut orchestrator = Orchestrator::new(OrchestratorConfig {
            notification_tiers: std::collections::HashMap::from([(
                "github_ci_failure".to_string(),
                NotificationTier::Chat,
            )]),
            actions: vec![CoordinatorActionConfig {
                trigger: "github_ci_failure".into(),
                action: "Forward the CI error".into(),
            }],
        });

        let notifications =
            orchestrator.process_signal(&make_signal("github_ci_failure", "ci-1", Severity::Error));

        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].tier, NotificationTier::Chat);
        assert_eq!(orchestrator.drain_actions().len(), 1);
    }
}
