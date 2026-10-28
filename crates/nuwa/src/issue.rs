//! 问题单 schema — first draft of the 山海经 issue contract.
//!
//! The issue is the unit of work: one fingerprint, one repair run, at most one
//! `agent/fix/*` branch. Language-specific fields (stack trace shape, log
//! window) arrive pre-parsed by the collector (青鸟); Nuwa treats them as
//! opaque context.

use std::path::Path;

use serde::Deserialize;

/// How the delivery gate will prove the bug is real and fixed.
///
/// The runner probes the base before the repair starts, so the prompt tells
/// the agent what the gate will actually require. Without this the agent was
/// always told "the check currently fails" — false in the common production
/// case (tests green, panic at runtime), where the only way through the gate
/// is to write a reproducer, and the agent was never told to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationMode {
    /// Nuwa froze and proved an owner oracle before the first repair write.
    OwnerTrusted,
    /// `reproduce_check` is configured: it must be red on base, green on the
    /// branch.
    Configured,
    /// No reproducer configured and `failing_check` is red on the base: the
    /// check doubles as the reproducer.
    #[default]
    FailingCheck,
    /// No reproducer configured and `failing_check` is green on the base: the
    /// branch must add `.nuwa/repro.sh` (`.nuwa/repro.cmd` on Windows).
    Reproducer,
}

