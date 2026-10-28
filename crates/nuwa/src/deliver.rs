//! 五色石交付面 v0 — verify → branch → commit → push → PR.
//!
//! Guardrails enforced here (not negotiable by the caller):
//! - independent verification gate: the failing check is re-run by Nuwa, not
//!   trusted from the agent's claim; failure aborts delivery.
//! - branch namespace: `agent/fix/<fingerprint>` only, never the base branch.
//! - one branch per fingerprint: an existing branch aborts (no duplicate PRs).

use std::future::Future;
use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context};
use shanhaijing::{CheckoutClaim, DeliveryClaim, DeliveryState, Store};

use crate::git_ops;
use crate::issue::Issue;
use crate::repair::RepairReport;

const NETWORK_GIT_TIMEOUT: Duration = Duration::from_secs(120);
const CLAIM_TTL_MS: i64 = 60_000;
const HEARTBEAT_PERIOD: Duration = Duration::from_secs(10);

#[derive(Debug)]
struct HeartbeatStopped;

impl std::fmt::Display for HeartbeatStopped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("delivery heartbeat stopped; authority is uncertain")
    }
}

impl std::error::Error for HeartbeatStopped {}

/// Only admission denials carry this type; storage/ownership faults do not.
#[derive(Debug)]
pub(crate) struct PrQuotaExceeded;

impl std::fmt::Display for PrQuotaExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("daily PR quota exhausted")
    }
}

impl std::error::Error for PrQuotaExceeded {}

pub(crate) fn ownership_uncertain(error: &anyhow::Error) -> bool {
    error.downcast_ref::<HeartbeatStopped>().is_some()
        || matches!(
            error.downcast_ref::<shanhaijing::ShanError>(),
            Some(shanhaijing::ShanError::OwnershipLost)
        )
}

fn require_claim(valid: bool) -> anyhow::Result<()> {
    if !valid {
        return Err(shanhaijing::ShanError::OwnershipLost.into());
    }
    Ok(())
}

pub(crate) fn ensure_delivery_owned(store: &Store, claim: &DeliveryClaim) -> anyhow::Result<()> {
    require_claim(store.delivery_claim_valid(claim, crate::now_ms())?)
}

pub(crate) async fn with_delivery_heartbeat<T>(
    store: &Store,
    claim: &DeliveryClaim,
    future: impl Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    with_claims_heartbeat(store, Some(claim), None, HEARTBEAT_PERIOD, future).await
}

fn ensure_claims(
    store: &Store,
    delivery: Option<&DeliveryClaim>,
    checkout: Option<&CheckoutClaim>,
) -> anyhow::Result<()> {
    if let Some(claim) = checkout {
        require_claim(store.checkout_claim_valid(claim, crate::now_ms())?)?;
    }
    if let Some(claim) = delivery {
        ensure_delivery_owned(store, claim)?;
    }
    Ok(())
}

async fn with_claims_heartbeat<T>(
    store: &Store,
    delivery: Option<&DeliveryClaim>,
    checkout: Option<&CheckoutClaim>,
    period: Duration,
    future: impl Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    ensure_claims(store, delivery, checkout).context(HeartbeatStopped)?;
    tokio::pin!(future);
    // Renew immediately as well: callers may hand us a still-valid claim
    // with less than one heartbeat period left on its lease.
    let mut ticks = tokio::time::interval_at(tokio::time::Instant::now(), period);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            biased;
            _ = ticks.tick() => {
                let now = crate::now_ms();
                if let Some(claim) = checkout {
                    require_claim(store.renew_checkout_claim(claim, now, CLAIM_TTL_MS)
                        .context(HeartbeatStopped)?).context(HeartbeatStopped)?;
                }
                if let Some(claim) = delivery {
                    require_claim(store.renew_delivery_claim(claim, now, CLAIM_TTL_MS)
                        .context(HeartbeatStopped)?).context(HeartbeatStopped)?;
                }
            }
            result = &mut future => {
                // A response does not confer authority: ownership may have
                // changed while it was in flight (including a lost response).
                ensure_claims(store, delivery, checkout).context(HeartbeatStopped)?;
                return result;
            }
        }
    }
}

struct DeliveryOwnership<'a> {
    store: &'a Store,
    checkout: &'a CheckoutClaim,
    period: Duration,
    healthy: std::sync::atomic::AtomicBool,
}

impl DeliveryOwnership<'_> {
    fn ensure(&self, delivery: Option<&DeliveryClaim>) -> anyhow::Result<()> {
        if !self.healthy.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(anyhow::Error::new(HeartbeatStopped));
        }
        ensure_claims(self.store, delivery, Some(self.checkout)).inspect_err(|_| {
            self.healthy
                .store(false, std::sync::atomic::Ordering::Relaxed);
        })
    }

    async fn wait<T>(
        &self,
        delivery: Option<&DeliveryClaim>,
        future: impl Future<Output = anyhow::Result<T>>,
    ) -> anyhow::Result<T> {
        self.ensure(delivery).context(HeartbeatStopped)?;
        let result = with_claims_heartbeat(
            self.store,
            delivery,
            Some(self.checkout),
            self.period,
            future,
        )
        .await;
        if result.as_ref().err().is_some_and(ownership_uncertain) {
            self.healthy
                .store(false, std::sync::atomic::Ordering::Relaxed);
        }
        result
    }
}

/// Test-only injection point after the branch exists but before the delivery
/// has a durable outcome. Production always passes `None`.
#[cfg_attr(not(test), allow(dead_code))]
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailPoint {
    TamperOwnerAfterVerification,
    TamperCandidateAfterVerification,
    AfterBranch,
    AfterCommit,
    AfterIntent,
    AfterSupersede,
    AfterPush,
    #[cfg(test)]
    RecordTokensAfterPush(i64),
    AfterPrRequested,
    AfterPrResponse,
}

/// Roll back only a known checkout still owned by this delivery. Unknown tips,
/// lost ownership and uncertain heartbeats preserve evidence for recovery.
struct DeliveryGuard<'a> {
    repo: &'a Path,
    verification: &'a crate::verify::VerificationContext,
    // A detached pool checkout must never attach the shared PR base branch.
    // Only an originally attached checkout requires that branch to stay pinned.
    base_ref: Option<String>,
    base_tip: String,
    branch: Option<String>,
    expected_branch: String,
    expected_tip: Option<String>,
    ownership: &'a DeliveryOwnership<'a>,
    delivery: Option<DeliveryClaim>,
    preserve_branch: bool,
    armed: bool,
}

impl<'a> DeliveryGuard<'a> {
    fn new(
        repo: &'a Path,
        base_ref: Option<&str>,
        tip: &str,
        ownership: &'a DeliveryOwnership<'a>,
        verification: &'a crate::verify::VerificationContext,
    ) -> Self {
        Self {
            repo,
            verification,
            base_ref: base_ref.map(str::to_string),
            base_tip: tip.to_string(),
            branch: None,
            expected_branch: base_ref.unwrap_or("HEAD").to_string(),
            expected_tip: Some(tip.to_string()),
            ownership,
            delivery: None,
            preserve_branch: false,
            armed: true,
        }
    }

    fn branch(&mut self, branch: &str, tip: &str) {
        self.branch = Some(branch.to_string());
        self.expected_branch = branch.to_string();
        self.expected_tip = Some(tip.to_string());
    }

    fn ensure(&self) -> anyhow::Result<()> {
        self.ownership.ensure(self.delivery.as_ref())?;
        let expected = self
            .expected_tip
            .as_deref()
            .context("unknown delivery checkout tip")?;
        if git_ops::current_branch(self.repo)? != self.expected_branch
            || git(self.repo, &["rev-parse", "HEAD"])? != expected
        {
            bail!("delivery checkout branch/tip changed; preserving evidence without cleanup");
        }
        if let Some(base) = &self.base_ref {
            let tip = git(self.repo, &["rev-parse", &format!("refs/heads/{base}")])?;
            if tip != self.base_tip {
                bail!("delivery checkout branch/tip changed; preserving evidence without cleanup");
            }
        }
        Ok(())
    }

    fn checkout_initial(&mut self, force: bool) -> anyhow::Result<()> {
        let mut args = vec!["checkout"];
        if force {
            args.push("-f");
        }
        if let Some(base) = &self.base_ref {
            args.push(base);
        } else {
            args.extend(["--detach", self.base_tip.as_str()]);
        }
        git(self.repo, &args)?;
        self.expected_branch = self.base_ref.as_deref().unwrap_or("HEAD").to_string();
        self.expected_tip = Some(self.base_tip.clone());
        Ok(())
    }

