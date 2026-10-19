//! 五色石交付面 v0 — verify → branch → commit → push → PR.
//!
//! Guardrails enforced here (not negotiable by the caller):
//! - independent verification gate: the failing check is re-run by Nuwa, not
//!   trusted from the agent's claim; failure aborts delivery.
//! - branch namespace: `agent/fix/<fingerprint>` only, never the base branch.
//! - one branch per fingerprint: an existing branch aborts (no duplicate PRs).

use std::path::Path;

use anyhow::{bail, Context};

use crate::git_ops;
use crate::issue::Issue;
use crate::repair::RepairReport;

/// Test-only injection point after the branch exists but before the delivery
/// has a durable outcome. Production always passes `None`.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailPoint {
    AfterBranch,
    AfterCommit,
}

/// Restores the runner's base checkout and removes the half-delivered branch
/// on every error path. Without this, a failed commit or PR API call leaves
/// the checkout on `agent/fix/*` and the next preflight refuses to run.
struct DeliveryGuard<'a> {
    repo: &'a Path,
    base: String,
    branch: Option<String>,
    armed: bool,
}

impl<'a> DeliveryGuard<'a> {
    fn new(repo: &'a Path, base: &str) -> Self {
        Self {
            repo,
            base: base.to_string(),
            branch: None,
            armed: true,
        }
    }

    fn branch(&mut self, branch: &str) {
        self.branch = Some(branch.to_string());
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for DeliveryGuard<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // `-f` discards a partially staged/edited tree; the runner owns this
        // checkout and the failed run is already recorded in telemetry. The
        // clean removes untracked files the agent created before the failure.
        let _ = git_ops::git(self.repo, &["checkout", "-f", &self.base]);
        let _ = git_ops::git(
            self.repo,
            &["clean", "-fd", "--", ":(exclude)nuwa.repo.json"],
        );
        if let Some(branch) = &self.branch {
            let _ = git_ops::delete_local_branch(self.repo, branch);
        }
    }
}

#[derive(Debug)]
pub struct DeliverReport {
    pub branch: String,
    pub base: String,
    pub commit: String,
    pub pr_url: Option<String>,
    /// GitHub owner/repo of the delivery remote (None when no PR was created).
    pub slug: Option<String>,
    pub pr_number: Option<u64>,
    /// Verification command plus combined output, bounded (see `truncate_evidence`).
    // Read by the work-loop run telemetry (M1b Task 4).
    pub verification_detail: String,
    /// `git diff --stat` of the delivered branch, bounded (see `truncate_evidence`).
    // Read by the work-loop run telemetry (M1b Task 4).
    pub change_stat: String,
    /// Delivery diff risk profile (tests/CI touched, sizes, forbidden hits).
    pub risk: crate::verify::DiffSummary,
}

pub(crate) fn git(repo: &Path, args: &[&str]) -> anyhow::Result<String> {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .with_context(|| format!("git {}", args.join(" ")))?;
    if !out.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn stack_excerpt(text: &str, max_chars: usize) -> String {
    root_cause_excerpt(text, max_chars)
}

fn root_cause_excerpt(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let cut: String = trimmed.chars().take(max_chars).collect();
    format!("{cut}…")
}

fn truncate_evidence(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let cut: String = trimmed.chars().take(max_chars).collect();
    format!("{cut}…")
}

/// `owner/repo` from a git remote URL (ssh / https, with or without `.git`).
fn parse_github_slug(url: &str) -> Option<String> {
    let trimmed = url.trim().trim_end_matches('/').trim_end_matches(".git");
    for prefix in [
        "git@github.com:",
        "ssh://git@github.com/",
        "https://github.com/",
        "http://github.com/",
    ] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            return normalize_slug(rest);
        }
    }
    None
}

