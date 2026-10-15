//! CI 闸门消费 — docs/03 流水线末段："CI 闸门：不过测试 → 自动关 PR，
//! 问题单回炉升级给所有者"。
//!
//! v0 polls the check-runs API (no webhook endpoint needed); merge stays
//! human (ADR-003) — nuwa only acts on red: close the PR, flip the issue.

use anyhow::{bail, Context};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// All completed checks green (neutral/skipped count as non-blocking).
    Success,
    /// At least one completed check failed.
    Failure,
    /// Checks not finished within the budget — inconclusive, leave the PR.
    TimedOut,
}

/// Aggregate one check-runs page. `None` = keep polling.
/// `runs`: (status, conclusion) pairs as the API reports them.
pub fn aggregate(total: u64, runs: &[(bool, Option<&str>)]) -> Option<Verdict> {
    if total == 0 {
        return None; // checks not registered yet (Actions startup delay)
    }
    let mut completed = 0u64;
    let mut failed = 0u64;
    let mut green = 0u64;
    for (is_completed, conclusion) in runs {
        if !is_completed {
            continue;
        }
        completed += 1;
        match *conclusion {
            Some("success") | Some("neutral") | Some("skipped") => green += 1,
            Some(_) => failed += 1,
            None => {}
        }
    }
    if failed > 0 {
        return Some(Verdict::Failure);
    }
    if completed == total && green == total {
        return Some(Verdict::Success);
    }
    None
}

/// Poll check-runs for `sha` until all complete, one fails, or the budget is
/// spent. `total == 0` is a transient state (Actions registers runs lazily).
pub async fn wait_for_checks(
    token: &str,
    slug: &str,
    sha: &str,
    timeout_secs: u64,
) -> anyhow::Result<Verdict> {
    let start = std::time::Instant::now();
    let http = reqwest::Client::new();
    loop {
        let resp = http
            .get(format!(
                "https://api.github.com/repos/{slug}/commits/{sha}/check-runs"
            ))
            .bearer_auth(token)
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "nuwa")
            .send()
            .await
            .context("poll check-runs")?;
        let status = resp.status();
        let payload: serde_json::Value = resp.json().await.unwrap_or_default();
        if !status.is_success() {
            bail!("check-runs {status}: {}", payload["message"].as_str().unwrap_or("(no message)"));
        }
        let total = payload["total_count"].as_u64().unwrap_or(0);
        let runs: Vec<(bool, Option<&str>)> = payload["check_runs"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .map(|r| {
                        (
                            r["status"].as_str() == Some("completed"),
                            r["conclusion"].as_str(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        if let Some(verdict) = aggregate(total, &runs) {
            return Ok(verdict);
        }
        if start.elapsed().as_secs() >= timeout_secs {
            return Ok(Verdict::TimedOut);
        }
        tokio::time::sleep(std::time::Duration::from_secs(15)).await;
    }
}

/// Close a PR (rejected path). Needs the same token's pulls:write.
pub async fn close_pr(token: &str, slug: &str, number: u64) -> anyhow::Result<()> {
    let resp = reqwest::Client::new()
        .patch(format!("https://api.github.com/repos/{slug}/pulls/{number}"))
        .bearer_auth(token)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "nuwa")
        .json(&serde_json::json!({ "state": "closed" }))
        .send()
        .await
        .context("PATCH pulls/../{number}")?;
    let status = resp.status();
    if !status.is_success() {
        bail!("close PR {status}");
    }
    Ok(())
}
