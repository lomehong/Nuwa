//! 修复流水线：常驻 runner（`work`）与一次性 `nuwa fix` 共用的唯一实现。
//!
//! 两条入口曾各自拼装修复任务与交付参数，漂移是必然的：CLI 路径漏掉仓库
//! 指令 notes、也漏掉外部输入脱敏。这里收敛「怎么修、怎么交」——调用方只
//! 负责寻址、记账与失败分类。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;

use crate::instructions::{RepoInstructions, TrustedInstructions};
use crate::{deliver, guard, issue, repair, secrets, skills, verify};

pub type ProgressSink = Arc<dyn Fn(repair::RepairProgress) + Send + Sync>;

pub(crate) const CHECKOUT_TTL_MS: i64 = 60_000;

/// Resolve symlinks/junctions before folding Windows' case-insensitive identity.
/// Failure is not permission to fall back to an unresolved alias.
pub(crate) fn checkout_identity(repo: &Path) -> anyhow::Result<String> {
    let path = std::fs::canonicalize(repo)
        .with_context(|| format!("canonicalize checkout {}", repo.display()))?;
    let path = path.to_str().context("checkout path is not UTF-8")?;
    #[cfg(windows)]
    let path = path.to_lowercase();
    Ok(path.to_string())
}

/// A real, renewable checkout claim; never represented by a fabricated runner.
pub(crate) struct CheckoutLease {
    store: Arc<shanhaijing::Store>,
    pub claim: shanhaijing::CheckoutClaim,
    healthy: std::sync::atomic::AtomicBool,
}

impl CheckoutLease {
    pub fn acquire(
        store: &Arc<shanhaijing::Store>,
        repo: &Path,
        owner: &str,
        instance: Option<&str>,
    ) -> anyhow::Result<Self> {
        let path = checkout_identity(repo)?;
        let now = crate::now_ms();
        let claim = match instance {
            Some(instance) => {
                store.claim_runner_checkout(&path, owner, instance, now, CHECKOUT_TTL_MS)?
            }
            None => store.claim_checkout(&path, owner, now, CHECKOUT_TTL_MS)?,
        }
        .context("checkout is already in use or runner instance is no longer active")?;
        Ok(Self {
            store: Arc::clone(store),
            claim,
            healthy: std::sync::atomic::AtomicBool::new(true),
        })
    }

    pub fn ensure(&self) -> anyhow::Result<()> {
        let result = self
            .store
            .checkout_claim_valid(&self.claim, crate::now_ms());
        match result {
            Ok(true) if self.healthy.load(std::sync::atomic::Ordering::SeqCst) => Ok(()),
            result => {
                self.healthy
                    .store(false, std::sync::atomic::Ordering::SeqCst);
                result?;
                Err(shanhaijing::ShanError::OwnershipLost.into())
            }
        }
    }

    pub async fn during<T>(
        &self,
        future: impl std::future::Future<Output = anyhow::Result<T>>,
    ) -> anyhow::Result<T> {
        self.during_with_period(Duration::from_secs(10), future)
            .await
    }

    async fn during_with_period<T>(
        &self,
        period: Duration,
        future: impl std::future::Future<Output = anyhow::Result<T>>,
    ) -> anyhow::Result<T> {
        self.ensure()?;
        let mut ticks = tokio::time::interval(period);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tokio::pin!(future);
        loop {
            tokio::select! {
                biased;
                _ = ticks.tick() => {
                    match self.store.renew_checkout_claim(&self.claim, crate::now_ms(), CHECKOUT_TTL_MS) {
                        Ok(true) => {}
                        result => {
                            self.healthy.store(false, std::sync::atomic::Ordering::SeqCst);
                            result?;
                            return Err(shanhaijing::ShanError::OwnershipLost.into());
                        }
                    }
                }
                result = &mut future => {
                    self.ensure()?;
                    return result;
                }
            }
        }
    }
}

impl Drop for CheckoutLease {
    fn drop(&mut self) {
        if self.healthy.load(std::sync::atomic::Ordering::SeqCst) {
            // The SQL predicate checks token, generation AND unexpired ownership.
            if let Err(error) = self
                .store
                .release_checkout_claim(&self.claim, crate::now_ms())
            {
                eprintln!("checkout    : release failed: {error}");
            }
        }
    }
}

