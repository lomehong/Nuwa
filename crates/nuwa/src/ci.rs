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

/// Poll Actions workflow runs for `sha` until all complete, one fails, or the
/// budget is spent. Uses the `Actions: read` permission (the fine-grained PAT
/// dropdown has no "Checks" entry; check-runs 403s on private repos without
/// it). `total_count == 0` is a transient state (Actions registers runs
/// lazily after the PR event).
pub async fn wait_for_ci(
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
                "https://api.github.com/repos/{slug}/actions/runs?head_sha={sha}"
            ))
            .bearer_auth(token)
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "nuwa")
            .send()
            .await
            .context("poll actions runs")?;
        let status = resp.status();
        let payload: serde_json::Value = resp.json().await.unwrap_or_default();
        if !status.is_success() {
            bail!("actions runs {status}: {}", payload["message"].as_str().unwrap_or("(no message)"));
        }
        let total = payload["total_count"].as_u64().unwrap_or(0);
        let runs: Vec<(bool, Option<&str>)> = payload["workflow_runs"]
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

/// `(slug, number)` from a `…/github.com/{owner}/{repo}/pull/{n}` URL.
pub fn parse_pr_url(url: &str) -> Option<(String, u64)> {
    let marker = "github.com/";
    let idx = url.find(marker)?;
    let rest = &url[idx + marker.len()..];
    let mut parts = rest.split('/');
    let owner = parts.next()?.trim();
    let repo = parts.next()?.trim();
    let kind = parts.next()?.trim();
    let number = parts.next()?.trim().parse::<u64>().ok()?;
    if kind != "pull" || owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some((format!("{owner}/{repo}"), number))
}

/// PR 的合并态：merged / open / closed(未合并).
pub async fn pr_merge_state(token: &str, slug: &str, number: u64) -> anyhow::Result<String> {
    let resp = reqwest::Client::new()
        .get(format!("https://api.github.com/repos/{slug}/pulls/{number}"))
        .bearer_auth(token)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "nuwa")
        .send()
        .await
        .context("GET pulls/../{number}")?;
    let status = resp.status();
    let payload: serde_json::Value = resp.json().await.unwrap_or_default();
    if !status.is_success() {
        bail!("pulls/{number} {status}: {}", payload["message"].as_str().unwrap_or("(no message)"));
    }
    if payload["merged"].as_bool() == Some(true) {
        return Ok("merged".to_string());
    }
    Ok(payload["state"].as_str().unwrap_or("unknown").to_string())
}

/// Journal a terminal lifecycle event (`merged`/`rejected`) that backs the
/// trends buckets (docs/09 §9.2). The `sync` reconciler and the work CI gate
/// share this helper so repo/actor/kind stay consistent.
pub(crate) fn journal_terminal(
    store: &shanhaijing::Store,
    fingerprint: &str,
    repo: &str,
    actor: &str,
    kind: &str,
    detail: &str,
    now: i64,
) -> shanhaijing::ShanResult<()> {
    store.record_event(fingerprint, now, Some(repo), actor, kind, detail)
}

/// 对账全部 pr_open 问题单（CLI `sync` 与 Web 控制台共用）。
/// merged → `merged`；closed 且未合并 → `rejected`；仍 open → 不动。
pub async fn reconcile_all(
    store: &shanhaijing::Store,
    token: &str,
    now: i64,
) -> anyhow::Result<Vec<String>> {
    use shanhaijing::State;
    let mut out = Vec::new();
    for row in store.list_issues().map_err(|e| anyhow::anyhow!(e))? {
        if row.state != "pr_open" {
            continue;
        }
        let Some(url) = &row.pr_url else { continue };
        let Some((slug, number)) = parse_pr_url(url) else {
            out.push(format!("skip       : {} — unparsable PR url {url}", row.fingerprint));
            continue;
        };
        match pr_merge_state(token, &slug, number).await {
            Ok(s) if s == "merged" => {
                store
                    .set_state(&row.fingerprint, State::Merged, now, Some(url))
                    .map_err(|e| anyhow::anyhow!(e))?;
                // Trends bucket producer: `state_changed` alone is not a
                // merge signal (docs/09 §9.2 merged/rejected).
                journal_terminal(
                    store,
                    &row.fingerprint,
                    &row.repo,
                    "system",
                    "merged",
                    url,
                    now,
                )
                .map_err(|e| anyhow::anyhow!(e))?;
                out.push(format!("merged     : {} → {url}", row.fingerprint));
            }
            Ok(s) if s == "closed" => {
                store
                    .set_state(
                        &row.fingerprint,
                        State::Rejected,
                        now,
                        Some("PR closed without merge"),
                    )
                    .map_err(|e| anyhow::anyhow!(e))?;
                let detail = format!("PR #{number} closed without merge");
                journal_terminal(
                    store,
                    &row.fingerprint,
                    &row.repo,
                    "system",
                    "rejected",
                    &detail,
                    now,
                )
                .map_err(|e| anyhow::anyhow!(e))?;
                out.push(format!(
                    "rejected   : {} (PR #{number} closed without merge)",
                    row.fingerprint
                ));
            }
            _ => out.push(format!("still open : {} — PR #{number}", row.fingerprint)),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::aggregate;
    use super::journal_terminal;
    use super::Verdict;

    fn run(completed: bool, conclusion: Option<&str>) -> (bool, Option<&str>) {
        (completed, conclusion)
    }

    #[test]
    fn all_green_is_success() {
        let runs = [run(true, Some("success")), run(true, Some("neutral"))];
        assert_eq!(aggregate(2, &runs), Some(Verdict::Success));
    }

    #[test]
    fn any_failure_is_failure() {
        let runs = [
            run(true, Some("success")),
            run(true, Some("failure")),
            run(false, None),
        ];
        assert_eq!(aggregate(3, &runs), Some(Verdict::Failure));
    }

    #[test]
    fn pending_runs_keep_polling() {
        let runs = [run(true, Some("success")), run(false, None)];
        assert_eq!(aggregate(2, &runs), None);
    }

    #[test]
    fn no_registered_runs_keep_polling() {
        assert_eq!(aggregate(0, &[]), None);
    }

    #[test]
    fn conclusion_missing_counts_unfinished() {
        let runs = [run(true, None)];
        assert_eq!(aggregate(1, &runs), None);
    }

    #[test]
    fn journal_terminal_writes_repo_actor_and_kind() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = shanhaijing::Store::open(dir.path().join("s.db")).expect("open");
        let fp = "fp-terminal";
        let repo = "example.com/app";

        journal_terminal(&store, fp, repo, "system", "merged", "https://example.com/pull/1", 100)
            .expect("merged event");
        journal_terminal(
            &store,
            fp,
            repo,
            "runner",
            "rejected",
            "ci_red: PR #2 closed without merge",
            200,
        )
        .expect("rejected event");

        let events = store.events_for(fp, 10).expect("events");
        assert_eq!(events.len(), 2);
        // Newest first: the CI-red rejection.
        assert_eq!(events[0].kind, "rejected");
        assert_eq!(events[0].actor.as_deref(), Some("runner"));
        assert_eq!(events[0].repo, repo);
        assert_eq!(events[0].ts, 200);
        assert_eq!(
            events[0].detail,
            "ci_red: PR #2 closed without merge"
        );
        assert_eq!(events[1].kind, "merged");
        assert_eq!(events[1].actor.as_deref(), Some("system"));
        assert_eq!(events[1].repo, repo);
        assert_eq!(events[1].ts, 100);
    }
}
