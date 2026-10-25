//! Continuous CI observation.
//!
//! A repair run waits for CI only for a bounded window. This scheduler owns the
//! lifecycle after that window: watches live in SQLite, survive a serve
//! restart, keep observing the delivered head, and converge red/merged/closed
//! outcomes into the issue and its memory.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;

use crate::ci::{self, Verdict};
use crate::webhook;
use shanhaijing::{CiWatchRow, CiWatchState, FailureDisposition, RepairMemoryEntry, Store};

pub const TICK_PERIOD: Duration = Duration::from_secs(30);
pub const PENDING_RECHECK_MS: i64 = 30_000;
pub const GREEN_RECHECK_MS: i64 = 5 * 60_000;
pub const WATCH_LEASE_MS: i64 = 120_000;
pub const MAX_WATCH_AGE_MS: i64 = 30 * 86_400_000;
pub const RETRY_COOLDOWN_MS: i64 = 30 * 60_000;

#[async_trait]
pub trait CiWatchApi: Send + Sync {
    async fn verdict(&self, slug: &str, sha: &str) -> Result<Option<Verdict>>;
    async fn pr_state(&self, slug: &str, number: u64) -> Result<String>;
    async fn close_pr(&self, slug: &str, number: u64) -> Result<()>;
}

pub struct GitHubCi {
    token: String,
}

impl GitHubCi {
    pub fn new(token: String) -> Self {
        Self { token }
    }
}

#[async_trait]
impl CiWatchApi for GitHubCi {
    async fn verdict(&self, slug: &str, sha: &str) -> Result<Option<Verdict>> {
        ci::poll_ci_once(&self.token, slug, sha).await
    }

    async fn pr_state(&self, slug: &str, number: u64) -> Result<String> {
        ci::pr_merge_state(&self.token, slug, number).await
    }

    async fn close_pr(&self, slug: &str, number: u64) -> Result<()> {
        ci::close_pr(&self.token, slug, number).await
    }
}

/// Poll every due watch once. Each watch is claimed atomically by moving its
/// next check forward, so two serve processes cannot process it concurrently.
pub async fn tick(store: &Store, api: &dyn CiWatchApi, now: i64) -> Result<usize> {
    let watches = store
        .claim_due_ci_watches(now, 20, WATCH_LEASE_MS)
        .context("claim due CI watches")?;
    let count = watches.len();
    for watch in &watches {
        if let Err(error) = advance(store, api, watch, now).await {
            let detail = format!("{error:#}");
            eprintln!("ci watch   : {} — {detail}", watch.fingerprint);
            let retries = watch.check_attempts.saturating_add(1);
            let backoff = (PENDING_RECHECK_MS * (1_i64 << retries.min(6))).min(3_600_000);
            let _ = store.update_ci_watch(
                watch.id,
                watch.state,
                watch.last_verdict.as_deref(),
                now + backoff,
                Some(&detail),
                watch.memory_written,
                None,
                now,
            );
        }
    }
    Ok(count)
}