/// 一次修复的完整输入。
pub struct RepairRequest {
    /// Config/skills root (the primary checkout). `nuwa.repo.json` and
    /// `.nuwa/skills` are read from here even when the edit happens elsewhere.
    pub repo: PathBuf,
    /// Editable checkout. `None` edits `repo` (single-run behavior); a pool
    /// passes a per-run worktree so concurrent runs cannot reset each other.
    pub workspace: Option<PathBuf>,
    pub issue: issue::Issue,
    pub sessions_dir: PathBuf,
    pub provider: repair::ProviderKind,
    pub max_turns: u32,
    /// 仓库指令（notes 与技能白名单）；一次性 CLI 允许缺失。
    pub repo_config: Option<RepoInstructions>,
    /// Pinned owner authority, never reloaded from the candidate after repair.
    pub owner: Option<TrustedInstructions>,
    /// Exact checkout base; `None` pins HEAD before any model execution.
    pub base: Option<String>,
    pub secrets: Arc<secrets::Secrets>,
    pub progress: Option<ProgressSink>,
    /// benchmark 场景注入的确定性 faux 脚本；普通 runner 为 None。
    pub faux_script: Option<rpi_ai::providers::faux::FauxScript>,
    /// 常驻 runner 的 run id；`None` 表示一次性 CLI（平铺会话目录）。
    pub run_id: Option<String>,
}

/// 已定址、尚未执行的修复。`session_dir` 在启动前即可确定，调用方可以先
/// 落库再开跑；失败路径的转录脱敏也由这里统一负责。
pub struct PreparedRepair {
    job: repair::RepairJob,
    session_dir: PathBuf,
    skills_used: Vec<String>,
    verification: Arc<verify::VerificationContext>,
}

impl PreparedRepair {
    pub fn session_dir(&self) -> &Path {
        &self.session_dir
    }

    pub fn issue(&self) -> &issue::Issue {
        &self.job.issue
    }

    pub fn verification(&self) -> &verify::VerificationContext {
        &self.verification
    }

    /// 本次注入的技能（`name@version`），供决策 trace 记录。
    pub fn skills_used(&self) -> &[String] {
        &self.skills_used
    }

    pub async fn execute(&self) -> anyhow::Result<repair::RepairReport> {
        self.verification.assert_delivery_allowed()?;
        self.verification.assert_integrity(&self.job.repo)?;
        let result = repair::run(&self.job).await;
        // 成功或失败都在 repair 停止写入后发布脱敏快照；serve 永不读取原始日志。
        let dir = match &result {
            Ok(report) => Path::new(&report.session_dir),
            Err(_) => self.session_dir.as_path(),
        };
        if let Err(error) = self.job.secrets.redact_session_dir(dir) {
            let detail = self
                .job
                .secrets
                .redact(&format!("publish redacted transcript failed: {error:#}"));
            eprintln!("transcript: {detail}");
            // Snapshots are auxiliary: serve never falls back to raw logs.
            // Keep both a successful report and an original repair error intact.
        }
        result
    }
}

/// While a repair is running, atomically republish the redacted snapshot every
/// few seconds so the console can show live progress without ever reading the
/// private JSONL (ADR-019). The final publish still happens in `execute`.
const LIVE_SNAPSHOT_INTERVAL: Duration = Duration::from_secs(3);

fn live_snapshot_progress(
    inner: Option<ProgressSink>,
    secrets: Arc<secrets::Secrets>,
    session_dir: PathBuf,
) -> ProgressSink {
    let last = std::sync::Mutex::new(
        std::time::Instant::now()
            .checked_sub(LIVE_SNAPSHOT_INTERVAL)
            .unwrap_or_else(std::time::Instant::now),
    );
    Arc::new(move |progress| {
        if let Some(inner) = &inner {
            inner(progress);
        }
        let due = {
            let mut last = last.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if last.elapsed() < LIVE_SNAPSHOT_INTERVAL {
                false
            } else {
                *last = std::time::Instant::now();
                true
            }
        };
        if due {
            if let Err(error) = secrets.redact_session_dir(&session_dir) {
                // A partial tail or an unreadable file must not abort the run;
                // the next tick (or the final publish) retries.
                eprintln!("transcript: live snapshot failed: {error:#}");
            }
        }
    })
}

