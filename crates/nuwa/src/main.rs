mod api;
mod benchmark;
mod check;
mod ci;
mod ci_watch;
mod deliver;
mod git_ops;
mod guard;
mod instructions;
mod issue;
mod pipeline;
mod repair;
mod secrets;
mod serve;
mod session;
mod settings;
mod skills;
mod trace;
mod verify;
mod web;
mod webhook;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context};

/// Shared by the ingest route: the issue store and its report token.
pub type Shared = (Arc<shanhaijing::Store>, Arc<Option<String>>);

#[derive(Debug)]
struct Cli {
    issue_path: PathBuf,
    repo: PathBuf,
    sessions_dir: PathBuf,
    provider: repair::ProviderKind,
    max_turns: u32,
    push: bool,
    create_pr: bool,
    no_deliver: bool,
}

fn parse_args(args: Vec<String>) -> anyhow::Result<Cli> {
    let mut args = args.into_iter();
    let mut cli = Cli {
        issue_path: PathBuf::new(),
        repo: PathBuf::new(),
        sessions_dir: PathBuf::from("spike/sessions"),
        provider: repair::ProviderKind::Anthropic { model: None },
        max_turns: 30,
        push: false,
        create_pr: false,
        no_deliver: false,
    };
    while let Some(flag) = args.next() {
        let mut value = |name: &str| {
            args.next()
                .with_context(|| format!("missing value for {name}"))
        };
        match flag.as_str() {
            "--issue" => cli.issue_path = PathBuf::from(value("--issue")?),
            "--repo" => cli.repo = PathBuf::from(value("--repo")?),
            "--sessions" => cli.sessions_dir = PathBuf::from(value("--sessions")?),
            "--provider" => match value("--provider")?.as_str() {
                "anthropic" => cli.provider = repair::ProviderKind::Anthropic { model: None },
                "faux" => cli.provider = repair::ProviderKind::Faux,
                "zai" => {
                    cli.provider = repair::ProviderKind::Zai {
                        base_url: None,
                        model: None,
                    }
                }
                other => bail!("unknown provider {other} (anthropic|faux|zai)"),
            },
            "--base-url" => match &mut cli.provider {
                repair::ProviderKind::Zai { base_url, .. } => {
                    *base_url = Some(value("--base-url")?)
                }
                _ => bail!("--base-url only applies to --provider zai"),
            },
            "--model" => {
                let model = value("--model")?;
                match &mut cli.provider {
                    repair::ProviderKind::Anthropic { model: m } => *m = Some(model),
                    repair::ProviderKind::Zai { model: m, .. } => *m = Some(model),
                    repair::ProviderKind::Faux => bail!("--model does not apply to faux"),
                }
            }
            "--max-turns" => cli.max_turns = value("--max-turns")?.parse()?,
            "--push" => cli.push = true,
            "--pr" => {
                cli.push = true;
                cli.create_pr = true;
            }
            "--no-deliver" => cli.no_deliver = true,
            other => bail!("unknown flag {other}"),
        }
    }
    if cli.issue_path.as_os_str().is_empty() || cli.repo.as_os_str().is_empty() {
        bail!("--issue and --repo are required");
    }
    Ok(cli)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let result = run_command().await;
    // Webhook delivery runs in a detached task. The events that matter most
    // (escalated, ci_red) are emitted right before the runner gives up on an
    // issue, and the process may exit immediately afterwards — without this
    // drain they die with the runtime.
    let _ = webhook::flush(std::time::Duration::from_secs(10)).await;
    result
}

async fn run_command() -> anyhow::Result<()> {
    let mut argv = std::env::args().skip(1);
    match argv.next().as_deref() {
        Some("fix") => run_fix().await,
        Some("ingest") => run_ingest(argv.collect()),
        Some("work") => run_work(argv.collect()).await,
        Some("serve") => serve::run_serve(argv.collect()).await,
        Some("status") => run_status(argv.collect()),
        Some("sync") => run_sync(argv.collect()).await,
        Some("reopen") => run_reopen(argv.collect()),
        Some("ignore") => run_ignore(argv.collect()),
        Some("check") => check::run(argv.collect()).await,
        Some("benchmark") => benchmark::run(argv.collect()).await,
        Some("migrate-env") => run_migrate_env(argv.collect()),
        other => bail!(
            "usage: nuwa fix --issue <path> --repo <path> [--provider anthropic|faux] [--model <id>] [--max-turns N] [--push] [--pr] [--no-deliver]\n            nuwa ingest --signal <path> [--db <path>]\n            nuwa work --db <path> --instructions <path> [--provider anthropic|faux] [--runner-id <id>] [--service-unit <unit>] [--push] [--pr] [--loop --interval N]\n            nuwa serve --db <path> [--listen 127.0.0.1:7321]   (HTTP ingest for 青鸟 HTTPSink)\n            nuwa status [--db <path>]\n            nuwa sync --db <path>   (pr_open 问题单与 GitHub 对账：merged→merged, closed→rejected)\n            nuwa reopen --db <path> --fingerprint <fp> [--reason <text>]   (owner reviewed an escalation)\n            nuwa ignore --db <path> --fingerprint <fp> [--reason <text>]   (owner mute / not a bug)\n            nuwa check --instructions <path> [--repo <path>] [--base main] [--json]   (preflight a repo config)\n            nuwa benchmark [run] [--db <path>] [--provider faux|zai|anthropic] [--model <id>] [--scenario <name,...>] [--max-turns N] [--json]   (repair benchmark; `benchmark list` prints scenarios)\n            nuwa migrate-env [--env-file <path>]   (deploy-time env key migration)\n        (got {other:?})"
        ),
    }
}

/// Shared by `ingest` (file) and `serve` (HTTP body): one JSON object or
/// JSONL — the 青鸟 FileSink/HTTPSink wire formats.
pub(crate) fn parse_signals(raw: &str) -> anyhow::Result<Vec<shanhaijing::Signal>> {
    match serde_json::from_str::<shanhaijing::Signal>(raw) {
        Ok(signal) => Ok(vec![signal]),
        Err(first_error) => raw
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|line| {
                serde_json::from_str(line).with_context(|| {
                    format!("not a Signal object ({first_error}) and line is not JSONL: {line:.80}")
                })
            })
            .collect(),
    }
}

/// Owner-reviewed reopen: escalated → new, cooldown cleared so the runner can
/// pick it up immediately. Escalated issues never auto-reopen on reports.
fn run_reopen(args: Vec<String>) -> anyhow::Result<()> {
    let mut db_path = std::path::PathBuf::from("shanhaijing.db");
    let mut fingerprint = None;
    let mut reason = None;
    let mut it = args.into_iter();
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--db" => db_path = PathBuf::from(it.next().context("--db needs a value")?),
            "--fingerprint" => {
                fingerprint = Some(it.next().context("--fingerprint needs a value")?)
            }
            "--reason" => reason = Some(it.next().context("--reason needs a value")?),
            other => bail!("unknown reopen flag {other}"),
        }
    }
    let fp = fingerprint.context("--fingerprint is required")?;
    let store = shanhaijing::Store::open(&db_path)
        .with_context(|| format!("open db {}", db_path.display()))?;
    store.reopen_issue(&fp, now_ms(), reason.as_deref())?;
    store.set_cooldown(&fp, 0)?;
    println!("reopened   : {fp} — ready for pickup");
    Ok(())
}

/// Owner mute: `new`/`escalated`/`rejected` → `ignored`. Reports are counted
/// but never auto-reopen it; only an explicit reopen does.
fn run_ignore(args: Vec<String>) -> anyhow::Result<()> {
    let mut db_path = std::path::PathBuf::from("shanhaijing.db");
    let mut fingerprint = None;
    let mut reason = String::new();
    let mut it = args.into_iter();
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--db" => db_path = PathBuf::from(it.next().context("--db needs a value")?),
            "--fingerprint" => {
                fingerprint = Some(it.next().context("--fingerprint needs a value")?)
            }
            "--reason" => reason = it.next().context("--reason needs a value")?,
            other => bail!("unknown ignore flag {other}"),
        }
    }
    let fp = fingerprint.context("--fingerprint is required")?;
    let store = shanhaijing::Store::open(&db_path)
        .with_context(|| format!("open db {}", db_path.display()))?;
    store.ignore_issue(&fp, &reason, "owner", now_ms())?;
    println!("ignored    : {fp} — reports will be counted, not repaired");
    Ok(())
}

/// 部署期 env 键迁移：把历史小写键规范为大写（启动迁移不能改变 systemd
/// 已注入当前进程的环境，所以作为独立部署步骤提供）。写前备份，原子替换。
fn run_migrate_env(args: Vec<String>) -> anyhow::Result<()> {
    let mut env_file = settings::env_file_path();
    let mut it = args.into_iter();
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--env-file" => {
                env_file = PathBuf::from(it.next().context("--env-file needs a value")?)
            }
            other => bail!("unknown migrate-env flag {other}"),
        }
    }
    let changed = settings::migrate_env_file_at(&env_file).map_err(|e| anyhow::anyhow!(e))?;
    if changed.is_empty() {
        println!(
            "env        : no lowercase keys to migrate in {}",
            env_file.display()
        );
    } else {
        println!(
            "env        : migrated {} -> {} (backup: {})",
            changed.join(", "),
            env_file.display(),
            env_file.with_extension("env.bak").display()
        );
    }
    Ok(())
}

/// 山海经 ingest — the collector-facing entry (`nuwa ingest --signal s.json`).
/// Fingerprint + dedup + cooldown decision happens here, per docs/07 A-1.
fn run_ingest(args: Vec<String>) -> anyhow::Result<()> {
    let mut signal_path = None;
    let mut db_path = std::path::PathBuf::from("shanhaijing.db");
    let mut it = args.into_iter();
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--signal" => {
                signal_path = Some(std::path::PathBuf::from(
                    it.next().context("--signal needs a value")?,
                ))
            }
            "--db" => db_path = std::path::PathBuf::from(it.next().context("--db needs a value")?),
            other => bail!("unknown ingest flag {other}"),
        }
    }
    let signal_path = signal_path.context("--signal <path> is required")?;
    let raw = std::fs::read_to_string(&signal_path)
        .with_context(|| format!("read {}", signal_path.display()))?;
    let store = shanhaijing::Store::open(&db_path)
        .with_context(|| format!("open db {}", db_path.display()))?;
    let now = now_ms();

    let signals = parse_signals(&raw)?;
    if signals.is_empty() {
        bail!("no signals in {}", signal_path.display());
    }
    for signal in &signals {
        println!("{}", report_one(&store, signal, now)?);
    }
    if let Some(issue) = store.ready_issue(now)? {
        println!(
            "next up    : {} [{}] {} — {}x, repo {}",
            issue.fingerprint, issue.severity, issue.title, issue.occurrences, issue.repo
        );
    }
    Ok(())
}

/// One report → the human/HTTP response line.
pub(crate) fn report_one(
    store: &shanhaijing::Store,
    signal: &shanhaijing::Signal,
    now: i64,
) -> anyhow::Result<String> {
    let line = match store.report_signal(signal, now)? {
        shanhaijing::ReportOutcome::NewIssue { fingerprint } => {
            format!("new issue  : {fingerprint} — ready for pickup")
        }
        shanhaijing::ReportOutcome::Deduped {
            fingerprint,
            occurrences,
        } => {
            format!("dedup      : {fingerprint} (occurrences={occurrences})")
        }
        shanhaijing::ReportOutcome::CooldownActive {
            fingerprint,
            retry_after_ms,
            ..
        } => {
            format!("cooldown   : {fingerprint} — retry in {retry_after_ms}ms")
        }
        shanhaijing::ReportOutcome::Muted {
            fingerprint,
            occurrences,
        } => {
            format!("muted      : {fingerprint} — ignored by owner, occurrences={occurrences}")
        }
        shanhaijing::ReportOutcome::Regression {
            fingerprint,
            occurrences,
        } => {
            format!("REGRESSION : {fingerprint} reopened (occurrences={occurrences})")
        }
    };
    Ok(line)
}

/// Resolve the sessions root once at runner startup. `runs.session_dir` is
/// persisted before (and kept through) a repair failure/interruption, and the
/// transcript endpoint only accepts absolute, `..`-free paths — a relative
/// `--sessions` default would leave those runs with an unreadable transcript
/// and shadow older readable runs.
fn absolute_sessions_dir(path: PathBuf) -> anyhow::Result<PathBuf> {
    std::path::absolute(&path).with_context(|| format!("resolve sessions dir {}", path.display()))
}