    fn restore(&mut self) -> anyhow::Result<()> {
        self.ensure()?;
        self.checkout_initial(false)?;
        self.disarm();
        Ok(())
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
        // Never reset an unknown tree or a successor's checkout. In particular,
        // a failed commit/merge whose new tip is unknown disables cleanup.
        if self.ensure().is_err() || self.verification.assert_integrity(self.repo).is_err() {
            return;
        }
        // Unstage excluded owner seeds before checkout -f can delete a newly
        // indexed file. This changes no original bytes and is never a restore.
        let exclusions = self.verification.excluded_delivery_paths();
        let reset = crate::verify::delivery_reset_args(&exclusions);
        if git_ops::git(
            self.repo,
            &reset.iter().map(String::as_str).collect::<Vec<_>>(),
        )
        .is_err()
        {
            return;
        }
        if self.ensure().is_err() {
            return;
        }
        let delivery_tip = self.expected_tip.clone();
        if self.checkout_initial(true).is_err() {
            return;
        }
        // Recheck authority before each destructive effect, not just on entry.
        if self.ensure().is_err() {
            return;
        }
        // Negative pathspecs alone do not stop Git clean from removing an
        // entire untracked parent directory. Its ignore layer must also see
        // every frozen seed. Nested negative pathspecs also defeat that ignore
        // layer, so clean uses -e for seeds and only the established config
        // pathspec, rather than delivery's nested staging/diff exclusions.
        let ignored: Vec<String> = exclusions
            .iter()
            .map(|path| {
                if path.starts_with(['!', '#']) {
                    format!("\\{path}")
                } else {
                    path.clone()
                }
            })
            .collect();
        let mut args = vec!["clean", "-fd"];
        for pattern in &ignored {
            args.extend(["-e", pattern.as_str()]);
        }
        args.push("--");
        args.push(":(exclude)nuwa.repo.json");
        if git_ops::git(self.repo, &args).is_err() {
            return;
        }
        if self.preserve_branch {
            return;
        }
        let Some(branch) = &self.branch else {
            return;
        };
        if self.ensure().is_ok()
            && git(self.repo, &["rev-parse", &format!("refs/heads/{branch}")]).ok() == delivery_tip
        {
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

/// Delivery failures caused by transport/API availability are not the issue's
/// fault and must not consume an attempt. Guard, verification and diff failures
/// stay issue-level; merge conflicts require a human rebase.
pub fn is_transient_delivery_error(error: &anyhow::Error) -> bool {
    let text = format!("{error:#}").to_ascii_lowercase();
    [
        "push failed",
        "ls-remote failed",
        "timed out",
        "timeout",
        "connection",
        "connect",
        "could not resolve",
        "502",
        "503",
        "504",
        "429",
        "rate limit",
    ]
    .iter()
    .any(|needle| text.contains(needle))
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
pub(crate) fn parse_github_slug(url: &str) -> Option<String> {
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

/// Compare the real index/commit to the verified artifact, not just the
/// excluded working-tree view. Hooks and pre-staged config cannot add bytes.
fn assert_delivery_tree(
    repo: &Path,
    verification: &crate::verify::VerificationContext,
    base: &str,
    tree: &str,
    committed: bool,
) -> anyhow::Result<()> {
    verification.assert_verified_candidate(repo, base)?;
    let actual = if committed {
        git(repo, &["rev-parse", "HEAD^{tree}"])?
    } else {
        git(repo, &["write-tree"])?
    };
    anyhow::ensure!(
        actual == tree,
        "delivery artifact tree differs from verified candidate"
    );
    Ok(())
}

/// Push with transient-network retries; on exhaustion the delivery guard
/// restores the base checkout and drops the local branch.
async fn push_with_retry(
    repo: &Path,
    branch: &str,
    ownership: &DeliveryOwnership<'_>,
    claim: &DeliveryClaim,
    verification: &crate::verify::VerificationContext,
    base: &str,
    tree: &str,
) -> anyhow::Result<()> {
    let mut last = anyhow::anyhow!("unreachable");
    for attempt in 1..=3 {
        ownership.ensure(Some(claim))?;
        let expected = git(repo, &["rev-parse", &format!("refs/heads/{branch}")])?;
        if expected != claim.delivery.head_sha {
            bail!("delivery branch changed before push; preserving evidence");
        }
        assert_delivery_tree(repo, verification, base, tree, true)?;
        match ownership
            .wait(
                Some(claim),
                git_ops::push_branch(repo, branch, NETWORK_GIT_TIMEOUT),
            )
            .await
        {
            Ok(()) => return Ok(()),
            Err(e) if e.downcast_ref::<HeartbeatStopped>().is_some() => return Err(e),
            Err(e) => last = e,
        }
        if attempt < 3 {
            eprintln!("push retry {attempt}/3 (transient network error), waiting 5s…");
            ownership
                .wait(Some(claim), async {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    Ok(())
                })
                .await?;
        }
    }
    Err(last.context(format!("push failed for branch {branch}")))
}

/// Remote tip of `refs/heads/<branch>`; `None` when the branch is absent.
async fn remote_branch_tip(
    repo: &Path,
    branch: &str,
    timeout: Duration,
) -> anyhow::Result<Option<String>> {
    let out = git_ops::git_ok(
        repo,
        &[
            "ls-remote",
            "--heads",
            "origin",
            &format!("refs/heads/{branch}"),
        ],
        timeout,
    )
    .await?;
    Ok(out
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().next())
        .map(str::to_string))
}

/// 交付证据必须对应远端真实存在的那个提交。ls-remote 失败或指向别的 SHA
/// 都按失败处理——"推上去了"不是证据，"推到这一步"才是。
async fn assert_pushed_commit(
    repo: &Path,
    branch: &str,
    expected: &str,
    timeout: Duration,
) -> anyhow::Result<()> {
    let mut last = String::from("(no remote branch)");
    for attempt in 1..=3 {
        match remote_branch_tip(repo, branch, timeout).await {
            Ok(Some(tip)) if tip == expected => return Ok(()),
            Ok(Some(tip)) => last = tip,
            Ok(None) => last = String::from("(no remote branch)"),
            Err(e) => last = format!("(ls-remote failed: {e:#})"),
        }
        if attempt < 3 {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    }
    bail!("pushed branch {branch} does not point at the verified commit {expected}: {last}");
}

/// Best-effort base refresh, before the first push. Verification remains pinned
/// to the original base when the remote is unavailable.
async fn fetch_base_tip(repo: &Path, base: &str, timeout: Duration) -> Option<String> {
    match git_ops::git_ok(repo, &["fetch", "--quiet", "origin", base], timeout).await {
        // Pin the fetched commit, even when origin's refspec does not update
        // refs/remotes/origin/<base>.
        Ok(_) => git(repo, &["rev-parse", "--verify", "FETCH_HEAD"]).ok(),
        Err(e) => {
            eprintln!("deliver   : base refresh skipped: {e:#}");
            None
        }
    }
}

/// Create the PR through the GitHub REST API. Per ADR-004 the server holds
/// minimal credentials: one fine-grained token with `pulls:write` scoped to
/// the registered repos — no interactive `gh` login, no CLI dependency.
/// Push itself stays on git (deploy key), separate from this token.
async fn create_pr_via_api(
    token: &str,
    input: &PullRequestInput<'_>,
) -> anyhow::Result<(String, u64, Option<i64>)> {
    let resp = crate::ci::github_http_client()?
        .post(format!("https://api.github.com/repos/{}/pulls", input.slug))
        .bearer_auth(token)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("User-Agent", "nuwa")
        .json(&serde_json::json!({
            "title": input.title,
            "head": input.branch,
            "base": input.base,
            "body": input.body,
            "draft": input.draft,
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
            if details.is_empty() {
                String::new()
            } else {
                format!(" — {details}")
            }
        );
    }
    let recovered = RecoveredPullRequest::from_json(&payload).context("malformed PR response")?;
    if payload["state"].as_str() != Some("open") || !recovered.matches(input.head_sha, input.base) {
        bail!("created PR head/base does not match verified delivery");
    }
    Ok((recovered.url, recovered.number, recovered.created_at_ms))
}

struct PullRequestInput<'a> {
    slug: &'a str,
    base: &'a str,
    branch: &'a str,
    head_sha: &'a str,
    title: &'a str,
    body: &'a str,
    draft: bool,
}

/// The production delivery path and local integration tests use this same
/// boundary. Git transport remains real; tests replace only GitHub HTTP.
#[async_trait::async_trait]
trait DeliveryGitHub: Send + Sync {
    fn repository(&self, repo: &Path) -> anyhow::Result<String>;
    async fn create(
        &self,
        input: &PullRequestInput<'_>,
    ) -> anyhow::Result<(String, u64, Option<i64>)>;
    async fn find(
        &self,
        slug: &str,
        branch: &str,
        head: &str,
        base: &str,
    ) -> anyhow::Result<Option<RecoveredPullRequest>>;
}

struct GitHubDelivery<'a> {
    token: Option<&'a str>,
}

impl GitHubDelivery<'_> {
    fn token(&self) -> anyhow::Result<&str> {
        self.token.context(
            "NUWA_GITHUB_TOKEN not set — PR creation needs a fine-grained token with pulls:write",
        )
    }
}

#[async_trait::async_trait]
impl DeliveryGitHub for GitHubDelivery<'_> {
    fn repository(&self, repo: &Path) -> anyhow::Result<String> {
        self.token()?;
        let origin = git(repo, &["remote", "get-url", "origin"])?;
        parse_github_slug(&origin).with_context(|| {
            format!(
                "parse GitHub owner/repo from origin `{}`",
                git_ops::sanitize_remote(&origin)
            )
        })
    }

    async fn create(
        &self,
        input: &PullRequestInput<'_>,
    ) -> anyhow::Result<(String, u64, Option<i64>)> {
        create_pr_via_api(self.token()?, input).await
    }

    async fn find(
        &self,
        slug: &str,
        branch: &str,
        head: &str,
        base: &str,
    ) -> anyhow::Result<Option<RecoveredPullRequest>> {
        find_pr_by_head(self.token()?, slug, branch, head, base).await
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecoveredPullRequest {
    pub url: String,
    pub number: u64,
    pub head_sha: String,
    pub base_ref: String,
    pub created_at_ms: Option<i64>,
}

impl RecoveredPullRequest {
    fn from_json(item: &serde_json::Value) -> Option<Self> {
        let recovered = Self {
            url: item["html_url"].as_str()?.to_string(),
            number: item["number"].as_u64()?,
            head_sha: item["head"]["sha"].as_str()?.to_string(),
            base_ref: item["base"]["ref"].as_str()?.to_string(),
            created_at_ms: item["created_at"].as_str().and_then(github_created_at_ms),
        };
        if recovered.url.is_empty()
            || recovered.number == 0
            || recovered.head_sha.is_empty()
            || recovered.base_ref.is_empty()
        {
            return None;
        }
        Some(recovered)
    }

    pub(crate) fn matches(&self, head_sha: &str, base_ref: &str) -> bool {
        self.head_sha == head_sha && self.base_ref == base_ref
    }
}

/// GitHub emits UTC RFC3339 timestamps with whole seconds. Invalid or missing
/// creation metadata must not prevent adoption of an otherwise valid PR.
fn github_created_at_ms(value: &str) -> Option<i64> {
    use cookie::time::{Date, Month, Time};

    let bytes = value.as_bytes();
    if bytes.len() != 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || bytes[19] != b'Z'
        || bytes.iter().enumerate().any(|(index, byte)| {
            !matches!(index, 4 | 7 | 10 | 13 | 16 | 19) && !byte.is_ascii_digit()
        })
    {
        return None;
    }
    let number = |range: std::ops::Range<usize>| value.get(range)?.parse::<u8>().ok();
    let date = Date::from_calendar_date(
        value.get(..4)?.parse().ok()?,
        Month::try_from(number(5..7)?).ok()?,
        number(8..10)?,
    )
    .ok()?;
    let time = Time::from_hms(number(11..13)?, number(14..16)?, number(17..19)?).ok()?;
    date.with_time(time)
        .assume_utc()
        .unix_timestamp()
        .checked_mul(1_000)
        .filter(|created| *created > 0)
}

fn select_recovered_pr(
    items: &[serde_json::Value],
    expected_head_sha: &str,
    expected_base_ref: &str,
) -> anyhow::Result<Option<RecoveredPullRequest>> {
    if items.is_empty() {
        return Ok(None);
    }
    // More than one result, closed history, malformed data, or a different
    // head/base is evidence, NOT proof that there is no PR for this branch.
    if items.len() != 1 || items[0]["state"].as_str() != Some("open") {
        bail!("PR lookup inconclusive: multiple or non-open PRs for branch");
    }
    let recovered = RecoveredPullRequest::from_json(&items[0])
        .context("PR lookup inconclusive: malformed PR")?;
    if !recovered.matches(expected_head_sha, expected_base_ref) {
        bail!("PR lookup inconclusive: head/base differs from verified delivery");
    }
    Ok(Some(recovered))
}

fn pr_lookup_url(slug: &str, branch: &str) -> anyhow::Result<reqwest::Url> {
    let owner = slug
        .split('/')
        .next()
        .filter(|owner| !owner.is_empty())
        .context("origin slug has no owner")?;
    let mut url = reqwest::Url::parse(&format!("https://api.github.com/repos/{slug}/pulls"))?;
    url.query_pairs_mut()
        .append_pair("state", "all")
        .append_pair("head", &format!("{owner}:{branch}"))
        .append_pair("per_page", "100");
    Ok(url)
}

fn recover_pr_response(
    payload: &serde_json::Value,
    paginated: bool,
    expected_head_sha: &str,
    expected_base_ref: &str,
) -> anyhow::Result<Option<RecoveredPullRequest>> {
    let items = payload
        .as_array()
        .context("PR lookup inconclusive: expected array")?;
    if paginated || items.len() >= 100 {
        bail!("PR lookup inconclusive: pagination limit");
    }
    select_recovered_pr(items, expected_head_sha, expected_base_ref)
}

/// Find the PR that GitHub may have created even though the HTTP response was
/// lost. Only an empty, well-formed, complete response means `Ok(None)`.
/// reqwest encodes the raw `owner:branch` query value exactly once.
pub(crate) async fn find_pr_by_head(
    token: &str,
    slug: &str,
    branch: &str,
    expected_head_sha: &str,
    expected_base_ref: &str,
) -> anyhow::Result<Option<RecoveredPullRequest>> {
    let resp = crate::ci::github_http_client()?
        .get(pr_lookup_url(slug, branch)?)
        .bearer_auth(token)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("User-Agent", "nuwa")
        .send()
        .await
        .context("GET pulls by head")?;
    let status = resp.status();
    let paginated = resp.headers().contains_key(reqwest::header::LINK);
    let payload: serde_json::Value = resp.json().await.context("parse PR lookup response")?;
    if !status.is_success() {
        bail!(
            "find PR by head {status}: {}",
            payload["message"].as_str().unwrap_or("(no message)")
        );
    }
    recover_pr_response(&payload, paginated, expected_head_sha, expected_base_ref)
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
    store: Option<&shanhaijing::Store>,
    run_id: Option<&str>,
    repo_id: &str,
    checkout_claim: Option<&CheckoutClaim>,
    verification: &crate::verify::VerificationContext,
    limits: crate::quota::DeliveryLimits,
) -> anyhow::Result<DeliverReport> {
    let github = GitHubDelivery {
        token: secrets.github_token.as_deref(),
    };
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
        store,
        run_id,
        repo_id,
        checkout_claim,
        verification,
        limits,
        &github,
        HEARTBEAT_PERIOD,
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
    store: Option<&shanhaijing::Store>,
    run_id: Option<&str>,
    repo_id: &str,
    checkout_claim: Option<&CheckoutClaim>,
    verification: &crate::verify::VerificationContext,
    limits: crate::quota::DeliveryLimits,
    github: &dyn DeliveryGitHub,
    heartbeat_period: Duration,
    fail_point: Option<FailPoint>,
) -> anyhow::Result<DeliverReport> {
    // ---- 1. cheap preconditions ----
    // Refusals must precede even local branch/staging effects and API lookup.
    verification.assert_delivery_allowed()?;
    verification.assert_integrity(repo)?;
    // Even a push without a PR needs durable intent and checkout authority.
    // Local callers/tests supply a private Store/claim rather than bypassing
    // the same guard used by the network path.
    let store = store.context("delivery requires a Store and checkout claim")?;
    let checkout = checkout_claim.context("delivery requires a valid checkout claim")?;
    if std::fs::canonicalize(repo)? != std::fs::canonicalize(&checkout.path)? {
        bail!("checkout claim belongs to a different checkout");
    }
    let ownership = DeliveryOwnership {
        store,
        checkout,
        period: heartbeat_period,
        healthy: std::sync::atomic::AtomicBool::new(true),
    };
    ownership.ensure(None)?;
    limits.check_tokens(store, repo_id, run_id, report, crate::now_ms())?;
    // Resolve credentials/remote BEFORE mutating or pushing anything. The fake
    // uses a fixed GitHub identity with a real local bare Git transport.
    let slug = if push && create_pr {
        Some(github.repository(repo)?)
    } else {
        None
    };
    let exclusions = verification.excluded_delivery_paths();
    let pathspecs = crate::verify::delivery_pathspecs(&exclusions);
    let mut args = vec!["status", "--porcelain", "--"];
    args.extend(pathspecs.iter().map(String::as_str));
    let status = git(repo, &args)?;
    if status.trim().is_empty() {
        bail!("agent made no changes — refusing to open an empty PR");
    }
    let initial_branch = git_ops::current_branch(repo)?;
    if initial_branch.starts_with("agent/fix/") {
        bail!("delivery must start on the base branch, not an agent/fix branch");
    }
    let initial_ref = (initial_branch != "HEAD").then_some(initial_branch.as_str());
    let base = match initial_ref {
        Some(base) => base.to_string(),
        None => crate::resolve_base(repo)?.to_string(),
    };
    // The logical PR base may move in another worktree. Verification and
    // restoration must stay pinned to the commit this worker actually repaired.
    let base_oid = git(repo, &["rev-parse", "--verify", "HEAD"])
        .context("resolve initial delivery checkout tip")?;
    anyhow::ensure!(
        base_oid == verification.base_sha(),
        "delivery base differs from prepared repair"
    );
    let branch = format!("agent/fix/{}", issue.short_fingerprint());
    if git(
        repo,
        &["rev-parse", "--verify", &format!("refs/heads/{branch}")],
    )
    .is_ok()
    {
        bail!("branch {branch} already exists — this fingerprint already has work in flight; refusing a duplicate PR");
    }
    // Created before verification so a failed gate gets the same clean
    // checkout/untracked-file rollback as a failed commit or PR call.
    let mut guard = DeliveryGuard::new(repo, initial_ref, &base_oid, &ownership, verification);

    // ---- 2. independent verification gate (base red / branch green) ----
    println!(
        "deliver   : verification gate `{}`…",
        issue.failing_check.command
    );
    let mut evidence = ownership
        .wait(
            None,
            crate::verify::verify(repo, issue, &base_oid, secrets, verification),
        )
        .await?;
    guard.ensure()?;
    verification.assert_verified_candidate(repo, &base_oid)?;
    let mut verified_base = base_oid.clone();
    let check_command = secrets.redact(&issue.failing_check.command);
    let mut evidence_detail = secrets.redact(&evidence.detail(issue));
    let evidence_path = sessions_dir
        .join(issue.short_fingerprint())
        .join("verification.txt");
    std::fs::create_dir_all(evidence_path.parent().with_context(|| "evidence dir")?)?;
    guard.ensure()?;
    std::fs::write(&evidence_path, &evidence_detail).context("write evidence")?;
    let mut verification_detail = truncate_evidence(&evidence_detail, 16_000);

    if fail_point == Some(FailPoint::TamperOwnerAfterVerification) {
        std::fs::write(repo.join("nuwa.repo.json"), "{}")?;
    }
    if fail_point == Some(FailPoint::TamperCandidateAfterVerification) {
        std::fs::write(repo.join("main.go"), "package tampered\n")?;
    }

    // ---- 3. branch + commit ----
    guard.ensure()?;
    verification.assert_verified_candidate(repo, &verified_base)?;
    git(repo, &["checkout", "-b", &branch])?;
    guard.branch(&branch, &base_oid);
    if fail_point == Some(FailPoint::AfterBranch) {
        anyhow::bail!("test fail point: after branch");
    }
    // Config and genuinely untracked owner inputs are not repair changes.
    // Use exactly the same exclusions/reset as the verified candidate index.
    guard.ensure()?;
    verification.assert_verified_candidate(repo, &verified_base)?;
    let add = crate::verify::delivery_add_args(repo, &exclusions)?;
    git(repo, &add.iter().map(String::as_str).collect::<Vec<_>>())?;
    // The candidate excludes even pre-staged inputs; the real index must use
    // the identical semantics, otherwise the committed artifact is different.
    let reset = crate::verify::delivery_reset_args(&exclusions);
    git(repo, &reset.iter().map(String::as_str).collect::<Vec<_>>())?;
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
    guard.ensure()?;
    assert_delivery_tree(
        repo,
        verification,
        &verified_base,
        &evidence.provenance.candidate_tree,
        false,
    )?;
    // If commit fails (e.g. a hook moved HEAD) we do not know the resulting tip.
    guard.expected_tip = None;
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
    let mut commit = git(repo, &["rev-parse", "HEAD"])?;
    guard.expected_tip = Some(commit.clone());
    guard.ensure()?;
    assert_delivery_tree(
        repo,
        verification,
        &verified_base,
        &evidence.provenance.candidate_tree,
        true,
    )?;
    if fail_point == Some(FailPoint::AfterCommit) {
        anyhow::bail!("test fail point: after commit");
    }
    let mut stat = git(repo, &["diff", "--stat", &format!("{base_oid}...HEAD")])?;
    let mut change_stat = truncate_evidence(&stat, 4_000);

    // ---- 4. push + PR ----
    let mut pr_url = None;
    let mut slug_out = None;
    let mut pr_number = None;
    let mut pr_created_at_ms = None;
    let mut delivery = if push {
        guard.ensure()?;
        Some(
            store
                .claim_delivery(
                    &shanhaijing::DeliveryStart {
                        id: uuid::Uuid::new_v4().to_string(),
                        run_id: run_id.map(str::to_string),
                        fingerprint: issue.fingerprint.clone(),
                        repo: repo_id.to_string(),
                        branch: branch.clone(),
                        base: base.clone(),
                        head_sha: commit.clone(),
                        now: crate::now_ms(),
                    },
                    &uuid::Uuid::new_v4().to_string(),
                    CLAIM_TTL_MS,
                )?
                .context("delivery identity is already owned or terminal")?,
        )
    } else {
        None
    };
    guard.delivery = delivery.clone();
    if delivery
        .as_ref()
        .is_some_and(|claim| claim.delivery.state != DeliveryState::Intent)
    {
        guard.preserve_branch = true;
        bail!(
            "unfinished delivery already has remote effects; recovery is required before retrying"
        );
    }
    if push && !create_pr {
        guard.ensure()?;
        // Persist the mode BEFORE any push attempt. A crash or lost response
        // must never let PR recovery mistake this branch for an absent-PR orphan.
        store.mark_delivery_owned(
            delivery.as_ref().context("missing delivery claim")?,
            DeliveryState::BranchOnly,
            None,
            None,
            Some("branch-only push; interruption requires manual review"),
            crate::now_ms(),
        )?;
    }
    if fail_point == Some(FailPoint::AfterIntent) {
        guard.disarm();
        bail!("test crash: after intent");
    }
    if push {
        // 基线在验证之后移动时，PR 会落后于 base（GitHub 也可能因此不开
        // 检查）。只合并一次：合并树重新过红绿验证，再把新 tip 推上去；
        // 冲突则中止交付、交人工 rebase——绝不把未验证的树提成 PR。
        if create_pr {
            guard.ensure()?;
            if let Some(tip) = ownership
                .wait(delivery.as_ref(), async {
                    Ok(fetch_base_tip(repo, &base, NETWORK_GIT_TIMEOUT).await)
                })
                .await?
            {
                let contains_base =
                    git(repo, &["merge-base", "--is-ancestor", &tip, &commit]).is_ok();
                if tip != base_oid {
                    println!(
                        "deliver   : base {base} moved after verification — merging once and re-verifying"
                    );
                    let merge_msg = format!(
                        "chore: merge origin/{base} before delivery\n\nNuwa re-verifies the merged tree."
                    );
                    guard.ensure()?;
                    assert_delivery_tree(
                        repo,
                        verification,
                        &verified_base,
                        &evidence.provenance.candidate_tree,
                        true,
                    )?;
                    if !contains_base {
                        guard.expected_tip = None;
                        if let Err(e) = git(
                            repo,
                            &[
                                "-c",
                                "user.name=nuwa[bot]",
                                "-c",
                                "user.email=nuwa@local",
                                "merge",
                                "--no-commit",
                                "--no-ff",
                                "-m",
                                &merge_msg,
                                &tip,
                            ],
                        ) {
                            // Unknown merge state: preserve it for recovery rather
                            // than resetting a tree we can no longer identify.
                            bail!(
                                "base {base} moved and merging it conflicted — delivery aborted, manual rebase required: {e:#}"
                            );
                        }
                        // --no-commit leaves HEAD on the known original commit.
                        guard.expected_tip = Some(commit.clone());
                    }
                    guard.ensure()?;
                    evidence = ownership
                        .wait(
                            delivery.as_ref(),
                            crate::verify::verify(repo, issue, &tip, secrets, verification),
                        )
                        .await
                        .context("base moved; the merged tree failed re-verification")?;
                    guard.ensure()?;
                    verified_base = tip.clone();
                    assert_delivery_tree(
                        repo,
                        verification,
                        &verified_base,
                        &evidence.provenance.candidate_tree,
                        false,
                    )?;
                    if !contains_base {
                        guard.expected_tip = None;
                        git(
                            repo,
                            &[
                                "-c",
                                "user.name=nuwa[bot]",
                                "-c",
                                "user.email=nuwa@local",
                                "commit",
                                "--no-edit",
                            ],
                        )?;
                        guard.expected_tip = Some(git(repo, &["rev-parse", "HEAD"])?);
                    }
                    let merged = git(repo, &["rev-parse", "HEAD"])?;
                    guard.ensure()?;
                    assert_delivery_tree(
                        repo,
                        verification,
                        &verified_base,
                        &evidence.provenance.candidate_tree,
                        true,
                    )?;
                    evidence_detail = secrets.redact(&evidence.detail(issue));
                    std::fs::write(&evidence_path, &evidence_detail)
                        .context("write merged evidence")?;
                    verification_detail = truncate_evidence(&evidence_detail, 16_000);
                    stat = git(repo, &["diff", "--stat", &format!("{tip}...HEAD")])?;
                    change_stat = truncate_evidence(&stat, 4_000);
                    if merged != commit {
                        commit = merged;
                        guard.ensure()?;
                        let next = store.supersede_delivery_owned(
                            delivery.as_ref().context("missing delivery claim")?,
                            &commit,
                            crate::now_ms(),
                            CLAIM_TTL_MS,
                        )?;
                        delivery = Some(next);
                        guard.delivery = delivery.clone();
                        if fail_point == Some(FailPoint::AfterSupersede) {
                            guard.disarm();
                            bail!("test crash: after supersede");
                        }
                    }
                }
            }
        }
        let claim = delivery.as_ref().context("missing delivery claim")?;
        guard.ensure()?;
        assert_delivery_tree(
            repo,
            verification,
            &verified_base,
            &evidence.provenance.candidate_tree,
            true,
        )?;
        if create_pr
            && !store.reserve_pr_slot_owned(claim, limits.daily_pr_limit, crate::now_ms())?
        {
            guard.ensure()?;
            store.mark_delivery_owned(
                claim,
                DeliveryState::Failed,
                None,
                None,
                Some("PR quota denied before any push attempt"),
                crate::now_ms(),
            )?;
            // Failed revokes delivery authority. Only this never-pushed local
            // branch may now be rolled back under the still-live checkout claim;
            // the guard continues checking its exact tip and frozen inputs.
            guard.delivery = None;
            return Err(PrQuotaExceeded.into());
        }
        // Once a push is attempted even its response can be lost. Keep the
        // exact local branch as evidence on every subsequent error path.
        guard.preserve_branch = true;
        push_with_retry(
            repo,
            &branch,
            &ownership,
            claim,
            verification,
            &verified_base,
            &evidence.provenance.candidate_tree,
        )
        .await?;
        if fail_point == Some(FailPoint::AfterPush) {
            guard.disarm();
            bail!("test crash: after push");
        }
        // 交付证据钉在远端真实 tip 上，而不是"推送命令返回成功"。
        ownership
            .wait(
                Some(claim),
                assert_pushed_commit(repo, &branch, &commit, NETWORK_GIT_TIMEOUT),
            )
            .await?;
        guard.ensure()?;
        assert_delivery_tree(
            repo,
            verification,
            &verified_base,
            &evidence.provenance.candidate_tree,
            true,
        )?;
        if create_pr {
            store.mark_delivery_owned(
                claim,
                DeliveryState::Pushed,
                None,
                None,
                Some("remote tip verified"),
                crate::now_ms(),
            )?;
            #[cfg(test)]
            if let Some(FailPoint::RecordTokensAfterPush(tokens)) = fail_point {
                store.update_run(
                    run_id.context("token injection requires a recorded run")?,
                    &shanhaijing::RunUpdate {
                        tokens_uncached: Some(tokens),
                        ..Default::default()
                    },
                )?;
            }
            let slug = slug.as_deref().context("missing GitHub identity")?;
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
            let pr_title = format!("fix: {title}");
            let input = PullRequestInput {
                slug,
                base: &base,
                branch: &branch,
                head_sha: &commit,
                title: &pr_title,
                body: &body,
                draft,
            };
            guard.ensure()?;
            assert_delivery_tree(
                repo,
                verification,
                &verified_base,
                &evidence.provenance.candidate_tree,
                true,
            )?;
            // Re-read durable usage at the final gate. After a push, quota
            // refusal must not trigger compensation: keep the exact checkout,
            // Pushed journal and reservation for find-only recovery. Releasing
            // a lease (or its expiry) is deliberately not a quota refund.
            if let Err(error) = limits.check_tokens(store, repo_id, run_id, report, crate::now_ms())
            {
                guard.disarm();
                return Err(error);
            }
            if !store.reserve_pr_slot_owned(claim, limits.daily_pr_limit, crate::now_ms())? {
                guard.disarm();
                return Err(PrQuotaExceeded.into());
            }
            store.mark_delivery_owned(
                claim,
                DeliveryState::PrRequested,
                None,
                None,
                Some("requesting PR"),
                crate::now_ms(),
            )?;
            if fail_point == Some(FailPoint::AfterPrRequested) {
                guard.disarm();
                bail!("test crash: after PR requested");
            }
            let created_pr = ownership.wait(Some(claim), github.create(&input)).await;
            let (url, number, created_at_ms) = match created_pr {
                Ok(created) => created,
                Err(e) => {
                    // Heartbeat failure is not an API failure. Do not perform
                    // another effect or change PrRequested after losing authority.
                    if ownership_uncertain(&e) {
                        return Err(e);
                    }
                    guard.ensure()?;
                    // A lost response means the PR may already exist. Adopt it
                    // only when it still identifies the exact verified delivery.
                    match ownership
                        .wait(Some(claim), github.find(slug, &branch, &commit, &base))
                        .await
                    {
                        Ok(Some(recovered)) if recovered.matches(&commit, &base) => {
                            println!(
                                "deliver   : PR creation response lost — adopted existing PR #{}",
                                recovered.number
                            );
                            (recovered.url, recovered.number, recovered.created_at_ms)
                        }
                        Ok(Some(recovered)) => {
                            guard.ensure()?;
                            store.mark_delivery_owned(
                                claim,
                                DeliveryState::Ambiguous,
                                Some(&recovered.url),
                                Some(recovered.number),
                                Some("recovered PR head/base differs from the verified delivery"),
                                crate::now_ms(),
                            )?;
                            return Err(anyhow::anyhow!(
                                "PR #{} head/base {}/{} does not match verified {}/{}",
                                recovered.number,
                                recovered.head_sha,
                                recovered.base_ref,
                                commit,
                                base
                            ));
                        }
                        Ok(None) => {
                            // Only definitive absence permits a lease-protected
                            // compare-and-delete. Changed/unknown refs keep evidence.
                            guard.ensure()?;
                            let deletion = ownership
                                .wait(Some(claim), async {
                                    git_ops::delete_remote_branch_if_matches(
                                        repo,
                                        &branch,
                                        &commit,
                                        Duration::from_secs(60),
                                    )
                                    .await?;
                                    if remote_branch_tip(repo, &branch, NETWORK_GIT_TIMEOUT)
                                        .await?
                                        .is_some()
                                    {
                                        bail!("orphan remote ref changed; preserving evidence");
                                    }
                                    Ok(())
                                })
                                .await;
                            let deletion = match deletion {
                                Err(error)
                                    if error.downcast_ref::<HeartbeatStopped>().is_some() =>
                                {
                                    return Err(error)
                                }
                                result => result,
                            };
                            guard.ensure()?;
                            if let Err(delete_error) = deletion {
                                store.mark_delivery_owned(
                                    claim,
                                    DeliveryState::Ambiguous,
                                    None,
                                    None,
                                    Some("PR absent but remote branch deletion inconclusive"),
                                    crate::now_ms(),
                                )?;
                                return Err(delete_error.context("safe orphan deletion failed"));
                            }
                            // Restore while the claim is still live; Failed is
                            // terminal and intentionally revokes delivery authority.
                            guard.restore()?;
                            ownership.ensure(Some(claim))?;
                            store.mark_delivery_owned(
                                claim,
                                DeliveryState::Failed,
                                None,
                                None,
                                Some("PR creation failed; no PR found"),
                                crate::now_ms(),
                            )?;
                            return Err(
                                e.context(format!("PR creation failed for branch {branch}"))
                            );
                        }
                        Err(lookup_error) => {
                            // Ambiguous: keep the branch and let startup
                            // reconciliation decide with a later lookup.
                            if ownership_uncertain(&lookup_error) {
                                return Err(lookup_error);
                            }
                            guard.ensure()?;
                            store.mark_delivery_owned(
                                claim,
                                DeliveryState::Ambiguous,
                                None,
                                None,
                                Some("PR lookup inconclusive; preserve branch for recovery"),
                                crate::now_ms(),
                            )?;
                            return Err(e.context(format!(
                                "PR creation failed and head lookup was inconclusive for branch {branch}: {lookup_error:#}"
                            )));
                        }
                    }
                }
            };
            if fail_point == Some(FailPoint::AfterPrResponse) {
                guard.disarm();
                bail!("test crash: after PR response");
            }
            pr_url = Some(url);
            slug_out = Some(slug.to_string());
            pr_number = Some(number);
            pr_created_at_ms = created_at_ms;
        }
    }

    // Keep the delivery branch, but restore the original checkout topology:
    // attached base for standalone runs, detached pinned tip for pool workers.
    if pr_url.is_some() {
        guard.ensure()?;
        ownership
            .wait(
                delivery.as_ref(),
                assert_pushed_commit(repo, &branch, &commit, NETWORK_GIT_TIMEOUT),
            )
            .await?;
    }
    guard.ensure()?;
    assert_delivery_tree(
        repo,
        verification,
        &verified_base,
        &evidence.provenance.candidate_tree,
        true,
    )?;
    guard.restore()?;
    if let Some(claim) = &delivery {
        ownership.ensure(Some(claim))?;
        if let (Some(url), Some(number), Some(slug)) = (&pr_url, pr_number, &slug_out) {
            let now = crate::now_ms();
            // The runner's published configuration is trusted metadata, unlike
            // an agent-editable working-tree nuwa.repo.json. Standalone runs
            // have the established default attempt budget (3).
            let max_attempts = store
                .list_runners()?
                .iter()
                .find(|runner| {
                    runner.repo == repo_id
                        && runner.repo_path.as_deref() == Some(checkout.path.as_str())
                })
                .and_then(|runner| {
                    serde_json::from_str::<serde_json::Value>(&runner.config_json).ok()
                })
                .and_then(|config| config["max_attempts"].as_u64())
                .unwrap_or(3)
                .max(1);
            let watch = shanhaijing::CiWatchInput {
                run_id: run_id.map(str::to_string),
                fingerprint: issue.fingerprint.clone(),
                repo: repo_id.to_string(),
                pr_url: url.clone(),
                slug: slug.clone(),
                pr_number: number,
                head_sha: commit.clone(),
                root_cause: Some(root_cause.clone()),
                change_stat: Some(change_stat.clone()),
                now,
                next_check_at: now,
                max_attempts,
            };
            ownership.ensure(Some(claim))?;
            store.complete_delivery_owned_at(
                claim,
                url,
                number,
                Some(&watch),
                pr_created_at_ms,
                crate::now_ms(),
            )?;
        } else {
            store.mark_delivery_owned(
                claim,
                DeliveryState::BranchPushed,
                None,
                None,
                Some("branch-only remote tip verified; checkout restored"),
                crate::now_ms(),
            )?;
        }
    }

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
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex;

    #[derive(Clone, Copy)]
    enum ApiMode {
        Success,
        ResponseLost,
        Empty,
        Inconclusive,
        WrongHead,
        WrongBase,
        AlterRemote,
        Blocked,
    }

    struct FakeGitHub {
        mode: ApiMode,
        origin: Option<PathBuf>,
        store_path: Option<PathBuf>,
        slot_at_create: Mutex<Option<(String, shanhaijing::PrSlotRow)>>,
        created: Mutex<Vec<RecoveredPullRequest>>,
        bodies: Mutex<Vec<String>>,
        creates: AtomicUsize,
        lookups: AtomicUsize,
        entered: tokio::sync::Notify,
        cancelled: AtomicBool,
    }

    const API_CREATED_AT_MS: i64 = 1_709_210_096_000;

    impl FakeGitHub {
        fn new(mode: ApiMode) -> Self {
            Self {
                mode,
                origin: None,
                store_path: None,
                slot_at_create: Mutex::new(None),
                created: Mutex::new(Vec::new()),
                bodies: Mutex::new(Vec::new()),
                creates: AtomicUsize::new(0),
                lookups: AtomicUsize::new(0),
                entered: tokio::sync::Notify::new(),
                cancelled: AtomicBool::new(false),
            }
        }
    }

    struct CancelledOnDrop<'a>(&'a AtomicBool);

    impl Drop for CancelledOnDrop<'_> {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[async_trait::async_trait]
    impl DeliveryGitHub for FakeGitHub {
        fn repository(&self, _repo: &Path) -> anyhow::Result<String> {
            Ok("acme/app".into())
        }

        async fn create(
            &self,
            input: &PullRequestInput<'_>,
        ) -> anyhow::Result<(String, u64, Option<i64>)> {
            self.creates.fetch_add(1, Ordering::SeqCst);
            if let Some(path) = &self.store_path {
                let store = Store::open(path).expect("independent API observer");
                let deliveries = store.pending_deliveries("acme/app").expect("pending");
                assert_eq!(deliveries.len(), 1);
                let id = &deliveries[0].id;
                let slot = store
                    .pr_slot_for_delivery(id)
                    .expect("slot query")
                    .expect("POST requires a slot");
                assert_eq!(slot.state, "requested");
                *self.slot_at_create.lock().expect("slot") = Some((id.clone(), slot));
            }
            self.bodies
                .lock()
                .expect("bodies")
                .push(input.body.to_string());
            if matches!(self.mode, ApiMode::Blocked) {
                let _cancelled = CancelledOnDrop(&self.cancelled);
                self.entered.notify_one();
                return std::future::pending().await;
            }
            let pr = RecoveredPullRequest {
                url: "https://github.com/acme/app/pull/7".into(),
                number: 7,
                head_sha: input.head_sha.to_string(),
                base_ref: input.base.to_string(),
                created_at_ms: Some(API_CREATED_AT_MS),
            };
            self.created.lock().expect("created").push(pr.clone());
            if matches!(self.mode, ApiMode::Success) {
                Ok((pr.url, pr.number, pr.created_at_ms))
            } else {
                bail!("synthetic PR response lost")
            }
        }

        async fn find(
            &self,
            _slug: &str,
            branch: &str,
            head: &str,
            base: &str,
        ) -> anyhow::Result<Option<RecoveredPullRequest>> {
            self.lookups.fetch_add(1, Ordering::SeqCst);
            if matches!(self.mode, ApiMode::Inconclusive) {
                bail!("synthetic inconclusive lookup");
            }
            if matches!(self.mode, ApiMode::AlterRemote) {
                let origin = self.origin.as_deref().expect("bare origin");
                let tip = git_output(origin, &["rev-parse", "refs/heads/main"]);
                git(
                    origin,
                    &["update-ref", &format!("refs/heads/{branch}"), tip.trim()],
                );
                return Ok(None);
            }
            if matches!(self.mode, ApiMode::Empty) {
                return Ok(None);
            }
            let mut pr = self.created.lock().expect("created").last().cloned();
            if let Some(pr) = &mut pr {
                assert!(
                    pr.matches(head, base),
                    "fake must preserve the requested identity"
                );
                if matches!(self.mode, ApiMode::WrongHead) {
                    pr.head_sha = "unverified-head".into();
                }
                if matches!(self.mode, ApiMode::WrongBase) {
                    pr.base_ref = "release".into();
                }
            }
            Ok(pr)
        }
    }

    fn checkout_claim(store: &Store, repo: &Path) -> CheckoutClaim {
        let path = std::fs::canonicalize(repo).expect("canonical checkout");
        store
            .claim_checkout(
                path.to_str().expect("path"),
                "test",
                crate::now_ms(),
                CLAIM_TTL_MS,
            )
            .expect("claim checkout")
            .expect("checkout owned")
    }

    struct DeliveryFixture {
        dir: tempfile::TempDir,
        repo: PathBuf,
        origin: PathBuf,
        store: Store,
        checkout: CheckoutClaim,
        verification: crate::verify::VerificationContext,
        limits: crate::quota::DeliveryLimits,
        run_id: Option<String>,
    }

    impl DeliveryFixture {
        async fn new() -> Self {
            Self::with_detached_head(false).await
        }

        async fn detached() -> Self {
            Self::with_detached_head(true).await
        }

        async fn with_detached_head(detached: bool) -> Self {
            Self::with_options(detached, None).await
        }

        async fn with_options(detached: bool, private_inputs: Option<bool>) -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            let source = dir.path().join("source");
            init_repo(&source);
            let origin = dir.path().join("origin.git");
            git(
                dir.path(),
                &["init", "--bare", origin.to_str().expect("path")],
            );
            git(
                &source,
                &["remote", "add", "origin", origin.to_str().expect("path")],
            );
            git(&source, &["push", "origin", "main"]);
            let repo = dir.path().join("linked");
            if detached {
                git(
                    &source,
                    &[
                        "worktree",
                        "add",
                        "--detach",
                        repo.to_str().expect("path"),
                        "HEAD",
                    ],
                );
            } else {
                git(&source, &["checkout", "-b", "host"]);
                git(
                    &source,
                    &["worktree", "add", repo.to_str().expect("path"), "main"],
                );
            }
            assert!(
                repo.join(".git").is_file(),
                "must exercise a linked worktree"
            );
            let mut config = serde_json::json!({
                "repo": "acme/app", "failing_check": {"command": check_command()},
            });
            if let Some(ignored) = private_inputs {
                std::fs::create_dir(repo.join("checks")).expect("owner checks");
                std::fs::write(repo.join("checks/oracle.py"),
                    "import os, pathlib, sys\nroot = pathlib.Path(os.environ['NUWA_VERIFY_ROOT'])\nexpected = pathlib.Path(__file__).with_name('expected.txt').read_text()\nfix = root / 'fix.go'\nsys.exit(0 if fix.is_file() and fix.read_text() == expected else 1)\n")
                    .expect("oracle");
                std::fs::write(repo.join("checks/expected.txt"), "package main\n")
                    .expect("owner expected bytes");
                let launcher = if cfg!(windows) { "python" } else { "python3" };
                config["verification_policy"] = serde_json::json!("strict");
                config["trusted_verification"] = serde_json::json!({
                    "check": {"command": format!("{launcher} checks/oracle.py"), "timeout_secs": 10},
                    "protected_inputs": ["checks/oracle.py", "checks/expected.txt"]
                });
                if ignored {
                    let exclude = git_output(&repo, &["rev-parse", "--git-path", "info/exclude"]);
                    std::fs::write(
                        repo.join(exclude.trim()),
                        "/checks/oracle.py\n/checks/expected.txt\n",
                    )
                    .expect("ignore only private input files");
                }
            }
            std::fs::write(repo.join("nuwa.repo.json"), config.to_string()).expect("config");
            let owner = crate::instructions::TrustedInstructions::load(
                &repo,
                &repo.join("nuwa.repo.json"),
                "HEAD",
                &dir.path().join("trusted"),
            )
            .expect("owner");
            let verification = crate::verify::prepare(
                &repo,
                "HEAD",
                Some(&owner),
                &crate::secrets::Secrets::default(),
            )
            .await
            .expect("prepare before edits");
            std::fs::write(repo.join("fix.go"), "package main\n").expect("fix");
            let store = Store::open(dir.path().join("delivery.db")).expect("store");
            let checkout = checkout_claim(&store, &repo);
            Self {
                dir,
                repo,
                origin,
                store,
                checkout,
                verification,
                limits: Default::default(),
                run_id: None,
            }
        }

        async fn deliver(
            &self,
            api: &dyn DeliveryGitHub,
            push: bool,
            pr: bool,
            fail: Option<FailPoint>,
        ) -> anyhow::Result<DeliverReport> {
            let mut issue = issue();
            issue.forbidden_paths = self.verification.protected_paths();
            deliver_inner(
                &self.repo,
                &issue,
                &report(self.dir.path()),
                push,
                pr,
                &self.dir.path().join("sessions"),
                &crate::secrets::Secrets::default(),
                None,
                false,
                Some(&self.store),
                self.run_id.as_deref(),
                "acme/app",
                Some(&self.checkout),
                &self.verification,
                self.limits,
                api,
                Duration::from_millis(5),
                fail,
            )
            .await
        }

        fn pending(&self) -> shanhaijing::DeliveryRow {
            let rows = self.store.pending_deliveries("acme/app").expect("pending");
            assert_eq!(rows.len(), 1);
            rows.into_iter().next().expect("delivery")
        }

        fn record_run(&mut self, tokens: i64) {
            self.store
                .report_signal(
                    &shanhaijing::Signal {
                        repo: "acme/app".into(),
                        language: "go".into(),
                        title: "panic: quota test".into(),
                        stack_trace: "panic: quota test\ngoroutine 1 [running]:\nacme/app.F()\n\t/app/a.go:1 +0x1".into(),
                        ..Default::default()
                    },
                    crate::now_ms(),
                )
                .expect("signal");
            let fingerprint = self.store.list_issues().expect("issues")[0]
                .fingerprint
                .clone();
            let id = "quota-run";
            self.store
                .start_run(&shanhaijing::RunStart {
                    id: id.into(),
                    fingerprint,
                    runner_id: "test".into(),
                    repo: "acme/app".into(),
                    started_at: crate::now_ms(),
                    provider: None,
                    model: None,
                })
                .expect("run");
            self.store
                .update_run(
                    id,
                    &shanhaijing::RunUpdate {
                        tokens_uncached: Some(tokens),
                        ..Default::default()
                    },
                )
                .expect("usage");
            self.run_id = Some(id.into());
        }

        fn slot(&self, delivery_id: &str) -> shanhaijing::PrSlotRow {
            self.store
                .pr_slot_for_delivery(delivery_id)
                .expect("slot query")
                .expect("reserved slot")
        }
    }

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
            verification_mode: Default::default(),
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
            context: Default::default(),
        }
    }