pub async fn prepare_repair(mut req: RepairRequest) -> anyhow::Result<PreparedRepair> {
    let workspace = req.workspace.clone().unwrap_or_else(|| req.repo.clone());
    let base = crate::git_ops::git(
        &workspace,
        &[
            "rev-parse",
            "--verify",
            req.base.as_deref().unwrap_or("HEAD"),
        ],
    )?;
    anyhow::ensure!(
        crate::git_ops::git(&workspace, &["rev-parse", "HEAD"])? == base,
        "repair checkout does not match the pinned base"
    );
    // A temporary private authority is outside every editable checkout. It is
    // needed only until prepare() has frozen its own immutable snapshot.
    let private = tempfile::tempdir().context("private preparation authority")?;
    let mut owner = req.owner.take();
    if owner.is_none()
        && (req.repo.join("nuwa.repo.json").exists()
            || crate::git_ops::git_raw(&req.repo, &["show", &format!("{base}:nuwa.repo.json")])
                .is_ok())
    {
        owner = Some(TrustedInstructions::load(
            &req.repo,
            &req.repo.join("nuwa.repo.json"),
            &base,
            private.path(),
        )?);
    }
    if let Some(owner) = &owner {
        verify::seed_owner_config(&req.repo, &owner.bytes)?;
        let config = owner.pinned_instructions(&req.repo, &base)?;
        // A supplied strict/oracle configuration cannot become legacy because
        // the authoritative source was absent or different.
        if let Some(supplied) = &req.repo_config {
            anyhow::ensure!(
                supplied.verification_policy == config.verification_policy
                    && supplied.trusted_verification.is_some()
                        == config.trusted_verification.is_some(),
                "supplied verification policy differs from owner authority"
            );
        }
        req.repo_config = Some(config);
    } else if let Some(config) = &req.repo_config {
        anyhow::ensure!(
            config.verification_policy == crate::instructions::VerificationPolicy::Compatibility
                && config.trusted_verification.is_none(),
            "strict/oracle configuration requires authoritative owner instructions"
        );
    }
    let verification = Arc::new(
        if checkout_identity(&req.repo)? == checkout_identity(&workspace)? {
            verify::prepare(&workspace, &base, owner.as_ref(), &req.secrets).await?
        } else {
            verify::prepare_workspace(&workspace, &req.repo, &base, owner.as_ref(), &req.secrets)
                .await?
        },
    );
    if let Some(config) = &req.repo_config {
        req.issue.failing_check = config.failing_check.clone();
        req.issue.reproduce_check = config.reproduce_check.clone();
        req.issue
            .forbidden_paths
            .extend(config.forbidden_paths.clone());
    }
    req.issue
        .forbidden_paths
        .extend(verification.protected_paths());
    req.issue.forbidden_paths.push("nuwa.repo.json".into());
    req.issue.forbidden_paths.sort();
    req.issue.forbidden_paths.dedup();
    req.issue.verification_mode = if verification.is_trusted() {
        issue::VerificationMode::OwnerTrusted
    } else if req.issue.reproduce_check.is_some() {
        issue::VerificationMode::Configured
    } else {
        // Compatibility only: this advisory result is owned by preparation,
        // not duplicated by entrypoints and never promoted to trusted proof.
        match verify::probe_working_tree(&workspace, &req.issue.failing_check, &req.secrets).await {
            Ok(out) if out.ok => issue::VerificationMode::Reproducer,
            Ok(_) => issue::VerificationMode::FailingCheck,
            Err(error) => {
                eprintln!("probe      : {}", req.secrets.redact(&format!("{error:#}")));
                issue::VerificationMode::FailingCheck
            }
        }
    };
    let session_dir = repair::session_dir_for(&req.sessions_dir, &req.issue, req.run_id.as_deref());
    let session_dir = std::path::absolute(&session_dir)
        .with_context(|| format!("resolve {}", session_dir.display()))?;
    // 技能选择在这里发生：两条入口共享同一套确定性规则与长度上限。
    let allowlist = req
        .repo_config
        .as_ref()
        .map(|config| config.skills.as_slice())
        .unwrap_or(&[]);
    let selection = skills::select(&req.repo, &req.issue, allowlist);
    for warning in &selection.warnings {
        eprintln!("skills    : {warning}");
    }
    let base_notes = req
        .repo_config
        .as_ref()
        .map(|config| config.notes.trim())
        .unwrap_or("");
    let notes = match (base_notes.is_empty(), selection.section.is_empty()) {
        (true, true) => String::new(),
        (false, true) => base_notes.to_string(),
        (true, false) => selection.section.trim_start().to_string(),
        (false, false) => format!("{base_notes}\n{}", selection.section),
    };
    let guards = Arc::new(guard::Guards::for_checkout(
        &workspace,
        req.issue.forbidden_paths.clone(),
        req.repo_config
            .as_ref()
            .map(|c| c.denied_read_paths.clone())
            .unwrap_or_default(),
    )?);
    let progress =
        live_snapshot_progress(req.progress, Arc::clone(&req.secrets), session_dir.clone());
    let job = repair::RepairJob {
        repo: workspace,
        issue: req.issue,
        sessions_dir: req.sessions_dir,
        provider: req.provider,
        max_turns: req.max_turns,
        guards,
        secrets: req.secrets,
        notes,
        progress: Some(progress),
        faux_script: req.faux_script,
        run_id: req.run_id,
    };
    Ok(PreparedRepair {
        job,
        session_dir,
        skills_used: selection.used,
        verification,
    })
}

/// 交付闸门的完整输入。策略参数（push/create_pr/草稿）由调用方决定，
/// 其余（护栏、脱敏、会话目录）在流水线内固定。
pub struct DeliveryRequest<'a> {
    pub repo: &'a Path,
    pub issue: &'a issue::Issue,
    pub report: &'a repair::RepairReport,
    pub sessions_dir: &'a Path,
    pub push: bool,
    pub create_pr: bool,
    pub public_base_url: Option<&'a str>,
    pub draft: bool,
    pub secrets: &'a secrets::Secrets,
    /// All delivery modes, including local-only, require this durable journal.
    pub store: Option<&'a shanhaijing::Store>,
    pub run_id: Option<&'a str>,
    pub repo_id: &'a str,
    pub checkout_claim: Option<&'a shanhaijing::CheckoutClaim>,
    pub verification: &'a verify::VerificationContext,
    pub limits: crate::quota::DeliveryLimits,
}

