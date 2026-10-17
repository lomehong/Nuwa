mod api;
mod ci;
mod deliver;
mod guard;
mod instructions;
mod issue;
mod repair;
mod serve;
mod session;
mod settings;
mod ui;
mod web;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context};

/// serve 各路由共享的状态：（问题单库，UI 鉴权令牌）。
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
                    cli.provider = repair::ProviderKind::Zai { base_url: None, model: None }
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
    let mut argv = std::env::args().skip(1);
    match argv.next().as_deref() {
        Some("fix") => run_fix().await,
        Some("ingest") => run_ingest(argv.collect()),
        Some("work") => run_work(argv.collect()).await,
        Some("serve") => serve::run_serve(argv.collect()).await,
        Some("status") => run_status(argv.collect()),
        Some("sync") => run_sync(argv.collect()).await,
        Some("reopen") => run_reopen(argv.collect()),
        Some("migrate-env") => run_migrate_env(argv.collect()),
        other => bail!(
            "usage: nuwa fix --issue <path> --repo <path> [--provider anthropic|faux] [--model <id>] [--max-turns N] [--push] [--pr] [--no-deliver]\n            nuwa ingest --signal <path> [--db <path>]\n            nuwa work --db <path> --instructions <path> [--provider anthropic|faux] [--runner-id <id>] [--service-unit <unit>] [--push] [--pr] [--loop --interval N]\n            nuwa serve --db <path> [--listen 127.0.0.1:7321]   (HTTP ingest for 青鸟 HTTPSink)\n            nuwa status [--db <path>]\n            nuwa sync --db <path>   (pr_open 问题单与 GitHub 对账：merged→merged, closed→rejected)\n            nuwa reopen --db <path> --fingerprint <fp>   (owner reviewed an escalation)\n            nuwa migrate-env [--env-file <path>]   (deploy-time env key migration)\n        (got {other:?})"
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
    let mut it = args.into_iter();
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--db" => db_path = PathBuf::from(it.next().context("--db needs a value")?),
            "--fingerprint" => {
                fingerprint = Some(it.next().context("--fingerprint needs a value")?)
            }
            other => bail!("unknown reopen flag {other}"),
        }
    }
    let fp = fingerprint.context("--fingerprint is required")?;
    let store =
        shanhaijing::Store::open(&db_path).with_context(|| format!("open db {}", db_path.display()))?;
    store.set_state_as(&fp, shanhaijing::State::New, now_ms(), Some("reopened by owner"), "owner")?;
    store.set_cooldown(&fp, 0)?;
    println!("reopened   : {fp} — ready for pickup");
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
        println!("env        : no lowercase keys to migrate in {}", env_file.display());
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
                signal_path =
                    Some(std::path::PathBuf::from(it.next().context("--signal needs a value")?))
            }
            "--db" => db_path = std::path::PathBuf::from(it.next().context("--db needs a value")?),
            other => bail!("unknown ingest flag {other}"),
        }
    }
    let signal_path = signal_path.context("--signal <path> is required")?;
    let raw =
        std::fs::read_to_string(&signal_path).with_context(|| format!("read {}", signal_path.display()))?;
    let store =
        shanhaijing::Store::open(&db_path).with_context(|| format!("open db {}", db_path.display()))?;
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
            issue.fingerprint,
            issue.severity,
            issue.title,
            issue.occurrences,
            issue.repo
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
        shanhaijing::ReportOutcome::Regression {
            fingerprint,
            occurrences,
        } => {
            format!("REGRESSION : {fingerprint} reopened (occurrences={occurrences})")
        }
    };
    Ok(line)
}

