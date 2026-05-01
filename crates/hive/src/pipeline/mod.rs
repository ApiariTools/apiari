//! Task pipeline — transforms raw prompts into structured agent execution.
//!
//! Stages: refine → context → plan → dispatch → verify.
//! Each stage produces an artifact (TASK.md, CONTEXT.md, PLAN.md) that is
//! inspectable, diffable, and reusable.

pub mod artifacts;
pub mod context;
pub mod dispatch;
pub mod plan;
pub mod refine;
pub mod verify;

use crate::worker::Worker;
use color_eyre::eyre::Result;
use std::path::Path;
use tokio::sync::mpsc;

/// Progress events emitted at each pipeline stage boundary.
#[derive(Debug, Clone)]
pub enum PipelineEvent {
    RefineComplete { title: String },
    RepoContextStarted { repo: String },
    RepoContextComplete { repo: String, file_count: usize },
    RepoPlanStarted { repo: String },
    RepoPlanComplete { repo: String, step_count: usize },
}

/// Pipeline stage identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PipelineStage {
    Raw,
    Refined,
    Contextualized,
    Planned,
    Dispatched,
    Verified,
    #[allow(dead_code)]
    Failed,
}

impl PipelineStage {
    /// Parse from a CLI string.
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "raw" => Some(Self::Raw),
            "refine" | "refined" => Some(Self::Refined),
            "context" | "contextualized" => Some(Self::Contextualized),
            "plan" | "planned" => Some(Self::Planned),
            "dispatch" | "dispatched" => Some(Self::Dispatched),
            "verify" | "verified" => Some(Self::Verified),
            _ => None,
        }
    }
}

impl std::fmt::Display for PipelineStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Raw => write!(f, "raw"),
            Self::Refined => write!(f, "refined"),
            Self::Contextualized => write!(f, "contextualized"),
            Self::Planned => write!(f, "planned"),
            Self::Dispatched => write!(f, "dispatched"),
            Self::Verified => write!(f, "verified"),
            Self::Failed => write!(f, "failed"),
        }
    }
}

/// Options for the multi-repo pipeline.
pub struct MultiPipelineOptions<'a> {
    pub raw_prompt: &'a str,
    pub workspace_root: &'a Path,
    pub conventions: Option<&'a str>,
    pub repos: Vec<&'a str>,
    pub profile: &'a str,
    pub until: Option<PipelineStage>,
    pub dry_run: bool,
    /// Optional channel for emitting progress events at each stage boundary.
    pub on_progress: Option<mpsc::UnboundedSender<PipelineEvent>>,
}

/// Result of a multi-repo pipeline run.
#[derive(Debug, Clone)]
pub struct MultiPipelineResult {
    pub task_md: String,
    pub title: String,
    pub per_repo: Vec<PerRepoPipelineResult>,
    pub stage_reached: PipelineStage,
}

/// Per-repo pipeline result.
#[derive(Debug, Clone)]
pub struct PerRepoPipelineResult {
    pub repo: String,
    pub context_md: Option<String>,
    pub plan_md: Option<String>,
    pub worktree_id: Option<String>,
}

/// Emit a progress event if a channel is configured.
fn emit(tx: &Option<mpsc::UnboundedSender<PipelineEvent>>, event: PipelineEvent) {
    if let Some(tx) = tx {
        let _ = tx.send(event);
    }
}