pub async fn deliver(req: DeliveryRequest<'_>) -> anyhow::Result<deliver::DeliverReport> {
    deliver::deliver(
        req.repo,
        req.issue,
        req.report,
        req.push,
        req.create_pr,
        req.sessions_dir,
        req.secrets,
        req.public_base_url,
        req.draft,
        req.store,
        req.run_id,
        req.repo_id,
        req.checkout_claim,
        req.verification,
        req.limits,
    )
    .await
}

/// 外部信号 → 问题单。原始信号是外部输入：进入 LLM 上下文前先脱敏，
/// 两条入口不再各写一份字段映射。
pub fn issue_from_snapshot(
    snapshot: &shanhaijing::IssueSnapshot,
    repo_config: &RepoInstructions,
    prior_context: String,
    secrets: &secrets::Secrets,
) -> issue::Issue {
    redact_issue(
        issue::Issue {
            fingerprint: snapshot.fingerprint.clone(),
            title: snapshot.title.clone(),
            language: snapshot.language.clone(),
            occurrences: snapshot.occurrences,
            severity: snapshot.severity.clone(),
            first_seen: format_timestamp(snapshot.first_seen),
            last_seen: format_timestamp(snapshot.last_seen),
            stack_trace: snapshot.stack_sample.clone(),
            log_window: snapshot.log_window.clone(),
            prior_context,
            failing_check: repo_config.failing_check.clone(),
            reproduce_check: repo_config.reproduce_check.clone(),
            forbidden_paths: repo_config.forbidden_paths.clone(),
            verification_mode: Default::default(),
        },
        secrets,
    )
}

/// Unix milliseconds as `YYYY-MM-DD HH:MM:SS UTC` for the repair prompt.
/// Hand-rolled so the runner needs no date library.
fn format_timestamp(ms: i64) -> String {
    if ms <= 0 {
        return "(unknown)".to_string();
    }
    let seconds = ms.div_euclid(1_000);
    let days = seconds.div_euclid(86_400);
    let secs_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02} UTC",
        secs_of_day / 3_600,
        (secs_of_day % 3_600) / 60,
        secs_of_day % 60
    )
}

/// Days since 1970-01-01 → `(year, month, day)`. Howard Hinnant's
/// `civil_from_days` (public-domain algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (
        if month <= 2 { year + 1 } else { year },
        month as u32,
        day as u32,
    )
}

/// 一次性 CLI 从文件加载的问题单同样是外部输入。
pub fn redact_issue(mut issue: issue::Issue, secrets: &secrets::Secrets) -> issue::Issue {
    issue.title = secrets.redact(&issue.title);
    issue.stack_trace = secrets.redact(&issue.stack_trace);
    issue.log_window = issue
        .log_window
        .iter()
        .map(|line| secrets.redact(line))
        .collect();
    issue.prior_context = secrets.redact(&issue.prior_context);
    issue
}