    fn fixture_delivery_start(
        branch: &str,
        base: &str,
        head_sha: &str,
    ) -> shanhaijing::DeliveryStart {
        shanhaijing::DeliveryStart {
            id: "read-delivery".into(),
            run_id: None,
            fingerprint: issue().fingerprint,
            repo: "acme/app".into(),
            branch: branch.into(),
            base: base.into(),
            head_sha: head_sha.into(),
            now: crate::now_ms(),
        }
    }

    #[tokio::test]
    async fn last_pr_slot_denial_is_terminal_before_push_and_never_creates() {
        let mut fixture = DeliveryFixture::new().await;
        fixture.limits.daily_pr_limit = 1;
        let other = Store::open(fixture.dir.path().join("delivery.db")).expect("other worker");
        let head = git_output(&fixture.repo, &["rev-parse", "HEAD"]);
        let claim = other
            .claim_delivery(
                &fixture_delivery_start("agent/fix/other", "main", head.trim()),
                "other-worker",
                CLAIM_TTL_MS,
            )
            .expect("claim")
            .expect("owned");
        assert!(other
            .reserve_pr_slot_owned(&claim, 1, crate::now_ms())
            .expect("last slot admitted"));
        let config = std::fs::read(fixture.repo.join("nuwa.repo.json")).expect("config");
        let api = FakeGitHub::new(ApiMode::Success);
        let error = fixture
            .deliver(&api, true, true, None)
            .await
            .expect_err("quota");
        assert!(
            error.downcast_ref::<PrQuotaExceeded>().is_some(),
            "{error:#}"
        );
        assert_eq!(api.creates.load(Ordering::SeqCst), 0);
        assert_eq!(api.lookups.load(Ordering::SeqCst), 0);
        assert_eq!(
            fixture.pending().id,
            claim.delivery.id,
            "denied intent is not pending"
        );
        assert!(remote_branch_tip(
            &fixture.repo,
            "agent/fix/fp-delivery",
            Duration::from_secs(5)
        )
        .await
        .expect("remote tip")
        .is_none());
        assert!(!git_ops::branch_exists(&fixture.repo, "agent/fix/fp-delivery").expect("branch"));
        assert_eq!(
            git_ops::current_branch(&fixture.repo).expect("base"),
            "main"
        );
        assert_eq!(
            std::fs::read(fixture.repo.join("nuwa.repo.json")).expect("config"),
            config
        );
        let slot = fixture.slot(&claim.delivery.id);
        assert_eq!(
            fixture
                .store
                .pr_slots_used("acme/app", slot.admission_day_start_ms)
                .expect("only the other worker counts"),
            1
        );
    }

