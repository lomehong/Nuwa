//! 五色石交付面 v0 — verify → branch → commit → push → PR.
//!
//! Guardrails enforced here (not negotiable by the caller):
//! - independent verification gate: the failing check is re-run by Nuwa, not
//!   trusted from the agent's claim; failure aborts delivery.
//! - branch namespace: `agent/fix/<fingerprint>` only, never the base branch.
//! - one branch per fingerprint: an existing branch aborts (no duplicate PRs).

use std::path::Path;

use anyhow::{bail, Context};

use crate::issue::Issue;
use crate::repair::RepairReport;

pub struct DeliverReport {
    pub branch: String,
    pub base: String,
    pub commit: String,
    pub pr_url: Option<String>,
}

fn git(repo: &Path, args: &[&str]) -> anyhow::Result<String> {
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

struct CheckOutput {
    ok: bool,
    output: String,
}

fn run_check(repo: &Path, command: &str) -> anyhow::Result<CheckOutput> {
    let (program, flag) = if cfg!(windows) { ("cmd", "/C") } else { ("sh", "-c") };
    let out = std::process::Command::new(program)
        .args([flag, command])
        .current_dir(repo)
        .output()
        .with_context(|| format!("run check `{command}`"))?;
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(CheckOutput {
        ok: out.status.success(),
        output: combined,
    })
}

fn root_cause_excerpt(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let cut: String = trimmed.chars().take(max_chars).collect();
    format!("{cut}…")
}

pub fn deliver(
    repo: &Path,
    issue: &Issue,
    report: &RepairReport,
    push: bool,
    create_pr: bool,
    sessions_dir: &Path,
) -> anyhow::Result<DeliverReport> {
    // ---- 1. independent verification gate (复现验证证据) ----
    println!("deliver   : verification gate `{}`…", issue.failing_check.command);
    let check = run_check(repo, &issue.failing_check.command)?;
    if !check.ok {
        bail!(
            "verification gate FAILED — the agent's claim did not reproduce; no branch, no PR:\n{}",
            check.output
        );
    }
    let evidence_path = sessions_dir
        .join(issue.short_fingerprint())
        .join("verification.txt");
    std::fs::create_dir_all(evidence_path.parent().with_context(|| "evidence dir")?)?;
    std::fs::write(
        &evidence_path,
        format!("$ {}\n\n{}", issue.failing_check.command, check.output),
    )
    .context("write evidence")?;

    // ---- 2. preconditions ----
    let status = git(repo, &["status", "--porcelain"])?;
    if status.trim().is_empty() {
        bail!("agent made no changes — refusing to open an empty PR");
    }
    let base = git(repo, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let branch = format!("agent/fix/{}", issue.short_fingerprint());
    if git(repo, &["rev-parse", "--verify", &format!("refs/heads/{branch}")]).is_ok() {
        bail!("branch {branch} already exists — this fingerprint already has work in flight; refusing a duplicate PR");
    }

    // ---- 3. branch + commit ----
    git(repo, &["checkout", "-b", &branch])?;
    git(repo, &["add", "-A"])?;
    let commit_msg = format!(
        "fix: {}\n\nNuwa issue {} (seen {}x, severity {}).\n\n{}\n\nVerification: `{}` passes (evidence: {}).",
        issue.title,
        issue.fingerprint,
        issue.occurrences,
        issue.severity,
        root_cause_excerpt(&report.root_cause_text, 400),
        issue.failing_check.command,
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
    let commit = git(repo, &["rev-parse", "HEAD"])?;
    let stat = git(repo, &["diff", "--stat", &format!("{base}...HEAD")])?;

    // ---- 4. push + PR ----
    let mut pr_url = None;
    if push {
        git(repo, &["push", "-u", "origin", &branch])?;
        if create_pr {
            let body = format!(
                "## 问题单\n\n- fingerprint: `{}`\n- severity: {}, seen {}x\n- link: 山海经 issue `{}`\n\n## 根因\n\n{}\n\n## 改动清单\n\n```text\n{}```\n\n## 验证证据\n\n```text\n{}```\n\n## 会话\n\n- run: `{}`\n- transcript: `{}`\n\n> 交付面护栏：分支 `agent/fix/*`；验证门通过后才提交；同指纹不重复开 PR。CI 与人工合并是最终闸门。\n",
                issue.fingerprint,
                issue.severity,
                issue.occurrences,
                issue.fingerprint,
                root_cause_excerpt(&report.root_cause_text, 800),
                stat,
                format!("$ {}\n{}", issue.failing_check.command, check.output),
                report.run_id,
                sessions_dir.join(issue.short_fingerprint()).display(),
            );
            let body_path = repo.join(".git").join("nuwa-pr-body.md");
            std::fs::write(&body_path, body).context("write PR body")?;
            let out = std::process::Command::new("gh")
                .args(["pr", "create"])
                .arg("--base").arg(&base)
                .arg("--head").arg(&branch)
                .arg("--title").arg(format!("fix: {}", issue.title))
                .arg("--body-file").arg(&body_path)
                .current_dir(repo)
                .output()
                .context("gh pr create")?;
            if !out.status.success() {
                bail!("gh pr create failed: {}", String::from_utf8_lossy(&out.stderr).trim());
            }
            pr_url = Some(String::from_utf8_lossy(&out.stdout).trim().to_string());
        }
    }

    Ok(DeliverReport {
        branch,
        base,
        commit,
        pr_url,
    })
}