impl VerificationMode {
    pub fn as_str(self) -> &'static str {
        match self {
            VerificationMode::OwnerTrusted => "owner_trusted",
            VerificationMode::Configured => "configured",
            VerificationMode::FailingCheck => "failing_check",
            VerificationMode::Reproducer => "reproducer",
        }
    }
}

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
    /// What the delivery gate will require; decided by probing the base before
    /// the repair run.
    #[serde(default)]
    pub verification_mode: VerificationMode,
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
        let verify_rule = match self.verification_mode {
            VerificationMode::OwnerTrusted => format!(
                "3. Nuwa has frozen an owner-provided oracle and confirmed it fails on the base. \
                 Fix the application code and run `{}` until it passes. Do not modify protected \
                 oracle inputs or configuration. Nuwa independently runs the same immutable oracle \
                 on base and candidate; agent-created reproducers and your report are not trusted proof.",
                self.failing_check.command
            ),
            VerificationMode::Reproducer => format!(
                "3. `{check}` already PASSES on the base commit — the bug only shows up at runtime. \
                 Add a self-contained reproducer at `.nuwa/repro.sh` (Windows: `.nuwa/repro.cmd`) \
                 that exits non-zero on the unfixed code and zero after your fix. The delivery \
                 gate copies it onto a clean base commit and requires red→green. Do not edit the check itself.",
                check = self.failing_check.command
            ),
            VerificationMode::Configured | VerificationMode::FailingCheck => format!(
                "3. Verify yourself: run `{check}` and keep iterating until it passes. \
                 Do not weaken or delete the check{extra}.",
                check = self.failing_check.command,
                extra = if self.reproduce_check.is_some() {
                    "; a REPRODUCE CHECK is provided and must fail before your change and pass after it"
                } else {
                    ""
                }
            ),
        };
        format!(
            "You are 应龙 (Yinglong), Nuwa's repair runner. Exactly one issue is assigned to you in the repository at {repo_display}. Stack: {lang}.\n\
             \n\
             Rules:\n\
             1. Investigate first: read the code named in the stack trace before changing anything.\n\
             2. Make the smallest correct change that fixes the root cause. No drive-by refactors, no formatting churn.\n\
             {verify_rule}\n\
             4. These paths are blocked by a tool guard and re-checked against the delivered diff: {forbidden}. Never run `git push`, `git commit`, `git checkout`, `git switch`, `git reset`, `git branch`, `git tag`, `git remote`, `git config`, or any `gh` command — Nuwa's delivery step owns git and PRs.\n\
             5. Report exactly one OUTCOME: fixed only when the repair is complete and verification passed; not_fixed when the bug remains or verification failed; unverified when verification was not run or is inconclusive. Your final message must end with exactly four lines:\n\
             ROOT CAUSE: <one sentence>\n\
             CHANGE: <what you changed and why>\n\
             VERIFICATION: <command> — <result>\n\
             OUTCOME: <fixed|not_fixed|unverified>\n",
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
        let closing = match self.verification_mode {
            VerificationMode::OwnerTrusted => format!(
                "The owner oracle failed on the pinned base. Locate and fix the root cause, and run `{}`. \
                 The normal check may already be green. Leave owner oracle inputs unchanged; Nuwa \
                 verifies the fix independently using its frozen oracle, not an agent-created proof.\n",
                self.failing_check.command
            ),
            VerificationMode::Reproducer => format!(
                "`{check}` passes on this base commit, so it cannot prove the bug. Write a self-contained reproducer at `.nuwa/repro.sh` (`.nuwa/repro.cmd` on Windows) that fails before your change and passes after it, fix the root cause, and keep iterating until both the reproducer and `{check}` are green.\n",
                check = self.failing_check.command
            ),
            VerificationMode::Configured => format!(
                "The bug is reproducible in this working copy: `{check}` currently fails{extra}. Locate the root cause, fix it, verify both checks.\n",
                check = self.failing_check.command,
                extra = self
                    .reproduce_check
                    .as_ref()
                    .map(|c| format!(" and `{}` must fail before your change and pass after it", c.command))
                    .unwrap_or_default()
            ),
            VerificationMode::FailingCheck => format!(
                "The bug is reproducible in this working copy: `{check}` currently fails. Locate the root cause, fix it, and keep iterating until it passes.\n",
                check = self.failing_check.command
            ),
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
             {}",
            self.fingerprint,
            if self.severity.is_empty() {
                "n/a"
            } else {
                &self.severity
            },
            self.occurrences,
            self.first_seen,
            self.last_seen,
            self.title,
            if self.stack_trace.is_empty() {
                "(none)"
            } else {
                &self.stack_trace
            },
            logs,
            self.failing_check.command,
            self.failing_check
                .target
                .as_deref()
                .map(|t| format!(" (failing: {t})"))
                .unwrap_or_default(),
            self.reproduce_check
                .as_ref()
                .map(|check| format!("REPRODUCE CHECK: {}\n", check.command))
                .unwrap_or_default(),
            if self.prior_context.trim().is_empty() {
                String::new()
            } else {
                format!("\n{}\n", self.prior_context.trim())
            },
            closing,
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
            verification_mode: Default::default(),
        }
    }

    #[test]
    fn final_report_requires_an_explicit_outcome() {
        let prompt = issue("").system_prompt("/repo");
        assert!(prompt.contains("exactly four lines"), "{prompt}");
        let fields: Vec<_> = prompt.lines().rev().take(4).collect();
        assert_eq!(fields[0], "OUTCOME: <fixed|not_fixed|unverified>");
        assert!(fields[1].starts_with("VERIFICATION:"));
        assert!(fields[2].starts_with("CHANGE:"));
        assert!(fields[3].starts_with("ROOT CAUSE:"));
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

    /// 复现模式必须对 agent 可见：base 上 failing_check 是绿的，唯一能通过
    /// 交付门的做法是写 `.nuwa/repro.sh`；prompt 说"check 当前失败"会让它
    /// 必然踩空（曾经所有 base 绿的单都在交付门硬失败）。
    #[test]
    fn reproducer_mode_tells_the_agent_to_write_a_reproducer() {
        let mut issue = issue("");
        issue.verification_mode = VerificationMode::Reproducer;
        let prompt = issue.user_prompt();
        assert!(prompt.contains(".nuwa/repro.sh"), "{prompt}");
        assert!(prompt.contains("passes on this base commit"), "{prompt}");
        assert!(
            !prompt.contains("currently fails"),
            "reproducer mode must not claim the check fails: {prompt}"
        );
        let system = issue.system_prompt("/repo");
        assert!(system.contains(".nuwa/repro.sh"), "{system}");
        assert!(system.contains("PASSES on the base commit"), "{system}");
    }

    #[test]
    fn failing_check_mode_states_the_check_currently_fails() {
        let mut issue = issue("");
        issue.verification_mode = VerificationMode::FailingCheck;
        let prompt = issue.user_prompt();
        assert!(prompt.contains("currently fails"), "{prompt}");
        assert!(!prompt.contains(".nuwa/repro.sh"), "{prompt}");
    }

    #[test]
    fn configured_mode_mentions_the_reproduce_check() {
        let mut issue = issue("");
        issue.verification_mode = VerificationMode::Configured;
        issue.reproduce_check = Some(FailingCheck {
            command: "sh .nuwa/repro.sh".into(),
            target: None,
            timeout_secs: 300,
        });
        let prompt = issue.user_prompt();
        assert!(
            prompt.contains("REPRODUCE CHECK: sh .nuwa/repro.sh"),
            "{prompt}"
        );
        assert!(prompt.contains("must fail before your change"), "{prompt}");
        let system = issue.system_prompt("/repo");
        assert!(system.contains("must fail before your change"), "{system}");
    }

    #[test]
    fn trusted_mode_never_requests_agent_generated_proof() {
        let mut issue = issue("");
        issue.verification_mode = VerificationMode::OwnerTrusted;
        for prompt in [issue.system_prompt("/repo"), issue.user_prompt()] {
            assert!(prompt.contains("oracle"));
            assert!(!prompt.contains(".nuwa/repro"));
            assert!(!prompt.contains("currently fails"));
        }
    }
}
