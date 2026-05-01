//! GitHub CLI wrapper — issue and PR operations via the `gh` CLI.
//!
//! All operations shell out to `gh` via `tokio::process::Command`. This avoids
//! pulling in a GitHub API client and reuses the user's existing authentication.

use color_eyre::eyre::{Result, WrapErr};
use serde::{Deserialize, Serialize};

/// A GitHub issue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    /// Issue number.
    pub number: u32,
    /// Issue title.
    pub title: String,
    /// Issue body text.
    #[serde(default)]
    pub body: String,
    /// Issue state ("open", "closed").
    pub state: String,
    /// Issue labels.
    #[serde(default)]
    pub labels: Vec<Label>,
    /// Issue URL.
    #[serde(default)]
    pub url: String,
}

/// A GitHub label.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Label {
    /// Label name.
    pub name: String,
}

/// Information about a created or existing pull request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrInfo {
    /// PR number.
    pub number: u32,
    /// PR title.
    pub title: String,
    /// PR URL.
    pub url: String,
    /// PR state ("open", "closed", "merged").
    pub state: String,
}

/// Wrapper around the `gh` CLI for GitHub operations.
pub struct GitHubCli {
    /// Path to the gh binary. Defaults to "gh".
    gh_bin: String,
}

impl GitHubCli {
    /// Create a new GitHub CLI wrapper.
    pub fn new() -> Self {
        Self {
            gh_bin: "gh".to_owned(),
        }
    }

    /// List open issues for a repository.
    ///
    /// `repo` should be in "owner/repo" format.
    pub async fn list_issues(&self, repo: &str) -> Result<Vec<Issue>> {
        let output = tokio::process::Command::new(&self.gh_bin)
            .args([
                "issue",
                "list",
                "--repo",
                repo,
                "--json",
                "number,title,body,state,labels,url",
                "--limit",
                "50",
            ])
            .output()
            .await
            .wrap_err("failed to run gh issue list")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            color_eyre::eyre::bail!("gh issue list failed: {stderr}");
        }

        let issues: Vec<Issue> = serde_json::from_slice(&output.stdout)
            .wrap_err("failed to parse gh issue list output")?;

        Ok(issues)
    }

    /// Get a single issue by number.
    ///
    /// `repo` should be in "owner/repo" format.
    pub async fn get_issue(&self, repo: &str, number: u32) -> Result<Issue> {
        let output = tokio::process::Command::new(&self.gh_bin)
            .args([
                "issue",
                "view",
                &number.to_string(),
                "--repo",
                repo,
                "--json",
                "number,title,body,state,labels,url",
            ])
            .output()
            .await
            .wrap_err("failed to run gh issue view")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            color_eyre::eyre::bail!("gh issue view failed: {stderr}");
        }

        let issue: Issue = serde_json::from_slice(&output.stdout)
            .wrap_err("failed to parse gh issue view output")?;

        Ok(issue)
    }

    /// Create a pull request.
    ///
    /// `repo` should be in "owner/repo" format.
    pub async fn create_pr(
        &self,
        repo: &str,
        title: &str,
        body: &str,
        branch: &str,
    ) -> Result<PrInfo> {
        let output = tokio::process::Command::new(&self.gh_bin)
            .args([
                "pr",
                "create",
                "--repo",
                repo,
                "--title",
                title,
                "--body",
                body,
                "--head",
                branch,
                "--json",
                "number,title,url,state",
            ])
            .output()
            .await
            .wrap_err("failed to run gh pr create")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            color_eyre::eyre::bail!("gh pr create failed: {stderr}");
        }

        let pr: PrInfo = serde_json::from_slice(&output.stdout)
            .wrap_err("failed to parse gh pr create output")?;

        Ok(pr)
    }

    /// Get the status of an existing PR.
    pub async fn get_pr(&self, repo: &str, number: u32) -> Result<PrInfo> {
        let output = tokio::process::Command::new(&self.gh_bin)
            .args([
                "pr",
                "view",
                &number.to_string(),
                "--repo",
                repo,
                "--json",
                "number,title,url,state",
            ])
            .output()
            .await
            .wrap_err("failed to run gh pr view")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            color_eyre::eyre::bail!("gh pr view failed: {stderr}");
        }

        let pr: PrInfo =
            serde_json::from_slice(&output.stdout).wrap_err("failed to parse gh pr view output")?;

        Ok(pr)
    }
}

impl Default for GitHubCli {
    fn default() -> Self {
        Self::new()
    }
}
