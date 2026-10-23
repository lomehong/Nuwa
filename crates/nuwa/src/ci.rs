//! CI 闸门消费 — docs/03 流水线末段："CI 闸门：不过测试 → 自动关 PR，
//! 问题单回炉升级给所有者"。
//!
//! v0 polls the check-runs API (no webhook endpoint needed); merge stays
//! human (ADR-003) — nuwa only acts on red: close the PR, flip the issue.

use anyhow::{bail, Context};
use serde::Serialize;

/// A bounded HTTP client for GitHub API calls.
///
/// Every GitHub request must carry a connect + response timeout: a connection
/// that opens but never returns a body would otherwise block the serial runner
/// forever, and the CI budget is only checked *after* a request completes, so
/// an unbounded request defeats it. Shared by the CI gate, PR close and PR
/// creation (deliver.rs).
pub(crate) fn github_http_client() -> anyhow::Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .context("build GitHub HTTP client")
}

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
    let http = github_http_client()?;
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
            bail!(
                "actions runs {status}: {}",
                payload["message"].as_str().unwrap_or("(no message)")
            );
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
    let resp = github_http_client()?
        .patch(format!(
            "https://api.github.com/repos/{slug}/pulls/{number}"
        ))
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
    let resp = github_http_client()?
        .get(format!(
            "https://api.github.com/repos/{slug}/pulls/{number}"
        ))
        .bearer_auth(token)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "nuwa")
        .send()
        .await
        .context("GET pulls/../{number}")?;
    let status = resp.status();
    let payload: serde_json::Value = resp.json().await.unwrap_or_default();
    if !status.is_success() {
        bail!(
            "pulls/{number} {status}: {}",
            payload["message"].as_str().unwrap_or("(no message)")
        );
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

/// Structured form of a `reconcile_all` result, for the Web console
/// `POST /api/v1/sync` (docs/09 §9.2). The CLI keeps printing the raw lines.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct SyncSummary {
    /// pr_open issues the reconciler examined (one result line each).
    pub checked: u64,
    pub merged: u64,
    pub rejected: u64,
    pub still_open: u64,
    /// Per-issue failures (`skip`/`error` lines), plus unknown prefixes.
    pub errors: Vec<SyncError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct SyncError {
    pub fingerprint: String,
    pub message: String,
}

/// Count `reconcile_all` result lines by their fixed prefixes: `merged`,
/// `rejected`, `still open`, plus the two error lines `skip` (unparsable PR
/// url) and `error` (per-issue GitHub failure, docs/09 §9.2). Unknown prefixes
/// become errors too instead of vanishing from the counters.
pub fn summarize_lines(lines: &[String]) -> SyncSummary {
    let mut summary = SyncSummary {
        checked: lines.len() as u64,
        ..SyncSummary::default()
    };
    for line in lines {
        let Some((prefix, rest)) = line.split_once(':') else {
            summary.errors.push(SyncError {
                fingerprint: String::new(),
                message: line.clone(),
            });
            continue;
        };
        let rest = rest.trim();
        match prefix.trim() {
            "merged" => summary.merged += 1,
            "rejected" => summary.rejected += 1,
            "still open" => summary.still_open += 1,
            // Both carry `fingerprint — message`; both stay 200-level errors
            // in the console summary, not failed reconciles.
            "skip" | "error" => {
                let (fingerprint, message) = rest
                    .split_once(" — ")
                    .map(|(fp, message)| (fp.trim().to_string(), message.trim().to_string()))
                    .unwrap_or_else(|| (String::new(), rest.to_string()));
                summary.errors.push(SyncError {
                    fingerprint,
                    message,
                });
            }
            _ => summary.errors.push(SyncError {
                fingerprint: String::new(),
                message: line.clone(),
            }),
        }
    }
    summary
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
        let Some(url) = &row.pr_url else {
            // A branch-only delivery (or a failed set_pr) used to leave the
            // issue in pr_open forever: every reconcile skipped it and
            // reopen/requeue reject pr_open. Surface it as rejected so the
            // owner can inspect/reopen instead of the row going stale.
            store
                .set_state(
                    &row.fingerprint,
                    State::Rejected,
                    now,
                    Some("pr_open without PR URL — branch-only delivery"),
                )
                .map_err(|e| anyhow::anyhow!(e))?;
            journal_terminal(
                store,
                &row.fingerprint,
                &row.repo,
                "system",
                "rejected",
                "pr_open without PR URL",
                now,
            )
            .map_err(|e| anyhow::anyhow!(e))?;
            out.push(format!("rejected   : {} (no PR URL)", row.fingerprint));
            continue;
        };
        let Some((slug, number)) = parse_pr_url(url) else {
            out.push(format!(
                "skip       : {} — unparsable PR url {url}",
                row.fingerprint
            ));
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
                let detail = format!("PR #{number} closed without merge");
                if store.reject_pr(&row.fingerprint, url, now, &detail, "system")? {
                    out.push(format!(
                        "rejected   : {} (PR #{number} closed without merge)",
                        row.fingerprint
                    ));
                } else {
                    out.push(format!(
                        "skip       : {} — issue state changed during reconcile",
                        row.fingerprint
                    ));
                }
            }
            Ok(_) => out.push(format!("still open : {} — PR #{number}", row.fingerprint)),
            // Per-issue GitHub failures stay 200-level for the console: the
            // summary's `errors[]` surfaces them, and the other issues are
            // still reconciled (docs/09 §9.2). Only local store failures
            // abort the whole pass.
            Err(error) => out.push(format!("error      : {} — {error}", row.fingerprint)),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::aggregate;
    use super::journal_terminal;
    use super::summarize_lines;
    use super::SyncError;
    use super::Verdict;

    #[tokio::test]
    async fn pr_open_without_url_is_rejected_instead_of_staying_stuck() {
        use shanhaijing::{State, Store};

        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        let t = 1_000_000;
        let signal = shanhaijing::Signal {
            repo: "example.com/app".into(),
            language: "go".into(),
            title: "panic: no pr".into(),
            severity: "high".into(),
            message: None,
            log_window: Vec::new(),
            stack_trace:
                "panic: no pr\ngoroutine 1 [running]:\nexample.com/app.F()\n\t/app/a.go:1 +0x1"
                    .into(),
        };
        let fp = match store.report_signal(&signal, t).expect("report") {
            shanhaijing::ReportOutcome::NewIssue { fingerprint } => fingerprint,
            other => panic!("expected new, got {other:?}"),
        };
        store
            .set_state(&fp, State::Triggered, t, None)
            .expect("triggered");
        store
            .set_state(&fp, State::Fixing, t, None)
            .expect("fixing");
        store
            .set_state(&fp, State::PrOpen, t, None)
            .expect("pr_open");

        let lines = crate::ci::reconcile_all(&store, "unused-token", t + 1)
            .await
            .expect("reconcile");
        assert!(
            lines.iter().any(|line| line.contains("no PR URL")),
            "{lines:?}"
        );
        let row = store
            .list_issues()
            .expect("issues")
            .into_iter()
            .find(|row| row.fingerprint == fp)
            .expect("issue");
        assert_eq!(row.state, "rejected");
        assert_eq!(
            store.attempts_for(&fp).expect("attempts"),
            0,
            "branch-only reconcile is not an issue-level repair failure"
        );
    }

    #[test]
    fn summarize_lines_counts_each_prefix_and_parses_skips() {
        let lines = vec![
            "merged     : fp-a → https://github.com/o/r/pull/1".to_string(),
            "rejected   : fp-b (PR #2 closed without merge)".to_string(),
            "still open : fp-c — PR #3".to_string(),
            "still open : fp-d — PR #4".to_string(),
            "skip       : fp-e — unparsable PR url not-a-url".to_string(),
        ];

        let summary = summarize_lines(&lines);

        assert_eq!(summary.checked, 5);
        assert_eq!(summary.merged, 1);
        assert_eq!(summary.rejected, 1);
        assert_eq!(summary.still_open, 2);
        assert_eq!(
            summary.errors,
            vec![SyncError {
                fingerprint: "fp-e".to_string(),
                message: "unparsable PR url not-a-url".to_string(),
            }]
        );
    }

    // The full result vocabulary: counters for terminal/non-terminal states
    // and both per-issue error lines surfaced in `errors[]`.
    #[test]
    fn summarize_lines_parses_merged_rejected_open_skip_and_error() {
        let lines = vec![
            "merged     : fp-a → https://github.com/o/r/pull/1".to_string(),
            "rejected   : fp-b (PR #2 closed without merge)".to_string(),
            "still open : fp-c — PR #3".to_string(),
            "skip       : fp-d — unparsable PR url nope".to_string(),
            "error      : fp-e — pulls/5 502: Bad Gateway".to_string(),
        ];

        let summary = summarize_lines(&lines);

        assert_eq!(summary.checked, 5, "every result line is checked");
        assert_eq!(summary.merged, 1);
        assert_eq!(summary.rejected, 1);
        assert_eq!(summary.still_open, 1);
        assert_eq!(
            summary.errors,
            vec![
                SyncError {
                    fingerprint: "fp-d".to_string(),
                    message: "unparsable PR url nope".to_string(),
                },
                SyncError {
                    fingerprint: "fp-e".to_string(),
                    message: "pulls/5 502: Bad Gateway".to_string(),
                },
            ],
            "GitHub per-issue failures must not be folded into still_open"
        );
    }

    #[test]
    fn summarize_lines_of_an_empty_reconcile_is_all_zero() {
        let summary = summarize_lines(&[]);
        assert_eq!(summary.checked, 0);
        assert_eq!(summary.merged, 0);
        assert_eq!(summary.rejected, 0);
        assert_eq!(summary.still_open, 0);
        assert!(summary.errors.is_empty());
    }

    // A future line kind must surface as an error instead of vanishing from
    // the console counters.
    #[test]
    fn summarize_lines_surfaces_unrecognized_lines_as_errors() {
        let lines = vec!["mystery    : fp-x — something new".to_string()];
        let summary = summarize_lines(&lines);
        assert_eq!(summary.checked, 1);
        assert_eq!(summary.merged + summary.rejected + summary.still_open, 0);
        assert_eq!(summary.errors.len(), 1);
        assert_eq!(summary.errors[0].message, lines[0]);
    }

    #[test]
    fn sync_summary_serializes_the_console_contract() {
        let lines = vec![
            "merged     : fp-a → https://github.com/o/r/pull/1".to_string(),
            "rejected   : fp-b (PR #2 closed without merge)".to_string(),
            "still open : fp-c — PR #3".to_string(),
            "skip       : fp-d — unparsable PR url nope".to_string(),
        ];
        let value = serde_json::to_value(summarize_lines(&lines)).expect("json");
        assert_eq!(
            value,
            serde_json::json!({
                "checked": 4,
                "merged": 1,
                "rejected": 1,
                "still_open": 1,
                "errors": [{ "fingerprint": "fp-d", "message": "unparsable PR url nope" }],
            })
        );
    }

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

        journal_terminal(
            &store,
            fp,
            repo,
            "system",
            "merged",
            "https://example.com/pull/1",
            100,
        )
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
        assert_eq!(events[0].detail, "ci_red: PR #2 closed without merge");
        assert_eq!(events[1].kind, "merged");
        assert_eq!(events[1].actor.as_deref(), Some("system"));
        assert_eq!(events[1].repo, repo);
        assert_eq!(events[1].ts, 100);
    }
}