    #[tokio::test]
    async fn recorded_token_denial_precedes_all_local_delivery_effects() {
        let mut fixture = DeliveryFixture::new().await;
        fixture.record_run(2);
        fixture.limits.max_tokens = 1;
        let before = git_output(&fixture.repo, &["status", "--porcelain"]);
        let api = FakeGitHub::new(ApiMode::Success);
        let error = fixture
            .deliver(&api, true, true, None)
            .await
            .expect_err("tokens");
        assert!(matches!(
            error.downcast_ref::<crate::quota::TokenGateExceeded>(),
            Some(crate::quota::TokenGateExceeded::Individual { spent: 2, limit: 1 })
        ));
        assert_eq!(
            git_output(&fixture.repo, &["status", "--porcelain"]),
            before
        );
        assert_eq!(api.creates.load(Ordering::SeqCst), 0);
        assert!(fixture
            .store
            .pending_deliveries("acme/app")
            .expect("pending")
            .is_empty());
        assert!(!fixture
            .dir
            .path()
            .join("sessions/fp-delivery/verification.txt")
            .exists());
    }

    #[tokio::test]
    async fn final_token_gate_reloads_usage_and_preserves_pushed_recovery_evidence() {
        let mut fixture = DeliveryFixture::new().await;
        fixture.record_run(0);
        fixture.limits.daily_token_limit = 1;
        let api = FakeGitHub::new(ApiMode::Success);
        let error = fixture
            .deliver(&api, true, true, Some(FailPoint::RecordTokensAfterPush(2)))
            .await
            .expect_err("fresh usage exceeds final gate");
        assert!(matches!(
            error.downcast_ref::<crate::quota::TokenGateExceeded>(),
            Some(crate::quota::TokenGateExceeded::Daily { spent: 2, limit: 1 })
        ));
        assert_eq!(api.creates.load(Ordering::SeqCst), 0);
        assert_eq!(api.lookups.load(Ordering::SeqCst), 0);
        let row = fixture.pending();
        assert_eq!(row.state, DeliveryState::Pushed);
        let slot = fixture.slot(&row.id);
        assert_eq!(slot.state, "reserved");
        assert_eq!(
            fixture
                .store
                .pr_slots_used("acme/app", slot.admission_day_start_ms)
                .expect("reservation retained"),
            1
        );
        assert_eq!(
            git_ops::current_branch(&fixture.repo).expect("evidence checkout"),
            row.branch
        );
        assert_eq!(
            git_output(&fixture.repo, &["rev-parse", "HEAD"]).trim(),
            row.head_sha
        );
        assert_eq!(
            remote_branch_tip(&fixture.repo, &row.branch, Duration::from_secs(5))
                .await
                .expect("remote evidence"),
            Some(row.head_sha)
        );
        assert!(fixture.repo.join("nuwa.repo.json").is_file());
    }