/// 应龙 runner v0 — claim the highest-priority ready issue from 山海经 and
/// drive it to a verified branch/PR. Failure anywhere after the claim
/// escalates the issue (docs/07 A-1 回炉升级). `--loop` keeps polling
/// (serial runner; the concurrent pool is A-3's remaining half).
async fn run_work(args: Vec<String>) -> anyhow::Result<()> {
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
    let mut max_tokens: u64 = env_d("NUWA_MAX_TOKENS", "1500000").parse().unwrap_or(1_500_000);
    let mut ci_timeout_secs: u64 = env_d("NUWA_CI_TIMEOUT", "600").parse().unwrap_or(600);
    let mut runner_id_given: Option<String> =
        std::env::var("NUWA_RUNNER_ID").ok().filter(|v| !v.is_empty());
    let mut service_unit = std::env::var("NUWA_SERVICE_UNIT")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "nuwa-work".to_string());
    let event_retention_days: u32 = env_d("NUWA_EVENT_RETENTION_DAYS", "90")
        .parse()
        .unwrap_or(90);
    let mut it = args.into_iter();
    while let Some(flag) = it.next() {
        let mut value =
            |name: &str| it.next().with_context(|| format!("missing value for {name}"));
        match flag.as_str() {
            "--db" => db_path = PathBuf::from(value("--db")?),
            "--instructions" => instructions_path = Some(PathBuf::from(value("--instructions")?)),
            "--sessions" => sessions_dir = PathBuf::from(value("--sessions")?),
            "--provider" => match value("--provider")?.as_str() {
                "anthropic" => provider_given = Some(repair::ProviderKind::Anthropic { model: None }),
                "faux" => provider_given = Some(repair::ProviderKind::Faux),
                "zai" => {
                    provider_given =
                        Some(repair::ProviderKind::Zai { base_url: None, model: None })
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
            "--ci-timeout" => ci_timeout_secs = value("--ci-timeout")?.parse()?,
            "--runner-id" => runner_id_given = Some(value("--runner-id")?),
            "--service-unit" => service_unit = value("--service-unit")?,
            other => bail!("unknown work flag {other}"),
        }
    }
    let provider = provider_given.unwrap_or_else(env_provider_default);
    let instructions_path = instructions_path.context("--instructions <path> is required")?;

    let store = Arc::new(
        shanhaijing::Store::open(&db_path)
            .with_context(|| format!("open db {}", db_path.display()))?,
    );
    let (repo_config, repo_dir) = instructions::RepoInstructions::load(&instructions_path)?;

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
    );
    let now = now_ms();
    let _ = store.mark_interrupted(&runner_id, now);
    let _ = store.prune(now, event_retention_days, 365);
    if let Err(e) = store.upsert_runner(&shanhaijing::RunnerUpsert {
        id: runner_id.clone(),
        repo: repo_config.repo.clone(),
        host: std::env::var("HOSTNAME")
            .or_else(|_| std::env::var("COMPUTERNAME"))
            .unwrap_or_else(|_| "host".to_string()),
        pid: Some(std::process::id() as i64),
        started_at: now,
        heartbeat_at: now,
        status: "starting".into(),
        provider: Some(provider_name),
        model: Some(model_name),
        daily_pr_limit: Some(daily_pr_limit as i64),
        repo_path: Some(repo_dir.display().to_string()),
        service_unit: Some(service_unit),
        config_json: config_json.clone(),
    }) {
        eprintln!("runner register failed (telemetry only): {e}");
    }
    let live = Arc::new(tokio::sync::Mutex::new(RunnerLive::new()));
    let heartbeat_interval = std::time::Duration::from_secs(interval_secs.clamp(5, 15));
    let heartbeat = spawn_heartbeat(
        Arc::clone(&store),
        runner_id.clone(),
        heartbeat_interval,
        Arc::clone(&live),
        config_json,
    );
    live_update(&live, "starting", None, None, None).await;

    let result: anyhow::Result<()> = async {
        loop {
            let worked = work_once(
                &store,
                &repo_config,
                &repo_dir,
                sessions_dir.clone(),
                provider.clone(),
                max_turns,
                push,
                create_pr,
                daily_pr_limit,
                max_tokens,
                ci_timeout_secs,
                &runner_id,
                Arc::clone(&live),
            )
            .await?;
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
    result
}

#[derive(Debug)]
struct RunnerLive {
    status: String,
    current_fingerprint: Option<String>,
    phase: Option<String>,
    last_error: Option<String>,
}

impl RunnerLive {
    fn new() -> Self {
        Self {
            status: "starting".into(),
            current_fingerprint: None,
            phase: None,
            last_error: None,
        }
    }
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
    let value = if trimmed.is_empty() { "runner" } else { trimmed };
    value.chars().take(128).collect()
}

fn default_runner_id(repo: &str) -> String {
    let host = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "host".to_string());
    sanitize_runner_id(&format!("{host}:{repo}"))
}

fn provider_meta(provider: &repair::ProviderKind) -> (String, String) {
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
) -> String {
    serde_json::json!({
        "interval_secs": interval_secs,
        "ci_timeout_secs": ci_timeout_secs,
        "max_turns": max_turns,
        "max_tokens": max_tokens,
        "failing_check": repo_config.failing_check.command,
        "forbidden_paths_count": repo_config.forbidden_paths.len(),
    })
    .to_string()
}

async fn live_snapshot(live: &SharedLive) -> (String, Option<String>, Option<String>, Option<String>) {
    let state = live.lock().await;
    (
        state.status.clone(),
        state.current_fingerprint.clone(),
        state.phase.clone(),
        state.last_error.clone(),
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
}

fn spawn_heartbeat(
    store: Arc<shanhaijing::Store>,
    runner_id: String,
    interval: std::time::Duration,
    live: SharedLive,
    config_json: String,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut logged_failure = false;
        loop {
            let (status, fp, phase, error) = live_snapshot(&live).await;
            let store = Arc::clone(&store);
            let runner_id = runner_id.clone();
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

/// One pickup attempt. `Ok(true)` = an issue was claimed and driven to a
/// terminal state this pass; `Ok(false)` = nothing pickable.
#[allow(clippy::too_many_arguments)]
async fn work_once(
    store: &Arc<shanhaijing::Store>,
    repo_config: &instructions::RepoInstructions,
    repo_dir: &Path,
    sessions_dir: PathBuf,
    provider: repair::ProviderKind,
    max_turns: u32,
    push: bool,
    create_pr: bool,
    daily_pr_limit: u64,
    max_tokens: u64,
    ci_timeout_secs: u64,
    runner_id: &str,
    live: SharedLive,
) -> anyhow::Result<bool> {
    let now = now_ms();
    // 活性必须在任何早退之前可见：配额待定/预检失败都不是"进程死了"。
    store.heartbeat(now)?;

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

    // ---- repo preflight: a dirty or off-main checkout wastes a run ----
    // These are environment faults (a previous run left the checkout behind),
    // not issue faults — bail loudly instead of claiming and escalating.
    let head = match deliver::git(repo_dir, &["rev-parse", "--abbrev-ref", "HEAD"]) {
        Ok(head) => head,
        Err(e) => {
            live_update(&live, "error", None, None, Some(format!("{e:#}"))).await;
            return Err(e);
        }
    };
    if head != "main" && head != "master" {
        let message = format!(
            "repo {} is on branch `{}` — runner only works from main/master; reset the checkout",
            repo_dir.display(),
            head
        );
        live_update(&live, "error", None, None, Some(message.clone())).await;
        bail!("{message}");
    }
    let dirty = match deliver::git(
        repo_dir,
        &["status", "--porcelain", "--", ":(exclude)nuwa.repo.json"],
    ) {
        Ok(dirty) => dirty,
        Err(e) => {
            live_update(&live, "error", None, None, Some(format!("{e:#}"))).await;
            return Err(e);
        }
    };
    if !dirty.trim().is_empty() {
        let message = format!(
            "repo {} has an unclean working tree — reset before running:\n{}",
            repo_dir.display(),
            dirty.trim()
        );
        live_update(&live, "error", None, None, Some(message.clone())).await;
        bail!("{message}");
    }

    // ---- claim (atomic: N runners can never share an issue) ----
    let Some(snapshot) =
        store.claim_ready_issue(now, now + shanhaijing::DEFAULT_COOLDOWN_MS)?
    else {
        println!("nothing ready for pickup");
        live_update(&live, "idle", None, None, None).await;
        return Ok(false);
    };
    let run_id = uuid::Uuid::new_v4().to_string();
    let (provider_name, model_name) = provider_meta(&provider);
    if snapshot.repo != repo_config.repo {
        let message = format!(
            "repo mismatch: issue `{}` belongs to `{}`, instructions are for `{}` — refusing (ADR-004)",
            snapshot.fingerprint,
            snapshot.repo,
            repo_config.repo
        );
        live_update(&live, "error", Some(&snapshot.fingerprint), None, Some(message.clone())).await;
        bail!("{message}");
    }
    let fp = snapshot.fingerprint.clone();
    // Build the issue before `start_run` so the deterministic session dir is
    // known for the first lifecycle write.
    let prob = issue::Issue {
        fingerprint: fp.clone(),
        title: snapshot.title.clone(),
        language: snapshot.language.clone(),
        occurrences: snapshot.occurrences,
        severity: snapshot.severity.clone(),
        first_seen: String::new(),
        last_seen: String::new(),
        stack_trace: snapshot.stack_sample.clone(),
        log_window: Vec::new(),
        failing_check: repo_config.failing_check.clone(),
        forbidden_paths: repo_config.forbidden_paths.clone(),
    };
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
    println!(
        "claimed    : {} [{}] {} — {}x\nrepo       : {}\nprovider   : {}",
        fp,
        snapshot.severity,
        snapshot.title,
        snapshot.occurrences,
        repo_dir.display(),
        match &provider {
            repair::ProviderKind::Faux => "faux".to_string(),
            repair::ProviderKind::Anthropic { model } => match model {
                Some(m) => format!("anthropic:{m}"),
                None => "anthropic:<catalog default>".to_string(),
            },
            repair::ProviderKind::Zai { model, .. } => match model {
                Some(m) => format!("zai:{m}"),
                None => "zai:glm-5.3-flash".to_string(),
            },
        },
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
    let guards = std::sync::Arc::new(guard::Guards::new(repo_config.forbidden_paths.clone()));
    let progress_store = Arc::clone(store);
    let progress_run = run_id.clone();
    let progress: Option<Arc<dyn Fn(repair::RepairProgress) + Send + Sync>> =
        Some(Arc::new(move |p| {
            let store = Arc::clone(&progress_store);
            let run_id = progress_run.clone();
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
    let session_dir = sessions_dir.join(prob.short_fingerprint());
    let job = repair::RepairJob {
        repo: repo_dir.to_path_buf(),
        issue: prob,
        sessions_dir,
        provider,
        max_turns,
        guards,
        notes: repo_config.notes.clone(),
        progress,
    };

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
    let report = match repair::run(&job).await {
        Ok(r) => r,
        Err(e) => {
            let detail = bounded_text(&format!("repair_failed: {e:#}"));
            let _ = store.finish_run(
                &run_id,
                &shanhaijing::RunFinish {
                    finished_at: now_ms(),
                    outcome: "repair_failed".into(),
                    ci_verdict: None,
                    error: Some(detail.clone()),
                },
            );
            live_update(&live, "error", Some(&fp), None, Some(detail.clone())).await;
            store.set_state_as(&fp, shanhaijing::State::Escalated, now_ms(), Some(detail.as_str()), "runner")?;
            bail!("repair failed — issue escalated: {e:#}");
        }
    };
    let cause = report
        .root_cause_text
        .lines()
        .find(|l| l.trim_start().starts_with("ROOT CAUSE"))
        .unwrap_or("(no ROOT CAUSE line)");
    println!("root cause : {cause}");

    // 事件排水在 prompt 返回时被 abort，尾部的 TurnEnd 可能缺失；以
    // RepairReport 的累计值/轮数为准覆盖进度回调的中间态。这不是尽力而为
    // 的遥测：证据必须在 run 结束前落库，所以等待阻塞池完成而不占用异步 worker。
    persist_run(
        store,
        &run_id,
        shanhaijing::RunUpdate {
            phase: Some("verify".into()),
            harness_run_id: Some(report.run_id.clone()),
            turns: Some(i64::from(report.turns)),
            tokens_uncached: Some(report.uncached_tokens as i64),
            tokens_cached: Some(report.cached_tokens as i64),
            root_cause: Some(report.root_cause_text.clone()),
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
        let detail = bounded_text(&format!("budget_exceeded: {spent} > {max_tokens}"));
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
        store.set_state_as(&fp, shanhaijing::State::Escalated, now_ms(), Some(detail.as_str()), "runner")?;
        bail!("{detail} — issue escalated");
    }
    println!("tokens     : {spent} (cap {max_tokens})");

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
    let delivered = match deliver::deliver(
        &job.repo,
        &job.issue,
        &report,
        push,
        create_pr,
        &job.sessions_dir,
    )
    .await
    {
        Ok(d) => d,
        Err(e) => {
            let detail = bounded_text(&format!("delivery_failed: {e:#}"));
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
            store.set_state_as(&fp, shanhaijing::State::Escalated, now_ms(), Some(detail.as_str()), "runner")?;
            bail!("delivery failed — issue escalated: {e:#}");
        }
    };

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
        Some(delivered.pr_url.as_deref().unwrap_or("branch pushed, no PR")),
        "runner",
    ) {
        let detail = bounded_text(&format!("delivery_failed: state update: {e:#}"));
        let _ = store.finish_run(
            &run_id,
            &shanhaijing::RunFinish {
                finished_at: now_ms(),
                outcome: "delivery_failed".into(),
                ci_verdict: None,
                error: Some(detail.clone()),
            },
        );
        live_update(&live, "error", Some(&fp), None, Some(detail)).await;
        return Err(e.into());
    }
    if let Some(url) = &delivered.pr_url {
        if let Err(e) = store.set_pr(&fp, url, now_ms()) {
            let detail = bounded_text(&format!("delivery_failed: set_pr: {e:#}"));
            let _ = store.finish_run(
                &run_id,
                &shanhaijing::RunFinish {
                    finished_at: now_ms(),
                    outcome: "delivery_failed".into(),
                    ci_verdict: None,
                    error: Some(detail.clone()),
                },
            );
            live_update(&live, "error", Some(&fp), None, Some(detail)).await;
            return Err(e.into());
        }
    }

    if !create_pr {
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
        let token = match std::env::var("NUWA_GITHUB_TOKEN")
            .or_else(|_| std::env::var("GITHUB_TOKEN"))
        {
            Ok(token) => token,
            Err(_) => {
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
                return Err(anyhow::anyhow!(detail));
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
                    &format!("head {}", &delivered.commit[..12.min(delivered.commit.len())]),
                );
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
                    live_update(&live, "error", Some(&fp), None, Some(detail)).await;
                    return Err(e);
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
                let _ = store.record_event(
                    &fp,
                    now_ms(),
                    Some(&repo_config.repo),
                    "runner",
                    "ci_red",
                    &format!("head {} — PR #{number} closed", &delivered.commit[..12.min(delivered.commit.len())]),
                );
                store.set_state_as(
                    &fp,
                    shanhaijing::State::Rejected,
                    now_ms(),
                    Some(&format!("ci_red: CI red — PR #{number} closed, back to owner")),
                    "runner",
                )?;
                // Trends bucket producer: the state change alone is not a
                // merge/rejection signal (docs/09 §9.2).
                let _ = store.record_event(
                    &fp,
                    now_ms(),
                    Some(&repo_config.repo),
                    "runner",
                    "rejected",
                    &format!("ci_red: PR #{number} closed without merge"),
                );
                live_update(
                    &live,
                    "error",
                    Some(&fp),
                    None,
                    Some(format!("CI red — PR #{number} closed")),
                )
                .await;
                bail!("CI red — PR #{number} closed, issue rejected (regression signal will reopen it)");
            }
            Ok(ci::Verdict::TimedOut) => {
                let _ = store.record_event(&fp, now_ms(), Some(&repo_config.repo), "runner", "ci_timeout", slug);
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
    let store =
        shanhaijing::Store::open(&db_path).with_context(|| format!("open db {}", db_path.display()))?;
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
fn env_provider_default() -> repair::ProviderKind {
    match env_d("NUWA_PROVIDER", "anthropic").as_str() {
        "zai" => repair::ProviderKind::Zai {
            base_url: std::env::var("NUWA_BASE_URL").ok().filter(|s| !s.is_empty()),
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
    let store =
        shanhaijing::Store::open(&db_path).with_context(|| format!("open db {}", db_path.display()))?;
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
    let prob = issue::Issue::load(&cli.issue_path).context("load issue")?;

    if !cli.repo.join(".git").exists() {
        bail!("{} is not a git repository", cli.repo.display());
    }

    println!(
        "== Nuwa fix run ==\nissue     : {}\nrepo      : {}\nprovider  : {}\nmax turns : {}",
        prob.fingerprint,
        cli.repo.display(),
        match &cli.provider {
            repair::ProviderKind::Faux => "faux".to_string(),
            repair::ProviderKind::Anthropic { model } => match model {
                Some(m) => format!("anthropic:{m}"),
                None => "anthropic:<catalog default>".to_string(),
            },
            repair::ProviderKind::Zai { model, .. } => match model {
                Some(m) => format!("zai:{m}"),
                None => "zai:glm-5.3-flash".to_string(),
            },
        },
        cli.max_turns,
    );

    let guards = std::sync::Arc::new(guard::Guards::new(prob.forbidden_paths.clone()));
    let job = repair::RepairJob {
        repo: cli.repo.clone(),
        issue: prob.clone(),
        sessions_dir: cli.sessions_dir.clone(),
        provider: cli.provider,
        max_turns: cli.max_turns,
        guards: guards.clone(),
        notes: String::new(),
        progress: None,
    };
    let report = repair::run(&job).await.context("repair run")?;

    println!(
        "\nrun       : {}\nturns     : {}\ntokens    : {} (cached {})",
        report.run_id, report.turns, report.uncached_tokens, report.cached_tokens
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

    let delivered = deliver::deliver(
        &cli.repo,
        &prob,
        &report,
        cli.push,
        cli.create_pr,
        &cli.sessions_dir,
    )
    .await?;
    println!(
        "\nbranch    : {}\nbase      : {}\ncommit    : {}\npr        : {}",
        delivered.branch,
        delivered.base,
        &delivered.commit[..12.min(delivered.commit.len())],
        delivered.pr_url.as_deref().unwrap_or("-"),
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
    fn runner_id_is_sanitized_and_stable() {
        let id = sanitize_runner_id("host/repo name:日本");
        assert!(id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-')));
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
        assert_eq!(provider_meta(&repair::ProviderKind::Faux), ("faux".into(), "faux".into()));
        assert_eq!(
            provider_meta(&repair::ProviderKind::Zai { base_url: None, model: None }),
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
            stack_trace: "panic: boom\ngoroutine 1 [running]:\nexample.com/spike.F()\n\t/app/a.go:1 +0x1".into(),
        };
        store.report_signal(&signal, now).expect("report");
        let fp = store.ready_issue(now).expect("ready").expect("issue").fingerprint;
        store.set_pr(&fp, "https://github.com/acme/spike/pull/1", now).expect("pr");

        let cfg = RepoInstructions {
            repo: "example.com/spike".into(),
            failing_check: FailingCheck { command: "true".into(), target: None },
            forbidden_paths: Vec::new(),
            notes: String::new(),
        };
        let worked = work_once(
            &store,
            &cfg,
            dir.path(),
            dir.path().join("sessions"),
            repair::ProviderKind::Faux,
            30,
            false,
            true,
            1,
            1_000_000,
            600,
            "test:runner",
            Arc::new(tokio::sync::Mutex::new(RunnerLive::new())),
        )
        .await
        .expect("work_once");

        assert!(!worked, "quota must stand down");
        let hb = store.last_heartbeat().expect("heartbeat").expect("some heartbeat");
        assert!((now_ms() - hb).abs() < 5_000, "heartbeat must be fresh");
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
            &["-c", "user.name=test", "-c", "user.email=test@example.com", "commit", "-m", "init"],
        );

        let store = Arc::new(
            shanhaijing::Store::open(dir.path().join("s.db")).expect("open"),
        );
        let now = now_ms();
        let signal = shanhaijing::Signal {
            repo: "example.com/spike".into(),
            language: "go".into(),
            title: "panic in Average".into(),
            severity: "high".into(),
            message: None,
            stack_trace: "panic: boom\ngoroutine 1 [running]:\nexample.com/spike.Average()\n\t/app/calc.go:17 +0x1".into(),
        };
        store.report_signal(&signal, now).expect("report");

        let cfg = RepoInstructions {
            repo: "example.com/spike".into(),
            failing_check: FailingCheck { command: "echo ok".into(), target: None },
            forbidden_paths: Vec::new(),
            notes: String::new(),
        };
        let live: SharedLive = Arc::new(tokio::sync::Mutex::new(RunnerLive::new()));
        let worked = work_once(
            &store,
            &cfg,
            &repo,
            dir.path().join("sessions"),
            repair::ProviderKind::Faux,
            5,
            false,
            false,
            5,
            1_000_000,
            600,
            "test:runner",
            Arc::clone(&live),
        )
        .await
        .expect("work");
        assert!(worked);

        let issue = store.list_issues().expect("issues").pop().expect("issue");
        let run = store.latest_run(&issue.fingerprint).expect("latest").expect("run");
        assert_eq!(run.phase, "done");
        assert_eq!(run.outcome.as_deref(), Some("success"));
        assert!(run.turns >= 1, "faux run must record at least one turn");
        assert!(run.session_dir.is_some());
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
        assert!(run.branch.as_deref().unwrap_or("").starts_with("agent/fix/"));
        assert_eq!(run.runner_id, "test:runner");
        // 交付后工作树已切回 main；修复内容在 agent 分支上取证。
        let branch = run.branch.clone().expect("branch");
        let fixed = git_output(&repo, &["show", &format!("{branch}:calc.go")]);
        assert!(fixed.contains("i < len(xs)"));
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
        std::fs::write(
            repo.join("nuwa.repo.json"),
            r#"{"repo":"example.com/spike","failing_check":{"command":"echo ok"},"forbidden_paths":[],"notes":""}"#,
        )
        .expect("instructions");
        git(&repo, &["init", "-b", "main"]);
        git(&repo, &["add", "."]);
        git(
            &repo,
            &["-c", "user.name=test", "-c", "user.email=test@example.com", "commit", "-m", "init"],
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
            stack_trace: "panic: boom\ngoroutine 1 [running]:\nexample.com/spike.Average()\n\t/app/calc.go:17 +0x1".into(),
        };
        let sig_b = shanhaijing::Signal {
            title: "panic in Pipeline".into(),
            stack_trace: "panic: boom\ngoroutine 1 [running]:\nexample.com/spike.Pipeline()\n\t/app/calc.go:25 +0x1".into(),
            ..sig_a.clone()
        };
        store.report_signal(&sig_a, now).expect("a");
        store.report_signal(&sig_b, now + 1).expect("b");
        let fp_a = store.ready_issue(now).expect("ready").expect("a").fingerprint;
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

        run_work(args(&db, &repo, &sessions)).await.expect("first run");

        let store = Store::open(&db).expect("reopen");
        let runners = store.list_runners().expect("runners");
        assert!(runners.iter().any(|r| r.id == "test:runner"), "runner must be registered");
        let run_a = store.latest_run(&fp_a).expect("latest a").expect("run a");
        assert_eq!(run_a.outcome.as_deref(), Some("success"));
        assert_eq!(run_a.runner_id, "test:runner");

        // Simulate a crashed in-flight run for B, owned by the same runner.
        store.set_state(&fp_b, State::Triggered, now + 2, None).expect("t");
        store.set_state(&fp_b, State::Fixing, now + 3, None).expect("f");
        store.set_state(&fp_b, State::Escalated, now + 4, None).expect("e");
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

        run_work(args(&db, &repo, &sessions)).await.expect("second run");

        let store = Store::open(&db).expect("reopen2");
        let run_b = store.latest_run(&fp_b).expect("latest b").expect("run b");
        assert_eq!(run_b.outcome.as_deref(), Some("interrupted"));
        assert!(run_b.finished_at.is_some());
    }
}
