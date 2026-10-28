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

use crate::ci::{self, PrSnapshot, PrState, Verdict};
use crate::webhook;
use shanhaijing::{CiWatchClaim, CiWatchRow, CiWatchState, CiWatchUpdate, Store};

pub const TICK_PERIOD: Duration = Duration::from_secs(30);
pub const PENDING_RECHECK_MS: i64 = 30_000;
pub const GREEN_RECHECK_MS: i64 = 5 * 60_000;
pub const WATCH_LEASE_MS: i64 = 120_000;
const WATCH_RENEW_PERIOD: Duration = Duration::from_secs(10);
pub const MAX_WATCH_AGE_MS: i64 = 30 * 86_400_000;
pub const RETRY_COOLDOWN_MS: i64 = 30 * 60_000;

#[async_trait]
pub trait CiWatchApi: Send + Sync {
    async fn verdict(&self, slug: &str, sha: &str) -> Result<Option<Verdict>>;
    async fn pr_state(&self, slug: &str, number: u64) -> Result<PrSnapshot>;
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

    async fn pr_state(&self, slug: &str, number: u64) -> Result<PrSnapshot> {
        ci::pr_snapshot(&self.token, slug, number).await
    }

    async fn close_pr(&self, slug: &str, number: u64) -> Result<()> {
        ci::close_pr(&self.token, slug, number).await
    }
}

/// Process at most 20 due watches, claiming each immediately before use. Never
/// queue already-leased rows behind a potentially slow GitHub request.
pub async fn tick(store: &Store, api: &dyn CiWatchApi, now: i64) -> Result<usize> {
    tick_with_lease(store, api, now, WATCH_LEASE_MS, WATCH_RENEW_PERIOD).await
}