    #[tokio::test]
    async fn delivery_failure_restores_base_and_removes_branch() {
        for fail in [FailPoint::AfterBranch, FailPoint::AfterCommit] {
            let fixture = DeliveryFixture::new().await;
            let err = fixture
                .deliver(&FakeGitHub::new(ApiMode::Success), false, false, Some(fail))
                .await
                .expect_err("fail point");
            assert!(format!("{err:#}").contains("fail point"));
            assert_eq!(
                git_ops::current_branch(&fixture.repo).expect("branch"),
                "main"
            );
            assert!(
                !git_ops::branch_exists(&fixture.repo, "agent/fix/fp-delivery").expect("exists")
            );
            assert!(
                fixture.repo.join("nuwa.repo.json").is_file(),
                "cleanup must exclude instructions"
            );
        }
    }

    #[tokio::test]
    async fn delivery_success_keeps_branch_and_returns_to_base() {
        let fixture = DeliveryFixture::new().await;
        let delivered = fixture
            .deliver(&FakeGitHub::new(ApiMode::Success), false, false, None)
            .await
            .expect("deliver");
        assert_eq!(delivered.branch, "agent/fix/fp-delivery");
        assert_eq!(
            git_ops::current_branch(&fixture.repo).expect("branch"),
            "main"
        );
        assert!(git_ops::branch_exists(&fixture.repo, &delivered.branch).expect("exists"));
    }

    #[tokio::test]
    async fn private_oracle_inputs_are_not_delivered_even_when_prestaged_or_ignored() {
        for ignored in [false, true] {
            let fixture = DeliveryFixture::with_options(false, Some(ignored)).await;
            // An exact exclusion must not hide a neighboring candidate file.
            std::fs::write(
                fixture.repo.join("checks/expected.txt.public"),
                "public fix\n",
            )
            .expect("candidate neighbor");
            git(&fixture.repo, &["add", "-f", "checks/expected.txt"]);
            let expected =
                crate::verify::candidate_tree_for_context(&fixture.repo, &fixture.verification)
                    .expect("candidate");
            let delivered = fixture
                .deliver(&FakeGitHub::new(ApiMode::Success), false, false, None)
                .await
                .expect("private oracle delivery");
            assert_eq!(delivered.risk.files.len(), 2);
            assert!(delivered.risk.forbidden_hits.is_empty());
            assert_eq!(
                git_output(
                    &fixture.repo,
                    &["rev-parse", &format!("{}^{{tree}}", delivered.commit)]
                )
                .trim(),
                expected
            );
            let paths = git_output(
                &fixture.repo,
                &["ls-tree", "-r", "--name-only", &delivered.commit],
            );
            assert_eq!(
                paths.lines().collect::<Vec<_>>(),
                ["checks/expected.txt.public", "fix.go", "main.go"]
            );
            fixture
                .verification
                .assert_integrity(&fixture.repo)
                .expect("originals retained");
        }
    }

    #[tokio::test]
    async fn rollback_preserves_frozen_untracked_oracle_inputs() {
        let fixture = DeliveryFixture::with_options(false, Some(false)).await;
        git(&fixture.repo, &["add", "checks/expected.txt"]);
        assert!(fixture
            .deliver(
                &FakeGitHub::new(ApiMode::Success),
                false,
                false,
                Some(FailPoint::AfterBranch)
            )
            .await
            .is_err());
        fixture
            .verification
            .assert_integrity(&fixture.repo)
            .expect("private inputs not cleaned");
        assert!(
            !fixture.repo.join("fix.go").exists(),
            "candidate rolled back"
        );
    }

    #[tokio::test]
    async fn delivery_rollback_never_restores_tampered_tracked_oracle_inputs() {
        let mut fixture = DeliveryFixture::with_options(false, Some(false)).await;
        git(&fixture.repo, &["add", "checks"]);
        git(&fixture.repo, &["commit", "-m", "track owner oracle only"]);
        let owner = crate::instructions::TrustedInstructions::load(
            &fixture.repo,
            &fixture.repo.join("nuwa.repo.json"),
            "HEAD",
            &fixture.dir.path().join("trusted"),
        )
        .expect("private config, tracked oracle");
        fixture.verification = crate::verify::prepare(
            &fixture.repo,
            "HEAD",
            Some(&owner),
            &crate::secrets::Secrets::default(),
        )
        .await
        .expect("freeze tracked originals");
        assert_eq!(
            fixture.verification.excluded_delivery_paths(),
            ["nuwa.repo.json"]
        );
        let ownership = DeliveryOwnership {
            store: &fixture.store,
            checkout: &fixture.checkout,
            period: Duration::from_millis(5),
            healthy: AtomicBool::new(true),
        };
        let tip = git_output(&fixture.repo, &["rev-parse", "HEAD"]);
        let guard = DeliveryGuard::new(
            &fixture.repo,
            Some("main"),
            tip.trim(),
            &ownership,
            &fixture.verification,
        );
        let original = fixture.repo.join("checks/expected.txt");
        std::fs::write(&original, "tampered\n").expect("protected original tamper");
        drop(guard);
        assert!(fixture
            .deliver(&FakeGitHub::new(ApiMode::Success), false, false, None)
            .await
            .is_err());
        assert_eq!(
            std::fs::read_to_string(original).expect("no restore"),
            "tampered\n"
        );
    }

    #[tokio::test]
    async fn prestaged_owner_config_is_excluded_from_the_verified_and_committed_tree() {
        let fixture = DeliveryFixture::new().await;
        git(&fixture.repo, &["add", "nuwa.repo.json"]);
        let expected = crate::verify::candidate_tree(&fixture.repo).expect("candidate");
        let delivered = fixture
            .deliver(&FakeGitHub::new(ApiMode::Success), false, false, None)
            .await
            .expect("delivery");
        let tree = git_output(
            &fixture.repo,
            &["rev-parse", &format!("{}^{{tree}}", delivered.commit)],
        );
        assert_eq!(tree.trim(), expected);
        assert!(git_output(
            &fixture.repo,
            &[
                "ls-tree",
                "--name-only",
                &delivered.commit,
                "nuwa.repo.json",
            ]
        )
        .trim()
        .is_empty());
        assert!(fixture.repo.join("nuwa.repo.json").is_file());
    }