/// Run the pipeline for multiple repos: refine (once) → context/plan/dispatch (per repo).
pub async fn run_pipeline_multi(opts: MultiPipelineOptions<'_>) -> Result<MultiPipelineResult> {
    let progress = opts.on_progress;

    // Stage 1: Refine (shared across all repos)
    tracing::info!("Stage 1: Refine");
    let refined = refine::refine(opts.raw_prompt, opts.conventions).await?;
    tracing::info!(title = %refined.title, "Refined");
    emit(
        &progress,
        PipelineEvent::RefineComplete {
            title: refined.title.clone(),
        },
    );

    if opts.until == Some(PipelineStage::Refined) {
        return Ok(MultiPipelineResult {
            task_md: refined.task_md,
            title: refined.title,
            per_repo: Vec::new(),
            stage_reached: PipelineStage::Refined,
        });
    }

    // Build targets: if repos specified, one per repo; otherwise whole workspace.
    struct Target {
        name: String,
        repo_flag: Option<String>,
        search_path: std::path::PathBuf,
    }

    let targets: Vec<Target> = if opts.repos.is_empty() {
        vec![Target {
            name: "workspace".to_string(),
            repo_flag: None,
            search_path: opts.workspace_root.to_path_buf(),
        }]
    } else {
        opts.repos
            .iter()
            .map(|r| Target {
                name: r.to_string(),
                repo_flag: Some(r.to_string()),
                search_path: opts.workspace_root.join(r),
            })
            .collect()
    };

    let until_context = opts.until == Some(PipelineStage::Contextualized);
    let until_plan = opts.until == Some(PipelineStage::Planned) || opts.dry_run;
    let workspace_root = opts.workspace_root.to_path_buf();
    let profile = opts.profile.to_string();
    let task_md_ref = refined.task_md.clone();
    let title_ref = refined.title.clone();

    // Run context + plan for all repos concurrently.
    let mut join_set = tokio::task::JoinSet::new();

    for target in targets {
        let progress = progress.clone();
        let task_md = task_md_ref.clone();
        let title = title_ref.clone();
        let ws_root = workspace_root.clone();
        let profile = profile.clone();

        join_set.spawn(async move {
            let label = &target.name;

            // Stage 2: Context
            tracing::info!(repo = %label, "Context started");
            emit(
                &progress,
                PipelineEvent::RepoContextStarted {
                    repo: label.clone(),
                },
            );
            let ctx = context::identify_context(&task_md, Some(&target.search_path)).await?;
            let file_count = ctx.relevant_files.len();
            tracing::info!(repo = %label, file_count, "Context complete");
            emit(
                &progress,
                PipelineEvent::RepoContextComplete {
                    repo: label.clone(),
                    file_count,
                },
            );

            if until_context {
                return Ok(PerRepoPipelineResult {
                    repo: label.clone(),
                    context_md: Some(ctx.context_md),
                    plan_md: None,
                    worktree_id: None,
                });
            }

            // Stage 3: Plan
            tracing::info!(repo = %label, "Plan started");
            emit(
                &progress,
                PipelineEvent::RepoPlanStarted {
                    repo: label.clone(),
                },
            );
            let planned = plan::create_plan(&task_md, &ctx.context_md).await?;
            let step_count = planned.steps.len();
            tracing::info!(repo = %label, step_count, "Plan complete");
            emit(
                &progress,
                PipelineEvent::RepoPlanComplete {
                    repo: label.clone(),
                    step_count,
                },
            );

            if until_plan {
                return Ok(PerRepoPipelineResult {
                    repo: label.clone(),
                    context_md: Some(ctx.context_md),
                    plan_md: Some(planned.plan_md),
                    worktree_id: None,
                });
            }

            // Stage 4: Dispatch
            tracing::info!(repo = %label, "Dispatch started");
            let prompt = dispatch::build_dispatch_prompt(&title, target.repo_flag.as_deref());
            let worker = Worker::new();
            let worktree_id = worker
                .dispatch_pipeline_task(
                    &ws_root,
                    &prompt,
                    target.repo_flag.as_deref(),
                    &profile,
                    &task_md,
                    Some(&ctx.context_md),
                    Some(&planned.plan_md),
                )
                .await?;
            tracing::info!(repo = %label, worktree_id, "Dispatched");

            Ok(PerRepoPipelineResult {
                repo: label.clone(),
                context_md: Some(ctx.context_md),
                plan_md: Some(planned.plan_md),
                worktree_id: Some(worktree_id),
            })
        });
    }

    // Collect results as they complete.
    let mut per_repo = Vec::new();
    let mut final_stage = PipelineStage::Planned;

    while let Some(result) = join_set.join_next().await {
        let repo_result: color_eyre::Result<PerRepoPipelineResult> =
            result.map_err(|e| color_eyre::eyre::eyre!("pipeline task panicked: {e}"))?;
        let repo_result = repo_result?;
        if repo_result.worktree_id.is_some() {
            final_stage = PipelineStage::Dispatched;
        }
        per_repo.push(repo_result);
    }

    if until_context {
        final_stage = PipelineStage::Contextualized;
    }

    // Sort by repo name for deterministic ordering.
    per_repo.sort_by(|a, b| a.repo.cmp(&b.repo));

    Ok(MultiPipelineResult {
        task_md: refined.task_md,
        title: refined.title,
        per_repo,
        stage_reached: final_stage,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipeline_stage_from_str() {
        assert_eq!(
            PipelineStage::from_str("refine"),
            Some(PipelineStage::Refined)
        );
        assert_eq!(
            PipelineStage::from_str("refined"),
            Some(PipelineStage::Refined)
        );
        assert_eq!(
            PipelineStage::from_str("context"),
            Some(PipelineStage::Contextualized)
        );
        assert_eq!(
            PipelineStage::from_str("plan"),
            Some(PipelineStage::Planned)
        );
        assert_eq!(
            PipelineStage::from_str("dispatch"),
            Some(PipelineStage::Dispatched)
        );
        assert_eq!(
            PipelineStage::from_str("verify"),
            Some(PipelineStage::Verified)
        );
        assert_eq!(PipelineStage::from_str("nope"), None);
    }

    #[test]
    fn pipeline_stage_display() {
        assert_eq!(PipelineStage::Raw.to_string(), "raw");
        assert_eq!(PipelineStage::Refined.to_string(), "refined");
        assert_eq!(PipelineStage::Failed.to_string(), "failed");
    }
}