/// 应龙 runner v0 — claim the highest-priority ready issue from 山海经 and
/// drive it to a verified branch/PR. Failure anywhere after the claim
/// escalates the issue (docs/07 A-1 回炉升级). `--loop` keeps polling
/// (serial runner; the concurrent pool is A-3's remaining half).
async fn run_work(args: Vec<String>) -> anyhow::Result<()> {
    webhook::init_from_env();
    // env 默认（设置页写入 nuwa.env）→ CLI 显式 flag 覆盖。
    let mut db_path = std::path::PathBuf::from("shanhaijing.db");
    let mut instructions_path = None;
    let mut sessions_dir = PathBuf::from("spike/sessions");
    let mut provider_given: Option<repair::ProviderKind> = None;
    let mut max_turns: u32 = env_d("NUWA_MAX_TURNS", "30").parse().unwrap_or(30);
    let mut push = false;
    let mut create_pr = false;
    let mut loop_run = false;
    let mut interval_secs: u64 = env_d("NUWA_INTERVAL", "30").parse().unwrap_or(30);
    let mut daily_pr_limit: u64 = env_d("NUWA_DAILY_PR_LIMIT", "5").parse().unwrap_or(5);
    let mut max_tokens: u64 = env_d("NUWA_MAX_TOKENS", "1500000")
        .parse()
        .unwrap_or(1_500_000);
    let mut daily_token_limit: u64 = env_d("NUWA_DAILY_TOKEN_LIMIT", "5000000")
        .parse()
        .unwrap_or(5_000_000);
    let mut monthly_token_limit: u64 = env_d("NUWA_MONTHLY_TOKEN_LIMIT", "50000000")
        .parse()
        .unwrap_or(50_000_000);
    let mut ci_timeout_secs: u64 = env_d("NUWA_CI_TIMEOUT", "600").parse().unwrap_or(600);
    let mut runner_id_given: Option<String> = std::env::var("NUWA_RUNNER_ID")
        .ok()
        .filter(|v| !v.is_empty());
    let mut service_unit = std::env::var("NUWA_SERVICE_UNIT")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "nuwa-work".to_string());
    let event_retention_days = crate::serve::retention_days_from_env(
        std::env::var("NUWA_EVENT_RETENTION_DAYS").ok().as_deref(),
    );
    let mut it = args.into_iter();
    while let Some(flag) = it.next() {
        let mut value = |name: &str| {
            it.next()
                .with_context(|| format!("missing value for {name}"))
        };
        match flag.as_str() {
            "--db" => db_path = PathBuf::from(value("--db")?),
            "--instructions" => instructions_path = Some(PathBuf::from(value("--instructions")?)),
            "--sessions" => sessions_dir = PathBuf::from(value("--sessions")?),
            "--provider" => match value("--provider")?.as_str() {
                "anthropic" => {
                    provider_given = Some(repair::ProviderKind::Anthropic { model: None })
                }
                "faux" => provider_given = Some(repair::ProviderKind::Faux),
                "zai" => {
                    provider_given = Some(repair::ProviderKind::Zai {
                        base_url: None,
                        model: None,
                    })
                }
                other => bail!("unknown provider {other}"),
            },
            "--base-url" => match &mut provider_given {
                Some(repair::ProviderKind::Zai { base_url, .. }) => {
                    *base_url = Some(value("--base-url")?)
                }
                _ => bail!("--base-url only applies to --provider zai"),
            },
            "--model" => {
                let model = value("--model")?;
                match &mut provider_given {
                    Some(repair::ProviderKind::Anthropic { model: m })
                    | Some(repair::ProviderKind::Zai { model: m, .. }) => *m = Some(model),
                    Some(repair::ProviderKind::Faux) => bail!("--model does not apply to faux"),
                    None => bail!("--model requires --provider"),
                }
            }
            "--max-turns" => max_turns = value("--max-turns")?.parse()?,
            "--push" => push = true,
            "--pr" => {
                push = true;
                create_pr = true;
            }
            "--loop" => loop_run = true,
            "--interval" => interval_secs = value("--interval")?.parse()?,
            "--daily-pr-limit" => daily_pr_limit = value("--daily-pr-limit")?.parse()?,
            "--max-tokens" => max_tokens = value("--max-tokens")?.parse()?,
            "--daily-token-limit" => daily_token_limit = value("--daily-token-limit")?.parse()?,
            "--monthly-token-limit" => {
                monthly_token_limit = value("--monthly-token-limit")?.parse()?
            }
            "--ci-timeout" => ci_timeout_secs = value("--ci-timeout")?.parse()?,
            "--runner-id" => runner_id_given = Some(value("--runner-id")?),
            "--service-unit" => service_unit = value("--service-unit")?,
            other => bail!("unknown work flag {other}"),
        }
    }
    let secrets = Arc::new(secrets::Secrets::load_and_scrub_env());
    let provider = provider_given.unwrap_or_else(env_provider_default);
    let instructions_path = instructions_path.context("--instructions <path> is required")?;
    // One absolute root drives both the pre-repair `runs.session_dir` write
    // (`work_once`) and the harness session dir (`RepairJob`).
    let sessions_dir = absolute_sessions_dir(sessions_dir)?;

    let store = Arc::new(
        shanhaijing::Store::open(&db_path)
            .with_context(|| format!("open db {}", db_path.display()))?,
    );
    let repo_dir = instructions_path
        .parent()
        .map(Path::to_path_buf)
        .context("instructions path has no parent")?;
    let base = resolve_base(&repo_dir)?;
    let mut trusted = instructions::TrustedInstructions::load(
        &repo_dir,
        &instructions_path,
        base,
        &sessions_dir.join("_trusted"),
    )?;
    let repo_config = trusted.instructions.clone();
    println!(
        "preflight   : instructions {:?} sha256 {}",
        trusted.source(),
        &trusted.sha256[..12.min(trusted.sha256.len())]
    );
    if trusted.verify_and_restore()? {
        eprintln!(
            "preflight   : restored {} from the trusted instruction source",
            trusted.path().display()
        );
    }

    let runner_id = runner_id_given
        .map(|id| sanitize_runner_id(&id))
        .unwrap_or_else(|| default_runner_id(&repo_config.repo));
    let (provider_name, model_name) = provider_meta(&provider);
    let config_json = effective_config_json(
        &repo_config,
        interval_secs,
        ci_timeout_secs,
        max_turns,
        max_tokens,
        daily_token_limit,
        monthly_token_limit,
    );
    let instance_id = uuid::Uuid::new_v4().to_string();
    // Compare checkouts by canonical path: `--repo a/../repo` and a symlinked
    // alias must not sneak two workers onto the same working tree.
    let repo_path = std::fs::canonicalize(&repo_dir)
        .or_else(|_| std::path::absolute(&repo_dir))
        .unwrap_or_else(|_| repo_dir.clone())
        .display()
        .to_string();
    let make_registration = |at: i64| shanhaijing::RunnerUpsert {
        id: runner_id.clone(),
        repo: repo_config.repo.clone(),
        host: std::env::var("HOSTNAME")
            .or_else(|_| std::env::var("COMPUTERNAME"))
            .unwrap_or_else(|_| "host".to_string()),
        pid: Some(std::process::id() as i64),
        started_at: at,
        heartbeat_at: at,
        status: "starting".into(),
        provider: Some(provider_name.clone()),
        model: Some(model_name.clone()),
        daily_pr_limit: Some(daily_pr_limit as i64),
        repo_path: Some(repo_path.clone()),
        service_unit: Some(service_unit.clone()),
        config_json: config_json.clone(),
    };
    // 同 id 双活不是无害的：每个进程启动时都会按 id 复活对方的在途单，两个
    // runner 会修同一张单、推同一分支。先等死进程的租约过期，仍被活进程持有
    // 就拒绝启动，而不是和它抢。
    let lease_wait = std::time::Duration::from_millis(RUNNER_LEASE_TTL_MS as u64 + 10_000);
    let lease_deadline = std::time::Instant::now() + lease_wait;
    let mut lease_wait_announced = false;
    loop {
        let at = now_ms();
        match store.try_acquire_runner_lease(
            &make_registration(at),
            &instance_id,
            at,
            RUNNER_LEASE_TTL_MS,
        ) {
            Ok(shanhaijing::RunnerLeaseOutcome::Acquired) => break,
            Ok(shanhaijing::RunnerLeaseOutcome::HeldByRunner)
                if std::time::Instant::now() < lease_deadline =>
            {
                if !lease_wait_announced {
                    eprintln!(
                        "lease       : runner id `{runner_id}` is held by another process; \
                         waiting up to {}s for its lease to expire",
                        lease_wait.as_secs()
                    );
                    lease_wait_announced = true;
                }
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            Ok(shanhaijing::RunnerLeaseOutcome::HeldByRunner) => bail!(
                "runner id `{runner_id}` is already active — stop the other process \
                 or pass a distinct --runner-id"
            ),
            Ok(shanhaijing::RunnerLeaseOutcome::CheckoutInUse(holder)) => bail!(
                "checkout `{repo_path}` is already used by live runner `{holder}` — \
                 give each worker its own clone"
            ),
            Err(e) => return Err(e).context("acquire runner lease"),
        }
    }
    #[cfg(unix)]
    spawn_sigterm_lease_release(Arc::clone(&store), runner_id.clone(), instance_id.clone());
    let now = now_ms();
    let _ = store.mark_interrupted(&runner_id, now);
    if let Err(error) = crate::serve::prune_retention(&store, now, event_retention_days) {
        eprintln!("warning    : retention prune failed: {error}");
    }
    let live = Arc::new(tokio::sync::Mutex::new(RunnerLive::new()));
    let heartbeat_interval = std::time::Duration::from_secs(interval_secs.clamp(5, 15));
    let heartbeat = spawn_heartbeat(
        Arc::clone(&store),
        runner_id.clone(),
        instance_id.clone(),
        heartbeat_interval,
        Arc::clone(&live),
        config_json,
    );
    live_update(&live, "starting", None, None, None).await;

    // 自愈：上一次进程崩溃/被杀留下的 `fixing` 单已无活着的 owner，claim 只领
    // `new`——不复活就永远卡死。启动时批量回 `new` 并清冷却，让本轮循环自然重领。
    match store.revive_orphaned_fixing(now, &runner_id) {
        Ok(0) => {}
        Ok(n) => println!("revive      : {n} orphaned fixing issue(s) requeued at startup"),
        Err(e) => eprintln!("warning : revive orphaned fixing failed: {e}"),
    }
    match cleanup_terminal_branches(&store, &repo_dir, push).await {
        Ok(0) => {}
        Ok(n) => println!("cleanup     : removed {n} terminal branch(es) at startup"),
        Err(e) => eprintln!("warning : terminal branch cleanup failed: {e:#}"),
    }
    match cleanup_sessions(&sessions_dir, &store, now, 365) {
        Ok(0) => {}
        Ok(n) => println!("cleanup     : removed {n} expired session dir(s) at startup"),
        Err(e) => eprintln!("warning : session cleanup failed: {e:#}"),
    }
    match reconcile_deliveries(
        &store,
        &repo_dir,
        &repo_config.repo,
        push,
        u64::from(repo_config.max_attempts.max(1)),
        &secrets,
    )
    .await
    {
        Ok(0) => {}
        Ok(n) => println!("delivery    : reconciled {n} unfinished delivery record(s)"),
        Err(e) => eprintln!("warning : delivery reconciliation failed: {e:#}"),
    }

    let mut cycles: u64 = 0;
    let result: anyhow::Result<()> = async {
        loop {
            // A delivery can become ambiguous after a crash mid-process, not
            // only at startup. Reconcile before claiming so the next cycle
            // adopts an already-created PR instead of opening a second one.
            if let Err(error) = reconcile_deliveries(
                &store,
                &repo_dir,
                &trusted.instructions.repo,
                push,
                u64::from(trusted.instructions.max_attempts.max(1)),
                &secrets,
            )
            .await
            {
                eprintln!("delivery    : reconciliation failed: {error:#}");
            }
            let worked = match work_once(
                &store,
                &mut trusted,
                &repo_dir,
                sessions_dir.clone(),
                provider.clone(),
                max_turns,
                push,
                create_pr,
                daily_pr_limit,
                max_tokens,
                daily_token_limit,
                monthly_token_limit,
                ci_timeout_secs,
                &runner_id,
                Arc::clone(&live),
                &secrets,
            )
            .await
            {
                Ok(worked) => worked,
                Err(e) => {
                    // A failed attempt is not a dead runner. Business outcomes
                    // (repair failed, budget exceeded, CI red) are values, not
                    // errors; what is left here is a real fault (db, checkout).
                    // Log it, keep polling, and let the next cycle retry.
                    let detail = bounded_text(&secrets.redact(&format!("work_failed: {e:#}")));
                    eprintln!("work       : {detail}");
                    live_update(&live, "error", None, None, Some(detail.clone())).await;
                    webhook::emit(webhook::WebhookEvent::new(
                        "runner_error",
                        None,
                        Some(trusted.instructions.repo.clone()),
                        detail,
                    ));
                    if !loop_run {
                        return Err(e);
                    }
                    false
                }
            };
            if worked {
                if let Ok(n) = cleanup_terminal_branches(&store, &repo_dir, push).await {
                    if n > 0 {
                        println!("cleanup     : removed {n} terminal branch(es)");
                    }
                }
            }
            cycles += 1;
            if cycles.is_multiple_of(100) {
                if let Ok(n) = cleanup_sessions(&sessions_dir, &store, now_ms(), 365) {
                    if n > 0 {
                        println!("cleanup     : removed {n} expired session dir(s)");
                    }
                }
            }
            if !loop_run {
                break;
            }
            if !worked {
                // Idle poll: back off. A worked issue may have a successor ready.
                tokio::time::sleep(std::time::Duration::from_secs(interval_secs)).await;
            }
        }
        Ok(())
    }
    .await;
    heartbeat.abort();
    if let Err(e) = store.release_runner_lease(&runner_id, &instance_id) {
        eprintln!("lease       : release failed: {e}");
    }
    result
}

#[derive(Debug)]
struct RunnerLive {
    status: String,
    current_fingerprint: Option<String>,
    phase: Option<String>,
    last_error: Option<String>,
    /// Real progress (phase/turn), not the independent liveness heartbeat.
    progress_at_ms: i64,
    /// Consecutive provider-side failures across runs.
    consecutive_provider_failures: u32,
    /// While this is in the future, the runner does not claim issues.
    stand_down_until_ms: i64,
}

impl RunnerLive {
    fn new() -> Self {
        Self {
            status: "starting".into(),
            current_fingerprint: None,
            phase: None,
            last_error: None,
            progress_at_ms: 0,
            consecutive_provider_failures: 0,
            stand_down_until_ms: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureClass {
    ProviderTransient,
    ProviderConfig,
    Environment,
    Issue,
}

fn classify_failure(error: &anyhow::Error) -> FailureClass {
    // Origin first: a failure that came from the model provider is tagged as
    // such by the repair runner. Classifying by message text alone let the
    // target repository's own output ("connection refused" from a test) be
    // read as a provider outage — the issue was then retried forever
    // without ever consuming an attempt.
    if let Some(provider) = error.downcast_ref::<repair::ProviderFailure>() {
        let text = provider.0.to_ascii_lowercase();
        if text.contains("401")
            || text.contains("403")
            || text.contains("unauthorized")
            || text.contains("invalid api key")
            || (text.contains("not set") && (text.contains("api_key") || text.contains("api key")))
        {
            return FailureClass::ProviderConfig;
        }
        return FailureClass::ProviderTransient;
    }
    let text = format!("{error:#}").to_ascii_lowercase();
    // Tool/verification failures belong to the issue, even when the text says
    // "timed out" (a hung test or a 300s bash timeout).
    if text.contains("command timed out")
        || text.contains("verification")
        || text.contains("reproduce_check")
        || text.contains("failing_check")
        || text.contains("run aborted")
        || text.contains("budget")
    {
        return FailureClass::Issue;
    }
    if text.contains("401")
        || text.contains("403")
        || text.contains("unauthorized")
        || (text.contains("not set") && (text.contains("api_key") || text.contains("api key")))
    {
        return FailureClass::ProviderConfig;
    }
    if text.contains("429")
        || text.contains("rate limit")
        || text.contains("provider")
        || text.contains("stream")
        || text.contains("connection")
        || text.contains("network")
        || text.contains("502")
        || text.contains("503")
        || text.contains("504")
        || text.contains("timed out")
        || text.contains("timeout")
    {
        return FailureClass::ProviderTransient;
    }
    if text.contains("dirty")
        || text.contains("working tree")
        || text.contains("toolchain")
        || text.contains("preflight")
    {
        return FailureClass::Environment;
    }
    FailureClass::Issue
}

async fn mark_provider_failure(live: &SharedLive, now: i64) -> bool {
    let mut state = live.lock().await;
    state.consecutive_provider_failures += 1;
    if state.consecutive_provider_failures >= 3 {
        state.stand_down_until_ms = now + 15 * 60_000;
        true
    } else {
        false
    }
}

async fn reset_provider_health(live: &SharedLive) {
    let mut state = live.lock().await;
    state.consecutive_provider_failures = 0;
    state.stand_down_until_ms = 0;
}

async fn mark_stand_down(live: &SharedLive, until_ms: i64) {
    let mut state = live.lock().await;
    state.stand_down_until_ms = until_ms;
}

async fn stand_down_remaining(live: &SharedLive, now: i64) -> Option<i64> {
    let state = live.lock().await;
    (state.stand_down_until_ms > now).then_some(state.stand_down_until_ms - now)
}

type SharedLive = Arc<tokio::sync::Mutex<RunnerLive>>;

fn sanitize_runner_id(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('-');
    let value = if trimmed.is_empty() {
        "runner"
    } else {
        trimmed
    };
    value.chars().take(128).collect()
}

fn default_runner_id(repo: &str) -> String {
    let host = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "host".to_string());
    sanitize_runner_id(&format!("{host}:{repo}"))
}

pub(crate) fn provider_meta(provider: &repair::ProviderKind) -> (String, String) {
    match provider {
        repair::ProviderKind::Faux => ("faux".into(), "faux".into()),
        repair::ProviderKind::Anthropic { model } => (
            "anthropic".into(),
            model.clone().unwrap_or_else(|| "<default>".into()),
        ),
        repair::ProviderKind::Zai { model, .. } => (
            "zai".into(),
            model.clone().unwrap_or_else(|| "glm-5.3-flash".into()),
        ),
    }
}

fn effective_config_json(
    repo_config: &instructions::RepoInstructions,
    interval_secs: u64,
    ci_timeout_secs: u64,
    max_turns: u32,
    max_tokens: u64,
    daily_token_limit: u64,
    monthly_token_limit: u64,
) -> String {
    serde_json::json!({
        "interval_secs": interval_secs,
        "ci_timeout_secs": ci_timeout_secs,
        "max_turns": max_turns,
        "max_tokens": max_tokens,
        "daily_token_limit": daily_token_limit,
        "monthly_token_limit": monthly_token_limit,
        "failing_check": repo_config.failing_check.command,
        "reproduce_check": repo_config.reproduce_check.as_ref().map(|c| c.command.clone()),
        "forbidden_paths_count": repo_config.forbidden_paths.len(),
        "max_attempts": repo_config.max_attempts,
        "draft_first_n": repo_config.draft_first_n,
        "skills": repo_config.skills,
    })
    .to_string()
}

async fn live_snapshot(
    live: &SharedLive,
) -> (String, Option<String>, Option<String>, Option<String>, i64) {
    let state = live.lock().await;
    (
        state.status.clone(),
        state.current_fingerprint.clone(),
        state.phase.clone(),
        state.last_error.clone(),
        state.progress_at_ms,
    )
}

/// Persist a run update off the async worker. Best-effort: a finished run
/// rejects late writes, and that error is intentionally dropped.
async fn persist_run(
    store: &Arc<shanhaijing::Store>,
    run_id: &str,
    update: shanhaijing::RunUpdate,
) {
    let store = Arc::clone(store);
    let run_id = run_id.to_string();
    let _ = tokio::task::spawn_blocking(move || {
        let _ = store.update_run(&run_id, &update);
    })
    .await;
}

/// Bound telemetry error text (~16 KiB) so a runaway command output cannot
/// bloat the runs table. Truncates on a UTF-8 boundary.
fn bounded_text(text: &str) -> String {
    const LIMIT: usize = 16_000;
    if text.len() <= LIMIT {
        return text.to_string();
    }
    let mut end = LIMIT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = text[..end].to_string();
    out.push_str("\n… (truncated)");
    out
}

/// Update the in-memory runner state. Memory-only by design: telemetry must
/// not block the repair/delivery hot path, so SQLite persistence is owned by
/// the background heartbeat task and the explicit run writes.
fn emit_webhook(event: &str, fingerprint: &str, repo: &str, detail: impl Into<String>) {
    webhook::emit(webhook::WebhookEvent::new(
        event,
        Some(fingerprint.to_string()),
        Some(repo.to_string()),
        detail,
    ));
}

async fn live_update(
    live: &SharedLive,
    status: &str,
    fp: Option<&str>,
    phase: Option<&str>,
    error: Option<String>,
) {
    let mut state = live.lock().await;
    state.status = status.to_string();
    state.current_fingerprint = fp.map(str::to_string);
    state.phase = phase.map(str::to_string);
    state.last_error = error;
    state.progress_at_ms = now_ms();
}

/// Same-id runner lease lifetime. The heartbeat renews it every 5–15s; a
/// crashed process stops renewing and its id becomes claimable after this long.
const RUNNER_LEASE_TTL_MS: i64 = 60_000;

/// systemd stops the unit with SIGTERM. Release the lease before exiting so a
/// restart does not wait out the full TTL; abrupt crashes still fall back to it.
#[cfg(unix)]
fn spawn_sigterm_lease_release(
    store: Arc<shanhaijing::Store>,
    runner_id: String,
    instance_id: String,
) {
    tokio::spawn(async move {
        let Ok(mut term) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        else {
            return;
        };
        term.recv().await;
        let _ = store.release_runner_lease(&runner_id, &instance_id);
        std::process::exit(0);
    });
}

fn spawn_heartbeat(
    store: Arc<shanhaijing::Store>,
    runner_id: String,
    instance_id: String,
    interval: std::time::Duration,
    live: SharedLive,
    config_json: String,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut logged_failure = false;
        loop {
            let (status, fp, phase, error, progress_at) = live_snapshot(&live).await;
            let store = Arc::clone(&store);
            let runner_id = runner_id.clone();
            let instance_id = instance_id.clone();
            let config_json = config_json.clone();
            let result = tokio::task::spawn_blocking(move || {
                let result = store.heartbeat_runner(
                    &runner_id,
                    &shanhaijing::RunnerHeartbeat {
                        heartbeat_at: now_ms(),
                        status,
                        current_fingerprint: fp,
                        phase,
                        config_json: Some(config_json),
                        last_error: error,
                    },
                );
                if result.is_ok() {
                    let _ = store.set_runner_progress(&runner_id, progress_at);
                    // Losing the lease means another process took over this id.
                    // Exit cleanly (not on-failure) so systemd does not start a
                    // third contender: the new holder is the one that should run.
                    match store.renew_runner_lease(
                        &runner_id,
                        &instance_id,
                        now_ms(),
                        RUNNER_LEASE_TTL_MS,
                    ) {
                        Ok(true) => {}
                        Ok(false) => {
                            eprintln!(
                                "lease       : runner id `{runner_id}` was taken over by \
                                 another process — stopping this runner"
                            );
                            std::process::exit(0);
                        }
                        Err(e) => eprintln!("lease renew failed: {e}"),
                    }
                }
                // v0.2.x 回滚保护：旧控制台仍读全局 heartbeat 表。
                let _ = store.heartbeat(now_ms());
                result
            })
            .await;
            if !logged_failure {
                match result {
                    Ok(Err(e)) => {
                        eprintln!("heartbeat: {e}");
                        logged_failure = true;
                    }
                    Err(e) => {
                        eprintln!("heartbeat task: {e}");
                        logged_failure = true;
                    }
                    Ok(Ok(())) => {}
                }
            }
            tokio::time::sleep(interval).await;
        }
    })
}

