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
    /// Prior run/owner context assembled by the runner for retries.
    #[serde(default)]
    pub prior_context: String,
    pub failing_check: FailingCheck,
    /// Optional command that proves the bug on the base and the fix on the
    /// branch. When absent, `failing_check` red-green or a `.nuwa/repro.*`
    /// script is used by the delivery gate.
    #[serde(default)]
    pub reproduce_check: Option<FailingCheck>,
    /// Paths the runner must never modify (deploy configs, keys, lockfiles…).
    /// Enforced by a `before_tool_call` veto, not by prompt instructions only.
    #[serde(default)]
    pub forbidden_paths: Vec<String>,
}

fn default_check_timeout() -> u64 {
    600
}

#[derive(Debug, Clone, Deserialize)]
pub struct FailingCheck {
    /// Shell command that currently fails and must pass after the fix.
    pub command: String,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default = "default_check_timeout")]
    pub timeout_secs: u64,
}

/// Branch- and dir-safe short id: alnum runs joined by `-`, max 16 chars.
pub fn short_fingerprint(fingerprint: &str) -> String {
    let cleaned: String = fingerprint
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    cleaned.trim_matches('-').chars().take(16).collect()
}

impl Issue {
    pub fn load(path: &Path) -> anyhow::Result<Issue> {
        let raw = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&raw)?)
    }

    pub fn short_fingerprint(&self) -> String {
        short_fingerprint(&self.fingerprint)
    }

    pub fn system_prompt(&self, repo_display: &str) -> String {
        format!(
            "You are 应龙 (Yinglong), Nuwa's repair runner. Exactly one issue is assigned to you in the repository at {repo_display}. Stack: {lang}.\n\
             \n\
             Rules:\n\
             1. Investigate first: read the code named in the stack trace before changing anything.\n\
             2. Make the smallest correct change that fixes the root cause. No drive-by refactors, no formatting churn.\n\
             3. Verify yourself: run `{check}` and keep iterating until it passes. Do not finish while it fails. If a REPRODUCE CHECK is provided, it must fail before your change and pass after it.\n\
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
        // `prior_context` is assembled by the runner from past runs and the
        // latest owner decision; it is included verbatim (already bounded and
        // redacted) so a retry is not a blind re-roll.

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
             {}\
             {}\
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
            self.reproduce_check
                .as_ref()
                .map(|check| format!("REPRODUCE CHECK: {}\n", check.command))
                .unwrap_or_default(),
            if self.prior_context.trim().is_empty() {
                String::new()
            } else {
                format!("\n{}\n", self.prior_context.trim())
            },
            self.failing_check.command,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue(prior_context: &str) -> Issue {
        Issue {
            fingerprint: "fp".into(),
            title: "panic".into(),
            language: "go".into(),
            occurrences: 1,
            severity: "high".into(),
            first_seen: String::new(),
            last_seen: String::new(),
            stack_trace: "panic: boom".into(),
            log_window: Vec::new(),
            prior_context: prior_context.into(),
            failing_check: FailingCheck {
                command: "go test ./...".into(),
                target: None,
                timeout_secs: 600,
            },
            reproduce_check: None,
            forbidden_paths: Vec::new(),
        }
    }

    #[test]
    fn user_prompt_includes_prior_context() {
        let prompt = issue("OWNER NOTE:\n- retry after deploy").user_prompt();
        assert!(prompt.contains("OWNER NOTE"));
        assert!(prompt.contains("retry after deploy"));
    }

    #[test]
    fn user_prompt_without_prior_context_has_no_header() {
        let prompt = issue("").user_prompt();
        assert!(!prompt.contains("PRIOR ATTEMPTS"));
        assert!(!prompt.contains("OWNER NOTE"));
    }
}