fn normalize_slug(rest: &str) -> Option<String> {
    let mut parts = rest.split('/');
    let owner = parts.next()?.trim();
    let repo = parts.next()?.trim();
    if owner.is_empty() || repo.is_empty() || parts.next().is_some() {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

/// Create the PR through the GitHub REST API. Per ADR-004 the server holds
/// minimal credentials: one fine-grained token with `pulls:write` scoped to
/// the registered repos — no interactive `gh` login, no CLI dependency.
/// Push itself stays on git (deploy key), separate from this token.
async fn create_pr_via_api(
    token: &str,
    slug: &str,
    base: &str,
    branch: &str,
    title: &str,
    body: &str,
    draft: bool,
) -> anyhow::Result<(String, u64)> {
    let resp = reqwest::Client::new()
        .post(format!("https://api.github.com/repos/{slug}/pulls"))
        .bearer_auth(token)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("User-Agent", "nuwa")
        .json(&serde_json::json!({
            "title": title,
            "head": branch,
            "base": base,
            "body": body,
            "draft": draft,
        }))
        .send()
        .await
        .context("POST https://api.github.com/repos/../pulls")?;
    let status = resp.status();
    let payload: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
    if !status.is_success() {
        let details = payload["errors"]
            .as_array()
            .map(|errs| {
                errs.iter()
                    .filter_map(|e| e["message"].as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            })
            .unwrap_or_default();
        bail!(
            "GitHub API {status}: {}{}",
            payload["message"].as_str().unwrap_or("(no message)"),
            if details.is_empty() { String::new() } else { format!(" — {details}") }
        );
    }
    let url = payload["html_url"]
        .as_str()
        .map(str::to_string)
        .context("PR response missing html_url")?;
    let number = payload["number"].as_u64().context("PR response missing number")?;
    Ok((url, number))
}

#[allow(clippy::too_many_arguments)]
pub async fn deliver(
    repo: &Path,
    issue: &Issue,
    report: &RepairReport,
    push: bool,
    create_pr: bool,
    sessions_dir: &Path,
    secrets: &crate::secrets::Secrets,
    public_base_url: Option<&str>,
    draft: bool,
) -> anyhow::Result<DeliverReport> {
    deliver_inner(
        repo,
        issue,
        report,
        push,
        create_pr,
        sessions_dir,
        secrets,
        public_base_url,
        draft,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn deliver_inner(
    repo: &Path,
    issue: &Issue,
    report: &RepairReport,
    push: bool,
    create_pr: bool,
    sessions_dir: &Path,
    secrets: &crate::secrets::Secrets,
    public_base_url: Option<&str>,
    draft: bool,
    fail_point: Option<FailPoint>,
) -> anyhow::Result<DeliverReport> {
    // ---- 1. cheap preconditions ----
    let status = git(repo, &["status", "--porcelain", "--", ":(exclude)nuwa.repo.json"])?;
    if status.trim().is_empty() {
        bail!("agent made no changes — refusing to open an empty PR");
    }
    let base = git(repo, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let branch = format!("agent/fix/{}", issue.short_fingerprint());
    if git(repo, &["rev-parse", "--verify", &format!("refs/heads/{branch}")]).is_ok() {
        bail!("branch {branch} already exists — this fingerprint already has work in flight; refusing a duplicate PR");
    }
    // Created before verification so a failed gate gets the same clean
    // checkout/untracked-file rollback as a failed commit or PR call.
    let mut guard = DeliveryGuard::new(repo, &base);
    guard.branch(&branch);

    // ---- 2. independent verification gate (base red / branch green) ----
    println!("deliver   : verification gate `{}`…", issue.failing_check.command);
    let evidence = crate::verify::verify(repo, issue, &base, secrets).await?;
    let check_command = secrets.redact(&issue.failing_check.command);
    let evidence_detail = secrets.redact(&evidence.detail(issue));
    let evidence_path = sessions_dir
        .join(issue.short_fingerprint())
        .join("verification.txt");
    std::fs::create_dir_all(evidence_path.parent().with_context(|| "evidence dir")?)?;
    std::fs::write(&evidence_path, &evidence_detail).context("write evidence")?;
    let verification_detail = truncate_evidence(&evidence_detail, 16_000);

    // ---- 3. branch + commit ----
    git(repo, &["checkout", "-b", &branch])?;
    if fail_point == Some(FailPoint::AfterBranch) {
        anyhow::bail!("test fail point: after branch");
    }
    // Exclude the Nuwa instruction file from the delivered patch: it is
    // runner configuration, not a fix. Otherwise `add -A` commits it onto
    // the agent branch and a rejected branch takes the repo's config with it.
    git(repo, &["add", "-A", "--", ":(exclude)nuwa.repo.json"])?;
    let root_cause = secrets.redact(&report.root_cause_text);
    let title = secrets.redact(&issue.title);
    let commit_msg = format!(
        "fix: {}\n\nNuwa issue {} (seen {}x, severity {}).\n\n{}\n\nVerification: `{}` — base red, branch green (mode {}, evidence: {}).",
        title,
        issue.fingerprint,
        issue.occurrences,
        issue.severity,
        root_cause_excerpt(&root_cause, 400),
        check_command,
        evidence.mode,
        evidence_path.display(),
    );
    git(
        repo,
        &[
            "-c",
            "user.name=nuwa[bot]",
            "-c",
            "user.email=nuwa@local",
            "commit",
            "-m",
            &commit_msg,
        ],
    )?;
    if fail_point == Some(FailPoint::AfterCommit) {
        anyhow::bail!("test fail point: after commit");
    }
    let commit = git(repo, &["rev-parse", "HEAD"])?;
    let stat = git(repo, &["diff", "--stat", &format!("{base}...HEAD")])?;
    let change_stat = truncate_evidence(&stat, 4_000);

    // ---- 4. push + PR ----
    let mut pr_url = None;
    let mut slug_out = None;
    let mut pr_number = None;
    if push {
        // GitHub connectivity from CN networks flakes — retry transient resets.
        let mut push_result = Err(anyhow::anyhow!("unreachable"));
        for attempt in 1..=3 {
            push_result = git_ops::push_branch(
                repo,
                &branch,
                std::time::Duration::from_secs(120),
            )
            .await;
            if push_result.is_ok() {
                break;
            }
            if attempt < 3 {
                eprintln!(
                    "push retry {attempt}/3 (transient network error), waiting 5s…"
                );
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        }
        // Push exhausted its retries: the guard restores the base checkout and
        // removes the local branch before the error escapes.
        if let Err(e) = push_result {
            return Err(e.context(format!("push failed for branch {branch}")));
        }
        if create_pr {
            let origin = git(repo, &["remote", "get-url", "origin"])
                .context("read origin remote (PR creation needs a GitHub remote)")?;
            let origin_display = git_ops::sanitize_remote(&origin);
            let slug = parse_github_slug(&origin)
                .with_context(|| format!("parse GitHub owner/repo from origin `{origin_display}`"))?;
            let public_base = public_base_url
                .map(|base| base.trim().trim_end_matches('/'))
                .filter(|base| !base.is_empty());
            let issue_link = match public_base {
                Some(base) => format!("[{fp}]({base}/issues/{fp})", fp = issue.fingerprint),
                None => format!("`{}`", issue.fingerprint),
            };
            let transcript = match public_base {
                Some(base) => format!(
                    "- transcript: [{fp}]({base}/issues/{fp})",
                    fp = issue.fingerprint
                ),
                None => "- transcript: (console base URL not configured)".to_string(),
            };
            let prior = if issue.prior_context.trim().is_empty() {
                "(none)".to_string()
            } else {
                issue.prior_context.trim().to_string()
            };
            let stack = {
                let redacted = secrets.redact(&issue.stack_trace);
                let excerpt = stack_excerpt(&redacted, 4_000);
                if excerpt.trim().is_empty() {
                    "(none)".to_string()
                } else {
                    excerpt
                }
            };
            let risk = &evidence.diff;
            let draft_label = if draft { "yes (draft)" } else { "no" };
            let body = format!(
                "## 问题单\n\n- issue: {issue_link}\n- severity: {severity}, seen {seen}x\n- draft: {draft_label}\n\n\
                 ## 风险画像\n\n- files: {files}, +{added}/-{removed}\n- touches tests: {tests}\n- touches CI: {ci}\n- forbidden hits: {forbidden}\n\n\
                 ## 根因\n\n{root_cause}\n\n\
                 ## 改动清单\n\n```text\n{stat}```\n\n\
                 ## 验证证据\n\n```text\n{evidence}```\n\n\
                 ## 前序尝试\n\n{prior}\n\n\
                 ## 堆栈\n\n```text\n{stack}\n```\n\n\
                 ## 会话\n\n- run: `{run_id}`\n{transcript}\n\n\
                 > 交付面护栏：分支 `agent/fix/*`；验证门要求 base 红/分支绿后才提交；同指纹不重复开 PR。CI 与人工合并是最终闸门。\n",
                severity = issue.severity,
                seen = issue.occurrences,
                files = risk.files.len(),
                added = risk.added,
                removed = risk.removed,
                tests = risk.touches_tests,
                ci = risk.touches_ci,
                forbidden = if risk.forbidden_hits.is_empty() {
                    "none".to_string()
                } else {
                    risk.forbidden_hits.join(", ")
                },
                root_cause = root_cause_excerpt(&root_cause, 800),
                evidence = evidence_detail,
                run_id = report.run_id,
            );
            let body_path = repo.join(".git").join("nuwa-pr-body.md");
            std::fs::write(&body_path, body).context("write PR body")?;
            let body = std::fs::read_to_string(&body_path).context("read PR body")?;
            let token = secrets.github_token.as_deref().context(
                "NUWA_GITHUB_TOKEN (or GITHUB_TOKEN) not set — PR creation needs a fine-grained token with pulls:write",
            )?;
            let (url, number) = create_pr_via_api(
                token,
                &slug,
                &base,
                &branch,
                &format!("fix: {title}"),
                &body,
                draft,
            )
            .await?;
            pr_url = Some(url);
            slug_out = Some(slug);
            pr_number = Some(number);
        }
    }

    // 交付完成：保留分支供 PR 使用，checkout 归位 base（远端分支已推送，
    // work 下一轮 preflight 需要 main 上的干净树）。
    guard.disarm();
    let _ = git(repo, &["checkout", &base]);

    Ok(DeliverReport {
        branch,
        base,
        commit,
        pr_url,
        slug: slug_out,
        pr_number,
        verification_detail,
        change_stat,
        risk: evidence.diff,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issue::FailingCheck;

    fn git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git");
        assert!(
            out.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn git_output(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git");
        assert!(
            out.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    fn init_repo(dir: &Path) {
        std::fs::create_dir_all(dir).expect("repo");
        git(dir, &["init", "-b", "main"]);
        git(dir, &["config", "user.name", "test"]);
        git(dir, &["config", "user.email", "test@example.com"]);
        std::fs::write(dir.join("main.go"), "package main\n").expect("write");
        git(dir, &["add", "."]);
        git(dir, &["commit", "-m", "init"]);
    }

    fn check_command() -> String {
        if cfg!(windows) {
            "if exist fix.go (exit 0) else (exit 1)".to_string()
        } else {
            "test -f fix.go".to_string()
        }
    }

    fn check_command_with_secret(secret: &str) -> String {
        if cfg!(windows) {
            format!("if exist fix.go (echo {secret}) else (exit 1)")
        } else {
            format!("test -f fix.go && echo {secret}")
        }
    }

    fn issue() -> Issue {
        Issue {
            fingerprint: "fp-delivery".into(),
            title: "panic: test".into(),
            language: "go".into(),
            occurrences: 1,
            severity: "high".into(),
            first_seen: String::new(),
            last_seen: String::new(),
            stack_trace: "panic: test".into(),
            log_window: Vec::new(),
            prior_context: String::new(),
            failing_check: FailingCheck {
                command: check_command(),
                target: None,
                timeout_secs: 600,
            },
            reproduce_check: None,
            forbidden_paths: Vec::new(),
        }
    }

    fn report(dir: &Path) -> RepairReport {
        RepairReport {
            run_id: "run-test".into(),
            root_cause_text: "ROOT CAUSE: test".into(),
            turns: 1,
            uncached_tokens: 0,
            cached_tokens: 0,
            session_dir: dir.join("sessions").display().to_string(),
        }
    }

    #[tokio::test]
    async fn delivery_failure_restores_base_and_removes_branch() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        init_repo(&repo);
        std::fs::write(repo.join("fix.go"), "package main\n").expect("write");

        let issue = issue();
        let branch = format!("agent/fix/{}", issue.short_fingerprint());
        let err = deliver_inner(
            &repo,
            &issue,
            &report(dir.path()),
            false,
            false,
            &dir.path().join("sessions"),
            &crate::secrets::Secrets::default(),
            None,
            false,
            Some(FailPoint::AfterBranch),
        )
        .await
        .expect_err("fail point must abort delivery");
        assert!(format!("{err:#}").contains("fail point"));
        assert_eq!(
            git_ops::current_branch(&repo).expect("branch"),
            "main",
            "failed delivery must restore the base checkout"
        );
        assert!(
            !git_ops::branch_exists(&repo, &branch).expect("exists"),
            "failed delivery must remove the half-delivered branch"
        );
    }

    #[tokio::test]
    async fn delivery_success_keeps_branch_and_returns_to_base() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        init_repo(&repo);
        std::fs::write(repo.join("fix.go"), "package main\n").expect("write");

        let issue = issue();
        let branch = format!("agent/fix/{}", issue.short_fingerprint());
        let delivered = deliver_inner(
            &repo,
            &issue,
            &report(dir.path()),
            false,
            false,
            &dir.path().join("sessions"),
            &crate::secrets::Secrets::default(),
            None,
            false,
            None,
        )
        .await
        .expect("deliver");
        assert_eq!(delivered.branch, branch);
        assert_eq!(git_ops::current_branch(&repo).expect("branch"), "main");
        assert!(git_ops::branch_exists(&repo, &branch).expect("exists"));
    }

    #[tokio::test]
    async fn delivery_redacts_secrets_in_commit_and_evidence() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        init_repo(&repo);
        std::fs::write(repo.join("fix.go"), "package main\n").expect("write");

        let mut issue = issue();
        issue.title = "panic secret-value-123456".into();
        issue.failing_check.command = check_command_with_secret("secret-value-123456");
        let secrets = crate::secrets::Secrets::with_values(vec![
            "secret-value-123456".to_string(),
        ]);
        deliver_inner(
            &repo,
            &issue,
            &report(dir.path()),
            false,
            false,
            &dir.path().join("sessions"),
            &secrets,
            None,
            false,
            None,
        )
        .await
        .expect("deliver");

        let log = git_output(&repo, &["log", "-1", "--format=%B"]);
        assert!(!log.contains("secret-value-123456"), "commit message leaks: {log}");
        let evidence = std::fs::read_to_string(
            dir.path()
                .join("sessions")
                .join(issue.short_fingerprint())
                .join("verification.txt"),
        )
        .expect("evidence");
        assert!(
            !evidence.contains("secret-value-123456"),
            "verification evidence leaks: {evidence}"
        );
    }

    #[test]
    fn evidence_truncation_keeps_bounded_text() {
        let long = "x".repeat(20_000);
        let out = truncate_evidence(&long, 100);
        assert!(out.ends_with('…'));
        assert_eq!(out.chars().count(), 101);
        assert_eq!(truncate_evidence("short", 100), "short");
    }

    #[test]
    fn parses_ssh_remote() {
        assert_eq!(
            parse_github_slug("git@github.com:acme/spike-target.git"),
            Some("acme/spike-target".to_string())
        );
    }

    #[test]
    fn parses_https_remote() {
        assert_eq!(
            parse_github_slug("https://github.com/acme/spike-target.git"),
            Some("acme/spike-target".to_string())
        );
    }

    #[test]
    fn parses_ssh_protocol_remote() {
        assert_eq!(
            parse_github_slug("ssh://git@github.com/acme/spike-target"),
            Some("acme/spike-target".to_string())
        );
    }

    #[test]
    fn rejects_non_github_and_malformed() {
        assert_eq!(parse_github_slug("https://gitlab.com/acme/spike-target"), None);
        assert_eq!(parse_github_slug("git@github.com:acme"), None);
        assert_eq!(parse_github_slug("/some/local/path"), None);
    }
}