fn record_delivery_recovery_failure(
    store: &shanhaijing::Store,
    fingerprint: &str,
    max_attempts: u64,
    now: i64,
) -> shanhaijing::ShanResult<shanhaijing::FailureOutcome> {
    store.record_issue_failure(
        fingerprint,
        now,
        max_attempts,
        now + 30 * 60_000,
        "delivery recovery: no PR found",
        "runner",
    )
}

/// Recover deliveries whose process died after GitHub created the PR but
/// before the local issue/run was updated. A definitive "no PR" allows the
/// orphan branch to be removed; an inconclusive lookup keeps it for a later
/// attempt rather than destroying evidence.
async fn reconcile_deliveries(
    store: &shanhaijing::Store,
    repo_dir: &Path,
    repo_id: &str,
    push: bool,
    max_attempts: u64,
    secrets: &secrets::Secrets,
) -> anyhow::Result<usize> {
    let pending = store.pending_deliveries(repo_id)?;
    if pending.is_empty() {
        return Ok(0);
    }
    let token = secrets
        .github_token
        .as_deref()
        .context("cannot reconcile deliveries without NUWA_GITHUB_TOKEN")?;
    let origin = git_ops::git(repo_dir, &["remote", "get-url", "origin"])?;
    let slug = deliver::parse_github_slug(&origin)
        .with_context(|| format!("parse GitHub owner/repo from origin `{origin}`"))?;
    let now = now_ms();
    let mut reconciled = 0usize;
    for delivery in pending {
        match deliver::find_pr_by_head(
            token,
            &slug,
            &delivery.branch,
            &delivery.head_sha,
            &delivery.base,
        )
        .await
        {
            Ok(Some(recovered)) if recovered.matches(&delivery.head_sha, &delivery.base) => {
                let adopted =
                    store.adopt_delivered_pr(&delivery.fingerprint, &recovered.url, now)?;
                store.mark_delivery(
                    &delivery.id,
                    shanhaijing::DeliveryState::PrCreated,
                    Some(&recovered.url),
                    Some(recovered.number),
                    Some("recovered the exact verified PR head/base"),
                    now,
                )?;
                if adopted {
                    let run = delivery
                        .run_id
                        .as_deref()
                        .and_then(|id| store.run_for(&delivery.fingerprint, id).ok().flatten());
                    let _ = store.upsert_ci_watch(&shanhaijing::CiWatchInput {
                        run_id: delivery.run_id.clone(),
                        fingerprint: delivery.fingerprint.clone(),
                        repo: delivery.repo.clone(),
                        pr_url: recovered.url,
                        slug: slug.clone(),
                        pr_number: recovered.number,
                        head_sha: delivery.head_sha.clone(),
                        root_cause: run.as_ref().and_then(|run| run.root_cause.clone()),
                        change_stat: run.as_ref().and_then(|run| run.change_stat.clone()),
                        now,
                        next_check_at: now,
                        max_attempts,
                    });
                }
                reconciled += 1;
            }
            Ok(Some(recovered)) => {
                store.mark_delivery(
                    &delivery.id,
                    shanhaijing::DeliveryState::Ambiguous,
                    Some(&recovered.url),
                    Some(recovered.number),
                    Some("recovered PR head/base differs from the verified delivery"),
                    now,
                )?;
                eprintln!(
                    "delivery    : {} recovery left ambiguous: PR #{} head/base {}/{} != verified {}/{}",
                    delivery.fingerprint,
                    recovered.number,
                    recovered.head_sha,
                    recovered.base_ref,
                    delivery.head_sha,
                    delivery.base
                );
            }
            Ok(None) => {
                if push {
                    let _ = git_ops::delete_remote_branch(
                        repo_dir,
                        &delivery.branch,
                        Duration::from_secs(60),
                    )
                    .await;
                }
                store.mark_delivery(
                    &delivery.id,
                    shanhaijing::DeliveryState::Failed,
                    None,
                    None,
                    Some("recovery found no PR; branch removed"),
                    now,
                )?;
                let state = store
                    .list_issues()?
                    .into_iter()
                    .find(|row| row.fingerprint == delivery.fingerprint)
                    .map(|row| row.state);
                if matches!(state.as_deref(), Some("triggered") | Some("fixing")) {
                    let _ = record_delivery_recovery_failure(
                        store,
                        &delivery.fingerprint,
                        max_attempts,
                        now,
                    );
                }
                reconciled += 1;
            }
            Err(error) => {
                store.mark_delivery(
                    &delivery.id,
                    shanhaijing::DeliveryState::Ambiguous,
                    None,
                    None,
                    Some(&format!("head lookup failed: {error:#}")),
                    now,
                )?;
                eprintln!(
                    "delivery    : {} reconciliation inconclusive: {error:#}",
                    delivery.fingerprint
                );
            }
        }
    }
    Ok(reconciled)
}

/// Resolve the base branch from local refs. The runner only supports the
/// repository's `main`/`master` checkout (docs/08).
fn resolve_base(repo_dir: &Path) -> anyhow::Result<&'static str> {
    if git_ops::branch_exists(repo_dir, "main")? {
        Ok("main")
    } else if git_ops::branch_exists(repo_dir, "master")? {
        Ok("master")
    } else {
        bail!(
            "repo {} has neither a `main` nor a `master` branch",
            repo_dir.display()
        )
    }
}

/// Refresh the runner's base and recover a checkout left behind by an
/// interrupted delivery. Fetch/reset failures are environment faults: the
/// caller stands down and retries on the next poll instead of crashing.
async fn preflight_repo(repo_dir: &Path, fetch: bool) -> anyhow::Result<&'static str> {
    let base = resolve_base(repo_dir)?;
    let head = git_ops::current_branch(repo_dir)?;
    if head != base {
        if head.starts_with("agent/fix/") {
            // The runner owns this checkout. The interrupted branch keeps its
            // commit for forensics; only the working tree is reset.
            git_ops::git(repo_dir, &["checkout", "-f", base])?;
            eprintln!("preflight   : recovered checkout from {head} to {base}");
        } else {
            bail!(
                "repo {} is on branch `{}` — runner only works from {base}; reset the checkout",
                repo_dir.display(),
                head
            );
        }
    }

    let has_origin = git_ops::git(repo_dir, &["remote", "get-url", "origin"]).is_ok();
    if has_origin {
        if fetch {
            git_ops::fetch_base(repo_dir, base, Duration::from_secs(120)).await?;
        }
        git_ops::reset_to_base(repo_dir, base)?;
    }

    let dirty = git_ops::git(
        repo_dir,
        &["status", "--porcelain", "--", ":(exclude)nuwa.repo.json"],
    )?;
    if !dirty.trim().is_empty() {
        eprintln!(
            "preflight   : unclean working tree — auto-reset to {base}:\n{}",
            dirty.trim()
        );
        if has_origin {
            git_ops::reset_to_base(repo_dir, base)?;
        } else {
            git_ops::git(repo_dir, &["reset", "--hard", "HEAD"])?;
        }
        let _ = git_ops::git(
            repo_dir,
            &["clean", "-fd", "--", ":(exclude)nuwa.repo.json"],
        );
        let still = git_ops::git(
            repo_dir,
            &["status", "--porcelain", "--", ":(exclude)nuwa.repo.json"],
        )?;
        if !still.trim().is_empty() {
            bail!(
                "repo {} working tree still unclean after auto-reset — manual intervention required:\n{}",
                repo_dir.display(),
                still.trim()
            );
        }
    }
    Ok(base)
}

/// Remove per-fingerprint session directories whose latest run is older than
/// the retention window. `_trusted` is runner configuration, never a session.
fn cleanup_sessions(
    root: &Path,
    store: &shanhaijing::Store,
    now: i64,
    retention_days: u32,
) -> anyhow::Result<usize> {
    let cutoff = now - i64::from(retention_days) * 86_400_000;
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error).with_context(|| format!("read {}", root.display())),
    };
    let issues = store.list_issues()?;
    let mut removed = 0usize;
    for entry in entries {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if !file_type.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('_') {
            continue;
        }
        let latest = issues
            .iter()
            .find(|issue| issue::short_fingerprint(&issue.fingerprint) == name)
            .map(|issue| {
                store
                    .latest_run(&issue.fingerprint)
                    .ok()
                    .flatten()
                    .map(|run| run.started_at)
                    .unwrap_or(issue.last_seen)
            });
        let keep = match latest {
            Some(latest) => latest >= cutoff,
            None => entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_millis() as i64 >= cutoff)
                .unwrap_or(true),
        };
        if !keep {
            std::fs::remove_dir_all(entry.path())
                .with_context(|| format!("remove {}", entry.path().display()))?;
            removed += 1;
        }
    }
    Ok(removed)
}

/// Remove `agent/fix/*` branches whose issue is already terminal. This keeps
/// the deterministic branch namespace reusable after merge/reject/escalation
/// and stops remote branches from accumulating forever.
async fn cleanup_terminal_branches(
    store: &shanhaijing::Store,
    repo_dir: &Path,
    push: bool,
) -> anyhow::Result<usize> {
    let listing = git_ops::git(
        repo_dir,
        &[
            "for-each-ref",
            "--format=%(refname:short)",
            "refs/heads/agent/fix/",
        ],
    )?;
    // The remote branches count too. A delivery that pushed but failed to
    // open its PR leaves a branch on GitHub with no local copy — invisible
    // to `for-each-ref`, and it would block the next push for that
    // fingerprint as a non-fast-forward.
    let mut branches: Vec<String> = listing
        .lines()
        .map(str::trim)
        .filter(|b| !b.is_empty())
        .map(str::to_string)
        .collect();
    if push {
        match git_ops::git_with_timeout(
            repo_dir,
            &["ls-remote", "--heads", "origin", "agent/fix/*"],
            Duration::from_secs(60),
        )
        .await
        {
            Ok(out) if out.success => {
                for line in out.stdout.lines() {
                    let Some((_, refname)) = line.split_once('\t') else {
                        continue;
                    };
                    let Some(short) = refname.strip_prefix("refs/heads/") else {
                        continue;
                    };
                    if short.is_empty() || branches.contains(&short.to_string()) {
                        continue;
                    }
                    branches.push(short.to_string());
                }
            }
            Ok(out) if out.timed_out => {
                eprintln!("cleanup     : remote branch listing timed out");
            }
            Ok(out) => eprintln!(
                "cleanup     : remote branch listing failed: {}",
                out.stderr.trim()
            ),
            Err(e) => eprintln!("cleanup     : remote branch listing failed: {e:#}"),
        }
    }
    if branches.is_empty() {
        return Ok(0);
    }
    let issues = store.list_issues()?;
    let mut removed = 0usize;
    for branch in branches {
        let Some(short) = branch.strip_prefix("agent/fix/") else {
            continue;
        };
        let Some(row) = issues
            .iter()
            .find(|row| issue::short_fingerprint(&row.fingerprint) == short)
        else {
            continue;
        };
        let terminal = matches!(
            row.state.as_str(),
            "merged" | "rejected" | "escalated" | "ignored"
        );
        // An escalated issue can still have an open PR after a manual state
        // change; never delete that branch out from under the owner.
        if !terminal || (row.state == "escalated" && row.pr_url.is_some()) {
            continue;
        }
        if git_ops::branch_exists(repo_dir, &branch)? {
            git_ops::delete_local_branch(repo_dir, &branch)?;
        }
        if push {
            // Best effort: a stale remote branch is not a reason to fail the
            // startup path; the next claim precheck retries it.
            if let Err(e) =
                git_ops::delete_remote_branch(repo_dir, &branch, Duration::from_secs(120)).await
            {
                eprintln!("cleanup     : could not delete remote {branch}: {e:#}");
            }
        }
        removed += 1;
    }
    Ok(removed)
}

/// Delete the deterministic branch left by a previous delivery for this
/// fingerprint. Remote deletion only matters when the runner pushes.
async fn remove_stale_branch(
    repo_dir: &Path,
    fingerprint: &str,
    push: bool,
) -> anyhow::Result<bool> {
    let branch = format!("agent/fix/{}", issue::short_fingerprint(fingerprint));
    let mut removed = false;
    if git_ops::branch_exists(repo_dir, &branch)? {
        git_ops::delete_local_branch(repo_dir, &branch)?;
        removed = true;
    }
    if push {
        // The remote copy can outlive the local one: a delivery that pushed
        // but failed to open its PR leaves the branch on GitHub. It would
        // block the next push for this fingerprint as a non-fast-forward.
        if git_ops::remote_branch_exists(repo_dir, &branch, Duration::from_secs(60)).await? {
            git_ops::delete_remote_branch(repo_dir, &branch, Duration::from_secs(120)).await?;
            removed = true;
        }
    }
    Ok(removed)
}

fn context_excerpt(text: &str, max_chars: usize) -> String {
    let first = text.lines().next().unwrap_or("").trim();
    first.chars().take(max_chars).collect()
}

/// Build the retry context injected into the next repair prompt: the last
/// owner decision, the last three run outcomes/root causes, and verified
/// fixes from other issues in the same repo (with provenance).
fn build_prior_context(
    store: &shanhaijing::Store,
    repo: &str,
    fingerprint: &str,
    secrets: &secrets::Secrets,
) -> String {
    let mut out = String::new();
    if let Ok(Some(event)) = store.latest_owner_event(fingerprint) {
        let detail = secrets.redact(&event.detail);
        if !detail.trim().is_empty() {
            out.push_str("OWNER NOTE:\n- ");
            out.push_str(detail.trim());
            out.push('\n');
        }
    }
    if let Ok(runs) = store.recent_runs(fingerprint, 3) {
        let mut header = false;
        for run in runs {
            if !header {
                out.push_str("\nPRIOR ATTEMPTS:\n");
                header = true;
            }
            let outcome = run.outcome.as_deref().unwrap_or("open");
            let ci = run.ci_verdict.as_deref().unwrap_or("-");
            let cause = secrets.redact(run.root_cause.as_deref().unwrap_or(""));
            let error = secrets.redact(run.error.as_deref().unwrap_or(""));
            out.push_str(&format!(
                "- started={} outcome={} ci={} root_cause={} error={}\n",
                run.started_at,
                outcome,
                ci,
                context_excerpt(&cause, 240),
                context_excerpt(&error, 240),
            ));
        }
    }
    if let Ok(memories) = store.recent_repair_memory(repo, fingerprint, 3) {
        if !memories.is_empty() {
            out.push_str(
                "\nVERIFIED FIXES IN THIS REPO (other issues; reference only, re-verify against current code):\n",
            );
        }
        for memory in memories {
            let cause = secrets.redact(&memory.root_cause);
            // Provenance first: a memory without a commit/PR cannot be checked.
            let source = memory.pr_url.clone().unwrap_or_else(|| {
                format!(
                    "commit {}",
                    &memory.commit_sha[..12.min(memory.commit_sha.len())]
                )
            });
            out.push_str(&format!(
                "- {} — root_cause={} change={}\n",
                source,
                context_excerpt(&cause, 200),
                context_excerpt(memory.change_stat.trim(), 120),
            ));
        }
    }
    out.chars().take(4_096).collect()
}

/// 只有通过独立验证门（以及 CI 绿灯或明确的 branch-only 成功）的修复才进入
/// 记忆；来源随内容一起保存。
fn remember_verified_fix(
    store: &shanhaijing::Store,
    repo: &str,
    fingerprint: &str,
    delivered: &deliver::DeliverReport,
    root_cause: &str,
) {
    let _ = store.record_repair_memory(&shanhaijing::RepairMemoryEntry {
        repo: repo.to_string(),
        fingerprint: fingerprint.to_string(),
        commit_sha: delivered.commit.clone(),
        pr_url: delivered.pr_url.clone(),
        root_cause: root_cause.to_string(),
        change_stat: delivered.change_stat.clone(),
        files: delivered.risk.files.len() as i64,
        recorded_at: now_ms(),
    });
}