async fn advance(store: &Store, api: &dyn CiWatchApi, watch: &CiWatchRow, now: i64) -> Result<()> {
    if now - watch.created_at > MAX_WATCH_AGE_MS {
        store
            .update_ci_watch(
                watch.id,
                CiWatchState::Expired,
                Some("expired"),
                now + GREEN_RECHECK_MS,
                Some("watch exceeded its maximum observation age"),
                watch.memory_written,
                Some(now),
                now,
            )
            .context("expire CI watch")?;
        return Ok(());
    }

    let verdict = api.verdict(&watch.slug, &watch.head_sha).await?;
    match verdict {
        None | Some(Verdict::TimedOut) => {
            store
                .update_ci_watch(
                    watch.id,
                    CiWatchState::Pending,
                    None,
                    now + PENDING_RECHECK_MS,
                    None,
                    watch.memory_written,
                    None,
                    now,
                )
                .context("defer CI watch")?;
        }
        Some(Verdict::Success) => {
            let pr = api.pr_state(&watch.slug, watch.pr_number).await?;
            match pr.as_str() {
                "merged" => {
                    store.update_ci_watch(
                        watch.id,
                        CiWatchState::Merged,
                        Some("green"),
                        now + GREEN_RECHECK_MS,
                        None,
                        watch.memory_written,
                        Some(now),
                        now,
                    )?;
                    return Ok(());
                }
                "closed" => {
                    store.update_ci_watch(
                        watch.id,
                        CiWatchState::Closed,
                        Some("green"),
                        now + GREEN_RECHECK_MS,
                        None,
                        watch.memory_written,
                        Some(now),
                        now,
                    )?;
                    return Ok(());
                }
                _ => {}
            }
            if watch.last_verdict.as_deref() != Some("green") {
                let _ = store.record_event(
                    &watch.fingerprint,
                    now,
                    Some(&watch.repo),
                    "runner",
                    "ci_green",
                    &format!("head {}", short(&watch.head_sha)),
                );
                if let Some(run_id) = &watch.run_id {
                    let _ = store.set_run_ci_verdict(run_id, "success");
                }
            }
            let mut memory_written = watch.memory_written;
            if !memory_written {
                if let (Some(root_cause), Some(change_stat)) =
                    (&watch.root_cause, &watch.change_stat)
                {
                    memory_written = store
                        .record_repair_memory(&RepairMemoryEntry {
                            repo: watch.repo.clone(),
                            fingerprint: watch.fingerprint.clone(),
                            commit_sha: watch.head_sha.clone(),
                            pr_url: Some(watch.pr_url.clone()),
                            root_cause: root_cause.clone(),
                            change_stat: change_stat.clone(),
                            files: 0,
                            recorded_at: now,
                        })
                        .is_ok();
                }
            }
            store
                .update_ci_watch(
                    watch.id,
                    CiWatchState::Green,
                    Some("green"),
                    now + GREEN_RECHECK_MS,
                    None,
                    memory_written,
                    None,
                    now,
                )
                .context("record green CI watch")?;
        }
        Some(Verdict::Failure) => {
            let pr = api.pr_state(&watch.slug, watch.pr_number).await?;
            if pr == "closed" {
                store.update_ci_watch(
                    watch.id,
                    CiWatchState::Closed,
                    Some("red"),
                    now + GREEN_RECHECK_MS,
                    None,
                    watch.memory_written,
                    Some(now),
                    now,
                )?;
                return Ok(());
            }
            if pr == "merged" {
                let _ = store.record_event(
                    &watch.fingerprint,
                    now,
                    Some(&watch.repo),
                    "runner",
                    "ci_red",
                    &format!(
                        "head {} — CI red after merge; no automatic rollback",
                        short(&watch.head_sha)
                    ),
                );
                if let Some(run_id) = &watch.run_id {
                    let _ = store.set_run_ci_verdict(run_id, "failure");
                }
                webhook::emit(webhook::WebhookEvent::new(
                    "ci_red",
                    Some(watch.fingerprint.clone()),
                    Some(watch.repo.clone()),
                    format!(
                        "PR #{} is merged but CI is red — manual follow-up required",
                        watch.pr_number
                    ),
                ));
                store.update_ci_watch(
                    watch.id,
                    CiWatchState::Red,
                    Some("red"),
                    now + GREEN_RECHECK_MS,
                    None,
                    watch.memory_written,
                    Some(now),
                    now,
                )?;
                return Ok(());
            }

            api.close_pr(&watch.slug, watch.pr_number).await?;
            let detail = format!("ci_red: PR #{} closed without merge", watch.pr_number);
            let outcome = match store.record_ci_rejection(
                &watch.fingerprint,
                &watch.pr_url,
                now,
                watch.max_attempts.max(1),
                now + RETRY_COOLDOWN_MS,
                &detail,
                "runner",
            ) {
                Ok(outcome) => outcome,
                Err(error) => {
                    // The issue already left pr_open (owner action, sync, or a
                    // previous observation). Close the watch instead of
                    // retrying a rejection that can never apply again.
                    store.update_ci_watch(
                        watch.id,
                        CiWatchState::Red,
                        Some("red"),
                        now + GREEN_RECHECK_MS,
                        Some(&format!("rejection not applicable: {error:#}")),
                        watch.memory_written,
                        Some(now),
                        now,
                    )?;
                    return Ok(());
                }
            };
            let _ = store.record_event(
                &watch.fingerprint,
                now,
                Some(&watch.repo),
                "runner",
                "ci_red",
                &format!(
                    "head {} — PR #{} closed",
                    short(&watch.head_sha),
                    watch.pr_number
                ),
            );
            if let Some(run_id) = &watch.run_id {
                let _ = store.set_run_ci_verdict(run_id, "failure");
            }
            webhook::emit(webhook::WebhookEvent::new(
                "ci_red",
                Some(watch.fingerprint.clone()),
                Some(watch.repo.clone()),
                if outcome.disposition == FailureDisposition::Retry {
                    format!(
                        "PR #{} closed after CI failure; retry scheduled (attempt {})",
                        watch.pr_number, outcome.attempts
                    )
                } else {
                    format!(
                        "PR #{} closed after CI failure; attempt cap reached, issue ignored",
                        watch.pr_number
                    )
                },
            ));
            store.update_ci_watch(
                watch.id,
                CiWatchState::Red,
                Some("red"),
                now + GREEN_RECHECK_MS,
                None,
                watch.memory_written,
                Some(now),
                now,
            )?;
        }
    }
    Ok(())
}