    #[tokio::test]
    async fn post_proof_owner_or_tree_tampering_cannot_commit_push_or_open_pr() {
        for fail in [
            FailPoint::TamperOwnerAfterVerification,
            FailPoint::TamperCandidateAfterVerification,
        ] {
            let fixture = DeliveryFixture::new().await;
            let initial = git_output(&fixture.repo, &["rev-parse", "HEAD"]);
            let api = FakeGitHub::new(ApiMode::Success);
            let error = fixture
                .deliver(&api, true, true, Some(fail))
                .await
                .expect_err("tampered proof");
            let detail = format!("{error:#}");
            assert!(
                detail.contains("integrity") || detail.contains("matching verification"),
                "{detail}"
            );
            assert_eq!(git_output(&fixture.repo, &["rev-parse", "HEAD"]), initial);
            assert!(
                !git_ops::branch_exists(&fixture.repo, "agent/fix/fp-delivery").expect("branch")
            );
            assert!(fixture
                .store
                .pending_deliveries("acme/app")
                .expect("journal")
                .is_empty());
            assert!(
                git_output(&fixture.origin, &["for-each-ref", "refs/heads/agent/fix/"])
                    .trim()
                    .is_empty()
            );
            assert_eq!(api.creates.load(Ordering::SeqCst), 0);
            assert_eq!(api.lookups.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test]
    async fn strict_missing_oracle_cannot_prepare_or_reuse_a_legacy_delivery_context() {
        for (push, pr) in [(false, false), (true, false), (true, true)] {
            let fixture = DeliveryFixture::new().await;
            let config = serde_json::json!({
                "repo": "acme/app", "failing_check": {"command": check_command()},
                "verification_policy": "strict",
            });
            std::fs::write(fixture.repo.join("nuwa.repo.json"), config.to_string())
                .expect("strict config");
            let owner = crate::instructions::TrustedInstructions::load(
                &fixture.repo,
                &fixture.repo.join("nuwa.repo.json"),
                "HEAD",
                &fixture.dir.path().join("strict-owner"),
            )
            .expect("owner");
            let initial = git_output(&fixture.repo, &["rev-parse", "HEAD"]);
            let index = git_output(&fixture.repo, &["write-tree"]);
            let error = crate::verify::prepare(
                &fixture.repo,
                "HEAD",
                Some(&owner),
                &crate::secrets::Secrets::default(),
            )
            .await
            .expect_err("strict needs oracle");
            assert!(format!("{error:#}").contains("strict"));
            let api = FakeGitHub::new(ApiMode::Success);
            assert!(fixture.deliver(&api, push, pr, None).await.is_err());
            assert_eq!(git_output(&fixture.repo, &["rev-parse", "HEAD"]), initial);
            assert_eq!(git_output(&fixture.repo, &["write-tree"]), index);
            assert!(
                !git_ops::branch_exists(&fixture.repo, "agent/fix/fp-delivery").expect("branch")
            );
            assert!(fixture
                .store
                .pending_deliveries("acme/app")
                .expect("journal")
                .is_empty());
            assert!(
                git_output(&fixture.origin, &["for-each-ref", "refs/heads/agent/fix/"])
                    .trim()
                    .is_empty()
            );
            assert_eq!(api.creates.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test]
    async fn detached_delivery_failure_restores_pinned_tip_without_checking_out_shared_main() {
        for fail in [FailPoint::AfterBranch, FailPoint::AfterCommit] {
            let fixture = DeliveryFixture::detached().await;
            let source = fixture.dir.path().join("source");
            let pinned = git_output(&fixture.repo, &["rev-parse", "HEAD"]);
            git(
                &source,
                &["commit", "--allow-empty", "-m", "refresh shared main"],
            );
            let shared_tip = git_output(&source, &["rev-parse", "HEAD"]);
            let error = fixture
                .deliver(&FakeGitHub::new(ApiMode::Success), false, false, Some(fail))
                .await
                .expect_err("fail point");
            assert!(format!("{error:#}").contains("fail point"), "{error:#}");
            assert_eq!(
                git_ops::current_branch(&fixture.repo).expect("branch"),
                "HEAD"
            );
            assert_eq!(git_output(&fixture.repo, &["rev-parse", "HEAD"]), pinned);
            assert_eq!(
                git_ops::current_branch(&source).expect("source branch"),
                "main"
            );
            assert_eq!(git_output(&source, &["rev-parse", "HEAD"]), shared_tip);
            assert!(
                !git_ops::branch_exists(&fixture.repo, "agent/fix/fp-delivery")
                    .expect("branch removed")
            );
            assert!(!fixture.repo.join("fix.go").exists());
            assert!(fixture.repo.join("nuwa.repo.json").is_file());
        }
    }

    #[tokio::test]
    async fn delivery_redacts_secrets_in_commit_and_evidence() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        init_repo(&repo);
        let verification =
            crate::verify::prepare(&repo, "HEAD", None, &crate::secrets::Secrets::default())
                .await
                .expect("prepare before edits");
        std::fs::write(repo.join("fix.go"), "package main\n").expect("write");

        let mut issue = issue();
        issue.title = "panic secret-value-123456".into();
        issue.failing_check.command = check_command_with_secret("secret-value-123456");
        let secrets = crate::secrets::Secrets::with_values(vec!["secret-value-123456".to_string()]);
        let store = Store::open(":memory:").expect("store");
        let checkout = checkout_claim(&store, &repo);
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
            Some(&store),
            None,
            "acme/app",
            Some(&checkout),
            &verification,
            Default::default(),
            &FakeGitHub::new(ApiMode::Success),
            HEARTBEAT_PERIOD,
            None,
        )
        .await
        .expect("deliver");

        let log = git_output(
            &repo,
            &["log", "-1", "--format=%B", "agent/fix/fp-delivery"],
        );
        assert!(
            !log.contains("secret-value-123456"),
            "commit message leaks: {log}"
        );
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
    fn transient_delivery_errors_are_separated_from_issue_faults() {
        for text in [
            "push failed for branch agent/fix/x: connection reset",
            "ls-remote failed: timed out",
            "PR creation failed: 503 Service Unavailable",
        ] {
            assert!(
                is_transient_delivery_error(&anyhow::anyhow!("{text}")),
                "{text}"
            );
        }
        for text in [
            "verification gate failed: failing_check does not pass",
            "delivery changes forbidden paths: .github/workflows",
            "base main moved and merging it conflicted",
        ] {
            assert!(
                !is_transient_delivery_error(&anyhow::anyhow!("{text}")),
                "{text}"
            );
        }
    }

    #[test]
    fn evidence_truncation_keeps_bounded_text() {
        let long = "x".repeat(20_000);
        let out = truncate_evidence(&long, 100);
        assert!(out.ends_with('…'));
        assert_eq!(out.chars().count(), 101);
        assert_eq!(truncate_evidence("short", 100), "short");
    }

    #[tokio::test]
    async fn pushed_commit_must_match_the_remote_tip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let origin = dir.path().join("origin.git");
        git(
            dir.path(),
            &["init", "--bare", origin.to_str().expect("utf8")],
        );
        let repo = dir.path().join("repo");
        init_repo(&repo);
        git(
            &repo,
            &["remote", "add", "origin", origin.to_str().expect("utf8")],
        );
        git(&repo, &["checkout", "-b", "agent/fix/fp-delivery"]);
        std::fs::write(repo.join("fix.go"), "package main\n").expect("write");
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "fix"]);
        let tip = git_output(&repo, &["rev-parse", "HEAD"]).trim().to_string();
        git(&repo, &["push", "origin", "agent/fix/fp-delivery"]);

        assert_pushed_commit(
            &repo,
            "agent/fix/fp-delivery",
            &tip,
            Duration::from_secs(10),
        )
        .await
        .expect("remote tip equals the verified commit");
        assert!(
            assert_pushed_commit(
                &repo,
                "agent/fix/fp-delivery",
                "0".repeat(40).as_str(),
                Duration::from_secs(10)
            )
            .await
            .is_err(),
            "a different SHA must fail post-delivery verification"
        );
    }