/// One pickup attempt. `Ok(true)` = an issue was claimed and driven to a
/// terminal state this pass; `Ok(false)` = nothing pickable.
#[allow(clippy::too_many_arguments)]
async fn work_once(
    store: &Arc<shanhaijing::Store>,
    trusted: &mut instructions::TrustedInstructions,
    repo_dir: &Path,
    sessions_dir: PathBuf,
    provider: repair::ProviderKind,
    max_turns: u32,
    push: bool,
    create_pr: bool,
    daily_pr_limit: u64,
    max_tokens: u64,
    daily_token_limit: u64,
    monthly_token_limit: u64,
    ci_timeout_secs: u64,
    runner_id: &str,
    live: SharedLive,
    secrets: &Arc<secrets::Secrets>,
) -> anyhow::Result<bool> {
    let now = now_ms();
    let repo_config = &trusted.instructions;
    // 活性必须在任何早退之前可见：配额待定/预检失败都不是"进程死了"。
    store.heartbeat(now)?;
    if let Some(remaining) = stand_down_remaining(&live, now).await {
        let detail = format!("provider/environment stand-down for {}s", remaining / 1_000);
        live_update(&live, "standing_down", None, None, Some(detail)).await;
        return Ok(false);
    }

    // ---- frequency gate: stand down when today's PR quota is spent ----
    // Checked BEFORE claiming so the issue stays `new` and is picked up
    // after the UTC window rolls over — no escalation noise for a full quota.
    if create_pr {
        let day_start = now - now.rem_euclid(86_400_000);
        let opened = store.prs_opened_since(&repo_config.repo, day_start)?;
        if opened >= daily_pr_limit {
            println!(
                "quota      : {opened}/{daily_pr_limit} PRs today for {} — standing down until the window rolls",
                repo_config.repo
            );
            live_update(&live, "standing_down", None, None, None).await;
            return Ok(false);
        }
    }

    // ---- spend caps: failures consume tokens too ----
    let day_start = now - now.rem_euclid(86_400_000);
    let daily_spent = store.tokens_used_since(Some(&repo_config.repo), day_start)?;
    if daily_spent >= daily_token_limit {
        let detail = format!(
            "daily token cap reached: {daily_spent}/{daily_token_limit} for {}",
            repo_config.repo
        );
        println!("quota      : {detail} — standing down");
        live_update(&live, "standing_down", None, None, Some(detail)).await;
        return Ok(false);
    }
    let month_start = now - 30 * 86_400_000;
    let monthly_spent = store.tokens_used_since(Some(&repo_config.repo), month_start)?;
    if monthly_spent >= monthly_token_limit {
        let detail = format!(
            "monthly token cap reached: {monthly_spent}/{monthly_token_limit} for {}",
            repo_config.repo
        );
        println!("quota      : {detail} — standing down");
        live_update(&live, "standing_down", None, None, Some(detail)).await;
        return Ok(false);
    }

    // ---- repo preflight: refresh the base and recover a half-delivered checkout ----
    // A previous delivery may have failed after `checkout -b` and left the
    // runner on the agent branch. The runner owns this checkout, so a clean
    // `agent/fix/*` branch is recovered automatically instead of crash-looping.
    //
    // The remote base is only fetched when this repo actually has work
    // waiting: an idle runner polls every 30–60s, and a fetch per poll was a
    // pointless round trip to GitHub.
    let pickable = store.count_ready_for_repo(now, &repo_config.repo)? > 0;
    let base = match preflight_repo(repo_dir, pickable).await {
        Ok(base) => base,
        Err(e) => {
            live_update(&live, "error", None, None, Some(format!("{e:#}"))).await;
            eprintln!("preflight   : {e:#}");
            // Environment fault, not an issue fault: stand down and retry next
            // poll instead of exiting into a systemd restart loop.
            return Ok(false);
        }
    };
    println!("preflight   : base {base}");
    let refresh = trusted.refresh()?;
    let repo_config = &trusted.instructions;
    if refresh.config_changed {
        eprintln!("preflight   : trusted instructions changed on base — reloaded");
    }
    if refresh.restored && !refresh.config_changed {
        store.record_event(
            "__system__",
            now_ms(),
            Some(&repo_config.repo),
            "runner",
            "config_tampered",
            "nuwa.repo.json restored from trusted source",
        )?;
    }

    // ---- claim (atomic: N runners can never share an issue) ----
    let Some(snapshot) = store.claim_ready_issue_for_repo(
        now,
        now + shanhaijing::DEFAULT_COOLDOWN_MS,
        &repo_config.repo,
        runner_id,
    )?
    else {
        println!("nothing ready for pickup");
        live_update(&live, "idle", None, None, None).await;
        return Ok(false);
    };
    let run_id = uuid::Uuid::new_v4().to_string();
    let (provider_name, model_name) = provider_meta(&provider);
    if snapshot.repo != repo_config.repo {
        // 毒单隔离：异 repo 的单若 bail 会让 runner crash-loop 在同一张单上
        // （claim 只看 new/cooldown，永远先领到它）。escalated 交给所有者
        // 处置，runner 继续下一轮（ADR-004 边界 + 2026-10-19 部署实战）。
        let message = format!(
            "repo mismatch: issue `{}` belongs to `{}`, instructions are for `{}` — escalated (ADR-004)",
            snapshot.fingerprint,
            snapshot.repo,
            repo_config.repo
        );
        let _ = store.set_state_as(
            &snapshot.fingerprint,
            shanhaijing::State::Escalated,
            now,
            Some(&message),
            "runner",
        );
        println!(
            "mismatch   : {} escalated (belongs to `{}`)",
            snapshot.fingerprint, snapshot.repo
        );
        emit_webhook(
            "escalated",
            &snapshot.fingerprint,
            &repo_config.repo,
            "repo mismatch: issue escalated (ADR-004)",
        );
        trace::record(
            store,
            &snapshot.fingerprint,
            &repo_config.repo,
            trace::Decision {
                stage: "claim",
                decision: "escalated",
                reason: "issue repo does not match this runner's instructions",
                details: serde_json::json!({
                    "issue_repo": snapshot.repo,
                    "runner_repo": repo_config.repo,
                }),
            },
        );
        live_update(&live, "idle", None, None, Some(message)).await;
        return Ok(true);
    }
    let fp = snapshot.fingerprint.clone();

    // ---- attempt cap: stop an automatic retry loop before spending a run ----
    let max_attempts = u64::from(repo_config.max_attempts.max(1));
    if snapshot.attempts >= max_attempts {
        let detail = format!(
            "attempt cap reached ({} >= {})",
            snapshot.attempts, max_attempts
        );
        let _ = store.ignore_issue(&fp, &detail, "runner", now);
        trace::record(
            store,
            &fp,
            &repo_config.repo,
            trace::Decision {
                stage: "claim",
                decision: "ignored",
                reason: "attempt cap reached",
                details: serde_json::json!({
                    "attempts": snapshot.attempts,
                    "max_attempts": max_attempts,
                }),
            },
        );
        println!("ignored    : {fp} — {detail}");
        emit_webhook("ignored", &fp, &repo_config.repo, detail.clone());
        live_update(&live, "idle", None, None, Some(detail)).await;
        return Ok(true);
    }

    // ---- stale branch precheck (before any LLM spend) ----
    // A deterministic branch survives a merged/rejected delivery. If it is
    // still present when the same fingerprint comes back, the old code only
    // discovered that in `deliver` after burning a full repair run.
    let stale_branch = format!("agent/fix/{}", issue::short_fingerprint(&fp));
    // The remote branch counts too: a delivery that pushed but failed to open
    // its PR leaves the branch on GitHub while the local copy is gone. The
    // next push would be rejected as non-fast-forward, and nothing would
    // ever clean it up.
    let local_stale = git_ops::branch_exists(repo_dir, &stale_branch)?;
    let remote_stale = push
        && git_ops::remote_branch_exists(repo_dir, &stale_branch, Duration::from_secs(60))
            .await
            .unwrap_or(false);
    if local_stale || remote_stale {
        let recorded_pr = store
            .list_issues()?
            .into_iter()
            .find(|row| row.fingerprint == fp)
            .and_then(|row| row.pr_url);
        if recorded_pr.is_some() {
            let message = format!(
                "branch {stale_branch} exists and the issue has a recorded PR — refusing to delete it; manual review required"
            );
            let _ = store.set_state_as(
                &fp,
                shanhaijing::State::Escalated,
                now,
                Some(&message),
                "runner",
            );
            trace::record(
                store,
                &fp,
                &repo_config.repo,
                trace::Decision {
                    stage: "preflight",
                    decision: "escalated",
                    reason: "stale branch has a recorded PR; refusing to delete it",
                    details: serde_json::json!({"branch": stale_branch, "pr_url": recorded_pr}),
                },
            );
            println!("branch     : {message}");
            emit_webhook("escalated", &fp, &repo_config.repo, message.clone());
            live_update(&live, "idle", None, None, Some(message)).await;
            return Ok(true);
        }
        if let Err(e) = remove_stale_branch(repo_dir, &fp, push).await {
            let message = format!(
                "stale branch {stale_branch} could not be removed: {e:#} — escalated before starting a repair"
            );
            let _ = store.set_state_as(
                &fp,
                shanhaijing::State::Escalated,
                now,
                Some(&message),
                "runner",
            );
            trace::record(
                store,
                &fp,
                &repo_config.repo,
                trace::Decision {
                    stage: "preflight",
                    decision: "escalated",
                    reason: "stale branch removal failed",
                    details: serde_json::json!({"branch": stale_branch, "error": secrets.redact(&format!("{e:#}"))}),
                },
            );
            println!("branch     : {message}");
            emit_webhook("escalated", &fp, &repo_config.repo, message.clone());
            live_update(&live, "idle", None, None, Some(message)).await;
            return Ok(true);
        }
        println!("branch     : removed stale {stale_branch} before repair");
    }

    let prior_context = build_prior_context(store, &repo_config.repo, &fp, secrets);
    // 问题单在 `start_run` 之前构建：确定性会话目录要在第一次生命周期写入时
    // 就已知。
    let mut prob = pipeline::issue_from_snapshot(&snapshot, repo_config, prior_context, secrets);
    // What will the delivery gate accept? Decided before the agent starts:
    // when `failing_check` is already green on the base (the common
    // production case — tests pass, the panic happens at runtime) the only
    // way through the gate is a reproducer, and the agent has to be told.
    // A repo that configures `reproduce_check` skips the probe entirely.
    prob.verification_mode = if repo_config.reproduce_check.is_some() {
        issue::VerificationMode::Configured
    } else {
        match verify::probe_working_tree(repo_dir, &repo_config.failing_check, secrets).await {
            Ok(out) if out.ok => issue::VerificationMode::Reproducer,
            Ok(_) => issue::VerificationMode::FailingCheck,
            Err(e) => {
                // The probe is advisory; the delivery gate re-runs it. A
                // failed probe means "assume the check reproduces".
                eprintln!("probe      : base check could not run: {e:#}");
                issue::VerificationMode::FailingCheck
            }
        }
    };
    println!("verify     : mode {}", prob.verification_mode.as_str());
    let _ = store.start_run(&shanhaijing::RunStart {
        id: run_id.clone(),
        fingerprint: fp.clone(),
        runner_id: runner_id.to_string(),
        repo: snapshot.repo.clone(),
        started_at: now,
        provider: Some(provider_name.clone()),
        model: Some(model_name.clone()),
    });
    live_update(&live, "working", Some(&fp), Some("claim"), None).await;
    let provider_label = pipeline::provider_label(&provider);
    println!(
        "claimed    : {} [{}] {} — {}x\nrepo       : {}\nprovider   : {}",
        fp,
        snapshot.severity,
        snapshot.title,
        snapshot.occurrences,
        repo_dir.display(),
        provider_label,
    );
    if let Err(e) = store.set_state_as(
        &fp,
        shanhaijing::State::Fixing,
        now,
        Some("repair run started"),
        "runner",
    ) {
        let detail = bounded_text(&format!("repair_failed: state update: {e:#}"));
        let _ = store.finish_run(
            &run_id,
            &shanhaijing::RunFinish {
                finished_at: now_ms(),
                outcome: "repair_failed".into(),
                ci_verdict: None,
                error: Some(detail.clone()),
            },
        );
        live_update(&live, "error", Some(&fp), None, Some(detail)).await;
        return Err(e.into());
    }

    // ---- repair ----
    let progress_store = Arc::clone(store);
    let progress_run = run_id.clone();
    let progress_live = Arc::clone(&live);
    let final_usage = Arc::new(std::sync::Mutex::new(repair::RepairProgress {
        turns: 0,
        uncached_tokens: 0,
        cached_tokens: 0,
    }));
    let progress_usage = Arc::clone(&final_usage);
    let progress: Option<pipeline::ProgressSink> = Some(Arc::new(move |p| {
        *progress_usage.lock().unwrap_or_else(|e| e.into_inner()) = p;
        let store = Arc::clone(&progress_store);
        let run_id = progress_run.clone();
        // Every completed turn is real progress: refresh the watchdog
        // timestamp even though the independent heartbeat keeps ticking.
        let live = Arc::clone(&progress_live);
        tokio::spawn(async move {
            live.lock().await.progress_at_ms = now_ms();
        });
        // Fire-and-forget: progress is best-effort telemetry and must not
        // sit in the event drain that drives the repair loop.
        tokio::spawn(async move {
            let update = shanhaijing::RunUpdate {
                turns: Some(i64::from(p.turns)),
                tokens_uncached: Some(p.uncached_tokens as i64),
                tokens_cached: Some(p.cached_tokens as i64),
                ..Default::default()
            };
            let _ = tokio::task::spawn_blocking(move || {
                let _ = store.update_run(&run_id, &update);
            })
            .await;
        });
    }));
    let prepared = pipeline::prepare_repair(pipeline::RepairRequest {
        repo: repo_dir.to_path_buf(),
        issue: prob,
        sessions_dir: sessions_dir.clone(),
        provider,
        max_turns,
        repo_config: Some(repo_config.clone()),
        secrets: Arc::clone(secrets),
        progress,
        faux_script: None,
        run_id: Some(run_id.clone()),
    })?;
    let session_dir = prepared.session_dir().to_path_buf();
    trace::record(
        store,
        &fp,
        &repo_config.repo,
        trace::Decision {
            stage: "repair",
            decision: "started",
            reason: "issue claimed; repository preflight passed",
            details: serde_json::json!({
                "provider": provider_label,
                "max_turns": max_turns,
                "skills": prepared.skills_used(),
                "session_dir": session_dir.display().to_string(),
            }),
        },
    );

    live_update(&live, "working", Some(&fp), Some("repair"), None).await;
    persist_run(
        store,
        &run_id,
        shanhaijing::RunUpdate {
            phase: Some("repair".into()),
            session_dir: Some(session_dir.display().to_string()),
            ..Default::default()
        },
    )
    .await;
    let repair_result = prepared.execute().await;
    let usage = *final_usage.lock().unwrap_or_else(|e| e.into_inner());
    let usage_store = Arc::clone(store);
    let usage_run = run_id.clone();
    tokio::task::spawn_blocking(move || {
        usage_store.update_run(
            &usage_run,
            &shanhaijing::RunUpdate {
                turns: Some(i64::from(usage.turns)),
                tokens_uncached: Some(usage.uncached_tokens as i64),
                tokens_cached: Some(usage.cached_tokens as i64),
                ..Default::default()
            },
        )
    })
    .await
    .context("persist final usage task")?
    .context("persist final usage")?;
    let report = match repair_result {
        Ok(r) => r,
        Err(e) => {
            let now = now_ms();
            let class = classify_failure(&e);
            let detail = bounded_text(&secrets.redact(&format!("repair_failed: {e:#}")));
            let outcome = match class {
                FailureClass::ProviderTransient => "provider_transient",
                FailureClass::ProviderConfig => "provider_config",
                FailureClass::Environment => "environment",
                FailureClass::Issue => "repair_failed",
            };
            let _ = store.finish_run(
                &run_id,
                &shanhaijing::RunFinish {
                    finished_at: now_ms(),
                    outcome: outcome.into(),
                    ci_verdict: None,
                    error: Some(detail.clone()),
                },
            );
            match class {
                FailureClass::ProviderTransient => {
                    // The issue is not at fault: release it for a later retry
                    // without consuming an attempt, and stand down after a
                    // short streak so a provider outage does not burn the queue.
                    let _ = store.release_issue(
                        &fp,
                        &format!("provider transient: {e:#}"),
                        now + 5 * 60_000,
                        now,
                    );
                    let standing = mark_provider_failure(&live, now).await;
                    trace::record(
                        store,
                        &fp,
                        &repo_config.repo,
                        trace::Decision {
                            stage: "repair",
                            decision: "released",
                            reason: "provider transient failure; issue is not at fault",
                            details: serde_json::json!({
                                "error": detail,
                                "stand_down": standing,
                            }),
                        },
                    );
                    if standing {
                        emit_webhook(
                            "provider_stand_down",
                            &fp,
                            &repo_config.repo,
                            "provider transient failures reached 3; standing down for 15 minutes",
                        );
                    }
                    live_update(
                        &live,
                        if standing { "standing_down" } else { "idle" },
                        None,
                        None,
                        Some(detail),
                    )
                    .await;
                    return Ok(false);
                }
                FailureClass::ProviderConfig => {
                    let _ = store.set_state_as(
                        &fp,
                        shanhaijing::State::Escalated,
                        now_ms(),
                        Some(detail.as_str()),
                        "runner",
                    );
                    emit_webhook(
                        "escalated",
                        &fp,
                        &repo_config.repo,
                        "provider configuration failure; issue escalated",
                    );
                    emit_webhook(
                        "provider_stand_down",
                        &fp,
                        &repo_config.repo,
                        "provider configuration failure; standing down for 15 minutes",
                    );
                    trace::record(
                        store,
                        &fp,
                        &repo_config.repo,
                        trace::Decision {
                            stage: "repair",
                            decision: "escalated",
                            reason: "provider configuration failure",
                            details: serde_json::json!({"error": detail}),
                        },
                    );
                    mark_stand_down(&live, now + 15 * 60_000).await;
                    live_update(&live, "standing_down", None, None, Some(detail)).await;
                    return Ok(false);
                }
                FailureClass::Environment => {
                    let _ = store.release_issue(
                        &fp,
                        &format!("environment failure: {e:#}"),
                        now + 5 * 60_000,
                        now,
                    );
                    trace::record(
                        store,
                        &fp,
                        &repo_config.repo,
                        trace::Decision {
                            stage: "repair",
                            decision: "released",
                            reason: "environment failure; not an issue fault",
                            details: serde_json::json!({"error": detail}),
                        },
                    );
                    live_update(&live, "error", Some(&fp), None, Some(detail)).await;
                    return Ok(false);
                }
                FailureClass::Issue => {
                    live_update(&live, "error", Some(&fp), None, Some(detail.clone())).await;
                    let outcome = store.record_issue_failure(
                        &fp,
                        now,
                        u64::from(repo_config.max_attempts.max(1)),
                        now + 30 * 60_000,
                        &detail,
                        "runner",
                    )?;
                    let retry = outcome.disposition == shanhaijing::FailureDisposition::Retry;
                    trace::record(
                        store,
                        &fp,
                        &repo_config.repo,
                        trace::Decision {
                            stage: "repair",
                            decision: if retry { "retry_scheduled" } else { "ignored" },
                            reason: "repair failed on an issue-level fault",
                            details: serde_json::json!({
                                "error": detail,
                                "attempts": outcome.attempts,
                                "max_attempts": repo_config.max_attempts,
                            }),
                        },
                    );
                    if retry {
                        println!(
                            "repair     : failed — retry scheduled (attempt {}/{}): {e:#}",
                            outcome.attempts, repo_config.max_attempts
                        );
                    } else {
                        emit_webhook("ignored", &fp, &repo_config.repo, detail.clone());
                        println!("repair     : failed — attempt cap reached, issue ignored: {e:#}");
                    }
                    return Ok(true);
                }
            }
        }
    };
    reset_provider_health(&live).await;
    let redacted_root_cause = secrets.redact(&report.root_cause_text);
    let cause = redacted_root_cause
        .lines()
        .find(|l| l.trim_start().starts_with("ROOT CAUSE"))
        .unwrap_or("(no ROOT CAUSE line)");
    println!("root cause : {cause}");
    println!(
        "context    : window {} tokens, {} auto-compaction(s), {} messages",
        report.context.model_context_window, report.context.compactions, report.context.messages
    );
    trace::record(
        store,
        &fp,
        &repo_config.repo,
        trace::Decision {
            stage: "repair",
            decision: "completed",
            reason: "harness returned a final report",
            details: serde_json::json!({
                "turns": report.turns,
                "turns_cap": max_turns,
                "tokens_uncached": report.uncached_tokens,
                "tokens_cached": report.cached_tokens,
                "token_cap": max_tokens,
                "context_window": report.context.model_context_window,
                "compactions": report.context.compactions,
                "messages": report.context.messages,
            }),
        },
    );

    // 排水完成后的报告补齐验证与上下文证据；累计 usage 已独立持久化。
    persist_run(
        store,
        &run_id,
        shanhaijing::RunUpdate {
            phase: Some("verify".into()),
            harness_run_id: Some(report.run_id.clone()),
            turns: Some(i64::from(report.turns)),
            tokens_uncached: Some(report.uncached_tokens as i64),
            tokens_cached: Some(report.cached_tokens as i64),
            compactions: Some(i64::from(report.context.compactions)),
            context_window: Some(report.context.model_context_window as i64),
            messages: Some(report.context.messages as i64),
            root_cause: Some(redacted_root_cause.clone()),
            session_dir: Some(report.session_dir.clone()),
            ..Default::default()
        },
    )
    .await;

    // ---- budget gate: per-issue token ceiling ----
    // The turn cap bounds the loop mid-run (RPI_MAX_TURNS_PER_RUN); this gate
    // is the spend ceiling — an over-budget run never reaches delivery.
    let spent = report.uncached_tokens + report.cached_tokens;
    if spent > max_tokens {
        let detail =
            bounded_text(&secrets.redact(&format!("budget_exceeded: {spent} > {max_tokens}")));
        let _ = store.finish_run(
            &run_id,
            &shanhaijing::RunFinish {
                finished_at: now_ms(),
                outcome: "budget_exceeded".into(),
                ci_verdict: None,
                error: Some(detail.clone()),
            },
        );
        live_update(&live, "error", Some(&fp), None, Some(detail.clone())).await;
        let failed_at = now_ms();
        let outcome = store.record_issue_failure(
            &fp,
            failed_at,
            u64::from(repo_config.max_attempts.max(1)),
            failed_at + 30 * 60_000,
            &detail,
            "runner",
        )?;
        let retry = outcome.disposition == shanhaijing::FailureDisposition::Retry;
        trace::record(
            store,
            &fp,
            &repo_config.repo,
            trace::Decision {
                stage: "budget",
                decision: if retry { "retry_scheduled" } else { "ignored" },
                reason: "per-issue token cap exceeded",
                details: serde_json::json!({
                    "spent_tokens": spent,
                    "cap_tokens": max_tokens,
                    "attempts": outcome.attempts,
                    "max_attempts": repo_config.max_attempts,
                }),
            },
        );
        if retry {
            println!(
                "budget     : {detail} — retry scheduled (attempt {}/{})",
                outcome.attempts, repo_config.max_attempts
            );
        } else {
            emit_webhook("ignored", &fp, &repo_config.repo, detail.clone());
            println!("budget     : {detail} — attempt cap reached, issue ignored");
        }
        return Ok(true);
    }
    println!(
        "tokens     : {spent} (per-issue cap {max_tokens}, daily cap {daily_token_limit}, monthly cap {monthly_token_limit})"
    );

    // ---- deliver ----
    live_update(&live, "working", Some(&fp), Some("deliver"), None).await;
    persist_run(
        store,
        &run_id,
        shanhaijing::RunUpdate {
            phase: Some("deliver".into()),
            ..Default::default()
        },
    )
    .await;
    let draft = repo_config.draft_first_n > 0
        && store.merged_pr_count(&repo_config.repo)? < u64::from(repo_config.draft_first_n);
    let public_base_url = std::env::var("NUWA_PUBLIC_BASE_URL")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let delivered = match pipeline::deliver(pipeline::DeliveryRequest {
        repo: repo_dir,
        issue: prepared.issue(),
        report: &report,
        sessions_dir: &sessions_dir,
        push,
        create_pr,
        public_base_url: public_base_url.as_deref(),
        draft,
        secrets,
        store: Some(store),
        run_id: Some(&run_id),
        repo_id: &repo_config.repo,
    })
    .await
    {
        Ok(d) => d,
        Err(e) => {
            let failed_at = now_ms();
            let detail = bounded_text(&secrets.redact(&format!("delivery_failed: {e:#}")));
            let _ = store.finish_run(
                &run_id,
                &shanhaijing::RunFinish {
                    finished_at: failed_at,
                    outcome: "delivery_failed".into(),
                    ci_verdict: None,
                    error: Some(detail.clone()),
                },
            );
            live_update(&live, "error", Some(&fp), None, Some(detail.clone())).await;
            if deliver::is_transient_delivery_error(&e) {
                store.release_issue(&fp, &detail, failed_at + 5 * 60_000, failed_at)?;
                trace::record(
                    store,
                    &fp,
                    &repo_config.repo,
                    trace::Decision {
                        stage: "deliver",
                        decision: "released",
                        reason: "transient delivery/network failure; attempt not consumed",
                        details: serde_json::json!({"error": detail}),
                    },
                );
                println!("delivery   : transient failure — released for retry: {e:#}");
                return Ok(false);
            }
            let outcome = store.record_issue_failure(
                &fp,
                failed_at,
                u64::from(repo_config.max_attempts.max(1)),
                failed_at + 30 * 60_000,
                &detail,
                "runner",
            )?;
            let retry = outcome.disposition == shanhaijing::FailureDisposition::Retry;
            trace::record(
                store,
                &fp,
                &repo_config.repo,
                trace::Decision {
                    stage: "deliver",
                    decision: if retry { "retry_scheduled" } else { "ignored" },
                    reason: "delivery failed on an issue-level fault",
                    details: serde_json::json!({
                        "error": detail,
                        "attempts": outcome.attempts,
                        "max_attempts": repo_config.max_attempts,
                    }),
                },
            );
            if retry {
                println!(
                    "delivery   : failed — retry scheduled (attempt {}/{}): {e:#}",
                    outcome.attempts, repo_config.max_attempts
                );
            } else {
                emit_webhook("ignored", &fp, &repo_config.repo, detail.clone());
                println!("delivery   : failed — attempt cap reached, issue ignored: {e:#}");
            }
            return Ok(true);
        }
    };

    println!(
        "risk       : files={} +{} -{} tests={} ci={}",
        delivered.risk.files.len(),
        delivered.risk.added,
        delivered.risk.removed,
        delivered.risk.touches_tests,
        delivered.risk.touches_ci
    );
    let pr_opened_at = delivered.pr_url.as_ref().map(|_| now_ms());
    // 交付证据同样必须在 run 结束前落库（阻塞池执行，不占用异步 worker）。
    persist_run(
        store,
        &run_id,
        shanhaijing::RunUpdate {
            phase: Some("ci".into()),
            branch: Some(delivered.branch.clone()),
            commit_sha: Some(delivered.commit.clone()),
            pr_url: delivered.pr_url.clone(),
            pr_opened_at,
            verification_ok: Some(true),
            verification_detail: Some(delivered.verification_detail.clone()),
            change_stat: Some(delivered.change_stat.clone()),
            session_dir: Some(report.session_dir.clone()),
            ..Default::default()
        },
    )
    .await;

    if let Err(e) = store.set_state_as(
        &fp,
        shanhaijing::State::PrOpen,
        now_ms(),
        Some(
            delivered
                .pr_url
                .as_deref()
                .unwrap_or("branch pushed, no PR"),
        ),
        "runner",
    ) {
        let detail =
            bounded_text(&secrets.redact(&format!("delivery_failed: state update: {e:#}")));
        let _ = store.finish_run(
            &run_id,
            &shanhaijing::RunFinish {
                finished_at: now_ms(),
                outcome: "delivery_failed".into(),
                ci_verdict: None,
                error: Some(detail.clone()),
            },
        );
        live_update(&live, "error", Some(&fp), None, Some(detail.clone())).await;
        let failed_at = now_ms();
        let _ = store.record_issue_failure(
            &fp,
            failed_at,
            u64::from(repo_config.max_attempts.max(1)),
            failed_at + 30 * 60_000,
            &detail,
            "runner",
        );
        return Err(e.into());
    }
    if let Some(url) = &delivered.pr_url {
        if let Err(e) = store.set_pr(&fp, url, now_ms()) {
            let detail = bounded_text(&secrets.redact(&format!("delivery_failed: set_pr: {e:#}")));
            let _ = store.finish_run(
                &run_id,
                &shanhaijing::RunFinish {
                    finished_at: now_ms(),
                    outcome: "delivery_failed".into(),
                    ci_verdict: None,
                    error: Some(detail.clone()),
                },
            );
            live_update(&live, "error", Some(&fp), None, Some(detail.clone())).await;
            emit_webhook("escalated", &fp, &repo_config.repo, detail);
            return Err(e.into());
        }
        emit_webhook("pr_opened", &fp, &repo_config.repo, url.clone());
    }
    if let (Some(slug), Some(number)) = (&delivered.slug, delivered.pr_number) {
        let _ = store.upsert_ci_watch(&shanhaijing::CiWatchInput {
            run_id: Some(run_id.clone()),
            fingerprint: fp.clone(),
            repo: repo_config.repo.clone(),
            pr_url: delivered
                .pr_url
                .clone()
                .unwrap_or_else(|| format!("https://github.com/{slug}/pull/{number}")),
            slug: slug.clone(),
            pr_number: number,
            head_sha: delivered.commit.clone(),
            root_cause: Some(redacted_root_cause.clone()),
            change_stat: Some(delivered.change_stat.clone()),
            now: now_ms(),
            next_check_at: now_ms(),
            max_attempts: u64::from(repo_config.max_attempts.max(1)),
        });
    }
    trace::record(
        store,
        &fp,
        &repo_config.repo,
        trace::Decision {
            stage: "deliver",
            decision: if !push {
                "delivered_local"
            } else if delivered.pr_url.is_some() {
                "pr_opened"
            } else {
                "branch_pushed"
            },
            reason: "verification gate green (base red, branch green)",
            details: serde_json::json!({
                "branch": delivered.branch,
                "commit": delivered.commit,
                "pr_url": delivered.pr_url,
                "draft": draft,
                "files": delivered.risk.files.len(),
                "added": delivered.risk.added,
                "removed": delivered.risk.removed,
                "touches_tests": delivered.risk.touches_tests,
                "touches_ci": delivered.risk.touches_ci,
            }),
        },
    );

    if !create_pr {
        trace::record(
            store,
            &fp,
            &repo_config.repo,
            trace::Decision {
                stage: "deliver",
                decision: "branch_only",
                reason: "PR creation disabled for this runner",
                details: serde_json::json!({"branch": delivered.branch, "commit": delivered.commit}),
            },
        );
        if push {
            remember_verified_fix(
                store,
                &repo_config.repo,
                &fp,
                &delivered,
                &redacted_root_cause,
            );
        }
        let _ = store.finish_run(
            &run_id,
            &shanhaijing::RunFinish {
                finished_at: now_ms(),
                outcome: "success".into(),
                ci_verdict: None,
                error: None,
            },
        );
        live_update(&live, "idle", None, None, None).await;
        println!(
            "delivered  : branch {} commit {} pr {}",
            delivered.branch,
            &delivered.commit[..12.min(delivered.commit.len())],
            delivered.pr_url.as_deref().unwrap_or("-"),
        );
        return Ok(true);
    }

    // ---- CI 闸门消费（docs/03 末段）：红 → 自动关 PR + rejected；绿/超时 → 留给人工合并 ----
    live_update(&live, "working", Some(&fp), Some("ci"), None).await;
    if let (Some(slug), Some(number)) = (&delivered.slug, delivered.pr_number) {
        let token = match secrets.github_token.clone() {
            Some(token) => token,
            None => {
                // PR 已开出；缺令牌不是交付失败，只是 CI 闸门不可用。仍要
                // 关闭 run，否则它会永远停在 open 状态。
                let detail = bounded_text("NUWA_GITHUB_TOKEN not set — cannot poll CI checks");
                let _ = store.update_run(
                    &run_id,
                    &shanhaijing::RunUpdate {
                        ci_verdict: Some("unavailable".into()),
                        ..Default::default()
                    },
                );
                let _ = store.finish_run(
                    &run_id,
                    &shanhaijing::RunFinish {
                        finished_at: now_ms(),
                        outcome: "success".into(),
                        ci_verdict: Some("unavailable".into()),
                        error: Some(detail.clone()),
                    },
                );
                live_update(&live, "idle", None, None, Some(detail.clone())).await;
                trace::record(
                    store,
                    &fp,
                    &repo_config.repo,
                    trace::Decision {
                        stage: "ci",
                        decision: "unavailable",
                        reason: "github token missing; checks cannot be polled",
                        details: serde_json::json!({"pr": number}),
                    },
                );
                // The PR is open and the run is closed. A missing token only
                // means the CI gate cannot be polled — it is not a runner
                // failure, so the loop keeps running.
                println!("ci gate    : {detail}");
                return Ok(true);
            }
        };
        println!(
            "ci gate   : polling checks for {} (timeout {ci_timeout_secs}s)…",
            &delivered.commit[..12.min(delivered.commit.len())]
        );
        let checks = ci::wait_for_ci(&token, slug, &delivered.commit, ci_timeout_secs).await;
        match checks {
            Ok(ci::Verdict::Success) => {
                println!("ci gate   : all checks green — PR open, awaiting human merge");
                let _ = store.update_run(
                    &run_id,
                    &shanhaijing::RunUpdate {
                        ci_verdict: Some("success".into()),
                        ..Default::default()
                    },
                );
                let _ = store.finish_run(
                    &run_id,
                    &shanhaijing::RunFinish {
                        finished_at: now_ms(),
                        outcome: "success".into(),
                        ci_verdict: Some("success".into()),
                        error: None,
                    },
                );
                let _ = store.record_event(
                    &fp,
                    now_ms(),
                    Some(&repo_config.repo),
                    "runner",
                    "ci_green",
                    &format!(
                        "head {}",
                        &delivered.commit[..12.min(delivered.commit.len())]
                    ),
                );
                trace::record(
                    store,
                    &fp,
                    &repo_config.repo,
                    trace::Decision {
                        stage: "ci",
                        decision: "ci_green",
                        reason: "all checks passed; awaiting human merge",
                        details: serde_json::json!({
                            "pr": number,
                            "slug": slug,
                            "commit": delivered.commit,
                        }),
                    },
                );
                remember_verified_fix(
                    store,
                    &repo_config.repo,
                    &fp,
                    &delivered,
                    &redacted_root_cause,
                );
                if let Ok(Some(watch)) = store.ci_watch_for(&fp) {
                    let _ = store.update_ci_watch(
                        watch.id,
                        shanhaijing::CiWatchState::Green,
                        Some("green"),
                        now_ms() + 5 * 60_000,
                        None,
                        true,
                        None,
                        now_ms(),
                    );
                }
            }
            Ok(ci::Verdict::Failure) => {
                if let Err(e) = ci::close_pr(&token, slug, number).await {
                    let detail = bounded_text(&format!("ci_red: close PR failed: {e:#}"));
                    let _ = store.finish_run(
                        &run_id,
                        &shanhaijing::RunFinish {
                            finished_at: now_ms(),
                            outcome: "ci_red".into(),
                            ci_verdict: Some("failure".into()),
                            error: Some(detail.clone()),
                        },
                    );
                    live_update(&live, "error", Some(&fp), None, Some(detail.clone())).await;
                    // The PR could not be closed — the issue must not stay in
                    // `pr_open` pretending the gate is still pending. Hand it
                    // to the owner as escalated.
                    let message = format!("ci_red: close PR failed: {e:#} — escalated");
                    let _ = store.set_state_as(
                        &fp,
                        shanhaijing::State::Escalated,
                        now_ms(),
                        Some(&message),
                        "runner",
                    );
                    emit_webhook("escalated", &fp, &repo_config.repo, message.clone());
                    println!("ci gate    : {message}");
                    return Ok(true);
                }
                let _ = store.update_run(
                    &run_id,
                    &shanhaijing::RunUpdate {
                        ci_verdict: Some("failure".into()),
                        ..Default::default()
                    },
                );
                let _ = store.finish_run(
                    &run_id,
                    &shanhaijing::RunFinish {
                        finished_at: now_ms(),
                        outcome: "ci_red".into(),
                        ci_verdict: Some("failure".into()),
                        error: Some(bounded_text(&format!("CI red — PR #{number} closed"))),
                    },
                );
                let failure = store.record_ci_rejection(
                    &fp,
                    delivered
                        .pr_url
                        .as_deref()
                        .context("delivered PR URL missing")?,
                    now_ms(),
                    u64::from(repo_config.max_attempts.max(1)),
                    now_ms() + 30 * 60_000,
                    &format!("ci_red: PR #{number} closed without merge"),
                    "runner",
                )?;
                let retry = failure.disposition == shanhaijing::FailureDisposition::Retry;
                let _ = store.record_event(
                    &fp,
                    now_ms(),
                    Some(&repo_config.repo),
                    "runner",
                    "ci_red",
                    &format!(
                        "head {} — PR #{number} closed",
                        &delivered.commit[..12.min(delivered.commit.len())]
                    ),
                );
                trace::record(
                    store,
                    &fp,
                    &repo_config.repo,
                    trace::Decision {
                        stage: "ci",
                        decision: if retry { "retry_scheduled" } else { "ignored" },
                        reason: "CI failed; PR closed and the failure policy advanced",
                        details: serde_json::json!({
                            "pr": number,
                            "slug": slug,
                            "commit": delivered.commit,
                            "attempts": failure.attempts,
                            "max_attempts": repo_config.max_attempts,
                        }),
                    },
                );
                emit_webhook(
                    "ci_red",
                    &fp,
                    &repo_config.repo,
                    if retry {
                        format!(
                            "PR #{number} closed after CI failure; retry scheduled (attempt {}/{})",
                            failure.attempts, repo_config.max_attempts
                        )
                    } else {
                        format!(
                            "PR #{number} closed after CI failure; attempt cap reached, issue ignored"
                        )
                    },
                );
                if let Ok(Some(watch)) = store.ci_watch_for(&fp) {
                    let _ = store.update_ci_watch(
                        watch.id,
                        shanhaijing::CiWatchState::Red,
                        Some("red"),
                        now_ms() + 5 * 60_000,
                        None,
                        watch.memory_written,
                        Some(now_ms()),
                        now_ms(),
                    );
                }
                let note = if retry {
                    format!(
                        "CI red — PR #{number} closed; retry scheduled (attempt {}/{})",
                        failure.attempts, repo_config.max_attempts
                    )
                } else {
                    format!("CI red — PR #{number} closed; attempt cap reached, issue ignored")
                };
                live_update(&live, "error", Some(&fp), None, Some(note.clone())).await;
                // CI red is a business outcome: the runner keeps running even
                // when a concurrent state change means it did not reject the issue.
                println!("ci gate    : {note}");
                return Ok(true);
            }
            Ok(ci::Verdict::TimedOut) => {
                let _ = store.record_event(
                    &fp,
                    now_ms(),
                    Some(&repo_config.repo),
                    "runner",
                    "ci_timeout",
                    slug,
                );
                trace::record(
                    store,
                    &fp,
                    &repo_config.repo,
                    trace::Decision {
                        stage: "ci",
                        decision: "ci_timeout",
                        reason: "checks unfinished inside the CI window; PR left open",
                        details: serde_json::json!({
                            "pr": number,
                            "slug": slug,
                            "timeout_secs": ci_timeout_secs,
                        }),
                    },
                );
                let _ = store.update_run(
                    &run_id,
                    &shanhaijing::RunUpdate {
                        ci_verdict: Some("timeout".into()),
                        ..Default::default()
                    },
                );
                let _ = store.finish_run(
                    &run_id,
                    &shanhaijing::RunFinish {
                        finished_at: now_ms(),
                        outcome: "success".into(),
                        ci_verdict: Some("timeout".into()),
                        error: None,
                    },
                );
                println!(
                    "ci gate   : checks unfinished in {ci_timeout_secs}s — PR left open, check manually"
                );
            }
            Err(e) => {
                // Checks unreadable (fine-grained PAT missing Actions/Checks
                // read on this repo): the PR stays open, the issue stays
                // pr_open — merging remains the human decision.
                let _ = store.update_run(
                    &run_id,
                    &shanhaijing::RunUpdate {
                        ci_verdict: Some("unavailable".into()),
                        ..Default::default()
                    },
                );
                let _ = store.finish_run(
                    &run_id,
                    &shanhaijing::RunFinish {
                        finished_at: now_ms(),
                        outcome: "success".into(),
                        ci_verdict: Some("unavailable".into()),
                        error: None,
                    },
                );
                println!(
                    "ci gate   : checks unavailable ({e:#}) — PR left open; \
                     grant the PAT Actions:Read-only + Contents:Read-only on this repo"
                );
                trace::record(
                    store,
                    &fp,
                    &repo_config.repo,
                    trace::Decision {
                        stage: "ci",
                        decision: "unavailable",
                        reason: "checks could not be read; PR left open for humans",
                        details: serde_json::json!({
                            "pr": number,
                            "slug": slug,
                            "error": secrets.redact(&format!("{e:#}")),
                        }),
                    },
                );
            }
        }
    } else {
        // Defensive: a PR was requested but there is no slug/number to poll.
        // The run still has to be closed; the PR (if any) stays open.
        let detail = bounded_text("PR number unavailable — CI gate skipped");
        let _ = store.update_run(
            &run_id,
            &shanhaijing::RunUpdate {
                ci_verdict: Some("unavailable".into()),
                ..Default::default()
            },
        );
        let _ = store.finish_run(
            &run_id,
            &shanhaijing::RunFinish {
                finished_at: now_ms(),
                outcome: "success".into(),
                ci_verdict: Some("unavailable".into()),
                error: Some(detail),
            },
        );
    }

    live_update(&live, "idle", None, None, None).await;
    println!(
        "delivered  : branch {} commit {} pr {}",
        delivered.branch,
        &delivered.commit[..12.min(delivered.commit.len())],
        delivered.pr_url.as_deref().unwrap_or("-"),
    );
    Ok(true)
}

