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
    /// GitHub owner/repo of the delivery remote (None when no PR was created).
    pub slug: Option<String>,
    pub pr_number: Option<u64>,
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
    slug: &str,
    base: &str,
    branch: &str,
    title: &str,
    body: &str,
) -> anyhow::Result<(String, u64)> {
    let token = std::env::var("NUWA_GITHUB_TOKEN")
        .or_else(|_| std::env::var("GITHUB_TOKEN"))
        .context(
            "NUWA_GITHUB_TOKEN (or GITHUB_TOKEN) not set — PR creation needs a fine-grained token with pulls:write",
        )?;
    let resp = reqwest::Client::new()
        .post(format!("https://api.github.com/repos/{slug}/pulls"))
        .bearer_auth(&token)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("User-Agent", "nuwa")
        .json(&serde_json::json!({
            "title": title,
            "head": branch,
            "base": base,
            "body": body,
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

pub async fn deliver(
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
    let status = git(repo, &["status", "--porcelain", "--", ":(exclude)nuwa.repo.json"])?;
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
    // Exclude the Nuwa instruction file from the delivered patch: it is
    // runner configuration, not a fix. Otherwise `add -A` commits it onto
    // the agent branch and a rejected branch takes the repo's config with it.
    git(repo, &["add", "-A", "--", ":(exclude)nuwa.repo.json"])?;
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
    let mut slug_out = None;
    let mut pr_number = None;
    if push {
        // GitHub connectivity from CN networks flakes — retry transient resets.
        let mut push_result = Err(anyhow::anyhow!("unreachable"));
        for attempt in 1..=3 {
            push_result = git(repo, &["push", "-u", "origin", &branch]);
            if push_result.is_ok() {
                break;
            }
            if attempt < 3 {
                eprintln!(
                    "push retry {attempt}/3 (transient network error), waiting 5s…"
                );
                std::thread::sleep(std::time::Duration::from_secs(5));
            }
        }
        // push 耗尽重试：checkout 送回 main（分支保留在本地供取证），
        // 避免工作树停在半交付态导致 runner preflight 持续拒绝服务。
        if let Err(e) = push_result {
            let _ = git(repo, &["checkout", &base]);
            return Err(e.context(format!("branch {branch} kept locally for forensics")));
        }
        if create_pr {
            let origin = git(repo, &["remote", "get-url", "origin"])
                .context("read origin remote (PR creation needs a GitHub remote)")?;
            let slug = parse_github_slug(&origin)
                .with_context(|| format!("parse GitHub owner/repo from origin `{origin}`"))?;
            let body = format!(
                "## 问题单\n\n- fingerprint: `{}`\n- severity: {}, seen {}x\n- link: 山海经 issue `{}`\n\n## 根因\n\n{}\n\n## 改动清单\n\n```text\n{}```\n\n## 验证证据\n\n```text\n{}```\n\n## 会话\n\n- run: `{}`\n- transcript: `{}`\n\n> 交付面护栏：分支 `agent/fix/*`；验证门通过后才提交；同指纹不重复开 PR。CI 与人工合并是最终闸门。\n",
                issue.fingerprint,
                issue.severity,
                issue.occurrences,
                issue.fingerprint,
                root_cause_excerpt(&report.root_cause_text, 800),
                stat,
                format_args!("$ {}\n{}", issue.failing_check.command, check.output),
                report.run_id,
                sessions_dir.join(issue.short_fingerprint()).display(),
            );
            let body_path = repo.join(".git").join("nuwa-pr-body.md");
            std::fs::write(&body_path, body).context("write PR body")?;
            let body = std::fs::read_to_string(&body_path).context("read PR body")?;
            let (url, number) = create_pr_via_api(
                &slug,
                &base,
                &branch,
                &format!("fix: {}", issue.title),
                &body,
            )
            .await?;
            pr_url = Some(url);
            slug_out = Some(slug);
            pr_number = Some(number);
        }
    }

    // 交付完成：checkout 归位 base 分支（远端分支已推送，work 下一轮
    // preflight 需要 main 上的干净树）。
    let _ = git(repo, &["checkout", &base]);

    Ok(DeliverReport {
        branch,
        base,
        commit,
        pr_url,
        slug: slug_out,
        pr_number,
    })
}

#[cfg(test)]
mod tests {
    use super::parse_github_slug;

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