    #[tokio::test]
    async fn missing_remote_branch_has_no_tip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let origin = dir.path().join("origin.git");
        git(
            dir.path(),
            &["init", "--bare", origin.to_str().expect("utf8")],
        );
        let repo = dir.path().join("repo");
        init_repo(&repo);
        git(
            &repo,
            &["remote", "add", "origin", origin.to_str().expect("utf8")],
        );
        assert_eq!(
            remote_branch_tip(&repo, "agent/fix/none", Duration::from_secs(10))
                .await
                .expect("ls-remote"),
            None
        );
    }

    #[test]
    fn recovery_requires_the_verified_head_and_expected_base() {
        let item = serde_json::json!({
            "state": "open",
            "html_url": "https://github.com/acme/app/pull/7",
            "number": 7,
            "head": { "sha": "verified-sha" },
            "base": { "ref": "main" }
        });
        let recovered = RecoveredPullRequest::from_json(&item).expect("open PR");
        assert_eq!(
            recovered.created_at_ms, None,
            "missing metadata stays unknown"
        );
        assert!(recovered.matches("verified-sha", "main"));
        assert!(!recovered.matches("new-unverified-sha", "main"));
        assert!(!recovered.matches("verified-sha", "release"));
    }

    #[test]
    fn recovery_parses_creation_evidence_without_requiring_it_for_pr_identity() {
        let mut item = serde_json::json!({
            "state": "open", "html_url": "https://github.com/acme/app/pull/7",
            "number": 7, "head": { "sha": "verified-sha" }, "base": { "ref": "main" }
        });
        for (raw, expected) in [
            (serde_json::Value::Null, None),
            (serde_json::json!(123), None),
            (serde_json::json!("1970-01-01T00:00:01Z"), Some(1_000)),
            (
                serde_json::json!("2024-02-29T12:34:56Z"),
                Some(API_CREATED_AT_MS),
            ),
        ] {
            item["created_at"] = raw;
            let recovered =
                select_recovered_pr(std::slice::from_ref(&item), "verified-sha", "main")
                    .expect("conclusive lookup")
                    .expect("valid PR");
            assert_eq!(recovered.created_at_ms, expected);
        }
        for malformed in [
            "",
            "not-a-date",
            "2023-02-29T12:34:56Z",
            "2024-13-01T00:00:00Z",
            "2024-01-00T00:00:00Z",
            "2024-01-01T24:00:00Z",
            "2024-01-01T00:60:00Z",
            "2024-01-01T00:00:60Z",
            "2024-01-01t00:00:00z",
            "2024-01-01T00:00:00+00:00",
            "2024-01-01T00:00:00.000Z",
            "2024-01-01T00:00:00Z ",
            "2024-01-01T00:00:éZ",
            "+024-01-01T00:00:00Z",
            "1970-01-01T00:00:00Z",
            "1969-12-31T23:59:59Z",
        ] {
            item["created_at"] = serde_json::json!(malformed);
            let recovered = RecoveredPullRequest::from_json(&item).expect("identity still valid");
            assert_eq!(recovered.created_at_ms, None, "{malformed}");
        }
    }

    #[test]
    fn recovery_does_not_treat_multiple_bases_as_definitive_absence() {
        let items = serde_json::json!([
            {
                "state": "open",
                "html_url": "https://github.com/acme/app/pull/7",
                "number": 7,
                "head": { "sha": "verified-sha" },
                "base": { "ref": "release" }
            },
            {
                "state": "open",
                "html_url": "https://github.com/acme/app/pull/8",
                "number": 8,
                "head": { "sha": "verified-sha" },
                "base": { "ref": "main" }
            }
        ]);

        assert!(
            select_recovered_pr(items.as_array().expect("array"), "verified-sha", "main").is_err()
        );
    }

    fn stalled_local_transport(repo: &Path) {
        // Git invokes this local SSH substitute; no DNS lookup or socket is used.
        git(
            repo,
            &["remote", "add", "origin", "ssh://unused.invalid/repo"],
        );
        git(repo, &["config", "ssh.variant", "ssh"]);
        git(
            repo,
            &["config", "core.sshCommand", "sh -c 'sleep 3; exit 1' --"],
        );
    }

    #[tokio::test]
    async fn systemic_remote_tip_transport_is_bounded() {
        let dir = tempfile::tempdir().expect("tempdir");
        init_repo(dir.path());
        stalled_local_transport(dir.path());
        let start = std::time::Instant::now();
        let err = remote_branch_tip(dir.path(), "main", Duration::from_millis(100))
            .await
            .expect_err("stalled transport");
        assert!(format!("{err:#}").contains("timed out"), "{err:#}");
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
    }

    #[tokio::test]
    async fn systemic_base_refresh_timeout_degrades_without_hanging() {
        let dir = tempfile::tempdir().expect("tempdir");
        init_repo(dir.path());
        stalled_local_transport(dir.path());
        let start = std::time::Instant::now();
        assert!(
            fetch_base_tip(dir.path(), "main", Duration::from_millis(100))
                .await
                .is_none()
        );
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "refresh exceeded its budget"
        );
    }

    #[tokio::test]
    async fn systemic_remote_tip_all_retries_are_bounded() {
        let dir = tempfile::tempdir().expect("tempdir");
        init_repo(dir.path());
        stalled_local_transport(dir.path());
        let start = std::time::Instant::now();
        let err = assert_pushed_commit(
            dir.path(),
            "main",
            &"0".repeat(40),
            Duration::from_millis(100),
        )
        .await
        .expect_err("must refuse delivery");
        assert!(format!("{err:#}").contains("timed out"), "{err:#}");
        assert!(start.elapsed() < std::time::Duration::from_secs(15));
    }

    #[tokio::test]
    async fn systemic_missing_remote_fails_tip_but_degrades_refresh() {
        let dir = tempfile::tempdir().expect("tempdir");
        init_repo(dir.path());
        assert!(
            remote_branch_tip(dir.path(), "main", Duration::from_millis(100))
                .await
                .is_err()
        );
        assert!(
            fetch_base_tip(dir.path(), "main", Duration::from_millis(100))
                .await
                .is_none()
        );
        assert!(assert_pushed_commit(
            dir.path(),
            "main",
            &"0".repeat(40),
            Duration::from_millis(100)
        )
        .await
        .is_err());
    }

    #[tokio::test]
    async fn systemic_delivery_merge_evidence_excludes_upstream_changes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let origin = dir.path().join("origin.git");
        git(
            dir.path(),
            &["init", "--bare", origin.to_str().expect("path")],
        );
        let repo = dir.path().join("repo");
        init_repo(&repo);
        git(
            &repo,
            &["remote", "add", "origin", origin.to_str().expect("path")],
        );
        git(&repo, &["push", "origin", "main"]);
        git(&repo, &["checkout", "-b", "upstream"]);
        std::fs::write(repo.join("upstream.txt"), "upstream\n".repeat(20)).expect("write");
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "upstream advance"]);
        git(&repo, &["push", "origin", "HEAD:main"]);
        let tip = git_output(&repo, &["rev-parse", "HEAD"]).trim().to_string();
        git(&repo, &["checkout", "main"]);
        // Keep origin/main stale: the fetched SHA, not a tracking-ref naming
        // convention, must determine the merged tree and red/green evidence.
        git(&repo, &["update-ref", "refs/remotes/origin/main", "main"]);
        git(
            &repo,
            &[
                "config",
                "remote.origin.fetch",
                "+refs/heads/main:refs/remotes/upstream/main",
            ],
        );
        assert_eq!(
            fetch_base_tip(&repo, "main", Duration::from_secs(10)).await,
            Some(tip)
        );
        let verification =
            crate::verify::prepare(&repo, "HEAD", None, &crate::secrets::Secrets::default())
                .await
                .expect("prepare before edits");
        std::fs::write(repo.join("fix.go"), "package main\n").expect("write");
        std::fs::write(repo.join("fix_test.go"), "package main\n").expect("write");
        let mut issue = issue();
        issue.failing_check.command = if cfg!(windows) {
            "type upstream.txt & if exist fix.go (exit 0) else (exit 1)".into()
        } else {
            "cat upstream.txt; test -f fix.go".into()
        };
        let secrets = crate::secrets::Secrets::default();
        let sessions = dir.path().join("sessions");
        let store = Store::open(":memory:").expect("store");
        let checkout = checkout_claim(&store, &repo);
        let api = FakeGitHub::new(ApiMode::Success);
        let delivered = deliver_inner(
            &repo,
            &issue,
            &report(dir.path()),
            true,
            true,
            &sessions,
            &secrets,
            None,
            false,
            Some(&store),
            None,
            "acme/app",
            Some(&checkout),
            &verification,
            Default::default(),
            &api,
            HEARTBEAT_PERIOD,
            None,
        )
        .await
        .expect("local transport and fake GitHub complete the actual delivery path");
        assert!(!delivered.change_stat.contains("upstream.txt"));
        assert_eq!(api.creates.load(Ordering::SeqCst), 1);
        let evidence = std::fs::read_to_string(
            sessions
                .join(issue.short_fingerprint())
                .join("verification.txt"),
        )
        .expect("evidence");
        assert!(
            evidence.contains("DIFF: 2 file(s), +2, -0, tests=true, ci=false"),
            "{evidence}"
        );
        let base_evidence = evidence
            .split("BRANCH failing_check:")
            .next()
            .expect("base evidence");
        assert_eq!(
            base_evidence
                .lines()
                .filter(|line| *line == "upstream")
                .count(),
            20,
            "base evidence must use the fetched SHA: {evidence}"
        );
        assert_eq!(git_ops::current_branch(&repo).expect("branch"), "main");
    }

    #[tokio::test]
    async fn linked_worktree_delivery_and_response_loss_persist_the_exact_head_watch() {
        for mode in [ApiMode::Success, ApiMode::ResponseLost] {
            let fixture = DeliveryFixture::new().await;
            let mut api = FakeGitHub::new(mode);
            api.store_path = Some(fixture.dir.path().join("delivery.db"));
            let delivered = fixture
                .deliver(&api, true, true, None)
                .await
                .expect("delivery");
            assert!(fixture.repo.join(".git").is_file());
            assert_eq!(api.creates.load(Ordering::SeqCst), 1);
            assert_eq!(
                api.lookups.load(Ordering::SeqCst),
                usize::from(matches!(mode, ApiMode::ResponseLost))
            );
            assert_eq!(delivered.base, "main");
            assert_eq!(delivered.pr_number, Some(7));
            let (delivery_id, requested) = api
                .slot_at_create
                .lock()
                .expect("slot")
                .clone()
                .expect("observed request reservation");
            let consumed = fixture.slot(&delivery_id);
            assert_eq!(
                requested.id, consumed.id,
                "response loss consumes the same slot"
            );
            assert_eq!(consumed.state, "consumed");
            assert_eq!(consumed.pr_url, delivered.pr_url);
            assert_eq!(
                requested.created_at_ms, None,
                "request time is not creation"
            );
            assert_eq!(consumed.created_at_ms, Some(API_CREATED_AT_MS));
            assert!(consumed
                .confirmed_at_ms
                .is_some_and(|now| now > API_CREATED_AT_MS));
            assert_eq!(
                fixture
                    .store
                    .pr_slots_used("acme/app", consumed.admission_day_start_ms)
                    .expect("single admission"),
                1
            );
            assert_eq!(
                git_ops::current_branch(&fixture.repo).expect("base"),
                "main"
            );
            assert_eq!(
                remote_branch_tip(&fixture.repo, &delivered.branch, Duration::from_secs(5))
                    .await
                    .expect("tip"),
                Some(delivered.commit.clone())
            );
            assert!(fixture
                .store
                .pending_deliveries("acme/app")
                .expect("pending")
                .is_empty());
            let watch = fixture
                .store
                .ci_watch_for(&issue().fingerprint)
                .expect("watch query")
                .expect("atomic watch");
            assert_eq!(watch.head_sha, delivered.commit);
            assert_eq!(watch.slug, "acme/app");
            assert_eq!(watch.root_cause.as_deref(), Some("ROOT CAUSE: test"));
            assert_eq!(
                watch.change_stat.as_deref(),
                Some(delivered.change_stat.as_str())
            );
            let files = git_output(
                &fixture.repo,
                &["ls-tree", "--name-only", &delivered.commit],
            );
            assert!(files.contains("fix.go"));
            assert!(
                !files.contains("nuwa.repo.json"),
                "instructions must never be committed"
            );
            assert!(api.bodies.lock().expect("body")[0].contains("ROOT CAUSE: test"));
        }
    }

    #[tokio::test]
    async fn detached_pool_delivery_preserves_pr_base_and_pinned_checkout() {
        for base in ["main", "master"] {
            for mode in [ApiMode::Success, ApiMode::ResponseLost] {
                let fixture = DeliveryFixture::detached().await;
                let source = fixture.dir.path().join("source");
                if base == "master" {
                    git(&source, &["branch", "-m", "master"]);
                    git(&source, &["push", "origin", "master"]);
                }
                let pinned = git_output(&fixture.repo, &["rev-parse", "HEAD"]);
                // Another worker may refresh the authoritative local base while
                // this detached worker still owns an older, verified checkout.
                std::fs::write(source.join("upstream.txt"), "new local base\n").expect("upstream");
                git(&source, &["add", "upstream.txt"]);
                git(&source, &["commit", "-m", "refresh shared base"]);
                let shared_tip = git_output(&source, &["rev-parse", "HEAD"]);
                let api = FakeGitHub::new(mode);
                let delivered = fixture
                    .deliver(&api, true, true, None)
                    .await
                    .expect("delivery");
                assert!(fixture.repo.join(".git").is_file());
                assert_eq!(delivered.base, base);
                assert_eq!(delivered.pr_number, Some(7));
                assert_eq!(
                    git_ops::current_branch(&fixture.repo).expect("branch"),
                    "HEAD"
                );
                assert_eq!(git_output(&fixture.repo, &["rev-parse", "HEAD"]), pinned);
                assert_eq!(
                    git_ops::current_branch(&source).expect("source branch"),
                    base
                );
                assert_eq!(git_output(&source, &["rev-parse", "HEAD"]), shared_tip);
                assert_eq!(
                    git_output(
                        &fixture.repo,
                        &["rev-parse", &format!("{}^", delivered.commit)]
                    ),
                    pinned
                );
                {
                    let created = api.created.lock().expect("created");
                    assert_eq!(created.len(), 1);
                    assert!(created[0].matches(&delivered.commit, base));
                }
                assert_eq!(
                    api.lookups.load(Ordering::SeqCst),
                    usize::from(matches!(mode, ApiMode::ResponseLost))
                );
                assert_eq!(
                    remote_branch_tip(&fixture.repo, &delivered.branch, Duration::from_secs(5))
                        .await
                        .expect("remote tip"),
                    Some(delivered.commit.clone())
                );
                assert!(fixture
                    .store
                    .pending_deliveries("acme/app")
                    .expect("pending")
                    .is_empty());
                let watch = fixture
                    .store
                    .ci_watch_for(&issue().fingerprint)
                    .expect("watch query")
                    .expect("watch");
                assert_eq!(watch.head_sha, delivered.commit);
            }
        }
    }

    #[tokio::test]
    async fn push_without_pr_finishes_with_nonrecoverable_terminal_identity() {
        let fixture = DeliveryFixture::new().await;
        let api = FakeGitHub::new(ApiMode::Success);
        let delivered = fixture
            .deliver(&api, true, false, None)
            .await
            .expect("push");
        let start = fixture_delivery_start(&delivered.branch, &delivered.base, &delivered.commit);
        let observer = Store::open(fixture.dir.path().join("delivery.db")).expect("observer");
        let row = observer
            .begin_delivery(&start)
            .expect("read exact identity");
        assert_eq!(row.state, DeliveryState::BranchPushed);
        assert_eq!(row.head_sha, delivered.commit);
        assert_eq!(row.run_id, start.run_id);
        assert_eq!(row.fingerprint, start.fingerprint);
        assert_eq!(row.base, start.base);
        assert!(row.pr_url.is_none());
        assert!(row.pr_number.is_none());
        assert_eq!(api.creates.load(Ordering::SeqCst), 0);
        assert_eq!(api.lookups.load(Ordering::SeqCst), 0);
        assert!(delivered.pr_url.is_none());
        assert!(observer
            .pending_deliveries("acme/app")
            .expect("pending")
            .is_empty());
        assert!(observer
            .claim_pending_deliveries(
                "acme/app",
                "reconciler",
                crate::now_ms() + CLAIM_TTL_MS,
                CLAIM_TTL_MS,
                10,
            )
            .expect("terminal is not recoverable")
            .is_empty());
        assert!(observer
            .claim_delivery(&start, "other", CLAIM_TTL_MS)
            .expect("terminal claim")
            .is_none());
        assert!(observer
            .ci_watch_for(&issue().fingerprint)
            .expect("watch")
            .is_none());
        assert_eq!(
            git_ops::current_branch(&fixture.repo).expect("base restored"),
            "main"
        );
        assert!(git_ops::branch_exists(&fixture.repo, &delivered.branch).expect("local branch"));
        assert_eq!(
            remote_branch_tip(&fixture.repo, &delivered.branch, Duration::from_secs(5))
                .await
                .expect("remote survives recovery claims"),
            Some(delivered.commit)
        );
    }

    #[tokio::test]
    async fn interrupted_branch_only_push_preserves_evidence_without_pr_recovery() {
        for (point, pushed) in [
            (FailPoint::AfterIntent, false),
            (FailPoint::AfterPush, true),
        ] {
            let fixture = DeliveryFixture::new().await;
            let api = FakeGitHub::new(ApiMode::Success);
            let error = fixture
                .deliver(&api, true, false, Some(point))
                .await
                .expect_err("crash");
            assert!(format!("{error:#}").contains("test crash"));
            let branch = format!("agent/fix/{}", issue().short_fingerprint());
            let head = git_output(&fixture.repo, &["rev-parse", "HEAD"]);
            let start = fixture_delivery_start(&branch, "main", head.trim());
            let observer = Store::open(fixture.dir.path().join("delivery.db")).expect("observer");
            let row = observer.begin_delivery(&start).expect("durable identity");
            assert_eq!(row.state, DeliveryState::BranchOnly);
            assert_eq!(row.head_sha, head.trim());
            assert!(row.pr_url.is_none());
            assert!(observer
                .pending_deliveries("acme/app")
                .expect("pending")
                .is_empty());
            assert!(observer
                .claim_pending_deliveries(
                    "acme/app",
                    "reconciler",
                    crate::now_ms() + CLAIM_TTL_MS,
                    CLAIM_TTL_MS,
                    10,
                )
                .expect("interrupted branch-only is not recoverable")
                .is_empty());
            assert_eq!(
                git_ops::current_branch(&fixture.repo).expect("evidence branch"),
                branch
            );
            assert!(git_ops::branch_exists(&fixture.repo, &branch).expect("local evidence"));
            assert_eq!(
                remote_branch_tip(&fixture.repo, &branch, Duration::from_secs(5))
                    .await
                    .expect("remote evidence survives"),
                pushed.then_some(row.head_sha)
            );
            assert_eq!(api.creates.load(Ordering::SeqCst), 0);
            assert_eq!(api.lookups.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test]
    async fn network_delivery_cannot_bypass_store_or_checkout_authority() {
        let fixture = DeliveryFixture::new().await;
        let api = FakeGitHub::new(ApiMode::Success);
        for (store, checkout) in [
            (None, Some(&fixture.checkout)),
            (Some(&fixture.store), None),
        ] {
            let error = deliver_inner(
                &fixture.repo,
                &issue(),
                &report(fixture.dir.path()),
                true,
                true,
                &fixture.dir.path().join("sessions"),
                &crate::secrets::Secrets::default(),
                None,
                false,
                store,
                None,
                "acme/app",
                checkout,
                &fixture.verification,
                Default::default(),
                &api,
                HEARTBEAT_PERIOD,
                None,
            )
            .await
            .expect_err("authority required");
            assert!(format!("{error:#}").contains("requires"));
        }
        assert!(fixture
            .store
            .release_checkout_claim(&fixture.checkout, crate::now_ms())
            .expect("release"));
        assert!(fixture.deliver(&api, true, true, None).await.is_err());
        assert_eq!(api.creates.load(Ordering::SeqCst), 0);
        assert_eq!(
            git_ops::current_branch(&fixture.repo).expect("base"),
            "main"
        );
        assert!(fixture.repo.join("fix.go").is_file());
        assert!(remote_branch_tip(
            &fixture.repo,
            "agent/fix/fp-delivery",
            Duration::from_secs(5)
        )
        .await
        .expect("tip")
        .is_none());
    }

    #[tokio::test]
    async fn crash_boundaries_leave_recoverable_exact_identities() {
        for (point, expected, pushed, created) in [
            (FailPoint::AfterIntent, DeliveryState::Intent, false, false),
            (FailPoint::AfterPush, DeliveryState::Intent, true, false),
            (
                FailPoint::AfterPrRequested,
                DeliveryState::PrRequested,
                true,
                false,
            ),
            (
                FailPoint::AfterPrResponse,
                DeliveryState::PrRequested,
                true,
                true,
            ),
        ] {
            let fixture = DeliveryFixture::new().await;
            let api = FakeGitHub::new(ApiMode::Success);
            let error = fixture
                .deliver(&api, true, true, Some(point))
                .await
                .expect_err("crash");
            assert!(format!("{error:#}").contains("test crash"));
            let row = fixture.pending();
            assert_eq!(row.state, expected);
            assert_eq!(row.base, "main");
            assert_eq!(
                git_output(&fixture.repo, &["rev-parse", "HEAD"]).trim(),
                row.head_sha
            );
            let remote = remote_branch_tip(&fixture.repo, &row.branch, Duration::from_secs(5))
                .await
                .expect("tip");
            assert_eq!(remote.as_deref(), pushed.then_some(row.head_sha.as_str()));
            let recovery =
                Store::open(fixture.dir.path().join("delivery.db")).expect("second process");
            let claims = recovery
                .claim_pending_deliveries(
                    "acme/app",
                    "recovery",
                    crate::now_ms() + CLAIM_TTL_MS + 1,
                    CLAIM_TTL_MS,
                    1,
                )
                .expect("reclaim after expiry");
            let claim = claims.first().expect("recoverable claim");
            ensure_delivery_owned(&recovery, claim).expect("owned");
            let recovered = with_delivery_heartbeat(
                &recovery,
                claim,
                api.find("acme/app", &row.branch, &row.head_sha, &row.base),
            )
            .await
            .expect("lookup");
            assert_eq!(recovered.is_some(), created);
            if let Some(pr) = recovered {
                assert!(pr.matches(&row.head_sha, &row.base));
                recovery
                    .complete_delivery_owned_at(
                        claim,
                        &pr.url,
                        pr.number,
                        None,
                        pr.created_at_ms,
                        crate::now_ms(),
                    )
                    .expect("adopt once");
                assert_eq!(fixture.slot(&row.id).created_at_ms, Some(API_CREATED_AT_MS));
                assert!(recovery
                    .pending_deliveries("acme/app")
                    .expect("pending")
                    .is_empty());
                assert_eq!(
                    api.creates.load(Ordering::SeqCst),
                    1,
                    "recovery never repeats POST"
                );
            }
        }
    }

    #[tokio::test]
    async fn merge_supersedes_before_any_push() {
        let fixture = DeliveryFixture::new().await;
        let source = fixture.dir.path().join("source");
        std::fs::write(source.join("upstream.txt"), "upstream\n").expect("upstream");
        git(&source, &["add", "upstream.txt"]);
        git(&source, &["commit", "-m", "advance base"]);
        git(&source, &["push", "origin", "HEAD:main"]);
        let api = FakeGitHub::new(ApiMode::Success);
        let error = fixture
            .deliver(&api, true, true, Some(FailPoint::AfterSupersede))
            .await
            .expect_err("crash");
        assert!(format!("{error:#}").contains("after supersede"));
        let row = fixture.pending();
        assert_eq!(row.state, DeliveryState::Intent);
        assert_eq!(
            git_output(&fixture.repo, &["rev-parse", "HEAD"]).trim(),
            row.head_sha
        );
        assert!(fixture.repo.join("upstream.txt").is_file());
        assert!(
            remote_branch_tip(&fixture.repo, &row.branch, Duration::from_secs(5))
                .await
                .expect("tip")
                .is_none(),
            "neither the old nor merged identity may be pushed before supersession"
        );
        assert_eq!(api.creates.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn inconclusive_or_mismatched_lookup_preserves_remote_and_local_evidence() {
        for mode in [
            ApiMode::Inconclusive,
            ApiMode::WrongHead,
            ApiMode::WrongBase,
        ] {
            let fixture = DeliveryFixture::new().await;
            let api = FakeGitHub::new(mode);
            assert!(fixture.deliver(&api, true, true, None).await.is_err());
            let row = fixture.pending();
            assert_eq!(row.state, DeliveryState::Ambiguous);
            let slot = fixture.slot(&row.id);
            assert_eq!(slot.state, "ambiguous");
            let recovery = Store::open(fixture.dir.path().join("delivery.db"))
                .expect("reopen does not prune ambiguous slots");
            let expired_at = crate::now_ms() + CLAIM_TTL_MS + 1;
            let stale = recovery
                .claim_pending_deliveries("acme/app", "old-recovery", expired_at, CLAIM_TTL_MS, 1)
                .expect("expired claim takeover")
                .remove(0);
            let takeover_at = expired_at + CLAIM_TTL_MS + 1;
            let next = recovery
                .claim_pending_deliveries("acme/app", "new-recovery", takeover_at, CLAIM_TTL_MS, 1)
                .expect("second takeover")
                .remove(0);
            assert!(matches!(
                fixture.store.mark_delivery_owned(
                    &stale,
                    DeliveryState::Failed,
                    None,
                    None,
                    None,
                    takeover_at
                ),
                Err(shanhaijing::ShanError::OwnershipLost)
            ));
            assert!(!fixture
                .store
                .release_delivery_claim(&stale, takeover_at)
                .expect("stale claim cannot release"));
            assert!(recovery
                .reserve_pr_slot_owned(&next, 0, takeover_at)
                .expect("same reservation survives expiry and quota zero"));
            let retained = fixture.slot(&row.id);
            assert_eq!(retained.id, slot.id);
            assert_eq!(retained.state, "ambiguous");
            assert_eq!(
                recovery
                    .pr_slots_used("acme/app", slot.admission_day_start_ms)
                    .expect("not refunded by expiry or stale owner"),
                1
            );
            assert_eq!(
                remote_branch_tip(&fixture.repo, &row.branch, Duration::from_secs(5))
                    .await
                    .expect("tip"),
                Some(row.head_sha)
            );
            assert!(git_ops::branch_exists(&fixture.repo, &row.branch).expect("local evidence"));
            assert!(fixture
                .store
                .ci_watch_for(&issue().fingerprint)
                .expect("watch query")
                .is_none());
        }
    }

    #[tokio::test]
    async fn orphan_deletion_requires_definitive_absence_and_unchanged_remote_sha() {
        for mode in [ApiMode::Empty, ApiMode::AlterRemote] {
            let fixture = DeliveryFixture::new().await;
            let mut api = FakeGitHub::new(mode);
            api.origin = Some(fixture.origin.clone());
            api.store_path = Some(fixture.dir.path().join("delivery.db"));
            assert!(fixture.deliver(&api, true, true, None).await.is_err());
            let (delivery_id, requested) = api
                .slot_at_create
                .lock()
                .expect("slot")
                .clone()
                .expect("request reservation");
            let slot = fixture.slot(&delivery_id);
            assert_eq!(slot.id, requested.id);
            let (state, used) = if matches!(mode, ApiMode::AlterRemote) {
                ("ambiguous", 1)
            } else {
                ("released", 0)
            };
            assert_eq!(slot.state, state);
            assert_eq!(
                fixture
                    .store
                    .pr_slots_used("acme/app", slot.admission_day_start_ms)
                    .expect("only definite failure releases quota"),
                used
            );
            let branch = "agent/fix/fp-delivery";
            let remote = remote_branch_tip(&fixture.repo, branch, Duration::from_secs(5))
                .await
                .expect("tip");
            if matches!(mode, ApiMode::AlterRemote) {
                assert_eq!(fixture.pending().state, DeliveryState::Ambiguous);
                let other_sha = git_output(&fixture.repo, &["rev-parse", "main"]);
                assert_eq!(
                    remote.as_deref(),
                    Some(other_sha.trim()),
                    "a successor's remote ref must survive"
                );
            } else {
                assert!(remote.is_none());
                assert!(fixture
                    .store
                    .pending_deliveries("acme/app")
                    .expect("terminal failed")
                    .is_empty());
            }
            assert!(git_ops::branch_exists(&fixture.repo, branch).expect("local evidence"));
        }
    }

    #[tokio::test]
    async fn ownership_loss_during_blocked_api_stops_effects_and_keeps_pr_requested() {
        for lose_checkout in [false, true] {
            let fixture = DeliveryFixture::new().await;
            let api = FakeGitHub::new(ApiMode::Blocked);
            // Bound each independent real-Git scenario, not the sum of both
            // setups/proofs. Cancellation below still has a strict 1s bound.
            let overall_deadline = tokio::time::Instant::now() + Duration::from_secs(60);
            let result = tokio::time::timeout_at(overall_deadline, async {
                let entered = api.entered.notified();
                tokio::pin!(entered);
                entered.as_mut().enable();
                let delivery = fixture.deliver(&api, true, true, None);
                tokio::pin!(delivery);
                tokio::select! {
                    _ = &mut entered => {}
                    _ = &mut delivery => panic!("delivery finished before blocking in the PR API"),
                }
                let other =
                    Store::open(fixture.dir.path().join("delivery.db")).expect("second process");
                let future_now = crate::now_ms() + CLAIM_TTL_MS + 1;
                if lose_checkout {
                    other
                        .claim_checkout(
                            &fixture.checkout.path,
                            "successor",
                            future_now,
                            CLAIM_TTL_MS,
                        )
                        .expect("takeover")
                        .expect("new checkout owner");
                } else {
                    let claims = other
                        .claim_pending_deliveries(
                            "acme/app",
                            "successor",
                            future_now,
                            CLAIM_TTL_MS,
                            1,
                        )
                        .expect("takeover");
                    assert_eq!(claims.len(), 1);
                }
                // Real Git setup uses the overall budget, not the cancellation
                // budget. The fixture renews every 5 ms, so even with scheduling
                // slack, ownership loss must cancel well within a second.
                tokio::time::timeout(Duration::from_secs(1), &mut delivery)
                    .await
                    .expect("heartbeat must promptly cancel blocked API after ownership loss")
            })
            .await
            .expect("delivery must complete within the overall test budget");
            let error = result.expect_err("ownership lost");
            assert!(
                error.downcast_ref::<HeartbeatStopped>().is_some(),
                "{error:#}"
            );
            assert!(
                api.cancelled.load(Ordering::SeqCst),
                "in-flight future must be dropped"
            );
            assert_eq!(
                api.lookups.load(Ordering::SeqCst),
                0,
                "lost ownership must not start recovery effects"
            );
            let row = fixture.pending();
            assert_eq!(row.state, DeliveryState::PrRequested);
            let slot = fixture.slot(&row.id);
            assert_eq!(slot.state, "requested");
            assert_eq!(
                fixture
                    .store
                    .pr_slots_used("acme/app", slot.admission_day_start_ms)
                    .expect("cancellation is not a quota refund"),
                1
            );
            assert_eq!(
                git_ops::current_branch(&fixture.repo).expect("branch"),
                row.branch
            );
            assert!(fixture.repo.join("fix.go").is_file());
            assert_eq!(
                remote_branch_tip(&fixture.repo, &row.branch, Duration::from_secs(5))
                    .await
                    .expect("tip"),
                Some(row.head_sha)
            );
        }
    }

    #[tokio::test]
    async fn heartbeat_renews_both_claims_throughout_a_wait() {
        let fixture = DeliveryFixture::new().await;
        let now = crate::now_ms();
        let claim = fixture
            .store
            .claim_delivery(
                &shanhaijing::DeliveryStart {
                    id: "heartbeat".into(),
                    run_id: None,
                    fingerprint: issue().fingerprint,
                    repo: "acme/app".into(),
                    branch: "agent/fix/fp-delivery".into(),
                    base: "main".into(),
                    head_sha: git_output(&fixture.repo, &["rev-parse", "HEAD"])
                        .trim()
                        .to_string(),
                    now,
                },
                "test",
                CLAIM_TTL_MS,
            )
            .expect("claim")
            .expect("owned");
        with_claims_heartbeat(
            &fixture.store,
            Some(&claim),
            Some(&fixture.checkout),
            Duration::from_millis(2),
            async {
                tokio::time::sleep(Duration::from_millis(40)).await;
                // A future timestamp observes the renewed expiry without advancing
                // or depending on Tokio's optional test clock feature.
                assert!(fixture
                    .store
                    .delivery_claim_valid(&claim, now + CLAIM_TTL_MS + 10)
                    .expect("delivery renewed"));
                assert!(fixture
                    .store
                    .checkout_claim_valid(&fixture.checkout, now + CLAIM_TTL_MS + 10)
                    .expect("checkout renewed"));
                Ok(())
            },
        )
        .await
        .expect("wait");
    }

    #[tokio::test]
    async fn local_guard_refuses_unknown_changed_or_unowned_checkout_cleanup() {
        for detached in [false, true] {
            for scenario in 0..4 {
                let fixture = DeliveryFixture::with_detached_head(detached).await;
                let ownership = DeliveryOwnership {
                    store: &fixture.store,
                    checkout: &fixture.checkout,
                    period: HEARTBEAT_PERIOD,
                    healthy: AtomicBool::new(true),
                };
                let tip = git_output(&fixture.repo, &["rev-parse", "HEAD"])
                    .trim()
                    .to_string();
                let mut guard = DeliveryGuard::new(
                    &fixture.repo,
                    (!detached).then_some("main"),
                    &tip,
                    &ownership,
                    &fixture.verification,
                );
                match scenario {
                    0 => guard.expected_tip = None,
                    1 => git(
                        &fixture.repo,
                        &["commit", "--allow-empty", "-m", "unexpected tip"],
                    ),
                    2 => git(&fixture.repo, &["checkout", "-b", "unexpected-branch"]),
                    _ => assert!(fixture
                        .store
                        .release_checkout_claim(&fixture.checkout, crate::now_ms())
                        .expect("release")),
                }
                drop(guard);
                assert!(
                    fixture.repo.join("fix.go").is_file(),
                    "uncertain cleanup must not remove evidence"
                );
            }
        }
    }

    #[test]
    fn ownership_uncertainty_uses_error_types_not_display_text() {
        for error in [
            anyhow::Error::new(HeartbeatStopped).context("outer context"),
            anyhow::Error::new(shanhaijing::ShanError::OwnershipLost).context("outer context"),
        ] {
            assert!(ownership_uncertain(&error));
        }
        assert!(!ownership_uncertain(&anyhow::anyhow!(
            "{}",
            HeartbeatStopped
        )));
        assert!(!ownership_uncertain(&anyhow::anyhow!(
            "{}",
            shanhaijing::ShanError::OwnershipLost
        )));
    }

    #[test]
    fn recovery_only_empty_results_are_definitive_absence() {
        assert!(select_recovered_pr(&[], "sha", "main")
            .expect("definitive empty")
            .is_none());
        let good = serde_json::json!({
            "state": "open", "html_url": "https://github.com/acme/app/pull/7", "number": 7,
            "head": {"sha": "sha"}, "base": {"ref": "main"}
        });
        assert!(
            select_recovered_pr(std::slice::from_ref(&good), "sha", "main")
                .expect("match")
                .is_some()
        );
        for item in [
            serde_json::Value::Null,
            serde_json::json!({"state": "open"}),
            serde_json::json!({"state": "closed", "html_url": "url", "number": 7,
                "head": {"sha": "sha"}, "base": {"ref": "main"}}),
        ] {
            assert!(select_recovered_pr(&[item], "sha", "main").is_err());
        }
        assert!(select_recovered_pr(std::slice::from_ref(&good), "different", "main").is_err());
        assert!(select_recovered_pr(&[good], "sha", "release").is_err());
        assert!(recover_pr_response(&serde_json::Value::Null, false, "sha", "main").is_err());
        assert!(recover_pr_response(&serde_json::json!({}), false, "sha", "main").is_err());
        assert!(recover_pr_response(&serde_json::json!([]), true, "sha", "main").is_err());
        assert!(recover_pr_response(
            &serde_json::json!(vec![serde_json::Value::Null; 100]),
            false,
            "sha",
            "main"
        )
        .is_err());
        assert!(
            recover_pr_response(&serde_json::json!([]), false, "sha", "main")
                .expect("empty complete page")
                .is_none()
        );
        let url = pr_lookup_url("acme/app", "agent/fix/fp-delivery").expect("lookup URL");
        assert_eq!(
            url.query_pairs()
                .find(|(key, _)| key == "head")
                .expect("head")
                .1,
            "acme:agent/fix/fp-delivery"
        );
        assert!(!url.as_str().contains("%252F"));
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
        assert_eq!(
            parse_github_slug("https://gitlab.com/acme/spike-target"),
            None
        );
        assert_eq!(parse_github_slug("git@github.com:acme"), None);
        assert_eq!(parse_github_slug("/some/local/path"), None);
    }
}