/// 对账：pr_open 问题单 ↔ GitHub 实际状态。merged → `merged`；
/// closed 且未合并 → `rejected`；仍 open → 不动。
async fn run_sync(args: Vec<String>) -> anyhow::Result<()> {
    let mut db_path = std::path::PathBuf::from("shanhaijing.db");
    let mut it = args.into_iter();
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--db" => db_path = PathBuf::from(it.next().context("--db needs a value")?),
            other => bail!("unknown sync flag {other}"),
        }
    }
    let token = std::env::var("NUWA_GITHUB_TOKEN")
        .or_else(|_| std::env::var("GITHUB_TOKEN"))
        .context("NUWA_GITHUB_TOKEN not set — sync needs to read PR state")?;
    let store = shanhaijing::Store::open(&db_path)
        .with_context(|| format!("open db {}", db_path.display()))?;
    let now = now_ms();
    for line in ci::reconcile_all(&store, &token, now).await? {
        println!("{line}");
    }
    Ok(())
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// env 读取（空值视为未设置）。
fn env_d(name: &str, default: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

/// 设置页驱动的 provider 默认值（CLI 显式 flag 优先）。
pub(crate) fn env_provider_default() -> repair::ProviderKind {
    match env_d("NUWA_PROVIDER", "anthropic").as_str() {
        "zai" => repair::ProviderKind::Zai {
            base_url: std::env::var("NUWA_BASE_URL")
                .ok()
                .filter(|s| !s.is_empty()),
            model: std::env::var("NUWA_MODEL").ok().filter(|s| !s.is_empty()),
        },
        "faux" => repair::ProviderKind::Faux,
        _ => repair::ProviderKind::Anthropic {
            model: std::env::var("NUWA_MODEL").ok().filter(|s| !s.is_empty()),
        },
    }
}

/// Ops view: every issue, newest sighting first.
fn run_status(args: Vec<String>) -> anyhow::Result<()> {
    let mut db_path = std::path::PathBuf::from("shanhaijing.db");
    let mut it = args.into_iter();
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--db" => db_path = PathBuf::from(it.next().context("--db needs a value")?),
            other => bail!("unknown status flag {other}"),
        }
    }
    let store = shanhaijing::Store::open(&db_path)
        .with_context(|| format!("open db {}", db_path.display()))?;
    println!(
        "{:<34} {:<10} {:<8} {:>4}  title / pr",
        "fingerprint", "state", "sev", "seen"
    );
    for row in store.list_issues()? {
        let pr_suffix = row
            .pr_url
            .as_deref()
            .map(|u| format!("  → {u}"))
            .unwrap_or_default();
        println!(
            "{:<34} {:<10} {:<8} {:>4}  {}{}",
            &row.fingerprint[..16.min(row.fingerprint.len())],
            row.state,
            row.severity,
            row.occurrences,
            row.title,
            pr_suffix,
        );
    }
    Ok(())
}