/// 人类可读的 provider 标识，打印与日志共用。
pub fn provider_label(provider: &repair::ProviderKind) -> String {
    match provider {
        repair::ProviderKind::Faux => "faux".to_string(),
        repair::ProviderKind::Anthropic { model } => match model {
            Some(m) => format!("anthropic:{m}"),
            None => "anthropic:<catalog default>".to_string(),
        },
        repair::ProviderKind::Zai { model, .. } => match model {
            Some(m) => format!("zai:{m}"),
            None => "zai:glm-5.3-flash".to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issue::FailingCheck;

    #[tokio::test]
    async fn checkout_renews_during_a_wait_without_progress_callbacks() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(shanhaijing::Store::open(":memory:").expect("store"));
        let lease = CheckoutLease::acquire(&store, dir.path(), "fix", None).expect("claim");
        let started = crate::now_ms();
        lease
            .during_with_period(Duration::from_millis(5), async {
                tokio::time::sleep(Duration::from_millis(30)).await;
                assert!(store.checkout_claim_valid(&lease.claim, started + CHECKOUT_TTL_MS + 1)?);
                Ok(())
            })
            .await
            .expect("renewed during provider/tool wait");
    }

    #[tokio::test]
    async fn checkout_loss_fails_closed_and_drop_preserves_successor() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(shanhaijing::Store::open(":memory:").expect("store"));
        let lease = CheckoutLease::acquire(&store, dir.path(), "old", None).expect("claim");
        let path = lease.claim.path.clone();
        let successor = std::sync::Mutex::new(None);
        assert!(lease
            .during(async {
                *successor.lock().expect("lock") = store.claim_checkout(
                    &path,
                    "successor",
                    crate::now_ms() + CHECKOUT_TTL_MS + 1,
                    CHECKOUT_TTL_MS,
                )?;
                Ok(())
            })
            .await
            .is_err());
        drop(lease);
        let claim = successor.lock().expect("lock").take().expect("successor");
        assert!(store
            .checkout_claim_valid(&claim, crate::now_ms())
            .expect("successor preserved"));
    }

    #[cfg(windows)]
    #[test]
    fn checkout_identity_folds_windows_case_after_canonicalization() {
        let dir = tempfile::tempdir().expect("tempdir");
        let alias = PathBuf::from(dir.path().to_str().expect("path").to_uppercase());
        assert_eq!(
            checkout_identity(&alias).expect("case alias"),
            checkout_identity(dir.path()).expect("identity")
        );
    }

    #[cfg(unix)]
    #[test]
    fn checkout_identity_resolves_symlink_aliases() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        let alias = dir.path().join("alias");
        std::fs::create_dir(&repo).expect("repo");
        std::os::unix::fs::symlink(&repo, &alias).expect("symlink");
        assert_eq!(
            checkout_identity(&repo).expect("repo"),
            checkout_identity(&alias).expect("alias")
        );
    }

    fn issue() -> issue::Issue {
        issue::Issue {
            fingerprint: "abc/def".into(),
            title: "token ghp_abcdefghijklmnopqrstuvwxyz0123456789".into(),
            language: "go".into(),
            occurrences: 1,
            severity: "high".into(),
            first_seen: String::new(),
            last_seen: String::new(),
            stack_trace: "panic".into(),
            log_window: vec!["auth header: Bearer ghp_abcdefghijklmnopqrstuvwxyz0123456789".into()],
            prior_context: String::new(),
            failing_check: FailingCheck {
                command: "go test ./...".into(),
                target: None,
                timeout_secs: 60,
            },
            reproduce_check: None,
            forbidden_paths: Vec::new(),
            verification_mode: Default::default(),
        }
    }

    #[test]
    fn redaction_covers_every_external_field() {
        let secrets = secrets::Secrets::with_values(vec![]);
        let redacted = redact_issue(issue(), &secrets);
        assert!(!redacted.title.contains("ghp_"));
        assert!(!redacted.log_window[0].contains("ghp_"));
    }

    fn snapshot() -> shanhaijing::IssueSnapshot {
        shanhaijing::IssueSnapshot {
            fingerprint: "abc/def".into(),
            title: "panic".into(),
            language: "go".into(),
            repo: "example.com/app".into(),
            severity: "high".into(),
            occurrences: 2,
            attempts: 0,
            stack_sample: "panic: boom".into(),
            log_window: vec!["line one".into(), "line two".into()],
            first_seen: 1_000,
            last_seen: 2_000,
        }
    }

    #[test]
    fn issue_from_snapshot_carries_the_log_window_and_timestamps() {
        let instructions = RepoInstructions {
            repo: "example.com/app".into(),
            failing_check: FailingCheck {
                command: "go test ./...".into(),
                target: None,
                timeout_secs: 60,
            },
            forbidden_paths: Vec::new(),
            notes: String::new(),
            skills: Vec::new(),
            reproduce_check: None,
            denied_read_paths: Vec::new(),
            verification_policy: Default::default(),
            trusted_verification: None,
            max_attempts: 3,
            draft_first_n: 0,
        };
        let built = issue_from_snapshot(
            &snapshot(),
            &instructions,
            String::new(),
            &secrets::Secrets::default(),
        );
        assert_eq!(
            built.log_window,
            vec!["line one".to_string(), "line two".to_string()]
        );
        assert_eq!(built.first_seen, "1970-01-01 00:00:01 UTC");
        assert_eq!(built.last_seen, "1970-01-01 00:00:02 UTC");
    }

    #[test]
    fn timestamps_format_as_utc_for_the_prompt() {
        assert_eq!(format_timestamp(0), "(unknown)");
        assert_eq!(format_timestamp(1_000), "1970-01-01 00:00:01 UTC");
        // Unix billennium — a well-known checkpoint.
        assert_eq!(
            format_timestamp(1_000_000_000_000),
            "2001-09-09 01:46:40 UTC"
        );
        // Leap day in a leap century (2000 is divisible by 400).
        assert_eq!(format_timestamp(951_782_400_000), "2000-02-29 00:00:00 UTC");
    }

    fn init_repo(repo: &Path) {
        std::fs::create_dir_all(repo).expect("repo");
        crate::git_ops::git(repo, &["init", "-b", "main"]).expect("init");
        crate::git_ops::git(
            repo,
            &[
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "--allow-empty",
                "-m",
                "base",
            ],
        )
        .expect("base");
    }

    fn preparation_request(repo: &Path, sessions: &Path) -> RepairRequest {
        let mut issue = issue();
        issue.failing_check.command = "echo ok".into();
        RepairRequest {
            repo: repo.to_path_buf(),
            workspace: None,
            issue,
            sessions_dir: sessions.to_path_buf(),
            provider: repair::ProviderKind::Faux,
            max_turns: 1,
            repo_config: None,
            owner: None,
            base: None,
            secrets: Arc::new(secrets::Secrets::default()),
            progress: None,
            faux_script: Some(rpi_ai::providers::faux::FauxScript::new().with_text("done")),
            run_id: None,
        }
    }

    #[tokio::test]
    async fn strict_without_oracle_blocks_before_model_even_without_a_config_file() {
        for on_disk in [false, true] {
            let dir = tempfile::tempdir().expect("fixture");
            let repo = dir.path().join("repo");
            init_repo(&repo);
            let config = serde_json::json!({
                "repo": "example.com/strict", "failing_check": {"command": "echo ok"},
                "verification_policy": "strict",
            });
            if on_disk {
                std::fs::write(repo.join("nuwa.repo.json"), config.to_string()).expect("config");
            }
            let sessions = dir.path().join("sessions");
            let mut req = preparation_request(&repo, &sessions);
            req.repo_config = Some(serde_json::from_value(config).expect("config"));
            let tip = crate::git_ops::git(&repo, &["rev-parse", "HEAD"]).expect("tip");
            let error = prepare_repair(req).await.err().expect("strict refusal");
            assert!(format!("{error:#}").contains("strict"));
            assert!(!sessions.exists(), "no model/session effects");
            assert_eq!(
                crate::git_ops::git(&repo, &["rev-parse", "HEAD"]).expect("tip"),
                tip
            );
            assert!(
                crate::git_ops::git(&repo, &["for-each-ref", "refs/heads/agent/fix/"])
                    .expect("refs")
                    .is_empty()
            );
        }
    }

    #[tokio::test]
    async fn pool_workspace_receives_exact_pinned_private_owner_config_before_preparation() {
        let dir = tempfile::tempdir().expect("fixture");
        let repo = dir.path().join("config");
        let workspace = dir.path().join("workspace");
        init_repo(&repo);
        crate::git_ops::git(
            &repo,
            &[
                "worktree",
                "add",
                "--detach",
                workspace.to_str().expect("path"),
                "HEAD",
            ],
        )
        .expect("workspace");
        let bytes = serde_json::json!({
            "repo": "example.com/pool", "failing_check": {"command": "echo ok"},
            "notes": "owner guidance", "denied_read_paths": ["private/**"],
        })
        .to_string();
        std::fs::write(repo.join("nuwa.repo.json"), &bytes).expect("private config");
        let owner = TrustedInstructions::load(
            &repo,
            &repo.join("nuwa.repo.json"),
            "HEAD",
            &dir.path().join("authority"),
        )
        .expect("owner");
        let mut req = preparation_request(&repo, &dir.path().join("sessions"));
        req.workspace = Some(workspace.clone());
        req.repo_config = Some(owner.instructions.clone());
        req.owner = Some(owner);
        assert!(!workspace.join("nuwa.repo.json").exists());
        let prepared = prepare_repair(req).await.expect("prepare workspace");
        assert_eq!(
            std::fs::read_to_string(workspace.join("nuwa.repo.json")).expect("seeded"),
            bytes
        );
        assert!(prepared.job.notes.contains("owner guidance"));
        prepared
            .verification()
            .assert_integrity(&workspace)
            .expect("workspace-bound context");
        std::fs::write(workspace.join("nuwa.repo.json"), "{}").expect("tamper");
        assert!(
            prepared.execute().await.is_err(),
            "do not refresh authority before model"
        );
        assert!(!prepared.session_dir().exists());
    }

    #[tokio::test]
    async fn pool_freezes_untracked_oracle_inputs_from_config_root_before_model() {
        let dir = tempfile::tempdir().expect("fixture");
        let repo = dir.path().join("config");
        let workspace = dir.path().join("workspace");
        init_repo(&repo);
        crate::git_ops::git(
            &repo,
            &[
                "worktree",
                "add",
                "--detach",
                workspace.to_str().expect("path"),
                "HEAD",
            ],
        )
        .expect("linked pool worktree");
        assert!(workspace.join(".git").is_file());
        std::fs::create_dir(repo.join("checks")).expect("private checks");
        let oracle = "import os, pathlib, sys\nroot = pathlib.Path(os.environ['NUWA_VERIFY_ROOT'])\nexpected = pathlib.Path(__file__).with_name('expected.txt').read_text()\nfix = root / 'fixed.txt'\nsys.exit(0 if fix.is_file() and fix.read_text() == expected else 1)\n";
        std::fs::write(repo.join("checks/oracle.py"), oracle).expect("oracle");
        std::fs::write(repo.join("checks/expected.txt"), "fixed\n").expect("expected");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                repo.join("checks/expected.txt"),
                std::fs::Permissions::from_mode(0o640),
            )
            .expect("private native mode");
        }
        let launcher = if cfg!(windows) { "python" } else { "python3" };
        let config = serde_json::json!({
            "repo": "example.com/pool", "failing_check": {"command": "echo ok"},
            "verification_policy": "strict",
            "trusted_verification": {
                "check": {"command": format!("{launcher} checks/oracle.py"), "timeout_secs": 10},
                "protected_inputs": ["checks/oracle.py", "checks/expected.txt"]
            }
        });
        std::fs::write(repo.join("nuwa.repo.json"), config.to_string()).expect("private config");
        let mut req = preparation_request(&repo, &dir.path().join("sessions"));
        req.workspace = Some(workspace.clone());
        let prepared = prepare_repair(req).await.expect("freeze then seed pool");
        for path in ["nuwa.repo.json", "checks/oracle.py", "checks/expected.txt"] {
            assert_eq!(
                std::fs::read(workspace.join(path)).expect("seeded bytes"),
                std::fs::read(repo.join(path)).expect("source bytes")
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    std::fs::metadata(workspace.join(path))
                        .expect("seed mode")
                        .permissions()
                        .mode(),
                    std::fs::metadata(repo.join(path))
                        .expect("source mode")
                        .permissions()
                        .mode()
                );
            }
        }
        std::fs::write(workspace.join("fixed.txt"), "fixed\n").expect("genuine fix");
        let context = prepared.verification();
        let evidence = verify::verify(
            &workspace,
            prepared.issue(),
            context.base_sha(),
            &secrets::Secrets::default(),
            context,
        )
        .await
        .expect("pool owner-green fix");
        assert_eq!(evidence.diff.files.len(), 1);
        assert_eq!(evidence.diff.files[0].path, "fixed.txt");
        assert!(!repo.join("fixed.txt").exists());
        std::fs::write(repo.join("checks/expected.txt"), "tampered\n").expect("source tamper");
        assert!(context.assert_integrity(&workspace).is_err());
        assert!(verify::verify(
            &workspace,
            prepared.issue(),
            context.base_sha(),
            &secrets::Secrets::default(),
            context,
        )
        .await
        .is_err());
        assert!(
            prepared.execute().await.is_err(),
            "no model after source tamper"
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("checks/expected.txt")).expect("no restore"),
            "tampered\n"
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("checks/expected.txt")).expect("frozen seed"),
            "fixed\n"
        );
    }

    #[tokio::test]
    async fn session_dir_is_per_run_and_absolute() {
        let dir = tempfile::tempdir().expect("fixture");
        init_repo(dir.path());
        let req = RepairRequest {
            repo: dir.path().to_path_buf(),
            workspace: None,
            issue: issue(),
            sessions_dir: PathBuf::from("sessions"),
            provider: repair::ProviderKind::Faux,
            max_turns: 1,
            repo_config: None,
            owner: None,
            base: None,
            secrets: Arc::new(secrets::Secrets::default()),
            progress: None,
            faux_script: None,
            run_id: Some("run-1".into()),
        };
        let prepared = prepare_repair(req).await.expect("prepare");
        assert!(prepared.session_dir().is_absolute());
        assert!(prepared.session_dir().ends_with("abc-def/run-1"));
    }

    #[tokio::test]
    async fn workspace_is_the_edit_root_not_the_config_root() {
        let dir = tempfile::tempdir().expect("test fixture");
        let config = dir.path().join("config");
        let workspace = dir.path().join("workspace");
        init_repo(&config);
        init_repo(&workspace);
        let prepared = prepare_repair(RepairRequest {
            repo: config.clone(),
            workspace: Some(workspace.clone()),
            issue: issue(),
            sessions_dir: dir.path().join("sessions"),
            provider: repair::ProviderKind::Faux,
            max_turns: 1,
            repo_config: None,
            owner: None,
            base: None,
            secrets: Arc::new(secrets::Secrets::default()),
            progress: None,
            faux_script: None,
            run_id: Some("workspace-split".into()),
        })
        .await
        .expect("prepare");
        assert_eq!(
            prepared.job.repo, workspace,
            "repair must edit the workspace"
        );
        assert_ne!(
            prepared.job.repo, config,
            "config/skills root must not be the edit root"
        );
    }

    #[test]
    fn cli_session_dir_stays_flat() {
        let dir = std::env::temp_dir().join("nuwa-pipeline-flat");
        let dir = repair::session_dir_for(&dir, &issue(), None);
        assert!(dir.ends_with("abc-def"));
    }

    async fn snapshot_repair(dir: &Path, fail: bool) -> PreparedRepair {
        init_repo(dir);
        prepare_repair(RepairRequest {
            repo: dir.to_path_buf(),
            workspace: None,
            issue: issue(),
            sessions_dir: dir.join("sessions"),
            provider: if fail {
                repair::ProviderKind::Anthropic { model: None }
            } else {
                repair::ProviderKind::Faux
            },
            max_turns: 1,
            repo_config: None,
            owner: None,
            base: None,
            secrets: Arc::new(secrets::Secrets::default()),
            progress: None,
            faux_script: Some(rpi_ai::providers::faux::FauxScript::new().with_text("done")),
            run_id: Some("snapshot-run".into()),
        })
        .await
        .expect("test fixture")
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn snapshot_is_published_after_success_or_repair_failure() {
        for fail in [false, true] {
            let dir = tempfile::tempdir().expect("test fixture");
            let prepared = snapshot_repair(dir.path(), fail).await;
            std::fs::create_dir_all(prepared.session_dir()).expect("test fixture");
            let raw = "{\"text\":\"ghp_abcdefghijklmnopqrstuvwxyz0123456789\"}\n";
            std::fs::write(prepared.session_dir().join("private.jsonl"), raw)
                .expect("test fixture");
            let result = prepared.execute().await;
            assert_eq!(result.is_err(), fail, "{:?}", result.err());
            assert_eq!(
                std::fs::read_to_string(prepared.session_dir().join("private.jsonl"))
                    .expect("test fixture"),
                raw
            );
            assert_eq!(
                std::fs::read_to_string(prepared.session_dir().join("redacted/private.jsonl"))
                    .expect("test fixture"),
                "{\"text\":\"[REDACTED]\"}\n"
            );
        }
    }

    #[tokio::test]
    async fn progress_publishes_a_live_redacted_snapshot() {
        let dir = tempfile::tempdir().expect("test fixture");
        init_repo(dir.path());
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let inner_calls = Arc::clone(&calls);
        let prepared = prepare_repair(RepairRequest {
            repo: dir.path().to_path_buf(),
            workspace: None,
            issue: issue(),
            sessions_dir: dir.path().join("sessions"),
            provider: repair::ProviderKind::Faux,
            max_turns: 1,
            repo_config: None,
            owner: None,
            base: None,
            secrets: Arc::new(secrets::Secrets::default()),
            progress: Some(Arc::new(move |progress| {
                inner_calls.lock().expect("calls").push(progress);
            })),
            faux_script: None,
            run_id: Some("live-snapshot".into()),
        })
        .await
        .expect("test fixture");
        std::fs::create_dir_all(prepared.session_dir()).expect("test fixture");
        std::fs::write(
            prepared.session_dir().join("private.jsonl"),
            "{\"text\":\"ghp_abcdefghijklmnopqrstuvwxyz0123456789\"}\n",
        )
        .expect("test fixture");

        prepared.job.progress.as_ref().expect("wrapped progress")(repair::RepairProgress {
            turns: 1,
            uncached_tokens: 10,
            cached_tokens: 2,
        });

        assert_eq!(
            calls.lock().expect("calls").len(),
            1,
            "the caller's progress sink must still run"
        );
        assert_eq!(
            std::fs::read_to_string(prepared.session_dir().join("redacted/private.jsonl"))
                .expect("live snapshot"),
            "{\"text\":\"[REDACTED]\"}\n"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn review_snapshot_failure_preserves_successful_report() {
        for obstructed in [false, true] {
            let dir = tempfile::tempdir().expect("test fixture");
            let prepared = snapshot_repair(dir.path(), false).await;
            std::fs::create_dir_all(prepared.session_dir()).expect("test fixture");
            std::fs::write(prepared.session_dir().join("private.jsonl"), "invalid JSON")
                .expect("test fixture");
            if obstructed {
                std::fs::write(prepared.session_dir().join("redacted"), "not a directory")
                    .expect("test fixture");
            }
            let report = prepared
                .execute()
                .await
                .expect("snapshot failure is auxiliary");
            assert_eq!(report.root_cause_text, "done");
            assert_eq!(Path::new(&report.session_dir), prepared.session_dir());
            assert!(report.turns > 0);
            assert!(!prepared
                .session_dir()
                .join("redacted/private.jsonl")
                .exists());
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn review_snapshot_failure_preserves_original_repair_error() {
        let dir = tempfile::tempdir().expect("test fixture");
        let prepared = snapshot_repair(dir.path(), true).await;
        let original = repair::run(&prepared.job)
            .await
            .err()
            .expect("missing synthetic key");
        std::fs::create_dir_all(prepared.session_dir()).expect("test fixture");
        std::fs::write(prepared.session_dir().join("private.jsonl"), "invalid JSON")
            .expect("test fixture");
        let error = prepared
            .execute()
            .await
            .err()
            .expect("original repair failure");
        assert_eq!(format!("{error:#}"), format!("{original:#}"));
        assert_eq!(
            error.root_cause().to_string(),
            original.root_cause().to_string()
        );
        assert!(!prepared
            .session_dir()
            .join("redacted/private.jsonl")
            .exists());
    }
}
