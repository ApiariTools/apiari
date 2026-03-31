use serde::{Deserialize, Serialize};

use crate::buzz::signal::SignalRecord;

use super::notify::classify_signal;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoordinatorActionConfig {
    pub trigger: String,
    pub action: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggeredAction {
    pub trigger: String,
    pub action: String,
}

pub fn actions_for_signal(
    signal: &SignalRecord,
    configured_actions: &[CoordinatorActionConfig],
) -> Vec<TriggeredAction> {
    let trigger = classify_signal(signal);

    configured_actions
        .iter()
        .filter(|candidate| candidate.trigger == trigger)
        .map(|candidate| TriggeredAction {
            trigger: trigger.clone(),
            action: candidate.action.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::buzz::signal::{Severity, SignalStatus};

    fn make_signal(source: &str, external_id: &str) -> SignalRecord {
        SignalRecord {
            id: 1,
            source: source.into(),
            external_id: external_id.into(),
            title: "Signal".into(),
            body: None,
            severity: Severity::Info,
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
    fn matches_classified_trigger() {
        let actions = vec![CoordinatorActionConfig {
            trigger: "swarm_pr_opened".into(),
            action: "Review the PR".into(),
        }];

        let matched = actions_for_signal(&make_signal("swarm", "swarm-pr-worker-1"), &actions);

        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].trigger, "swarm_pr_opened");
    }
}