async fn run_fix() -> anyhow::Result<()> {
    let cli = parse_args(std::env::args().skip(2).collect())?;
    webhook::init_from_env();
    let secrets = Arc::new(secrets::Secrets::load_and_scrub_env());
    // 文件里的问题单是外部输入：与常驻 runner 一样先脱敏。
    let prob = pipeline::redact_issue(
        issue::Issue::load(&cli.issue_path).context("load issue")?,
        &secrets,
    );

    if !cli.repo.join(".git").exists() {
        bail!("{} is not a git repository", cli.repo.display());
    }

    println!(
        "== Nuwa fix run ==\nissue     : {}\nrepo      : {}\nprovider  : {}\nmax turns : {}",
        prob.fingerprint,
        cli.repo.display(),
        pipeline::provider_label(&cli.provider),
        cli.max_turns,
    );

    // 工作区若有仓库指令，其 notes 与技能白名单与常驻 runner 走同一条注入路径。
    let repo_config = instructions::load_working_tree(&cli.repo)?;
    let prepared = pipeline::prepare_repair(pipeline::RepairRequest {
        repo: cli.repo.clone(),
        issue: prob.clone(),
        sessions_dir: cli.sessions_dir.clone(),
        provider: cli.provider,
        max_turns: cli.max_turns,
        repo_config,
        secrets: Arc::clone(&secrets),
        progress: None,
        faux_script: None,
        run_id: None,
    })?;
    let report = prepared.execute().await.context("repair run")?;

    println!(
        "\nrun       : {}\nturns     : {}\ntokens    : {} (cached {})\ncontext   : window {} tokens, {} auto-compaction(s), {} messages",
        report.run_id,
        report.turns,
        report.uncached_tokens,
        report.cached_tokens,
        report.context.model_context_window,
        report.context.compactions,
        report.context.messages,
    );
    let cause = report
        .root_cause_text
        .lines()
        .find(|l| l.trim_start().starts_with("ROOT CAUSE"))
        .unwrap_or("(runner did not emit a ROOT CAUSE line)");
    println!("root cause: {cause}");

    if cli.no_deliver {
        println!("\n--no-deliver: stopping before the delivery gate");
        return Ok(());
    }

    let delivered = pipeline::deliver(pipeline::DeliveryRequest {
        repo: &cli.repo,
        issue: &prob,
        report: &report,
        sessions_dir: &cli.sessions_dir,
        push: cli.push,
        create_pr: cli.create_pr,
        public_base_url: None,
        draft: false,
        secrets: &secrets,
        store: None,
        run_id: None,
        repo_id: "",
    })
    .await?;
    println!(
        "\nbranch    : {}\nbase      : {}\ncommit    : {}\npr        : {}\nrisk      : files={} tests={} ci={}",
        delivered.branch,
        delivered.base,
        &delivered.commit[..12.min(delivered.commit.len())],
        delivered.pr_url.as_deref().unwrap_or("-"),
        delivered.risk.files.len(),
        delivered.risk.touches_tests,
        delivered.risk.touches_ci,
    );
    Ok(())
}
pub const NUWA_BUILD_MARKER: &str = "MARKER-XYZ-9L";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instructions::RepoInstructions;
    use crate::issue::FailingCheck;

    #[test]
    fn failure_classifier_separates_provider_from_issue_faults() {
        assert_eq!(
            classify_failure(&anyhow::anyhow!("LLM provider returned 429 rate limit")),
            FailureClass::ProviderTransient
        );
        assert_eq!(
            classify_failure(&anyhow::anyhow!("ANTHROPIC_API_KEY not set")),
            FailureClass::ProviderConfig
        );
        assert_eq!(
            classify_failure(&anyhow::anyhow!("Command timed out after 300s")),
            FailureClass::Issue
        );
        assert_eq!(
            classify_failure(&anyhow::anyhow!("verification gate failed")),
            FailureClass::Issue
        );
        assert_eq!(
            classify_failure(&anyhow::anyhow!("repo working tree dirty")),
            FailureClass::Environment
        );
    }

    /// 目标仓的测试输出（"connection refused"）不得被当成 provider 故障。
    /// 交付门的失败是普通错误，靠 `failing_check` 前缀先归为 issue；带
    /// `ProviderFailure` 标记的错误则无论文本都按 provider 处理。
    #[test]
    fn target_repo_output_is_not_mistaken_for_a_provider_outage() {
        let verification = anyhow::anyhow!(
            "failing_check does not pass on the branch:\n\
             go test ./...: dial tcp 127.0.0.1:5432: connect: connection refused"
        );
        assert_eq!(
            classify_failure(&verification),
            FailureClass::Issue,
            "a failing check in the target repo is the issue's fault"
        );

        // A provider-tagged failure stays a provider failure whatever its
        // text looks like — that is the point of tagging it at the source.
        let provider = anyhow::Error::new(repair::ProviderFailure(
            "connection refused: dial tcp 127.0.0.1:5432".to_string(),
        ))
        .context("repair run");
        assert_eq!(classify_failure(&provider), FailureClass::ProviderTransient);

        let throttled = anyhow::Error::new(repair::ProviderFailure(
            "429 rate limit exceeded".to_string(),
        ))
        .context("repair run");
        assert_eq!(
            classify_failure(&throttled),
            FailureClass::ProviderTransient
        );

        let auth = anyhow::Error::new(repair::ProviderFailure(
            "401 unauthorized: invalid x-api-key".to_string(),
        ))
        .context("repair run");
        assert_eq!(classify_failure(&auth), FailureClass::ProviderConfig);
    }

    #[test]
    fn relative_sessions_root_becomes_absolute() {
        // The default `spike/sessions` is relative; the transcript endpoint
        // rejects relative run paths, so run_work resolves the root once.
        let absolute = absolute_sessions_dir(PathBuf::from("spike/sessions")).expect("absolutize");
        assert!(absolute.is_absolute(), "{absolute:?}");
        assert!(
            absolute.ends_with(Path::new("spike").join("sessions")),
            "{absolute:?}"
        );

        // An already absolute root is kept as-is.
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("sessions");
        assert_eq!(absolute_sessions_dir(root.clone()).expect("absolute"), root);
    }

    #[test]
    fn runner_id_is_sanitized_and_stable() {
        let id = sanitize_runner_id("host/repo name:日本");
        assert!(id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-')));
        assert!(id.len() <= 128);
        assert!(!id.is_empty());
        assert_eq!(sanitize_runner_id("---"), "runner");
    }

    #[test]
    fn default_runner_id_contains_repo() {
        let id = default_runner_id("example.com/spike-target");
        assert!(id.contains("example.com") || id.contains("spike-target"));
    }

    #[test]
    fn provider_labels_are_stable() {
        assert_eq!(
            provider_meta(&repair::ProviderKind::Faux),
            ("faux".into(), "faux".into())
        );
        assert_eq!(
            provider_meta(&repair::ProviderKind::Zai {
                base_url: None,
                model: None
            }),
            ("zai".into(), "glm-5.3-flash".into())
        );
        assert_eq!(
            provider_meta(&repair::ProviderKind::Anthropic { model: None }),
            ("anthropic".into(), "<default>".into())
        );
    }

    #[test]
    fn bounded_text_caps_run_errors() {
        const LIMIT: usize = 16_000;
        let marker = "\n… (truncated)";
        assert_eq!(bounded_text("boom"), "boom");
        let long = "x".repeat(20_000);
        let bounded = bounded_text(&long);
        assert!(bounded.len() <= LIMIT + marker.len());
        assert!(bounded.ends_with("(truncated)"));
        // Multi-byte input must truncate on a char boundary.
        let wide = "错".repeat(10_000);
        assert!(bounded_text(&wide).len() <= LIMIT + marker.len());
    }

    #[test]
    fn migrate_env_command_normalizes_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nuwa.env");
        std::fs::write(&path, "nuwa_interval=30\n").expect("write");
        run_migrate_env(vec!["--env-file".into(), path.display().to_string()]).expect("migrate");
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(text.contains("NUWA_INTERVAL=30"));
        assert!(!text.contains("nuwa_interval"));
        assert!(path.with_extension("env.bak").exists());
    }

    #[tokio::test]
    async fn quota_stand_down_still_writes_heartbeat() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(shanhaijing::Store::open(dir.path().join("s.db")).expect("open"));
        let now = now_ms();
        let signal = shanhaijing::Signal {
            repo: "example.com/spike".into(),
            language: "go".into(),
            title: "panic: boom".into(),
            severity: "high".into(),
            message: None,
            log_window: Vec::new(),
            stack_trace:
                "panic: boom\ngoroutine 1 [running]:\nexample.com/spike.F()\n\t/app/a.go:1 +0x1"
                    .into(),
        };
        store.report_signal(&signal, now).expect("report");
        let fp = store
            .ready_issue(now)
            .expect("ready")
            .expect("issue")
            .fingerprint;
        store
            .set_pr(&fp, "https://github.com/acme/spike/pull/1", now)
            .expect("pr");

        let cfg = RepoInstructions {
            repo: "example.com/spike".into(),
            failing_check: FailingCheck {
                command: "true".into(),
                target: None,
                timeout_secs: 600,
            },
            forbidden_paths: Vec::new(),
            notes: String::new(),
            skills: Vec::new(),
            reproduce_check: None,
            max_attempts: 3,
            draft_first_n: 0,
        };
        let worked = work_once(
            &store,
            &mut test_trusted_instructions(dir.path(), &cfg),
            dir.path(),
            dir.path().join("sessions"),
            repair::ProviderKind::Faux,
            30,
            false,
            true,
            1,
            1_000_000,
            5_000_000,
            50_000_000,
            600,
            "test:runner",
            Arc::new(tokio::sync::Mutex::new(RunnerLive::new())),
            &Arc::new(secrets::Secrets::default()),
        )
        .await
        .expect("work_once");

        assert!(!worked, "quota must stand down");
        let hb = store
            .last_heartbeat()
            .expect("heartbeat")
            .expect("some heartbeat");
        assert!((now_ms() - hb).abs() < 5_000, "heartbeat must be fresh");
    }

    /// 异 repo 毒单：work_once 不 crash、不消耗 PR 配额，单被 escalated，
    /// runner 返回"工作过"以继续下一轮（2026-10-19 部署实战：spike 遗留单
    /// 曾让 runner crash-loop 在同一张单上）。
    #[tokio::test]
    async fn repo_mismatch_isolates_poison_issue() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo_dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(shanhaijing::Store::open(dir.path().join("s.db")).expect("open"));
        let now = now_ms();
        let signal = shanhaijing::Signal {
            repo: "example.com/other-repo".into(),
            language: "go".into(),
            title: "panic: boom".into(),
            severity: "high".into(),
            message: None,
            log_window: Vec::new(),
            stack_trace:
                "panic: boom\ngoroutine 1 [running]:\nexample.com/other.F()\n\t/app/a.go:1 +0x1"
                    .into(),
        };
        store.report_signal(&signal, now).expect("report");

        // preflight 要求一个干净的 main checkout（claim 之前检查）。
        git(repo_dir.path(), &["init", "-q", "-b", "main"]);
        git(
            repo_dir.path(),
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "--allow-empty",
                "-q",
                "-m",
                "init",
            ],
        );

        let cfg = RepoInstructions {
            repo: "example.com/spike".into(),
            failing_check: FailingCheck {
                command: "true".into(),
                target: None,
                timeout_secs: 600,
            },
            forbidden_paths: Vec::new(),
            notes: String::new(),
            skills: Vec::new(),
            reproduce_check: None,
            max_attempts: 3,
            draft_first_n: 0,
        };
        let worked = work_once(
            &store,
            &mut test_trusted_instructions(repo_dir.path(), &cfg),
            repo_dir.path(),
            dir.path().join("sessions"),
            repair::ProviderKind::Faux,
            30,
            false,
            true,
            1,
            1_000_000,
            5_000_000,
            50_000_000,
            600,
            "test:runner",
            Arc::new(tokio::sync::Mutex::new(RunnerLive::new())),
            &Arc::new(secrets::Secrets::default()),
        )
        .await
        .expect("work_once must not fail on a mismatched issue");

        assert!(
            !worked,
            "a repo-filtered claim leaves another repo's issue alone"
        );
        let fp = fp_of(&store, now);
        let row = store
            .list_issues()
            .expect("rows")
            .into_iter()
            .find(|r| r.fingerprint == fp)
            .expect("issue");
        assert_eq!(
            row.state, "new",
            "repo filtering must not consume or escalate another repo's issue"
        );
    }

    /// Helper: the single issue's fingerprint in a fresh test store.
    fn fp_of(store: &shanhaijing::Store, _now: i64) -> String {
        store
            .list_issues()
            .expect("rows")
            .first()
            .expect("an issue exists")
            .fingerprint
            .clone()
    }

    fn git(dir: &std::path::Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .expect("git");
        assert!(status.success(), "git {:?} failed", args);
    }

    fn git_output(dir: &std::path::Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git");
        assert!(out.status.success(), "git {:?} failed", args);
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    fn test_trusted_instructions(
        repo: &Path,
        cfg: &RepoInstructions,
    ) -> instructions::TrustedInstructions {
        let check = |value: &FailingCheck| {
            serde_json::json!({
                "command": value.command, "target": value.target, "timeout_secs": value.timeout_secs,
            })
        };
        let value = serde_json::json!({
            "repo": cfg.repo, "failing_check": check(&cfg.failing_check),
            "reproduce_check": cfg.reproduce_check.as_ref().map(check),
            "forbidden_paths": cfg.forbidden_paths, "notes": cfg.notes, "skills": cfg.skills,
            "max_attempts": cfg.max_attempts, "draft_first_n": cfg.draft_first_n,
        });
        let path = repo.join("nuwa.repo.json");
        std::fs::write(&path, value.to_string()).expect("test instructions");
        instructions::TrustedInstructions::load(
            repo,
            &path,
            "main",
            &repo.join(".git/test-trusted"),
        )
        .expect("trusted test instructions")
    }

    fn fixed_loop_check() -> String {
        if cfg!(windows) {
            "if exist fixed.marker (exit 0) else (exit 1)".to_string()
        } else {
            "test -f fixed.marker".to_string()
        }
    }

    fn init_git_repo(dir: &std::path::Path) {
        std::fs::create_dir_all(dir).expect("repo");
        git(dir, &["init", "-b", "main"]);
        git(dir, &["config", "user.name", "test"]);
        git(dir, &["config", "user.email", "test@example.com"]);
        std::fs::write(dir.join("f.txt"), "one\n").expect("write");
        git(dir, &["add", "."]);
        git(dir, &["commit", "-m", "init"]);
    }

    #[tokio::test]
    async fn preflight_recovers_from_agent_branch() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        init_git_repo(&repo);
        git(&repo, &["checkout", "-b", "agent/fix/abc"]);

        let base = preflight_repo(&repo, true).await.expect("preflight");
        assert_eq!(base, "main");
        assert_eq!(
            git_output(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
            "main"
        );
    }

    #[tokio::test]
    async fn preflight_fetches_remote_main() {
        let dir = tempfile::tempdir().expect("tempdir");
        let origin = dir.path().join("origin.git");
        std::fs::create_dir_all(&origin).expect("origin");
        git(&origin, &["init", "--bare", "-b", "main"]);

        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).expect("work");
        git(
            &work,
            &["clone", origin.to_str().expect("origin path"), "."],
        );
        git(&work, &["checkout", "-b", "main"]);
        git(&work, &["config", "user.name", "test"]);
        git(&work, &["config", "user.email", "test@example.com"]);
        std::fs::write(work.join("f.txt"), "one\n").expect("write");
        git(&work, &["add", "."]);
        git(&work, &["commit", "-m", "init"]);
        git(&work, &["push", "-u", "origin", "main"]);

        let other = dir.path().join("other");
        std::fs::create_dir_all(&other).expect("other");
        git(
            &other,
            &["clone", origin.to_str().expect("origin path"), "."],
        );
        git(&other, &["config", "user.name", "test"]);
        git(&other, &["config", "user.email", "test@example.com"]);
        std::fs::write(other.join("g.txt"), "two\n").expect("write");
        git(&other, &["add", "."]);
        git(&other, &["commit", "-m", "second"]);
        git(&other, &["push", "origin", "main"]);

        preflight_repo(&work, true).await.expect("preflight");
        assert!(
            work.join("g.txt").exists(),
            "preflight must fetch origin/main"
        );
        assert_eq!(
            git_output(&work, &["rev-parse", "HEAD"]).trim(),
            git_output(&work, &["rev-parse", "origin/main"]).trim()
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn first_claim_after_fetch_uses_new_forbidden_paths() {
        assert_first_claim_uses_new_config(true).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn review_first_private_claim_adopts_tracked_guard_and_command() {
        assert_first_claim_uses_new_config(false).await;
    }

    async fn assert_first_claim_uses_new_config(initially_tracked: bool) {
        let dir = tempfile::tempdir().expect("tempdir");
        let origin = dir.path().join("origin");
        init_git_repo(&origin);
        let config = serde_json::json!({
            "repo": "example.com/fresh-config",
            "failing_check": {"command": "exit 1", "timeout_secs": 10},
            "forbidden_paths": []
        });
        if initially_tracked {
            std::fs::write(origin.join("nuwa.repo.json"), config.to_string()).expect("config");
        }
        std::fs::write(origin.join("calc.go"), "for i := 0; i <= len(xs); i++ {\n")
            .expect("source");
        git(&origin, &["add", "."]);
        git(&origin, &["commit", "-m", "initial repair config"]);
        let repo = dir.path().join("runner");
        git(
            dir.path(),
            &["clone", origin.to_str().expect("origin"), "runner"],
        );
        if !initially_tracked {
            std::fs::write(repo.join("nuwa.repo.json"), config.to_string())
                .expect("private config");
        }
        let sessions = dir.path().join("sessions");
        let mut trusted = instructions::TrustedInstructions::load(
            &repo,
            &repo.join("nuwa.repo.json"),
            "main",
            &sessions.join("_trusted"),
        )
        .expect("trusted config");
        assert_eq!(
            trusted.source() == instructions::TrustSource::Tracked,
            initially_tracked
        );
        let mut updated = config;
        updated["failing_check"]["command"] = serde_json::json!(fixed_loop_check());
        updated["forbidden_paths"] = serde_json::json!(["fixed.marker"]);
        std::fs::write(origin.join("nuwa.repo.json"), updated.to_string()).expect("new config");
        git(&origin, &["add", "nuwa.repo.json"]);
        git(&origin, &["commit", "-m", "protect marker"]);
        trusted.refresh().expect("refresh cached remote");
        let store = Arc::new(shanhaijing::Store::open(dir.path().join("s.db")).expect("store"));
        store
            .report_signal(
                &shanhaijing::Signal {
                    repo: "example.com/fresh-config".into(),
                    language: "go".into(),
                    title: "panic".into(),
                    severity: "high".into(),
                    message: None,
                    log_window: Vec::new(),
                    stack_trace: "panic: bounds\nexample.com/app.F()\n /app/a.go:1".into(),
                },
                now_ms(),
            )
            .expect("signal");
        work_once(
            &store,
            &mut trusted,
            &repo,
            sessions,
            repair::ProviderKind::Faux,
            10,
            false,
            false,
            5,
            1_000_000,
            5_000_000,
            50_000_000,
            10,
            "fresh:runner",
            Arc::new(tokio::sync::Mutex::new(RunnerLive::new())),
            &Arc::new(secrets::Secrets::default()),
        )
        .await
        .expect("work cycle");
        assert_eq!(trusted.source(), instructions::TrustSource::Tracked);
        assert_eq!(
            trusted.instructions.failing_check.command,
            fixed_loop_check()
        );
        let row = store.list_issues().expect("issues").remove(0);
        assert_eq!(
            row.state, "new",
            "the first run must enforce the fetched base's forbidden paths and schedule a retry"
        );
        assert_eq!(row.attempts, 1);
        assert!(!repo.join("fixed.marker").exists());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn provider_stand_down_starts_after_the_probe_finishes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        init_git_repo(&repo);
        std::fs::write(repo.join("probe.cjs"), "Atomics.wait(new Int32Array(new SharedArrayBuffer(4)),0,0,300);require('fs').writeFileSync('probe-finished',String(Date.now()));process.exit(1);").expect("probe script");
        git(&repo, &["add", "probe.cjs"]);
        git(&repo, &["commit", "-m", "slow preflight probe"]);
        let cfg = RepoInstructions {
            repo: "example.com/backoff".into(),
            failing_check: FailingCheck {
                command: "node probe.cjs".into(),
                target: None,
                timeout_secs: 10,
            },
            forbidden_paths: Vec::new(),
            notes: String::new(),
            skills: Vec::new(),
            reproduce_check: None,
            max_attempts: 3,
            draft_first_n: 0,
        };
        let store = Arc::new(shanhaijing::Store::open(dir.path().join("s.db")).expect("store"));
        store
            .report_signal(
                &shanhaijing::Signal {
                    repo: cfg.repo.clone(),
                    language: "node".into(),
                    title: "failure".into(),
                    severity: "high".into(),
                    message: None,
                    log_window: Vec::new(),
                    stack_trace: "failure at calculate".into(),
                },
                now_ms(),
            )
            .expect("signal");
        let live = Arc::new(tokio::sync::Mutex::new(RunnerLive::new()));
        let worked = work_once(
            &store,
            &mut test_trusted_instructions(&repo, &cfg),
            &repo,
            dir.path().join("sessions"),
            repair::ProviderKind::Anthropic { model: None },
            10,
            false,
            false,
            5,
            1_000_000,
            5_000_000,
            50_000_000,
            10,
            "backoff:runner",
            Arc::clone(&live),
            &Arc::new(secrets::Secrets::default()),
        )
        .await
        .expect("work cycle");
        assert!(!worked);
        let probe_finished: i64 = std::fs::read_to_string(repo.join("probe-finished"))
            .expect("probe timestamp")
            .parse()
            .expect("timestamp");
        assert!(
            live.lock().await.stand_down_until_ms >= probe_finished + 15 * 60_000,
            "preflight time must not consume failure backoff"
        );
        let fp = fp_of(&store, 0);
        assert_eq!(store.get(&fp).expect("issue").expect("exists").attempts, 0);
    }

    #[test]
    fn session_cleanup_removes_expired_fingerprint_dirs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("sessions");
        std::fs::create_dir_all(root.join("_trusted")).expect("trusted");
        let store = shanhaijing::Store::open(dir.path().join("s.db")).expect("open");
        let now = now_ms();

        let mut fps = Vec::new();
        for (i, title) in ["old session", "new session"].iter().enumerate() {
            let signal = shanhaijing::Signal {
                repo: "example.com/spike".into(),
                language: "go".into(),
                title: (*title).into(),
                severity: "high".into(),
                message: None,
                log_window: Vec::new(),
                stack_trace: format!(
                    "panic: {title}\ngoroutine 1 [running]:\nexample.com/spike.F{i}()\n\t/app/a.go:1 +0x1"
                ),
            };
            store.report_signal(&signal, now).expect("report");
            let fp = store
                .list_issues()
                .expect("issues")
                .into_iter()
                .find(|issue| issue.title == *title)
                .expect("issue")
                .fingerprint;
            let short = issue::short_fingerprint(&fp);
            std::fs::create_dir_all(root.join(&short)).expect("session dir");
            store
                .start_run(&shanhaijing::RunStart {
                    id: format!("run-{i}"),
                    fingerprint: fp.clone(),
                    runner_id: "test:runner".into(),
                    repo: "example.com/spike".into(),
                    started_at: if i == 0 { now - 2 * 86_400_000 } else { now },
                    provider: None,
                    model: None,
                })
                .expect("start");
            fps.push(short);
        }

        let removed = cleanup_sessions(&root, &store, now, 1).expect("cleanup");
        assert_eq!(removed, 1);
        assert!(
            !root.join(&fps[0]).exists(),
            "old session dir must be removed"
        );
        assert!(root.join(&fps[1]).exists(), "new session dir must remain");
        assert!(
            root.join("_trusted").exists(),
            "trusted config must survive"
        );
    }

    #[tokio::test]
    async fn cleanup_terminal_branches_removes_only_terminal() {
        use shanhaijing::State;

        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        init_git_repo(&repo);
        let store = shanhaijing::Store::open(dir.path().join("s.db")).expect("open");
        let now = now_ms();

        let mut states = Vec::new();
        for (i, target) in ["merged", "rejected", "escalated", "new"]
            .iter()
            .enumerate()
        {
            let title = format!("cleanup {i}");
            let signal = shanhaijing::Signal {
                repo: "example.com/spike".into(),
                language: "go".into(),
                title: title.clone(),
                severity: "high".into(),
                message: None,
                log_window: Vec::new(),
                stack_trace: format!(
                    "panic: cleanup {i}\ngoroutine 1 [running]:\nexample.com/spike.F{i}()\n\t/app/a.go:1 +0x1"
                ),
            };
            store.report_signal(&signal, now).expect("report");
            let fp = store
                .list_issues()
                .expect("issues")
                .into_iter()
                .find(|row| row.title == title)
                .expect("issue")
                .fingerprint;
            match *target {
                "merged" => {
                    store
                        .set_state(&fp, State::Triggered, now, None)
                        .expect("triggered");
                    store
                        .set_state(&fp, State::Fixing, now, None)
                        .expect("fixing");
                    store
                        .set_state(&fp, State::PrOpen, now, None)
                        .expect("pr_open");
                    store
                        .set_state(&fp, State::Merged, now, None)
                        .expect("merged");
                }
                "rejected" => {
                    store
                        .set_state(&fp, State::Triggered, now, None)
                        .expect("triggered");
                    store
                        .set_state(&fp, State::Fixing, now, None)
                        .expect("fixing");
                    store
                        .set_state(&fp, State::PrOpen, now, None)
                        .expect("pr_open");
                    store
                        .set_state(&fp, State::Rejected, now, None)
                        .expect("rejected");
                }
                "escalated" => {
                    store
                        .set_state(&fp, State::Triggered, now, None)
                        .expect("triggered");
                    store
                        .set_state(&fp, State::Escalated, now, None)
                        .expect("escalated");
                }
                _ => {}
            }
            let branch = format!("agent/fix/{}", issue::short_fingerprint(&fp));
            git(&repo, &["branch", &branch]);
            states.push((branch, *target));
        }

        let removed = cleanup_terminal_branches(&store, &repo, false)
            .await
            .expect("cleanup");
        assert_eq!(removed, 3, "only terminal branches are removed");
        for (branch, target) in states {
            let exists = git_ops::branch_exists(&repo, &branch).expect("exists");
            assert_eq!(exists, target == "new", "{branch} ({target})");
        }
    }

    #[tokio::test]
    async fn remove_stale_branch_deletes_local_branch() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        init_git_repo(&repo);
        git(&repo, &["branch", "agent/fix/fp"]);
        assert!(git_ops::branch_exists(&repo, "agent/fix/fp").expect("exists"));
        let removed = remove_stale_branch(&repo, "fp", false)
            .await
            .expect("remove");
        assert!(removed);
        assert!(!git_ops::branch_exists(&repo, "agent/fix/fp").expect("gone"));
    }

    // The harness calls `block_in_place`, which panics on the default
    // current-thread test runtime.
    #[tokio::test(flavor = "multi_thread")]
    async fn faux_run_writes_run_telemetry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).expect("repo");
        std::fs::write(
            repo.join("calc.go"),
            "package main\n\nfunc Average(xs []float64) float64 {\n\
             \tvar sum float64\n\
             \tfor i := 0; i <= len(xs); i++ {\n\
             \t\tsum += xs[i]\n\
             \t}\n\
             \treturn sum / float64(len(xs))\n\
             }\n",
        )
        .expect("write");
        git(&repo, &["init", "-b", "main"]);
        git(&repo, &["add", "."]);
        git(
            &repo,
            &[
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "init",
            ],
        );

        let store = Arc::new(shanhaijing::Store::open(dir.path().join("s.db")).expect("open"));
        let now = now_ms();
        let signal = shanhaijing::Signal {
            repo: "example.com/spike".into(),
            language: "go".into(),
            title: "panic in Average".into(),
            severity: "high".into(),
            message: None,
            log_window: Vec::new(),
            stack_trace: "panic: boom\ngoroutine 1 [running]:\nexample.com/spike.Average()\n\t/app/calc.go:17 +0x1".into(),
        };
        store.report_signal(&signal, now).expect("report");

        let cfg = RepoInstructions {
            repo: "example.com/spike".into(),
            failing_check: FailingCheck {
                command: fixed_loop_check(),
                target: None,
                timeout_secs: 600,
            },
            forbidden_paths: Vec::new(),
            notes: String::new(),
            skills: Vec::new(),
            reproduce_check: None,
            max_attempts: 3,
            draft_first_n: 0,
        };
        let live: SharedLive = Arc::new(tokio::sync::Mutex::new(RunnerLive::new()));
        let worked = work_once(
            &store,
            &mut test_trusted_instructions(&repo, &cfg),
            &repo,
            dir.path().join("sessions"),
            repair::ProviderKind::Faux,
            5,
            false,
            false,
            5,
            1_000_000,
            5_000_000,
            50_000_000,
            600,
            "test:runner",
            Arc::clone(&live),
            &Arc::new(secrets::Secrets::default()),
        )
        .await
        .expect("work");
        assert!(worked);

        let issue = store.list_issues().expect("issues").pop().expect("issue");
        let run = store
            .latest_run(&issue.fingerprint)
            .expect("latest")
            .expect("run");
        assert_eq!(run.phase, "done");
        assert_eq!(run.outcome.as_deref(), Some("success"));
        assert!(run.turns >= 1, "faux run must record at least one turn");
        assert!(run.session_dir.is_some());
        assert!(
            Path::new(run.session_dir.as_deref().expect("session dir")).is_absolute(),
            "the transcript endpoint requires an absolute session dir: {:?}",
            run.session_dir
        );
        assert!(run.harness_run_id.is_some());
        assert!(
            !run.verification_detail.as_deref().unwrap_or("").is_empty(),
            "verification evidence must land on the run"
        );
        assert!(
            !run.change_stat.as_deref().unwrap_or("").is_empty(),
            "diff stat must land on the run"
        );
        assert!(run.verification_ok == Some(true));
        assert!(run
            .branch
            .as_deref()
            .unwrap_or("")
            .starts_with("agent/fix/"));
        assert_eq!(run.runner_id, "test:runner");
        // 交付后工作树已切回 main；修复内容在 agent 分支上取证。
        let branch = run.branch.clone().expect("branch");
        let fixed = git_output(&repo, &["show", &format!("{branch}:calc.go")]);
        assert!(fixed.contains("i < len(xs)"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn failure_budget_retries_then_ignores_at_the_attempt_cap() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).expect("repo");
        std::fs::write(
            repo.join("calc.go"),
            "package main\n\nfunc Average(xs []float64) float64 {\n\
             \tvar sum float64\n\
             \tfor i := 0; i <= len(xs); i++ {\n\
             \t\tsum += xs[i]\n\
             \t}\n\
             \treturn sum / float64(len(xs))\n\
             }\n",
        )
        .expect("write");
        git(&repo, &["init", "-b", "main"]);
        git(&repo, &["add", "."]);
        git(
            &repo,
            &[
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "init",
            ],
        );

        let store = Arc::new(shanhaijing::Store::open(dir.path().join("s.db")).expect("open"));
        let now = now_ms();
        let signal = shanhaijing::Signal {
            repo: "example.com/spike".into(),
            language: "go".into(),
            title: "panic in Average".into(),
            severity: "high".into(),
            message: None,
            log_window: Vec::new(),
            stack_trace: "panic: boom\ngoroutine 1 [running]:\nexample.com/spike.Average()\n\t/app/calc.go:17 +0x1".into(),
        };
        store.report_signal(&signal, now).expect("report");
        let fp = store.list_issues().expect("issues")[0].fingerprint.clone();
        let cfg = RepoInstructions {
            repo: "example.com/spike".into(),
            failing_check: FailingCheck {
                command: fixed_loop_check(),
                target: None,
                timeout_secs: 600,
            },
            forbidden_paths: Vec::new(),
            notes: String::new(),
            skills: Vec::new(),
            reproduce_check: None,
            max_attempts: 2,
            draft_first_n: 0,
        };
        let live: SharedLive = Arc::new(tokio::sync::Mutex::new(RunnerLive::new()));

        for expected in ["new", "ignored"] {
            work_once(
                &store,
                &mut test_trusted_instructions(&repo, &cfg),
                &repo,
                dir.path().join("sessions"),
                repair::ProviderKind::Faux,
                5,
                false,
                false,
                5,
                0,
                5_000_000,
                50_000_000,
                600,
                "test:runner",
                Arc::clone(&live),
                &Arc::new(secrets::Secrets::default()),
            )
            .await
            .expect("work");
            let issue = store.list_issues().expect("issues").pop().expect("issue");
            assert_eq!(issue.state, expected);
            store
                .set_cooldown(&fp, 0)
                .expect("retry immediately in test");
        }
        assert_eq!(store.attempts_for(&fp).expect("attempts"), 2);
    }

    #[test]
    fn failure_recovery_without_pr_consumes_an_attempt() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = shanhaijing::Store::open(dir.path().join("s.db")).expect("open");
        let now = now_ms();
        store
            .report_signal(
                &shanhaijing::Signal {
                    repo: "example.com/spike".into(),
                    language: "go".into(),
                    title: "panic in Average".into(),
                    severity: "high".into(),
                    message: None,
                    log_window: Vec::new(),
                    stack_trace: "panic: boom\ngoroutine 1 [running]:\nexample.com/spike.Average()\n\t/app/calc.go:17 +0x1".into(),
                },
                now,
            )
            .expect("report");
        let fp = store.list_issues().expect("issues")[0].fingerprint.clone();
        store
            .set_state(&fp, shanhaijing::State::Triggered, now, None)
            .expect("triggered");

        record_delivery_recovery_failure(&store, &fp, 2, now).expect("failure outcome");

        let issue = store.list_issues().expect("issues").pop().expect("issue");
        assert_eq!(issue.state, "new");
        assert_eq!(issue.attempts, 1);
        assert!(store.ready_issue(now).expect("cooldown").is_none());
    }

    #[tokio::test]
    async fn attempt_cap_ignores_before_repair() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        init_git_repo(&repo);
        let store = Arc::new(shanhaijing::Store::open(dir.path().join("s.db")).expect("open"));
        let now = now_ms();
        let signal = shanhaijing::Signal {
            repo: "example.com/spike".into(),
            language: "go".into(),
            title: "panic in Average".into(),
            severity: "high".into(),
            message: None,
            log_window: Vec::new(),
            stack_trace: "panic: boom\ngoroutine 1 [running]:\nexample.com/spike.Average()\n\t/app/calc.go:17 +0x1".into(),
        };
        store.report_signal(&signal, now).expect("report");
        let fp = store.list_issues().expect("issues")[0].fingerprint.clone();
        for _ in 0..3 {
            store.increment_attempts(&fp).expect("attempt");
        }

        let cfg = RepoInstructions {
            repo: "example.com/spike".into(),
            failing_check: FailingCheck {
                command: "true".into(),
                target: None,
                timeout_secs: 600,
            },
            forbidden_paths: Vec::new(),
            notes: String::new(),
            skills: Vec::new(),
            reproduce_check: None,
            max_attempts: 3,
            draft_first_n: 0,
        };
        let worked = work_once(
            &store,
            &mut test_trusted_instructions(&repo, &cfg),
            &repo,
            dir.path().join("sessions"),
            repair::ProviderKind::Faux,
            5,
            false,
            false,
            5,
            1_000_000,
            5_000_000,
            50_000_000,
            600,
            "test:runner",
            Arc::new(tokio::sync::Mutex::new(RunnerLive::new())),
            &Arc::new(secrets::Secrets::default()),
        )
        .await
        .expect("work");
        assert!(worked);
        let row = store
            .list_issues()
            .expect("issues")
            .into_iter()
            .find(|row| row.fingerprint == fp)
            .expect("issue");
        assert_eq!(row.state, "ignored");
        assert!(
            store.latest_run(&fp).expect("latest").is_none(),
            "attempt cap must stop before any repair run"
        );
    }

    #[test]
    fn prior_context_includes_owner_note_and_last_run() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = shanhaijing::Store::open(dir.path().join("s.db")).expect("open");
        let now = now_ms();
        let signal = shanhaijing::Signal {
            repo: "example.com/spike".into(),
            language: "go".into(),
            title: "panic in Average".into(),
            severity: "high".into(),
            message: None,
            log_window: Vec::new(),
            stack_trace: "panic: boom\ngoroutine 1 [running]:\nexample.com/spike.Average()\n\t/app/calc.go:17 +0x1".into(),
        };
        store.report_signal(&signal, now).expect("report");
        let fp = store.list_issues().expect("issues")[0].fingerprint.clone();
        store
            .start_run(&shanhaijing::RunStart {
                id: "run-prior".into(),
                fingerprint: fp.clone(),
                runner_id: "test:runner".into(),
                repo: "example.com/spike".into(),
                started_at: now,
                provider: Some("faux".into()),
                model: Some("faux".into()),
            })
            .expect("start");
        store
            .update_run(
                "run-prior",
                &shanhaijing::RunUpdate {
                    root_cause: Some("ROOT CAUSE: off-by-one loop".into()),
                    ..Default::default()
                },
            )
            .expect("update");
        store
            .finish_run(
                "run-prior",
                &shanhaijing::RunFinish {
                    finished_at: now + 1,
                    outcome: "repair_failed".into(),
                    ci_verdict: None,
                    error: Some("verification gate failed".into()),
                },
            )
            .expect("finish");
        store
            .ignore_issue(&fp, "not a bug", "owner", now + 2)
            .expect("ignore");
        store
            .reopen_issue(&fp, now + 3, Some("retry after upstream deploy"))
            .expect("reopen");

        store
            .record_repair_memory(&shanhaijing::RepairMemoryEntry {
                repo: "example.com/spike".into(),
                fingerprint: "other-fp".into(),
                commit_sha: "0123456789abcdef0123456789abcdef01234567".into(),
                pr_url: Some("https://github.com/acme/spike/pull/7".into()),
                root_cause: "ROOT CAUSE: nil map guard".into(),
                change_stat: " calc.go | 2 +-".into(),
                files: 1,
                recorded_at: now,
            })
            .expect("memory");
        store
            .record_repair_memory(&shanhaijing::RepairMemoryEntry {
                repo: "example.com/spike".into(),
                fingerprint: fp.clone(),
                commit_sha: "feedface".into(),
                pr_url: None,
                root_cause: "ROOT CAUSE: current issue memory must be excluded".into(),
                change_stat: String::new(),
                files: 1,
                recorded_at: now,
            })
            .expect("current memory");

        let context = build_prior_context(
            &store,
            "example.com/spike",
            &fp,
            &secrets::Secrets::default(),
        );
        assert!(context.contains("OWNER NOTE"), "{context}");
        assert!(context.contains("retry after upstream deploy"), "{context}");
        assert!(context.contains("PRIOR ATTEMPTS"), "{context}");
        assert!(context.contains("ROOT CAUSE: off-by-one loop"), "{context}");
        assert!(context.contains("VERIFIED FIXES IN THIS REPO"), "{context}");
        assert!(
            context.contains("https://github.com/acme/spike/pull/7"),
            "{context}"
        );
        assert!(
            !context.contains("current issue memory must be excluded"),
            "the issue's own memory duplicates PRIOR ATTEMPTS: {context}"
        );
        assert!(context.contains("verification gate failed"), "{context}");
    }

    // Process-level evidence: `run_work` itself must register the runner,
    // record the finished run, and reset a crashed in-flight run at startup.
    // The faux test above bypasses all of that by calling `work_once` directly.
    #[tokio::test(flavor = "multi_thread")]
    async fn run_work_registers_and_marks_interrupted() {
        use shanhaijing::{State, Store};

        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).expect("repo");
        std::fs::write(
            repo.join("calc.go"),
            "package main\n\nfunc Average(xs []float64) float64 {\n\
             \tvar sum float64\n\
             \tfor i := 0; i <= len(xs); i++ {\n\
             \t\tsum += xs[i]\n\
             \t}\n\
             \treturn sum / float64(len(xs))\n\
             }\n",
        )
        .expect("calc");
        let instructions = serde_json::json!({
            "repo": "example.com/spike",
            "failing_check": { "command": fixed_loop_check() },
            "forbidden_paths": [],
            "notes": ""
        });
        std::fs::write(repo.join("nuwa.repo.json"), instructions.to_string())
            .expect("instructions");
        git(&repo, &["init", "-b", "main"]);
        git(&repo, &["add", "."]);
        git(
            &repo,
            &[
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "init",
            ],
        );

        let db = dir.path().join("m1b.db");
        let sessions = dir.path().join("sessions");
        let now = now_ms();
        let store = Store::open(&db).expect("open");
        let sig_a = shanhaijing::Signal {
            repo: "example.com/spike".into(),
            language: "go".into(),
            title: "panic in Average".into(),
            severity: "high".into(),
            message: None,
            log_window: Vec::new(),
            stack_trace: "panic: boom\ngoroutine 1 [running]:\nexample.com/spike.Average()\n\t/app/calc.go:17 +0x1".into(),
        };
        let sig_b = shanhaijing::Signal {
            title: "panic in Pipeline".into(),
            log_window: Vec::new(),
            stack_trace: "panic: boom\ngoroutine 1 [running]:\nexample.com/spike.Pipeline()\n\t/app/calc.go:25 +0x1".into(),
            ..sig_a.clone()
        };
        store.report_signal(&sig_a, now).expect("a");
        store.report_signal(&sig_b, now + 1).expect("b");
        let fp_a = store
            .ready_issue(now)
            .expect("ready")
            .expect("a")
            .fingerprint;
        // B is newer; A is picked first by last_seen ASC. Identify B separately.
        let fp_b = store
            .list_issues()
            .expect("issues")
            .into_iter()
            .find(|i| i.fingerprint != fp_a)
            .expect("b")
            .fingerprint;
        drop(store);

        let args = |db: &std::path::Path, repo: &std::path::Path, sessions: &std::path::Path| {
            vec![
                "--db".to_string(),
                db.display().to_string(),
                "--instructions".to_string(),
                repo.join("nuwa.repo.json").display().to_string(),
                "--sessions".to_string(),
                sessions.display().to_string(),
                "--provider".to_string(),
                "faux".to_string(),
                "--runner-id".to_string(),
                "test:runner".to_string(),
                "--interval".to_string(),
                "5".to_string(),
                "--max-turns".to_string(),
                "5".to_string(),
                "--max-tokens".to_string(),
                "1000000".to_string(),
            ]
        };

        run_work(args(&db, &repo, &sessions))
            .await
            .expect("first run");

        let store = Store::open(&db).expect("reopen");
        let runners = store.list_runners().expect("runners");
        assert!(
            runners.iter().any(|r| r.id == "test:runner"),
            "runner must be registered"
        );
        let run_a = store.latest_run(&fp_a).expect("latest a").expect("run a");
        assert_eq!(run_a.outcome.as_deref(), Some("success"));
        assert_eq!(run_a.runner_id, "test:runner");
        let session_dir = run_a.session_dir.as_deref().expect("run a session dir");
        assert!(
            Path::new(session_dir).is_absolute(),
            "run_work must persist an absolute session dir: {session_dir}"
        );

        // Simulate a crashed in-flight run for B, owned by the same runner.
        store
            .set_state(&fp_b, State::Triggered, now + 2, None)
            .expect("t");
        store
            .set_state(&fp_b, State::Fixing, now + 3, None)
            .expect("f");
        store
            .set_state(&fp_b, State::Escalated, now + 4, None)
            .expect("e");
        store
            .start_run(&shanhaijing::RunStart {
                id: "open-run".into(),
                fingerprint: fp_b.clone(),
                runner_id: "test:runner".into(),
                repo: "example.com/spike".into(),
                started_at: now + 5,
                provider: Some("faux".into()),
                model: Some("faux".into()),
            })
            .expect("open run");
        drop(store);

        run_work(args(&db, &repo, &sessions))
            .await
            .expect("second run");

        let store = Store::open(&db).expect("reopen2");
        let run_b = store.latest_run(&fp_b).expect("latest b").expect("run b");
        assert_eq!(run_b.outcome.as_deref(), Some("interrupted"));
        assert!(run_b.finished_at.is_some());
    }
}