/// Observe only this delivered PR/head during the foreground budget. The
/// persisted schedule and claim fence are shared with serve; a live owner or
/// a future check is never bypassed. Cancellation drops the API future and
/// heartbeat, leaving any acquired lease to expire for durable recovery.
pub async fn wait_for_delivery(
    store: &Store,
    api: &dyn CiWatchApi,
    slug: &str,
    number: u64,
    head_sha: &str,
    timeout: Duration,
) -> Result<Option<CiWatchRow>> {
    let wait = async {
        let clock = Clock {
            start_now: crate::now_ms(),
            start: tokio::time::Instant::now(),
        };
        loop {
            let row = store
                .ci_watch_for_identity(slug, number, head_sha)
                .context("load delivered CI watch")?
                .with_context(|| {
                    format!("unknown CI watch for {slug} PR #{number} head {head_sha}")
                })?;
            if row.state == CiWatchState::Green || row.state.is_terminal() {
                return Ok(Some(row));
            }
            if row.check_attempts > 0 {
                if let Some(error) = &row.last_error {
                    anyhow::bail!("CI watch for {slug} PR #{number}: {error}");
                }
            }
            let now = clock.now();
            if row.next_check_at <= now {
                if let Some(claim) = store
                    .claim_ci_watch_owned(row.id, now, WATCH_LEASE_MS)
                    .context("claim delivered CI watch")?
                {
                    process_claim(
                        store,
                        api,
                        &claim,
                        &clock,
                        WATCH_LEASE_MS,
                        WATCH_RENEW_PERIOD,
                    )
                    .await?;
                    // Reload committed evidence, including errors recorded by
                    // process_claim, rather than returning the claimed snapshot.
                    continue;
                }
            }
            // A concurrent scheduler may finish before next_check_at. Reload
            // without an early API poll or a busy loop on a live claim.
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    };
    tokio::time::timeout(timeout, wait)
        .await
        .unwrap_or(Ok(None))
}

/// Use an observation-relative monotonic clock, not wall time: tick callers
/// may supply a synthetic `now`, but awaiting GitHub still ages leases.
struct Clock {
    start_now: i64,
    start: tokio::time::Instant,
}

impl Clock {
    fn now(&self) -> i64 {
        self.start_now
            .saturating_add(i64::try_from(self.start.elapsed().as_millis()).unwrap_or(i64::MAX))
    }
}

async fn tick_with_lease(
    store: &Store,
    api: &dyn CiWatchApi,
    now: i64,
    lease_ms: i64,
    renew_period: Duration,
) -> Result<usize> {
    let clock = Clock {
        start_now: now,
        start: tokio::time::Instant::now(),
    };
    let mut count = 0;
    while count < 20 {
        let Some(claim) = store
            .claim_due_ci_watches_owned(clock.now(), 1, lease_ms)
            .context("claim due CI watch")?
            .pop()
        else {
            break;
        };
        count += 1;
        process_claim(store, api, &claim, &clock, lease_ms, renew_period).await?;
    }
    Ok(count)
}

async fn process_claim(
    store: &Store,
    api: &dyn CiWatchApi,
    claim: &CiWatchClaim,
    clock: &Clock,
    lease_ms: i64,
    renew_period: Duration,
) -> Result<()> {
    if !store.ci_watch_claim_valid(claim, clock.now())? {
        return Ok(());
    }
    // Keep the entire operation (including its final fenced writes and error
    // release) inside the heartbeat scope. Dropping this future cancels any
    // blocked verdict/snapshot/close request as soon as ownership is lost.
    let work = async {
        let result = async {
            let observation = observe(store, api, claim, clock).await?;
            // Give the synchronous atomic finish a fresh lease as well as its
            // own transactional fence; the heartbeat stays alive until commit.
            if !store.renew_ci_watch_claim(claim, clock.now(), lease_ms)? {
                return Ok(());
            }
            let now = clock.now();
            let pending = observation.state == CiWatchState::Pending;
            let terminal = !matches!(
                observation.state,
                CiWatchState::Pending | CiWatchState::Green
            );
            store.finish_ci_watch_owned(
                claim,
                &CiWatchUpdate {
                    state: observation.state,
                    last_verdict: observation.verdict,
                    next_check_at: now.saturating_add(if observation.reject_open_pr {
                        RETRY_COOLDOWN_MS
                    } else if pending {
                        PENDING_RECHECK_MS
                    } else {
                        GREEN_RECHECK_MS
                    }),
                    last_error: observation.reason.as_deref(),
                    terminal_at: terminal.then_some(now),
                    reject_open_pr: observation.reject_open_pr,
                },
                now,
            )?;
            // finish authenticates ownership and commits all local evidence
            // atomically, then releases the claim. Never notify on a stale or
            // failed finish; do not re-check the now-released claim afterward.
            if observation.verdict == Some("red") {
                webhook::emit(webhook::WebhookEvent::new(
                    "ci_red",
                    Some(claim.watch.fingerprint.clone()),
                    Some(claim.watch.repo.clone()),
                    format!(
                        "PR #{}: {}",
                        claim.watch.pr_number,
                        observation
                            .reason
                            .as_deref()
                            .unwrap_or("CI failure; closed head verified")
                    ),
                ));
            }
            Ok::<_, anyhow::Error>(())
        }
        .await;
        if let Err(error) = result {
            if !store.renew_ci_watch_claim(claim, clock.now(), lease_ms)? {
                return Ok(());
            }
            let detail = format!("{error:#}");
            eprintln!("ci watch   : {} — {detail}", claim.watch.fingerprint);
            let retries = claim.watch.check_attempts.saturating_add(1);
            let backoff = (PENDING_RECHECK_MS * (1_i64 << retries.min(6))).min(3_600_000);
            let now = clock.now();
            match store.record_ci_watch_error_owned(
                claim,
                now.saturating_add(backoff),
                &detail,
                now,
            ) {
                Ok(()) | Err(shanhaijing::ShanError::OwnershipLost) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok::<_, anyhow::Error>(())
    };
    tokio::pin!(work);
    let mut heartbeat =
        tokio::time::interval_at(tokio::time::Instant::now() + renew_period, renew_period);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            biased;
            _ = heartbeat.tick() => {
                if !store.renew_ci_watch_claim(claim, clock.now(), lease_ms)? {
                    return Ok(());
                }
            }
            result = &mut work => return result,
        }
    }
}

struct Observation {
    state: CiWatchState,
    verdict: Option<&'static str>,
    reason: Option<String>,
    reject_open_pr: bool,
}

impl Observation {
    fn new(state: CiWatchState, verdict: Option<&'static str>) -> Self {
        Self {
            state,
            verdict,
            reason: None,
            reject_open_pr: false,
        }
    }

    fn head_changed(watch: &CiWatchRow, snapshot: &PrSnapshot) -> Option<Self> {
        (snapshot.head_sha != watch.head_sha).then(|| Self {
            state: CiWatchState::HeadChanged,
            verdict: Some("head_changed"),
            reason: Some(format!(
                "PR head changed from {} to {}; not following the replacement head",
                watch.head_sha, snapshot.head_sha
            )),
            reject_open_pr: false,
        })
    }

    fn external_red(state: PrState) -> Option<Self> {
        match state {
            PrState::Open => None,
            PrState::Closed => Some(Self {
                state: CiWatchState::Closed,
                verdict: Some("red"),
                reason: Some("CI red; PR externally closed; no automatic reopen".into()),
                reject_open_pr: false,
            }),
            PrState::Merged => Some(Self {
                state: CiWatchState::Red,
                verdict: Some("red"),
                reason: Some("CI red after merge; manual follow-up, no automatic rollback".into()),
                reject_open_pr: false,
            }),
        }
    }
}

async fn observe(
    store: &Store,
    api: &dyn CiWatchApi,
    claim: &CiWatchClaim,
    clock: &Clock,
) -> Result<Observation> {
    let watch = &claim.watch;
    if clock.now().saturating_sub(watch.created_at) > MAX_WATCH_AGE_MS {
        let mut expired = Observation::new(CiWatchState::Expired, Some("expired"));
        expired.reason = Some("watch exceeded its maximum observation age".into());
        return Ok(expired);
    }
    let verdict = api.verdict(&watch.slug, &watch.head_sha).await?;
    if verdict.is_none() {
        return Ok(Observation::new(CiWatchState::Pending, None));
    }
    let snapshot = api.pr_state(&watch.slug, watch.pr_number).await?;
    if let Some(changed) = Observation::head_changed(watch, &snapshot) {
        return Ok(changed);
    }
    if verdict == Some(Verdict::Success) {
        return Ok(Observation::new(
            match snapshot.state {
                PrState::Open => CiWatchState::Green,
                PrState::Closed => CiWatchState::Closed,
                PrState::Merged => CiWatchState::Merged,
            },
            Some("green"),
        ));
    }
    if let Some(external) = Observation::external_red(snapshot.state) {
        return Ok(external);
    }

    // Re-fetch immediately before the destructive effect: the verdict and
    // first snapshot may already be obsolete. GitHub offers no atomic
    // expected-head close, so a residual TOCTOU race remains even with this
    // preflight and the post-close verification below.
    let before_close = api.pr_state(&watch.slug, watch.pr_number).await?;
    if let Some(changed) = Observation::head_changed(watch, &before_close) {
        return Ok(changed);
    }
    if let Some(external) = Observation::external_red(before_close.state) {
        return Ok(external);
    }
    if !store.ci_watch_can_reject_issue(claim, clock.now())? {
        return Ok(Observation {
            state: CiWatchState::HeadChanged,
            verdict: Some("head_changed"),
            reason: Some("watch no longer owns current issue delivery".into()),
            reject_open_pr: false,
        });
    }
    api.close_pr(&watch.slug, watch.pr_number).await?;
    let after_close = api.pr_state(&watch.slug, watch.pr_number).await?;
    if let Some(changed) = Observation::head_changed(watch, &after_close) {
        return Ok(changed);
    }
    match after_close.state {
        PrState::Closed => Ok(Observation {
            state: CiWatchState::Red,
            verdict: Some("red"),
            reason: Some("CI red; closed PR verified at the watched head".into()),
            reject_open_pr: true,
        }),
        PrState::Merged => {
            Observation::external_red(PrState::Merged).context("missing merged CI outcome")
        }
        PrState::Open => anyhow::bail!("PR close not confirmed; watched head is still open"),
    }
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
            let token = crate::env_store::var("NUWA_GITHUB_TOKEN")
                .or_else(|| crate::env_store::var("GITHUB_TOKEN"))
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
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex;

    use shanhaijing::{CiWatchInput, RepairMemoryEntry, RunStart, RunUpdate, Signal, State, Store};

    struct FakeApi {
        verdicts: Mutex<Vec<Option<Verdict>>>,
        pr_state: Mutex<PrSnapshot>,
        snapshots: Mutex<Vec<Result<PrSnapshot>>>,
        closed: Mutex<Vec<u64>>,
        close_error: bool,
    }

    fn snapshot(state: PrState, now: i64) -> PrSnapshot {
        PrSnapshot {
            state,
            head_sha: format!("abc{now:013}"),
        }
    }

    fn memory(store: &Store) -> Vec<shanhaijing::RepairMemoryRow> {
        store
            .recent_repair_memory("example.com/ci-watch", "", 10)
            .expect("memory")
    }

    impl FakeApi {
        fn new(verdict: Verdict, state: PrState, now: i64) -> Self {
            Self {
                verdicts: Mutex::new(vec![Some(verdict)]),
                pr_state: Mutex::new(snapshot(state, now)),
                snapshots: Mutex::new(Vec::new()),
                closed: Mutex::new(Vec::new()),
                close_error: false,
            }
        }

        fn script(&self, snapshots: Vec<Result<PrSnapshot>>) {
            *self.snapshots.lock().expect("snapshots") = snapshots;
        }
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

        async fn pr_state(&self, _slug: &str, _number: u64) -> Result<PrSnapshot> {
            let mut snapshots = self.snapshots.lock().expect("snapshots");
            if snapshots.is_empty() {
                Ok(self.pr_state.lock().expect("pr state").clone())
            } else {
                snapshots.remove(0)
            }
        }

        async fn close_pr(&self, _slug: &str, number: u64) -> Result<()> {
            if self.close_error {
                anyhow::bail!("GitHub close unavailable");
            }
            self.closed.lock().expect("closed").push(number);
            self.pr_state.lock().expect("pr state").state = PrState::Closed;
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
                    ..Default::default()
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
            .update_run(
                &format!("run-{now}"),
                &RunUpdate {
                    commit_sha: Some(format!("abc{now:013}")),
                    pr_url: Some("https://github.com/example/app/pull/7".into()),
                    ..RunUpdate::default()
                },
            )
            .expect("run delivery");
        store.upsert_ci_watch(&watch_input(fp, now)).expect("watch");
    }

    fn watch_input(fp: &str, now: i64) -> CiWatchInput {
        CiWatchInput {
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
        }
    }

    fn to_pr_open(store: &Store, fp: &str, now: i64) {
        for state in [State::Triggered, State::Fixing, State::PrOpen] {
            store.set_state(fp, state, now, None).expect("state");
        }
        store
            .set_pr(fp, "https://github.com/example/app/pull/7", now)
            .expect("pr");
    }

    struct FlakyApi {
        failures: Mutex<u32>,
    }

    #[async_trait]
    impl CiWatchApi for FlakyApi {
        async fn verdict(&self, _slug: &str, _sha: &str) -> Result<Option<Verdict>> {
            let mut failures = self.failures.lock().expect("failures");
            if *failures > 0 {
                *failures -= 1;
                anyhow::bail!("GitHub unavailable")
            }
            Ok(Some(Verdict::Success))
        }

        async fn pr_state(&self, _slug: &str, _number: u64) -> Result<PrSnapshot> {
            Ok(snapshot(PrState::Open, 3))
        }

        async fn close_pr(&self, _slug: &str, _number: u64) -> Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn error_backoff_grows_and_resets_after_success() {
        let store = Store::open(":memory:").expect("store");
        let fp = issue(&store, 1);
        to_pr_open(&store, &fp, 2);
        watch(&store, &fp, 3);
        let api = FlakyApi {
            failures: Mutex::new(2),
        };

        tick(&store, &api, 10).await.expect("first failure");
        let row = store.ci_watch_for(&fp).expect("watch").expect("row");
        assert_eq!(row.check_attempts, 1);
        assert_eq!(
            row.next_check_at,
            row.last_checked_at.expect("checked") + 60_000
        );

        tick(&store, &api, row.next_check_at)
            .await
            .expect("second failure");
        let row = store.ci_watch_for(&fp).expect("watch").expect("row");
        assert_eq!(row.check_attempts, 2);
        assert_eq!(
            row.next_check_at,
            row.last_checked_at.expect("checked") + 120_000
        );

        tick(&store, &api, row.next_check_at)
            .await
            .expect("recovery");
        let row = store.ci_watch_for(&fp).expect("watch").expect("row");
        assert_eq!(row.state, CiWatchState::Green);
        assert_eq!(row.check_attempts, 0);
    }

    struct PrStateErrorApi;

    #[async_trait]
    impl CiWatchApi for PrStateErrorApi {
        async fn verdict(&self, _slug: &str, _sha: &str) -> Result<Option<Verdict>> {
            Ok(Some(Verdict::Success))
        }

        async fn pr_state(&self, _slug: &str, _number: u64) -> Result<PrSnapshot> {
            anyhow::bail!("PR state unavailable")
        }

        async fn close_pr(&self, _slug: &str, _number: u64) -> Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn green_pr_state_error_does_not_publish_evidence_early() {
        let store = Store::open(":memory:").expect("store");
        let fp = issue(&store, 1);
        to_pr_open(&store, &fp, 2);
        watch(&store, &fp, 3);

        assert!(tick(&store, &PrStateErrorApi, 10).await.is_ok());
        assert!(!store
            .events_for(&fp, 20)
            .expect("events")
            .iter()
            .any(|event| matches!(event.kind.as_str(), "ci_watch" | "ci_green")));
    }

    #[tokio::test]
    async fn green_is_not_terminal_and_records_memory_once() {
        let store = Store::open(":memory:").expect("store");
        let fp = issue(&store, 1);
        to_pr_open(&store, &fp, 2);
        watch(&store, &fp, 3);
        let api = FakeApi::new(Verdict::Success, PrState::Open, 3);
        tick(&store, &api, 10).await.expect("tick");
        let row = store.ci_watch_for(&fp).expect("watch").expect("row");
        assert_eq!(row.state, CiWatchState::Green);
        assert_eq!(row.last_verdict.as_deref(), Some("green"));
        assert_eq!(
            store
                .run_for(&fp, "run-3")
                .expect("run")
                .expect("row")
                .ci_verdict
                .as_deref(),
            Some("success")
        );
        assert!(row.memory_written);
        assert!(store
            .events_for(&fp, 20)
            .expect("events")
            .iter()
            .any(|event| event.kind == "ci_green"));
        let due = row.next_check_at;
        api.verdicts
            .lock()
            .expect("verdicts")
            .push(Some(Verdict::Success));
        tick(&store, &api, due).await.expect("second green");
        assert_eq!(memory(&store).len(), 1);
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
        let api = FakeApi::new(Verdict::Failure, PrState::Open, 3);
        tick(&store, &api, 10).await.expect("tick");
        assert_eq!(*api.closed.lock().expect("closed"), vec![7]);
        let issue_row = &store.list_issues().expect("issues")[0];
        assert_eq!(issue_row.state, "new");
        assert_eq!(issue_row.attempts, 1);
        assert_eq!(
            store.ci_watch_for(&fp).expect("watch").expect("row").state,
            CiWatchState::Red
        );
        assert_eq!(
            store
                .run_for(&fp, "run-3")
                .expect("run")
                .expect("row")
                .ci_verdict
                .as_deref(),
            Some("failure")
        );
        assert!(store
            .events_for(&fp, 20)
            .expect("events")
            .iter()
            .any(|e| e.kind == "ci_red"));

        // A second cycle reaches the cap and becomes ignored.
        to_pr_open(&store, &fp, 20);
        watch(&store, &fp, 21);
        let api = FakeApi::new(Verdict::Failure, PrState::Open, 21);
        tick(&store, &api, 30).await.expect("tick");
        let issue_row = &store.list_issues().expect("issues")[0];
        assert_eq!(issue_row.state, "ignored");
        assert_eq!(issue_row.attempts, 2);
    }

    #[tokio::test]
    async fn terminal_green_after_merge_still_records_memory() {
        let store = Store::open(":memory:").expect("store");
        let fp = issue(&store, 1);
        to_pr_open(&store, &fp, 2);
        watch(&store, &fp, 3);
        let api = FakeApi::new(Verdict::Success, PrState::Merged, 3);
        tick(&store, &api, 10).await.expect("tick");

        let row = store.ci_watch_for(&fp).expect("watch").expect("row");
        assert_eq!(row.state, CiWatchState::Merged);
        assert!(row.memory_written);
        assert_eq!(
            store
                .recent_repair_memory("example.com/ci-watch", "", 10)
                .expect("memory")
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn red_after_external_close_alerts_without_reopening() {
        let store = Store::open(":memory:").expect("store");
        let fp = issue(&store, 1);
        to_pr_open(&store, &fp, 2);
        watch(&store, &fp, 3);
        let api = FakeApi::new(Verdict::Failure, PrState::Closed, 3);
        tick(&store, &api, 10).await.expect("tick");

        assert!(api.closed.lock().expect("closed").is_empty());
        assert_eq!(store.list_issues().expect("issues")[0].state, "pr_open");
        assert!(store
            .events_for(&fp, 20)
            .expect("events")
            .iter()
            .any(|event| event.kind == "ci_red"));
        assert_eq!(
            store.ci_watch_for(&fp).expect("watch").expect("row").state,
            CiWatchState::Closed
        );
    }

    #[tokio::test]
    async fn red_after_merge_only_alerts_and_never_closes() {
        let store = Store::open(":memory:").expect("store");
        let fp = issue(&store, 1);
        to_pr_open(&store, &fp, 2);
        watch(&store, &fp, 3);
        let api = FakeApi::new(Verdict::Failure, PrState::Merged, 3);
        tick(&store, &api, 10).await.expect("tick");
        assert!(api.closed.lock().expect("closed").is_empty());
        assert_eq!(store.list_issues().expect("issues")[0].state, "pr_open");
        assert_eq!(
            store.ci_watch_for(&fp).expect("watch").expect("row").state,
            CiWatchState::Red
        );
    }

    #[tokio::test]
    async fn changed_head_never_publishes_green_or_rejects_the_issue() {
        for (verdict, changed_at, closes) in [
            (Verdict::Success, 0, 0),
            (Verdict::Failure, 0, 0),
            (Verdict::Failure, 1, 0),
            (Verdict::Failure, 2, 1),
        ] {
            let store = Store::open(":memory:").expect("store");
            let fp = issue(&store, 1);
            to_pr_open(&store, &fp, 2);
            watch(&store, &fp, 3);
            let api = FakeApi::new(verdict, PrState::Open, 3);
            let mut snapshots = (0..changed_at)
                .map(|_| Ok(snapshot(PrState::Open, 3)))
                .collect::<Vec<_>>();
            // Preserve the first 12 characters to catch prefix comparisons.
            let mut changed = snapshot(PrState::Closed, 3);
            changed.head_sha.push('x');
            snapshots.push(Ok(changed));
            api.script(snapshots);

            tick(&store, &api, 10).await.expect("tick");
            let row = store.ci_watch_for(&fp).expect("watch").expect("row");
            assert_eq!(row.state, CiWatchState::HeadChanged);
            assert_eq!(row.head_sha, snapshot(PrState::Open, 3).head_sha);
            assert!(row.terminal_at.is_some());
            assert!(row.last_error.expect("reason").contains("not following"));
            assert_eq!(api.closed.lock().expect("closed").len(), closes);
            assert_eq!(store.list_issues().expect("issues")[0].state, "pr_open");
            assert_eq!(store.list_issues().expect("issues")[0].attempts, 0);
            assert!(memory(&store).is_empty());
            assert!(!store
                .events_for(&fp, 20)
                .expect("events")
                .iter()
                .any(|e| e.kind == "ci_green"));
            assert_eq!(
                tick(&store, &api, 1_000_000).await.expect("terminal tick"),
                0
            );
        }
    }

    #[tokio::test]
    async fn snapshot_errors_at_every_red_phase_retry_without_rejection() {
        for failed_at in 0..3 {
            let store = Store::open(":memory:").expect("store");
            let fp = issue(&store, 1);
            to_pr_open(&store, &fp, 2);
            watch(&store, &fp, 3);
            let api = FakeApi::new(Verdict::Failure, PrState::Open, 3);
            let mut snapshots = (0..failed_at)
                .map(|_| Ok(snapshot(PrState::Open, 3)))
                .collect::<Vec<_>>();
            snapshots.push(Err(anyhow::anyhow!("malformed or unavailable PR snapshot")));
            api.script(snapshots);

            tick(&store, &api, 10).await.expect("tick");
            let row = store.ci_watch_for(&fp).expect("watch").expect("row");
            assert_eq!(row.state, CiWatchState::Pending);
            assert_eq!(row.check_attempts, 1);
            assert!(row.terminal_at.is_none());
            assert_ne!(row.last_verdict.as_deref(), Some("red"));
            assert!(store
                .run_for(&fp, "run-3")
                .expect("run")
                .expect("row")
                .ci_verdict
                .is_none());
            assert_eq!(
                api.closed.lock().expect("closed").len(),
                usize::from(failed_at == 2)
            );
            assert_eq!(store.list_issues().expect("issues")[0].attempts, 0);
            assert_eq!(store.list_issues().expect("issues")[0].state, "pr_open");
            assert!(!store
                .events_for(&fp, 20)
                .expect("events")
                .iter()
                .any(|e| e.kind == "ci_red"));
        }
    }

    #[tokio::test]
    async fn external_close_or_merge_during_red_processing_never_rejects() {
        for external_at in 1..3 {
            for state in [PrState::Closed, PrState::Merged] {
                // A confirmed close after OUR close is the sole rejection case.
                if external_at == 2 && state == PrState::Closed {
                    continue;
                }
                let store = Store::open(":memory:").expect("store");
                let fp = issue(&store, 1);
                to_pr_open(&store, &fp, 2);
                watch(&store, &fp, 3);
                let api = FakeApi::new(Verdict::Failure, PrState::Open, 3);
                let mut snapshots = (0..external_at)
                    .map(|_| Ok(snapshot(PrState::Open, 3)))
                    .collect::<Vec<_>>();
                snapshots.push(Ok(snapshot(state, 3)));
                api.script(snapshots);
                tick(&store, &api, 10).await.expect("tick");
                assert_eq!(store.list_issues().expect("issues")[0].state, "pr_open");
                assert_eq!(store.list_issues().expect("issues")[0].attempts, 0);
                assert_eq!(
                    api.closed.lock().expect("closed").len(),
                    usize::from(external_at == 2)
                );
                assert!(store
                    .ci_watch_for(&fp)
                    .expect("watch")
                    .expect("row")
                    .terminal_at
                    .is_some());
            }
        }
    }

    #[tokio::test]
    async fn unconfirmed_close_is_retry_not_red() {
        let store = Store::open(":memory:").expect("store");
        let fp = issue(&store, 1);
        to_pr_open(&store, &fp, 2);
        watch(&store, &fp, 3);
        let api = FakeApi::new(Verdict::Failure, PrState::Open, 3);
        api.script((0..3).map(|_| Ok(snapshot(PrState::Open, 3))).collect());
        tick(&store, &api, 10).await.expect("tick");
        let row = store.ci_watch_for(&fp).expect("watch").expect("row");
        assert_eq!(row.state, CiWatchState::Pending);
        assert_eq!(row.check_attempts, 1);
        assert_eq!(store.list_issues().expect("issues")[0].attempts, 0);
    }

    #[tokio::test]
    async fn close_api_error_is_retry_not_red() {
        let store = Store::open(":memory:").expect("store");
        let fp = issue(&store, 1);
        to_pr_open(&store, &fp, 2);
        watch(&store, &fp, 3);
        let mut api = FakeApi::new(Verdict::Failure, PrState::Open, 3);
        api.close_error = true;
        tick(&store, &api, 10).await.expect("tick");
        let row = store.ci_watch_for(&fp).expect("watch").expect("row");
        assert_eq!(row.state, CiWatchState::Pending);
        assert_eq!(row.check_attempts, 1);
        assert!(row.terminal_at.is_none());
        assert!(row.last_verdict.is_none());
        assert!(store
            .run_for(&fp, "run-3")
            .expect("run")
            .expect("row")
            .ci_verdict
            .is_none());
        assert_eq!(store.list_issues().expect("issues")[0].state, "pr_open");
        assert_eq!(store.list_issues().expect("issues")[0].attempts, 0);
        assert!(memory(&store).is_empty());
    }

    #[tokio::test]
    async fn green_on_externally_closed_pr_is_not_eligible_memory() {
        let store = Store::open(":memory:").expect("store");
        let fp = issue(&store, 1);
        to_pr_open(&store, &fp, 2);
        watch(&store, &fp, 3);
        tick(
            &store,
            &FakeApi::new(Verdict::Success, PrState::Closed, 3),
            10,
        )
        .await
        .expect("tick");
        let row = store.ci_watch_for(&fp).expect("watch").expect("row");
        assert_eq!(row.state, CiWatchState::Closed);
        assert!(!row.memory_written);
        assert!(memory(&store).is_empty());
    }

    #[tokio::test]
    async fn terminal_old_watch_revokes_only_its_exact_memory() {
        for state in [PrState::Open, PrState::Closed, PrState::Merged] {
            let store = Store::open(":memory:").expect("store");
            let fp = issue(&store, 1);
            to_pr_open(&store, &fp, 2);
            watch(&store, &fp, 3);
            tick(
                &store,
                &FakeApi::new(Verdict::Success, PrState::Open, 3),
                10,
            )
            .await
            .expect("green");
            let due = store
                .ci_watch_for(&fp)
                .expect("watch")
                .expect("row")
                .next_check_at;
            store
                .record_verified_repair_memory(
                    &RepairMemoryEntry {
                        repo: "example.com/ci-watch".into(),
                        fingerprint: fp.clone(),
                        commit_sha: "newer-verified-head".into(),
                        pr_url: Some("https://github.com/example/app/pull/8".into()),
                        root_cause: "newer repair".into(),
                        change_stat: "new.go | 1 +".into(),
                        files: 1,
                        recorded_at: 20,
                    },
                    "local_verification",
                    "newer-run",
                )
                .expect("newer memory");
            let api = FakeApi::new(Verdict::Failure, state, 3);
            if state == PrState::Merged {
                // The old watch sees the replacement head, not its own merge.
                api.pr_state.lock().expect("snapshot").head_sha = "newer-verified-head".into();
            }
            tick(&store, &api, due).await.expect("terminal");
            let memory = memory(&store);
            assert_eq!(memory.len(), 1);
            assert_eq!(memory[0].commit_sha, "newer-verified-head");
        }
    }

    #[tokio::test]
    async fn terminal_watch_revokes_previously_green_evidence() {
        for state in [PrState::Open, PrState::Closed, PrState::Merged] {
            let store = Store::open(":memory:").expect("store");
            let fp = issue(&store, 1);
            to_pr_open(&store, &fp, 2);
            watch(&store, &fp, 3);
            tick(
                &store,
                &FakeApi::new(Verdict::Success, PrState::Open, 3),
                10,
            )
            .await
            .expect("green");
            assert_eq!(memory(&store).len(), 1);
            let due = store
                .ci_watch_for(&fp)
                .expect("watch")
                .expect("row")
                .next_check_at;
            let api = FakeApi::new(Verdict::Failure, state, 3);
            if state == PrState::Merged {
                api.pr_state.lock().expect("snapshot").head_sha.push('x');
            }
            tick(&store, &api, due).await.expect("terminal");
            assert!(memory(&store).is_empty());
            let row = store.ci_watch_for(&fp).expect("watch").expect("row");
            assert!(!row.memory_written);
            assert!(row.terminal_at.is_some());
        }
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum BlockAt {
        Verdict,
        Snapshot,
        Close,
        PostClose,
    }

    struct BlockingApi {
        stage: BlockAt,
        head_at: i64,
        entered: tokio::sync::Notify,
        release: tokio::sync::Notify,
        cancelled: AtomicBool,
        verdicts: AtomicUsize,
        snapshots: AtomicUsize,
        closed: AtomicUsize,
    }

    struct CancelGuard<'a> {
        cancelled: &'a AtomicBool,
        armed: bool,
    }

    impl Drop for CancelGuard<'_> {
        fn drop(&mut self) {
            if self.armed {
                self.cancelled.store(true, Ordering::SeqCst);
            }
        }
    }

    impl BlockingApi {
        fn new(stage: BlockAt) -> Self {
            Self {
                stage,
                head_at: 3,
                entered: tokio::sync::Notify::new(),
                release: tokio::sync::Notify::new(),
                cancelled: AtomicBool::new(false),
                verdicts: AtomicUsize::new(0),
                snapshots: AtomicUsize::new(0),
                closed: AtomicUsize::new(0),
            }
        }

        async fn block(&self) {
            let mut guard = CancelGuard {
                cancelled: &self.cancelled,
                armed: true,
            };
            self.entered.notify_one();
            self.release.notified().await;
            guard.armed = false;
        }
    }

    #[async_trait]
    impl CiWatchApi for BlockingApi {
        async fn verdict(&self, _slug: &str, sha: &str) -> Result<Option<Verdict>> {
            assert_eq!(sha, snapshot(PrState::Open, self.head_at).head_sha);
            self.verdicts.fetch_add(1, Ordering::SeqCst);
            if self.stage == BlockAt::Verdict {
                self.block().await;
            }
            Ok(Some(Verdict::Failure))
        }

        async fn pr_state(&self, _slug: &str, _number: u64) -> Result<PrSnapshot> {
            let call = self.snapshots.fetch_add(1, Ordering::SeqCst);
            if (self.stage == BlockAt::Snapshot && call == 0)
                || (self.stage == BlockAt::PostClose && call == 2)
            {
                self.block().await;
            }
            let state = if self.closed.load(Ordering::SeqCst) > 0 {
                PrState::Closed
            } else {
                PrState::Open
            };
            Ok(snapshot(state, self.head_at))
        }

        async fn close_pr(&self, _slug: &str, _number: u64) -> Result<()> {
            if self.stage == BlockAt::Close {
                self.block().await;
            }
            self.closed.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn waiting_watch_is_not_preclaimed_behind_a_blocked_request() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("watches.db");
        let store = Store::open(&path).expect("store");
        let other = Store::open(&path).expect("independent store");
        let fp = issue(&store, 1);
        to_pr_open(&store, &fp, 2);
        watch(&store, &fp, 3);
        watch(&store, &fp, 4);
        let api = BlockingApi::new(BlockAt::Verdict);
        let control = async {
            api.entered.notified().await;
            let claims = other
                .claim_due_ci_watches_owned(10, 20, WATCH_LEASE_MS)
                .expect("waiting watch");
            assert_eq!(claims.len(), 1);
            assert_eq!(
                claims[0].watch.head_sha,
                snapshot(PrState::Open, 4).head_sha
            );
            api.release.notify_one();
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(tick(&store, &api, 10), control)
        })
        .await
        .expect("tick completes");
        assert_eq!(result.expect("tick"), 1);
        assert_eq!(store.list_issues().expect("issues")[0].attempts, 0);
    }

    #[tokio::test]
    async fn tick_claims_at_most_twenty_rows() {
        let store = Store::open(":memory:").expect("store");
        let fp = issue(&store, 1);
        for now in 3..24 {
            watch(&store, &fp, now);
        }
        let api = FakeApi::new(Verdict::Success, PrState::Open, 3);
        api.verdicts.lock().expect("verdicts").clear();
        assert_eq!(tick(&store, &api, 30).await.expect("tick"), 20);
        assert_eq!(
            store
                .claim_due_ci_watches_owned(30, 20, WATCH_LEASE_MS)
                .expect("remaining")
                .len(),
            1
        );
    }

    struct StealingApi {
        other: Store,
        verdict: Verdict,
        snapshots: AtomicUsize,
        closed: AtomicBool,
    }

    #[async_trait]
    impl CiWatchApi for StealingApi {
        async fn verdict(&self, _slug: &str, _sha: &str) -> Result<Option<Verdict>> {
            Ok(Some(self.verdict.clone()))
        }

        async fn pr_state(&self, _slug: &str, _number: u64) -> Result<PrSnapshot> {
            let call = self.snapshots.fetch_add(1, Ordering::SeqCst);
            let steal_at = usize::from(self.verdict == Verdict::Failure);
            if call == steal_at {
                // Complete immediately, before the next heartbeat can fire.
                let claims = self
                    .other
                    .claim_due_ci_watches_owned(1_000_000, 1, WATCH_LEASE_MS)?;
                assert_eq!(claims.len(), 1);
            }
            Ok(snapshot(PrState::Open, 3))
        }

        async fn close_pr(&self, _slug: &str, _number: u64) -> Result<()> {
            self.closed.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn ownership_is_revalidated_before_close_and_final_evidence_writes() {
        for verdict in [Verdict::Success, Verdict::Failure] {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("watches.db");
            let store = Store::open(&path).expect("store");
            let fp = issue(&store, 1);
            to_pr_open(&store, &fp, 2);
            watch(&store, &fp, 3);
            let api = StealingApi {
                other: Store::open(&path).expect("independent store"),
                verdict,
                snapshots: AtomicUsize::new(0),
                closed: AtomicBool::new(false),
            };
            assert_eq!(tick(&store, &api, 10).await.expect("tick"), 1);
            assert!(!api.closed.load(Ordering::SeqCst));
            assert_eq!(
                store.ci_watch_for(&fp).expect("watch").expect("row").state,
                CiWatchState::Pending
            );
            assert!(store
                .run_for(&fp, "run-3")
                .expect("run")
                .expect("row")
                .ci_verdict
                .is_none());
            assert!(memory(&store).is_empty());
            assert_eq!(store.list_issues().expect("issues")[0].attempts, 0);
        }
    }

    #[tokio::test]
    async fn independent_store_takeover_cancels_blocked_api_futures() {
        for stage in [
            BlockAt::Verdict,
            BlockAt::Snapshot,
            BlockAt::Close,
            BlockAt::PostClose,
        ] {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("watches.db");
            let store = Store::open(&path).expect("store");
            let other = Store::open(&path).expect("independent store");
            let fp = issue(&store, 1);
            to_pr_open(&store, &fp, 2);
            watch(&store, &fp, 3);
            let api = BlockingApi::new(stage);
            let control = async {
                api.entered.notified().await;
                // Another process is now the owner of the expired generation.
                let claims = other
                    .claim_due_ci_watches_owned(10_000, 1, WATCH_LEASE_MS)
                    .expect("takeover");
                assert_eq!(claims.len(), 1);
                claims
            };
            let (result, claims) = tokio::time::timeout(Duration::from_secs(2), async {
                tokio::join!(
                    tick_with_lease(&store, &api, 10, 1_000, Duration::from_millis(10)),
                    control
                )
            })
            .await
            .expect("blocked future cancelled");
            assert_eq!(result.expect("tick"), 1);
            assert!(api.cancelled.load(Ordering::SeqCst));
            assert_eq!(
                api.closed.load(Ordering::SeqCst),
                usize::from(stage == BlockAt::PostClose)
            );
            assert!(other
                .ci_watch_claim_valid(&claims[0], 10_001)
                .expect("owner valid"));
            let row = store.ci_watch_for(&fp).expect("watch").expect("row");
            assert_eq!(row.state, CiWatchState::Pending);
            assert_eq!(row.check_attempts, 0);
            assert!(row.last_checked_at.is_none());
            assert!(store
                .run_for(&fp, "run-3")
                .expect("run")
                .expect("row")
                .ci_verdict
                .is_none());
            assert_eq!(store.list_issues().expect("issues")[0].attempts, 0);
            assert!(memory(&store).is_empty());
            assert!(!store
                .events_for(&fp, 20)
                .expect("events")
                .iter()
                .any(|e| matches!(e.kind.as_str(), "ci_red" | "ci_green")));
        }
    }

    #[tokio::test]
    async fn heartbeat_renews_through_every_blocked_phase_with_synthetic_time() {
        for stage in [
            BlockAt::Verdict,
            BlockAt::Snapshot,
            BlockAt::Close,
            BlockAt::PostClose,
        ] {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("watches.db");
            let store = Store::open(&path).expect("store");
            let other = Store::open(&path).expect("independent store");
            let fp = issue(&store, 1);
            to_pr_open(&store, &fp, 2);
            watch(&store, &fp, 3);
            let api = BlockingApi::new(stage);
            let started = tokio::time::Instant::now();
            let control = async {
                api.entered.notified().await;
                tokio::time::sleep(Duration::from_millis(450)).await;
                let now = 10 + i64::try_from(started.elapsed().as_millis()).expect("elapsed");
                assert!(other
                    .claim_due_ci_watches_owned(now, 1, 200)
                    .expect("contender")
                    .is_empty());
                api.release.notify_one();
            };
            let (result, ()) = tokio::time::timeout(Duration::from_secs(3), async {
                tokio::join!(
                    tick_with_lease(&store, &api, 10, 200, Duration::from_millis(10)),
                    control
                )
            })
            .await
            .expect("tick completes");
            assert_eq!(result.expect("tick"), 1);
            assert!(!api.cancelled.load(Ordering::SeqCst));
            let row = store.ci_watch_for(&fp).expect("watch").expect("row");
            assert_eq!(row.state, CiWatchState::Red);
            assert!(row.last_checked_at.expect("checked") >= 450);
            assert_eq!(store.list_issues().expect("issues")[0].attempts, 1);
        }
    }

    #[tokio::test]
    async fn foreground_wait_uses_exact_delivery_not_latest_fingerprint() {
        let store = Store::open(":memory:").expect("store");
        let now = crate::now_ms();
        let fp = issue(&store, now - 2);
        to_pr_open(&store, &fp, now - 1);
        watch(&store, &fp, now);
        watch(&store, &fp, now + 1);
        let api = FakeApi::new(Verdict::Success, PrState::Open, now);
        let head = snapshot(PrState::Open, now).head_sha;
        let row = wait_for_delivery(
            &store,
            &api,
            "example/app",
            7,
            &head,
            Duration::from_secs(1),
        )
        .await
        .expect("wait")
        .expect("green");
        assert_eq!(row.head_sha, head);
        assert_eq!(row.state, CiWatchState::Green);
        let latest = store.ci_watch_for(&fp).expect("watch").expect("latest");
        assert_ne!(row.id, latest.id);
        assert_eq!(latest.state, CiWatchState::Pending);
        assert!(latest.last_checked_at.is_none());
        assert!(store
            .run_for(&fp, &format!("run-{}", now + 1))
            .expect("run")
            .expect("row")
            .ci_verdict
            .is_none());
    }

    #[tokio::test]
    async fn foreground_wait_returns_head_changed_without_following_it() {
        let store = Store::open(":memory:").expect("store");
        let now = crate::now_ms();
        let fp = issue(&store, now - 2);
        to_pr_open(&store, &fp, now - 1);
        watch(&store, &fp, now);
        let api = FakeApi::new(Verdict::Success, PrState::Open, now + 1);
        let head = snapshot(PrState::Open, now).head_sha;
        let row = wait_for_delivery(
            &store,
            &api,
            "example/app",
            7,
            &head,
            Duration::from_secs(1),
        )
        .await
        .expect("wait")
        .expect("terminal");
        assert_eq!(row.state, CiWatchState::HeadChanged);
        assert_eq!(row.head_sha, head);
        api.verdicts
            .lock()
            .expect("verdicts")
            .push(Some(Verdict::Success));
        assert_eq!(
            wait_for_delivery(
                &store,
                &api,
                "example/app",
                7,
                &head,
                Duration::from_secs(1)
            )
            .await
            .expect("persisted wait")
            .expect("terminal")
            .id,
            row.id
        );
        assert_eq!(api.verdicts.lock().expect("verdicts").len(), 1);
        assert!(memory(&store).is_empty());
    }

    #[tokio::test]
    async fn foreground_wait_never_polls_a_future_check_early() {
        let store = Store::open(":memory:").expect("store");
        let now = crate::now_ms();
        let fp = issue(&store, now - 2);
        to_pr_open(&store, &fp, now - 1);
        watch(&store, &fp, now);
        let api = FakeApi::new(Verdict::Success, PrState::Open, now);
        *api.verdicts.lock().expect("verdicts") = vec![None];
        api.verdicts
            .lock()
            .expect("verdicts")
            .push(Some(Verdict::Success));
        let head = snapshot(PrState::Open, now).head_sha;
        assert!(wait_for_delivery(
            &store,
            &api,
            "example/app",
            7,
            &head,
            Duration::from_millis(30)
        )
        .await
        .expect("bounded wait")
        .is_none());
        let row = store.ci_watch_for(&fp).expect("watch").expect("row");
        assert_eq!(row.state, CiWatchState::Pending);
        assert!(row.last_checked_at.is_some());
        assert!(row.next_check_at >= now + PENDING_RECHECK_MS);
        assert_eq!(api.verdicts.lock().expect("verdicts").len(), 1);
    }

    #[tokio::test]
    async fn foreground_wait_reports_unknown_identity_and_durably_records_api_error() {
        let store = Store::open(":memory:").expect("store");
        let now = crate::now_ms();
        let fp = issue(&store, now - 2);
        to_pr_open(&store, &fp, now - 1);
        watch(&store, &fp, now);
        let head = snapshot(PrState::Open, now).head_sha;
        let api = FlakyApi {
            failures: Mutex::new(1),
        };
        let unknown = wait_for_delivery(
            &store,
            &api,
            "example/app",
            8,
            &head,
            Duration::from_secs(1),
        )
        .await
        .expect_err("unknown identity");
        assert!(unknown.to_string().contains("unknown CI watch"));
        let error = wait_for_delivery(
            &store,
            &api,
            "example/app",
            7,
            &head,
            Duration::from_secs(1),
        )
        .await
        .expect_err("unavailable");
        assert!(error.to_string().contains("GitHub unavailable"));
        let row = store.ci_watch_for(&fp).expect("watch").expect("row");
        assert_eq!(row.state, CiWatchState::Pending);
        assert_eq!(row.check_attempts, 1);
        assert!(row
            .last_error
            .expect("durable error")
            .contains("GitHub unavailable"));
        assert!(row.terminal_at.is_none());
        assert!(store
            .claim_ci_watch_owned(row.id, row.next_check_at, WATCH_LEASE_MS)
            .expect("recoverable")
            .is_some());
    }

    #[tokio::test]
    async fn foreground_and_serve_share_one_poll_and_one_close() {
        for foreground_first in [true, false] {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("watches.db");
            let store = Store::open(&path).expect("store");
            let other = Store::open(&path).expect("independent store");
            let now = crate::now_ms();
            let fp = issue(&store, now - 2);
            to_pr_open(&store, &fp, now - 1);
            watch(&store, &fp, now);
            let head = snapshot(PrState::Open, now).head_sha;
            let mut api = BlockingApi::new(BlockAt::Verdict);
            api.head_at = now;
            let owner = async {
                if foreground_first {
                    let row = wait_for_delivery(
                        &store,
                        &api,
                        "example/app",
                        7,
                        &head,
                        Duration::from_secs(2),
                    )
                    .await
                    .expect("foreground")
                    .expect("red");
                    assert_eq!(row.state, CiWatchState::Red);
                } else {
                    assert_eq!(tick(&store, &api, now).await.expect("serve"), 1);
                }
            };
            let contender = async {
                api.entered.notified().await;
                if foreground_first {
                    assert_eq!(
                        tick(&other, &api, crate::now_ms())
                            .await
                            .expect("contending serve"),
                        0
                    );
                    api.release.notify_one();
                } else {
                    let release = async {
                        tokio::time::sleep(Duration::from_millis(25)).await;
                        assert_eq!(api.verdicts.load(Ordering::SeqCst), 1);
                        api.release.notify_one();
                    };
                    let (row, ()) = tokio::join!(
                        wait_for_delivery(
                            &other,
                            &api,
                            "example/app",
                            7,
                            &head,
                            Duration::from_secs(2)
                        ),
                        release
                    );
                    assert_eq!(
                        row.expect("contending foreground").expect("red").state,
                        CiWatchState::Red
                    );
                }
            };
            tokio::time::timeout(Duration::from_secs(3), async {
                tokio::join!(owner, contender)
            })
            .await
            .expect("contention bounded");
            assert_eq!(api.verdicts.load(Ordering::SeqCst), 1);
            assert_eq!(api.closed.load(Ordering::SeqCst), 1);
            assert_eq!(store.list_issues().expect("issues")[0].attempts, 1);
            assert_eq!(
                store
                    .events_for(&fp, 50)
                    .expect("events")
                    .iter()
                    .filter(|e| e.kind == "ci_watch")
                    .count(),
                1
            );
        }
    }

    #[tokio::test]
    async fn foreground_timeout_and_cancellation_leave_recoverable_claims() {
        for cancel in [false, true] {
            for stage in [
                BlockAt::Verdict,
                BlockAt::Snapshot,
                BlockAt::Close,
                BlockAt::PostClose,
            ] {
                let store = Store::open(":memory:").expect("store");
                let now = crate::now_ms();
                let fp = issue(&store, now - 2);
                to_pr_open(&store, &fp, now - 1);
                watch(&store, &fp, now);
                let head = snapshot(PrState::Open, now).head_sha;
                let mut api = BlockingApi::new(stage);
                api.head_at = now;
                tokio::time::timeout(Duration::from_secs(1), async {
                    if cancel {
                        let wait = wait_for_delivery(
                            &store,
                            &api,
                            "example/app",
                            7,
                            &head,
                            Duration::from_secs(60),
                        );
                        tokio::pin!(wait);
                        tokio::select! {
                            _ = api.entered.notified() => {}
                            result = &mut wait => panic!("blocked wait returned: {result:?}"),
                        }
                    } else {
                        assert!(wait_for_delivery(
                            &store,
                            &api,
                            "example/app",
                            7,
                            &head,
                            Duration::from_millis(30)
                        )
                        .await
                        .expect("timeout")
                        .is_none());
                    }
                })
                .await
                .expect("entire wait bounded");
                assert!(api.cancelled.load(Ordering::SeqCst));
                assert_eq!(
                    api.closed.load(Ordering::SeqCst),
                    usize::from(stage == BlockAt::PostClose)
                );
                let row = store.ci_watch_for(&fp).expect("watch").expect("row");
                assert_eq!(row.state, CiWatchState::Pending);
                assert!(row.last_checked_at.is_none());
                assert!(row.last_error.is_none());
                assert!(store
                    .claim_ci_watch_owned(row.id, crate::now_ms(), WATCH_LEASE_MS)
                    .expect("live lease")
                    .is_none());
                assert!(store
                    .claim_ci_watch_owned(
                        row.id,
                        crate::now_ms() + WATCH_LEASE_MS + 1,
                        WATCH_LEASE_MS
                    )
                    .expect("expired lease recoverable")
                    .is_some());
                assert_eq!(store.list_issues().expect("issues")[0].attempts, 0);
                assert!(memory(&store).is_empty());
                assert!(!store
                    .events_for(&fp, 50)
                    .expect("events")
                    .iter()
                    .any(|e| matches!(e.kind.as_str(), "ci_red" | "ci_green")));
            }
        }
    }

    struct SupersedingApi {
        other: Store,
        fp: String,
        replace_run: bool,
        snapshots: AtomicUsize,
        closed: AtomicBool,
    }

    #[async_trait]
    impl CiWatchApi for SupersedingApi {
        async fn verdict(&self, _slug: &str, _sha: &str) -> Result<Option<Verdict>> {
            Ok(Some(Verdict::Failure))
        }

        async fn pr_state(&self, _slug: &str, _number: u64) -> Result<PrSnapshot> {
            if self.snapshots.fetch_add(1, Ordering::SeqCst) == 1 {
                if self.replace_run {
                    self.other.start_run(&RunStart {
                        id: "replacement-run".into(),
                        fingerprint: self.fp.clone(),
                        runner_id: "runner".into(),
                        repo: "example.com/ci-watch".into(),
                        started_at: 20,
                        provider: None,
                        model: None,
                    })?;
                    self.other.update_run(
                        "replacement-run",
                        &RunUpdate {
                            commit_sha: Some(snapshot(PrState::Open, 3).head_sha),
                            pr_url: Some("https://github.com/example/app/pull/7".into()),
                            ..RunUpdate::default()
                        },
                    )?;
                } else {
                    self.other
                        .set_pr(&self.fp, "https://github.com/example/app/pull/8", 20)?;
                }
            }
            Ok(snapshot(PrState::Open, 3))
        }

        async fn close_pr(&self, _slug: &str, _number: u64) -> Result<()> {
            self.closed.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn superseded_issue_delivery_is_rechecked_after_the_second_snapshot() {
        for replace_run in [false, true] {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("watches.db");
            let store = Store::open(&path).expect("store");
            let fp = issue(&store, 1);
            to_pr_open(&store, &fp, 2);
            watch(&store, &fp, 3);
            let api = SupersedingApi {
                other: Store::open(&path).expect("independent store"),
                fp: fp.clone(),
                replace_run,
                snapshots: AtomicUsize::new(0),
                closed: AtomicBool::new(false),
            };
            tick(&store, &api, 10).await.expect("tick");
            assert_eq!(api.snapshots.load(Ordering::SeqCst), 2);
            assert!(!api.closed.load(Ordering::SeqCst));
            let row = store.ci_watch_for(&fp).expect("watch").expect("row");
            assert_eq!(row.state, CiWatchState::HeadChanged);
            assert_eq!(
                row.last_error.as_deref(),
                Some("watch no longer owns current issue delivery")
            );
            assert!(row.terminal_at.is_some());
            assert_eq!(store.list_issues().expect("issues")[0].state, "pr_open");
            assert_eq!(store.list_issues().expect("issues")[0].attempts, 0);
            assert!(memory(&store).is_empty());
        }
    }

    #[tokio::test]
    async fn standalone_watch_without_issue_never_closes_a_pr() {
        let store = Store::open(":memory:").expect("store");
        store
            .upsert_ci_watch(&CiWatchInput {
                run_id: None,
                ..watch_input("standalone", 3)
            })
            .expect("standalone watch");
        let api = FakeApi::new(Verdict::Failure, PrState::Open, 3);
        tick(&store, &api, 10).await.expect("tick");
        assert!(api.closed.lock().expect("closed").is_empty());
        let row = store
            .ci_watch_for("standalone")
            .expect("watch")
            .expect("row");
        assert_eq!(row.state, CiWatchState::HeadChanged);
        assert_eq!(
            row.last_error.as_deref(),
            Some("watch no longer owns current issue delivery")
        );
        assert!(store.list_issues().expect("issues").is_empty());
    }
}
