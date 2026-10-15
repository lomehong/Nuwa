//! 问题单 schema — first draft of the 山海经 issue contract.
//!
//! The issue is the unit of work: one fingerprint, one repair run, at most one
//! `agent/fix/*` branch. Language-specific fields (stack trace shape, log
//! window) arrive pre-parsed by the collector (青鸟); Nuwa treats them as
//! opaque context.

use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Issue {
    /// Stable problem identity (normalized stack + module + error class).
    /// Deduplication, cooldown and branch naming all key off this.
    pub fingerprint: String,
    pub title: String,
    pub language: String,
    #[serde(default)]
    pub occurrences: u64,
    #[serde(default)]
    pub severity: String,
    #[serde(default)]
    pub first_seen: String,
    #[serde(default)]
    pub last_seen: String,
    #[serde(default)]
    pub stack_trace: String,
    /// Log lines around the incident (collector-trimmed, sanitized).
    #[serde(default)]
    pub log_window: Vec<String>,
    pub failing_check: FailingCheck,
    /// Paths the runner must never modify (deploy configs, keys, lockfiles…).
    /// Enforced by a `before_tool_call` veto, not by prompt instructions only.
    #[serde(default)]
    pub forbidden_paths: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FailingCheck {
    /// Shell command that currently fails and must pass after the fix.
    pub command: String,
    #[serde(default)]
    pub target: Option<String>,
}

impl Issue {
    pub fn load(path: &Path) -> anyhow::Result<Issue> {
        let raw = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&raw)?)
    }

    /// Branch- and dir-safe short id: alnum runs joined by `-`, max 16 chars.
    pub fn short_fingerprint(&self) -> String {
        let cleaned: String = self
            .fingerprint
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        cleaned.trim_matches('-').chars().take(16).collect()
    }

    pub fn system_prompt(&self, repo_display: &str) -> String {
        format!(
            "You are 应龙 (Yinglong), Nuwa's repair runner. Exactly one issue is assigned to you in the repository at {repo_display}. Stack: {lang}.\n\
             \n\
             Rules:\n\
             1. Investigate first: read the code named in the stack trace before changing anything.\n\
             2. Make the smallest correct change that fixes the root cause. No drive-by refactors, no formatting churn.\n\
             3. Verify yourself: run `{check}` and keep iterating until it passes. Do not finish while it fails.\n\
             4. These paths are hard-vetoed (attempts are blocked before execution): {forbidden}. Never run `git push`, `git checkout`, `git switch`, `git reset`, `git branch`, `git tag`, or any `gh` command — Nuwa's delivery step owns git and PRs.\n\
             5. Your final message must end with exactly three lines:\n\
             ROOT CAUSE: <one sentence>\n\
             CHANGE: <what you changed and why>\n\
             VERIFICATION: <command> — <result>\n",
            check = self.failing_check.command,
            forbidden = self.forbidden_paths.join(", "),
            lang = self.language,
        )
    }

    pub fn user_prompt(&self) -> String {
        let logs = if self.log_window.is_empty() {
            "(none)".to_string()
        } else {
            self.log_window
                .iter()
                .map(|l| format!("  {l}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        format!(
            "ISSUE {} (severity {}, seen {}x, first {}, last {})\n\
             TITLE: {}\n\
             \n\
             STACK TRACE:\n{}\n\
             \n\
             LOG WINDOW:\n{}\n\
             \n\
             FAILING CHECK: {}{}\n\
             \n\
             The bug is reproducible in this working copy: `{}` currently fails. Locate the root cause, fix it, verify.\n",
            self.fingerprint,
            if self.severity.is_empty() { "n/a" } else { &self.severity },
            self.occurrences,
            self.first_seen,
            self.last_seen,
            self.title,
            if self.stack_trace.is_empty() { "(none)" } else { &self.stack_trace },
            logs,
            self.failing_check.command,
            self.failing_check.target.as_deref().map(|t| format!(" (failing: {t})")).unwrap_or_default(),
            self.failing_check.command,
        )
    }
}