fn short(sha: &str) -> &str {
    &sha[..12.min(sha.len())]
}

/// Spawn the serve-owned scheduler. The token is re-read each tick so a
/// restarted serve picks up a refreshed env file; a missing token leaves
/// watches durable but unobserved instead of dropping them.
pub fn spawn(store: Arc<Store>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(TICK_PERIOD);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let token = std::env::var("NUWA_GITHUB_TOKEN")
                .or_else(|_| std::env::var("GITHUB_TOKEN"))
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty());
            let Some(token) = token else {
                continue;
            };
            let api = GitHubCi::new(token);
            if let Err(error) = tick(&store, &api, crate::now_ms()).await {
                eprintln!("ci watch   : scheduler tick failed: {error:#}");
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use shanhaijing::{CiWatchInput, RunStart, Signal, State, Store};

    #[derive(Default)]
    struct FakeApi {
        verdicts: Mutex<Vec<Option<Verdict>>>,
        pr_state: Mutex<String>,
        closed: Mutex<Vec<u64>>,
    }

    #[async_trait]
    impl CiWatchApi for FakeApi {
        async fn verdict(&self, _slug: &str, _sha: &str) -> Result<Option<Verdict>> {
            let mut verdicts = self.verdicts.lock().expect("verdicts");
            if verdicts.is_empty() {
                Ok(None)
            } else {
                Ok(verdicts.remove(0))
            }
        }

        async fn pr_state(&self, _slug: &str, _number: u64) -> Result<String> {
            Ok(self.pr_state.lock().expect("pr state").clone())
        }

        async fn close_pr(&self, _slug: &str, number: u64) -> Result<()> {
            self.closed.lock().expect("closed").push(number);
            Ok(())
        }
    }

    fn issue(store: &Store, now: i64) -> String {
        store
            .report_signal(
                &Signal {
                    repo: "example.com/ci-watch".into(),
                    language: "go".into(),
                    title: "panic".into(),
                    severity: "high".into(),
                    message: None,
                    log_window: Vec::new(),
                    stack_trace: "panic: boom\nexample.com/app.F()\n /app/a.go:1".into(),
                },
                now,
            )
            .expect("signal");
        store
            .ready_issue(now)
            .expect("ready")
            .expect("issue")
            .fingerprint
    }

    fn watch(store: &Store, fp: &str, now: i64) {
        store
            .start_run(&RunStart {
                id: format!("run-{now}"),
                fingerprint: fp.into(),
                runner_id: "runner".into(),
                repo: "example.com/ci-watch".into(),
                started_at: now,
                provider: None,
                model: None,
            })
            .expect("run");
        store
            .upsert_ci_watch(&CiWatchInput {
                run_id: Some(format!("run-{now}")),
                fingerprint: fp.into(),
                repo: "example.com/ci-watch".into(),
                pr_url: "https://github.com/example/app/pull/7".into(),
                slug: "example/app".into(),
                pr_number: 7,
                head_sha: format!("abc{now:013}"),
                root_cause: Some("ROOT CAUSE: boom".into()),
                change_stat: Some("f.go | 1 +".into()),
                now,
                next_check_at: now,
                max_attempts: 2,
            })
            .expect("watch");
    }

    fn to_pr_open(store: &Store, fp: &str, now: i64) {
        for state in [State::Triggered, State::Fixing, State::PrOpen] {
            store.set_state(fp, state, now, None).expect("state");
        }
        store
            .set_pr(fp, "https://github.com/example/app/pull/7", now)
            .expect("pr");
    }

    #[tokio::test]
    async fn green_is_not_terminal_and_records_memory_once() {
        let store = Store::open(":memory:").expect("store");
        let fp = issue(&store, 1);
        to_pr_open(&store, &fp, 2);
        watch(&store, &fp, 3);
        let api = FakeApi {
            verdicts: Mutex::new(vec![Some(Verdict::Success)]),
            pr_state: Mutex::new("open".into()),
            closed: Mutex::new(Vec::new()),
        };
        tick(&store, &api, 10).await.expect("tick");
        let row = store.ci_watch_for(&fp).expect("watch").expect("row");
        assert_eq!(row.state, CiWatchState::Green);
        assert!(row.memory_written);
        assert!(store
            .events_for(&fp, 20)
            .expect("events")
            .iter()
            .any(|event| event.kind == "ci_green"));
        assert_eq!(
            store.ci_watch_for(&fp).expect("watch").expect("row").state,
            CiWatchState::Green
        );
    }

    #[tokio::test]
    async fn red_closes_open_pr_and_schedules_or_ignores_at_cap() {
        let store = Store::open(":memory:").expect("store");
        let fp = issue(&store, 1);
        to_pr_open(&store, &fp, 2);
        watch(&store, &fp, 3);
        let api = FakeApi {
            verdicts: Mutex::new(vec![Some(Verdict::Failure)]),
            pr_state: Mutex::new("open".into()),
            closed: Mutex::new(Vec::new()),
        };
        tick(&store, &api, 10).await.expect("tick");
        assert_eq!(*api.closed.lock().expect("closed"), vec![7]);
        let issue_row = &store.list_issues().expect("issues")[0];
        assert_eq!(issue_row.state, "new");
        assert_eq!(issue_row.attempts, 1);
        assert_eq!(
            store.ci_watch_for(&fp).expect("watch").expect("row").state,
            CiWatchState::Red
        );

        // A second cycle reaches the cap and becomes ignored.
        to_pr_open(&store, &fp, 20);
        watch(&store, &fp, 21);
        let api = FakeApi {
            verdicts: Mutex::new(vec![Some(Verdict::Failure)]),
            pr_state: Mutex::new("open".into()),
            closed: Mutex::new(Vec::new()),
        };
        tick(&store, &api, 30).await.expect("tick");
        let issue_row = &store.list_issues().expect("issues")[0];
        assert_eq!(issue_row.state, "ignored");
        assert_eq!(issue_row.attempts, 2);
    }

    #[tokio::test]
    async fn red_after_merge_only_alerts_and_never_closes() {
        let store = Store::open(":memory:").expect("store");
        let fp = issue(&store, 1);
        to_pr_open(&store, &fp, 2);
        watch(&store, &fp, 3);
        let api = FakeApi {
            verdicts: Mutex::new(vec![Some(Verdict::Failure)]),
            pr_state: Mutex::new("merged".into()),
            closed: Mutex::new(Vec::new()),
        };
        tick(&store, &api, 10).await.expect("tick");
        assert!(api.closed.lock().expect("closed").is_empty());
        assert_eq!(store.list_issues().expect("issues")[0].state, "pr_open");
        assert_eq!(
            store.ci_watch_for(&fp).expect("watch").expect("row").state,
            CiWatchState::Red
        );
    }
}
