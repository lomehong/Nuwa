mod api;
mod benchmark;
mod check;
mod ci;
mod ci_watch;
mod config_activation;
mod deliver;
mod env_store;
mod git_ops;
mod guard;
mod instructions;
mod issue;
mod pipeline;
mod quota;
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
mod workspace;

use std::collections::{BTreeMap, BTreeSet};
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
    db: PathBuf,
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
        db: PathBuf::from("shanhaijing.db"),
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
            "--db" => cli.db = PathBuf::from(value("--db")?),
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

fn main() -> anyhow::Result<()> {
    // Snapshot + scrub before any runtime thread exists: setenv/unsetenv racing
    // getenv/environ/fork is unsafe, and the snapshot is what later reads use.
    env_store::init_from_process_env();
    secrets::Secrets::scrub_os_env();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("build tokio runtime")?;
    let result = runtime.block_on(run_command());
    // Webhook delivery runs in a detached task. The events that matter most
    // (escalated, ci_red) are emitted right before the runner gives up on an
    // issue, and the process may exit immediately afterwards — without this
    // drain they die with the runtime.
    let _ = runtime.block_on(webhook::flush(std::time::Duration::from_secs(10)));
    result
}

async fn run_command() -> anyhow::Result<()> {
    let mut argv = std::env::args().skip(1);
    match argv.next().as_deref() {
        // Heap-pin command futures so verifier/repair state does not accumulate
        // in the dispatch frame on Windows' smaller main-thread stack.
        Some("fix") => Box::pin(run_fix()).await,
        Some("ingest") => run_ingest(argv.collect()),
        Some("work") => Box::pin(run_work(argv.collect())).await,
        Some("serve") => Box::pin(serve::run_serve(argv.collect())).await,
        Some("status") => run_status(argv.collect()),
        Some("sync") => Box::pin(run_sync(argv.collect())).await,
        Some("reopen") => run_reopen(argv.collect()),
        Some("ignore") => run_ignore(argv.collect()),
        Some("check") => Box::pin(check::run(argv.collect())).await,
        Some("benchmark") => Box::pin(benchmark::run(argv.collect())).await,
        Some("migrate-env") => run_migrate_env(argv.collect()),
        other => bail!(
            "usage: nuwa fix --issue <path> --repo <path> [--provider anthropic|faux] [--model <id>] [--max-turns N] [--push] [--pr] [--no-deliver]\n            nuwa ingest --signal <path> [--db <path>] [--trust-observations <source-id>]\n              --trust-observations: owner attests imported observation times and exact environment/release relations are verified; never inferred from JSON or HTTP authentication\n            nuwa work --db <path> --instructions <path> [--provider anthropic|faux] [--runner-id <id>] [--service-unit <unit>] [--pool N] [--push] [--pr] [--loop --interval N]\n            nuwa serve --db <path> [--listen 127.0.0.1:7321]   (HTTP ingest for 青鸟 HTTPSink)\n            nuwa status [--db <path>]\n            nuwa sync --db <path>   (pr_open 问题单与 GitHub 对账：merged→merged, closed→rejected)\n            nuwa reopen --db <path> --fingerprint <fp> [--reason <text>]   (owner reviewed an escalation)\n            nuwa ignore --db <path> --fingerprint <fp> [--reason <text>]   (owner mute / not a bug)\n            nuwa check --instructions <path> [--repo <path>] [--base main] [--json]   (preflight a repo config)\n            nuwa benchmark [run] [--db <path>] [--provider faux|zai|anthropic] [--model <id>] [--scenario <name,...>] [--max-turns N] [--json]   (repair benchmark; `benchmark list` prints scenarios)\n            nuwa migrate-env [--env-file <path>]   (deploy-time env key migration)\n        (got {other:?})"
        ),
    }
}

/// Shared by `ingest` (file) and `serve` (HTTP body): one JSON object or
/// JSONL — the 青鸟 FileSink/HTTPSink wire formats.
pub(crate) fn parse_signals(raw: &str) -> anyhow::Result<Vec<shanhaijing::Signal>> {
    match serde_json::from_str::<shanhaijing::Signal>(raw) {
        Ok(signal) => Ok(vec![signal]),
        Err(_) => raw
            .lines()
            .enumerate()
            .filter(|(_, line)| !line.trim().is_empty())
            .map(|(index, line)| {
                serde_json::from_str(line).map_err(|_| {
                    // Serde errors can include invalid field values. Never
                    // reflect collector metadata in CLI or HTTP errors.
                    anyhow::anyhow!("invalid Signal JSON/JSONL at line {}", index + 1)
                })
            })
            .collect(),
    }
}

/// Preflight the whole batch before any receipts are committed. Store remains
/// authoritative (including source/event conflicts discovered during writes).
pub(crate) fn validate_observations(
    signals: &[shanhaijing::Signal],
    source: Option<&str>,
) -> shanhaijing::ShanResult<()> {
    let fields = signals.iter().flat_map(|signal| {
        [
            (
                signal.environment.as_deref(),
                shanhaijing::MAX_ENVIRONMENT_BYTES,
            ),
            (
                signal.release_sha.as_deref(),
                shanhaijing::MAX_RELEASE_SHA_BYTES,
            ),
            (
                signal.source_event_id.as_deref(),
                shanhaijing::MAX_SOURCE_EVENT_ID_BYTES,
            ),
        ]
    });
    for (value, max) in fields.chain(std::iter::once((
        source,
        shanhaijing::MAX_TRUSTED_SOURCE_BYTES,
    ))) {
        if value.is_some_and(|value| {
            value.trim().is_empty() || value.len() > max || value.chars().any(char::is_control)
        }) {
            return Err(shanhaijing::ShanError::InvalidObservation);
        }
    }
    Ok(())
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
/// Only an owner's explicit `--trust-observations <source-id>` attests that
/// imported times and the exact environment/release relation were verified.
fn run_ingest(args: Vec<String>) -> anyhow::Result<()> {
    let mut signal_path = None;
    let mut db_path = std::path::PathBuf::from("shanhaijing.db");
    let mut trusted_source = None;
    let mut it = args.into_iter();
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--signal" => {
                signal_path = Some(std::path::PathBuf::from(
                    it.next().context("--signal needs a value")?,
                ))
            }
            "--db" => db_path = std::path::PathBuf::from(it.next().context("--db needs a value")?),
            "--trust-observations" => {
                trusted_source = Some(
                    it.next()
                        .context("--trust-observations needs a source-id")?,
                );
            }
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
    validate_observations(&signals, trusted_source.as_deref())?;
    for (index, signal) in signals.iter().enumerate() {
        let line = report_one_with_source(&store, signal, now, trusted_source.as_deref())
            .with_context(|| {
                format!(
                    "ingest stopped after {index}/{} signals; earlier receipts remain committed",
                    signals.len()
                )
            })?;
        println!("{line}");
    }
    if let Some(issue) = store.ready_issue(now)? {
        println!(
            "next up    : {} [{}] {} — {}x, repo {}",
            issue.fingerprint, issue.severity, issue.title, issue.occurrences, issue.repo
        );
    }
    Ok(())
}

/// Untrusted report → the human/HTTP response line. Authentication and JSON
/// fields never attest observation authority.
pub(crate) fn report_one(
    store: &shanhaijing::Store,
    signal: &shanhaijing::Signal,
    now: i64,
) -> anyhow::Result<String> {
    report_one_with_source(store, signal, now, None)
}

/// Owner-attested import path; `source` must come from CLI intent, not JSON.
fn report_one_with_source(
    store: &shanhaijing::Store,
    signal: &shanhaijing::Signal,
    now: i64,
    source: Option<&str>,
) -> anyhow::Result<String> {
    let line = match store.report_signal_with_source(signal, now, source)? {
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
        shanhaijing::ReportOutcome::Delayed {
            fingerprint,
            occurrences,
        } => {
            format!(
                "delayed    : {fingerprint} — terminal state unchanged (occurrences={occurrences})"
            )
        }
        shanhaijing::ReportOutcome::Duplicate {
            fingerprint,
            occurrences,
        } => {
            format!("duplicate  : {fingerprint} (occurrences={occurrences})")
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
    let environment = env_store::vars();
    let process_instance_id = uuid::Uuid::new_v4().to_string();
    let started_at_ms = now_ms();
    webhook::init_from_env();
    run_work_with_environment(
        args,
        environment,
        process_instance_id,
        started_at_ms,
        Arc::new(secrets::Secrets::load()),
    )
    .await
}

/// Injectable startup inputs keep runtime evidence tests off the host's config
/// and credentials. Production captures the environment exactly once above.
async fn run_work_with_environment(
    args: Vec<String>,
    environment: BTreeMap<String, String>,
    process_instance_id: String,
    started_at_ms: i64,
    secrets: Arc<secrets::Secrets>,
) -> anyhow::Result<()> {
    // env 默认（设置页写入 nuwa.env）→ CLI 显式 flag 覆盖。
    let mut db_path = std::path::PathBuf::from("shanhaijing.db");
    let mut instructions_path = None;
    let mut sessions_dir = PathBuf::from("spike/sessions");
    let mut provider_given: Option<repair::ProviderKind> = None;
    let (mut max_turns, turns_source) = startup_number(&environment, "NUWA_MAX_TURNS", 30_u32);
    let mut push = false;
    let mut create_pr = false;
    let mut loop_run = false;
    let (mut interval_secs, interval_source) =
        startup_number(&environment, "NUWA_INTERVAL", 30_u64);
    let (mut pool, pool_source) = startup_number(&environment, "NUWA_POOL", 1_u32);
    let (mut daily_pr_limit, pr_source) =
        startup_number(&environment, "NUWA_DAILY_PR_LIMIT", 5_u64);
    let (mut max_tokens, tokens_source) =
        startup_number(&environment, "NUWA_MAX_TOKENS", 1_500_000_u64);
    let (mut daily_token_limit, daily_source) =
        startup_number(&environment, "NUWA_DAILY_TOKEN_LIMIT", 5_000_000_u64);
    let (mut monthly_token_limit, monthly_source) =
        startup_number(&environment, "NUWA_MONTHLY_TOKEN_LIMIT", 50_000_000_u64);
    let (mut ci_timeout_secs, ci_source) = startup_number(&environment, "NUWA_CI_TIMEOUT", 600_u64);
    let mut runner_id_given = startup_env_opt(&environment, "NUWA_RUNNER_ID");
    let mut service_unit = startup_env_opt(&environment, "NUWA_SERVICE_UNIT")
        .unwrap_or_else(|| "nuwa-work".to_string());
    let event_retention_days = crate::serve::retention_days_from_env(
        environment
            .get("NUWA_EVENT_RETENTION_DAYS")
            .map(String::as_str),
    );
    let mut cli_flags = BTreeSet::new();
    let mut it = args.into_iter();
    while let Some(flag) = it.next() {
        cli_flags.insert(flag.clone());
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
            "--pool" => pool = value("--pool")?.parse()?,
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
    if !(1..=16).contains(&pool) {
        bail!("--pool must be between 1 and 16");
    }
    let cli_provider = provider_given.is_some();
    let provider = provider_given.unwrap_or_else(|| provider_from_environment(&environment));
    let instructions_path = instructions_path.context("--instructions <path> is required")?;
    // One absolute root drives both the pre-repair `runs.session_dir` write
    // (`work_once`) and the harness session dir (`RepairJob`).
    let sessions_dir = absolute_sessions_dir(sessions_dir)?;

    let store = Arc::new(
        shanhaijing::Store::open(&db_path)
            .with_context(|| format!("open db {}", db_path.display()))?,
    );
    let env_path = startup_env_path(&environment);
    // One proof before any worker is spawned; never re-read the env file in
    // run_runner, including when a worker has to wait for its lease.
    let mut startup_config = config_activation::StartupConfig::capture(
        &store,
        &env_path,
        "work",
        process_instance_id,
        started_at_ms,
        &environment,
    );
    for (key, flag, value, source) in [
        (
            "NUWA_MAX_TURNS",
            "--max-turns",
            max_turns.to_string(),
            turns_source,
        ),
        (
            "NUWA_INTERVAL",
            "--interval",
            interval_secs.to_string(),
            interval_source,
        ),
        ("NUWA_POOL", "--pool", pool.to_string(), pool_source),
        (
            "NUWA_DAILY_PR_LIMIT",
            "--daily-pr-limit",
            daily_pr_limit.to_string(),
            pr_source,
        ),
        (
            "NUWA_MAX_TOKENS",
            "--max-tokens",
            max_tokens.to_string(),
            tokens_source,
        ),
        (
            "NUWA_DAILY_TOKEN_LIMIT",
            "--daily-token-limit",
            daily_token_limit.to_string(),
            daily_source,
        ),
        (
            "NUWA_MONTHLY_TOKEN_LIMIT",
            "--monthly-token-limit",
            monthly_token_limit.to_string(),
            monthly_source,
        ),
        (
            "NUWA_CI_TIMEOUT",
            "--ci-timeout",
            ci_timeout_secs.to_string(),
            ci_source,
        ),
    ] {
        set_startup_effective(
            &mut startup_config,
            key,
            Some(value),
            if cli_flags.contains(flag) {
                "cli"
            } else {
                source
            },
        );
    }
    set_startup_effective(
        &mut startup_config,
        "NUWA_SERVICE_UNIT",
        Some(service_unit.clone()),
        if cli_flags.contains("--service-unit") {
            "cli"
        } else if startup_env_opt(&environment, "NUWA_SERVICE_UNIT").is_some() {
            "environment"
        } else {
            "default"
        },
    );
    set_startup_effective(
        &mut startup_config,
        "NUWA_ENV_FILE",
        Some(env_path.to_string_lossy().into_owned()),
        if environment.contains_key("NUWA_ENV_FILE") {
            "environment"
        } else {
            "default"
        },
    );
    let public_url = startup_env_opt(&environment, "NUWA_PUBLIC_BASE_URL");
    let public_source = if public_url.is_some() {
        "environment"
    } else {
        "default"
    };
    set_startup_effective(
        &mut startup_config,
        "NUWA_PUBLIC_BASE_URL",
        public_url,
        public_source,
    );
    record_startup_provider(&mut startup_config, &provider, cli_provider, &environment);
    let repo_dir = instructions_path
        .parent()
        .map(Path::to_path_buf)
        .context("instructions path has no parent")?;
    let base = resolve_base(&repo_dir)?;
    let trusted = instructions::TrustedInstructions::load(
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

    let runner_source = if cli_flags.contains("--runner-id") {
        "cli"
    } else if runner_id_given.is_some() {
        "environment"
    } else {
        "derived"
    };
    let host = environment
        .get("HOSTNAME")
        .or_else(|| environment.get("COMPUTERNAME"))
        .cloned()
        .unwrap_or_else(|| "host".into());
    let runner_id = runner_id_given
        .map(|id| sanitize_runner_id(&id))
        .unwrap_or_else(|| default_runner_id(&repo_config.repo, &host));
    set_startup_effective(
        &mut startup_config,
        "NUWA_RUNNER_ID",
        Some(runner_id.clone()),
        runner_source,
    );
    let leases = Arc::new(std::sync::Mutex::new(Vec::new()));
    let shared_repo = Arc::new(tokio::sync::Mutex::new(()));
    #[cfg(unix)]
    spawn_sigterm_lease_release(Arc::clone(&store), Arc::clone(&leases));
    if pool == 1 {
        return run_runner(RunnerRun {
            store,
            startup_config,
            host,
            trusted,
            repo_dir: repo_dir.clone(),
            config_root: repo_dir,
            sessions_dir,
            provider,
            max_turns,
            push,
            create_pr,
            loop_run,
            interval_secs,
            daily_pr_limit,
            max_tokens,
            daily_token_limit,
            monthly_token_limit,
            ci_timeout_secs,
            runner_id,
            service_unit,
            secrets,
            event_retention_days,
            maintenance: true,
            leases,
            shared_repo,
        })
        .await;
    }

    // In-process pool: worker #1 edits the primary checkout; #2..N get
    // detached worktrees. Each worker owns its runner id, lease, heartbeat and
    // trusted-instruction copy; the DB queue stays shared.
    // Drop registrations left by a crashed pool; new temp paths are unique.
    let _ = git_ops::git(&repo_dir, &["worktree", "prune"]);
    let mut worktrees = Vec::new();
    for i in 2..=pool {
        worktrees.push(
            workspace::RunWorkspace::worktree(&repo_dir, base, &format!("{runner_id}-{i}"))
                .with_context(|| format!("provision pool worktree {i}"))?,
        );
    }
    let mut specs = Vec::with_capacity(pool as usize);
    let mut primary_trusted = Some(trusted);
    for i in 1..=pool {
        let (worker_repo, worker_trusted, worker_id, maintenance) = if i == 1 {
            (
                repo_dir.clone(),
                primary_trusted
                    .take()
                    .context("primary trusted instructions")?,
                runner_id.clone(),
                true,
            )
        } else {
            let worker_trusted = instructions::TrustedInstructions::load(
                &repo_dir,
                &instructions_path,
                base,
                &sessions_dir.join("_trusted").join(format!("worker-{i}")),
            )
            .with_context(|| format!("load trusted instructions for worker {i}"))?;
            if worker_trusted.verify_and_restore()? {
                eprintln!(
                    "preflight   : worker {i} restored {} from the trusted instruction source",
                    worker_trusted.path().display()
                );
            }
            (
                worktrees[(i - 2) as usize].path().to_path_buf(),
                worker_trusted,
                sanitize_runner_id(&format!("{runner_id}-{i}")),
                false,
            )
        };
        specs.push(RunnerRun {
            store: Arc::clone(&store),
            startup_config: startup_config.clone(),
            host: host.clone(),
            trusted: worker_trusted,
            repo_dir: worker_repo,
            config_root: repo_dir.clone(),
            sessions_dir: sessions_dir.clone(),
            provider: provider.clone(),
            max_turns,
            push,
            create_pr,
            loop_run,
            interval_secs,
            daily_pr_limit,
            max_tokens,
            daily_token_limit,
            monthly_token_limit,
            ci_timeout_secs,
            runner_id: worker_id,
            service_unit: service_unit.clone(),
            secrets: Arc::clone(&secrets),
            event_retention_days,
            maintenance,
            leases: Arc::clone(&leases),
            shared_repo: Arc::clone(&shared_repo),
        });
    }
    // Keep the worktrees (and their Drop cleanup) alive until every worker exits.
    let _worktrees = worktrees;
    let results = futures::future::join_all(specs.into_iter().map(|spec| {
        let id = spec.runner_id.clone();
        async move {
            let result = run_runner(spec).await;
            if let Err(error) = &result {
                eprintln!("worker {id}: {error:#}");
            }
            result
        }
    }))
    .await;
    if let Some(error) = results.into_iter().find_map(Result::err) {
        return Err(error);
    }
    Ok(())
}

/// Everything one runner needs. A pool builds several of these with distinct
/// ids, checkouts, and trusted-instruction copies.
struct RunnerRun {
    store: Arc<shanhaijing::Store>,
    startup_config: config_activation::StartupConfig,
    host: String,
    trusted: instructions::TrustedInstructions,
    repo_dir: PathBuf,
    config_root: PathBuf,
    sessions_dir: PathBuf,
    provider: repair::ProviderKind,
    max_turns: u32,
    push: bool,
    create_pr: bool,
    loop_run: bool,
    interval_secs: u64,
    daily_pr_limit: u64,
    max_tokens: u64,
    daily_token_limit: u64,
    monthly_token_limit: u64,
    ci_timeout_secs: u64,
    runner_id: String,
    service_unit: String,
    secrets: Arc<secrets::Secrets>,
    event_retention_days: u32,
    /// Only the first worker prunes retention and runs startup session cleanup.
    maintenance: bool,
    leases: Arc<std::sync::Mutex<Vec<(String, String)>>>,
    /// Serializes fetch/reset/branch-cleanup across workers sharing one repo.
    shared_repo: Arc<tokio::sync::Mutex<()>>,
}

async fn run_runner(spec: RunnerRun) -> anyhow::Result<()> {
    let RunnerRun {
        store,
        mut startup_config,
        host,
        mut trusted,
        repo_dir,
        config_root,
        sessions_dir,
        provider,
        max_turns,
        push,
        create_pr,
        loop_run,
        interval_secs,
        daily_pr_limit,
        max_tokens,
        daily_token_limit,
        monthly_token_limit,
        ci_timeout_secs,
        runner_id,
        service_unit,
        secrets,
        event_retention_days,
        maintenance,
        leases,
        shared_repo,
    } = spec;
    let repo_config = trusted.instructions.clone();
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
    startup_config.runner_id = Some(runner_id.clone());
    startup_config.runner_instance_id = Some(instance_id.clone());
    if startup_config
        .values
        .get("NUWA_RUNNER_ID")
        .and_then(|v| v.value.as_deref())
        != Some(runner_id.as_str())
    {
        set_startup_effective(
            &mut startup_config,
            "NUWA_RUNNER_ID",
            Some(runner_id.clone()),
            "derived",
        );
    }
    // Compare checkouts by canonical path: `--repo a/../repo` and a symlinked
    // alias must not sneak two workers onto the same working tree.
    let repo_path = pipeline::checkout_identity(&repo_dir)?;
    let make_registration = |at: i64| shanhaijing::RunnerUpsert {
        id: runner_id.clone(),
        repo: repo_config.repo.clone(),
        host: host.clone(),
        pid: Some(std::process::id() as i64),
        started_at: startup_config.started_at_ms,
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
    if let Err(error) = persist_runner_startup(&store, &startup_config, now_ms()) {
        let _ = store.release_runner_lease(&runner_id, &instance_id);
        return Err(error);
    }
    leases
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push((runner_id.clone(), instance_id.clone()));
    let now = now_ms();
    let has_pending_delivery = !store.pending_deliveries(&repo_config.repo)?.is_empty();
    if !has_pending_delivery {
        store.mark_interrupted(&runner_id, now)?;
    }
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
    match if has_pending_delivery {
        Ok(0)
    } else {
        store.revive_orphaned_fixing(now, &runner_id)
    } {
        Ok(0) => {}
        Ok(n) => println!("revive      : {n} orphaned fixing issue(s) requeued at startup"),
        Err(e) => eprintln!("warning : revive orphaned fixing failed: {e}"),
    }
    if maintenance {
        let _guard = shared_repo.lock().await;
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
    }
    {
        let _guard = shared_repo.lock().await;
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
    }

    let mut cycles: u64 = 0;
    let result: anyhow::Result<()> = async {
        loop {
            // A delivery can become ambiguous after a crash mid-process, not
            // only at startup. Reconcile before claiming so the next cycle
            // adopts an already-created PR instead of opening a second one.
            {
                let _guard = shared_repo.lock().await;
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
            }
            let worked = match work_once_in(
                &store,
                &mut trusted,
                &repo_dir,
                &config_root,
                shared_repo.as_ref(),
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
                Some(&instance_id),
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
                let _guard = shared_repo.lock().await;
                if let Ok(n) = cleanup_terminal_branches(&store, &repo_dir, push).await {
                    if n > 0 {
                        println!("cleanup     : removed {n} terminal branch(es)");
                    }
                }
            }
            cycles += 1;
            if maintenance && cycles.is_multiple_of(100) {
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
    leases
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .retain(|(id, _)| id != &runner_id);
    result
}

#[derive(Debug)]
/// One active repair in this process. The single-run loop has at most one; a
/// future in-process pool has several.
struct RunLive {
    phase: Option<String>,
}

struct RunnerLive {
    status: String,
    /// Active repairs keyed by fingerprint. The runner row stays single-valued,
    /// so with more than one active run the heartbeat reports neither a
    /// fingerprint nor a phase instead of arbitrarily picking one.
    runs: BTreeMap<String, RunLive>,
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
            runs: BTreeMap::new(),
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

fn default_runner_id(repo: &str, host: &str) -> String {
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
    let (fingerprint, phase) = if state.runs.len() == 1 {
        let (fingerprint, run) = state.runs.first_key_value().expect("one active run");
        (Some(fingerprint.clone()), run.phase.clone())
    } else {
        (None, None)
    };
    (
        state.status.clone(),
        fingerprint,
        phase,
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

/// Final spend is not telemetry: a failed write must stop result handling.
async fn persist_final_usage(
    store: &Arc<shanhaijing::Store>,
    run_id: &str,
    mut usage: repair::RepairProgress,
    report: Option<&repair::RepairReport>,
) -> anyhow::Result<()> {
    if let Some(report) = report {
        usage.turns = usage.turns.max(report.turns);
        usage.uncached_tokens = usage.uncached_tokens.max(report.uncached_tokens);
        usage.cached_tokens = usage.cached_tokens.max(report.cached_tokens);
    }
    let update = shanhaijing::RunUpdate {
        turns: Some(i64::from(usage.turns)),
        tokens_uncached: Some(
            i64::try_from(usage.uncached_tokens).context("uncached usage overflow")?,
        ),
        tokens_cached: Some(i64::try_from(usage.cached_tokens).context("cached usage overflow")?),
        ..Default::default()
    };
    let store = Arc::clone(store);
    let run_id = run_id.to_string();
    tokio::task::spawn_blocking(move || store.update_run(&run_id, &update))
        .await
        .context("persist final usage task")?
        .context("persist final usage")
}

fn accumulate_usage(total: &mut repair::RepairProgress, progress: repair::RepairProgress) {
    total.turns = total.turns.max(progress.turns);
    total.uncached_tokens = total.uncached_tokens.max(progress.uncached_tokens);
    total.cached_tokens = total.cached_tokens.max(progress.cached_tokens);
}

async fn finish_token_gate(
    store: &shanhaijing::Store,
    run_id: &str,
    fp: &str,
    config: &instructions::RepoInstructions,
    gate: quota::TokenGateExceeded,
    live: &SharedLive,
    secrets: &secrets::Secrets,
) -> anyhow::Result<bool> {
    let now = now_ms();
    let detail = bounded_text(&secrets.redact(&format!("budget_exceeded: {gate}")));
    if store
        .pending_deliveries(&config.repo)?
        .iter()
        .any(|delivery| delivery.run_id.as_deref() == Some(run_id))
    {
        // A post-push gate must not requeue this issue or finish its run:
        // recovery still owns the remote evidence and any PR may already exist.
        mark_stand_down(live, now + 60_000).await;
        live_update(live, "standing_down", Some(fp), None, Some(detail)).await;
        return Ok(false);
    }
    let individual = matches!(gate, quota::TokenGateExceeded::Individual { .. });
    let (spent, limit) = match gate {
        quota::TokenGateExceeded::Individual { spent, limit }
        | quota::TokenGateExceeded::Daily { spent, limit }
        | quota::TokenGateExceeded::Monthly { spent, limit } => (spent, limit),
    };
    if !individual {
        // Shared spend is not an issue fault. Keep the truthful scope/reason
        // rather than consuming an attempt or reporting delivery failure.
        store.release_issue(fp, &detail, now + 15 * 60_000, now)?;
    }
    store.finish_run(
        run_id,
        &shanhaijing::RunFinish {
            finished_at: now,
            outcome: "budget_exceeded".into(),
            ci_verdict: None,
            error: Some(detail.clone()),
        },
    )?;
    if !individual {
        trace::record(
            store,
            fp,
            &config.repo,
            trace::Decision {
                stage: "budget",
                decision: "released",
                reason: "repository token cap exceeded; attempt not consumed",
                details: serde_json::json!({"error": detail}),
            },
        );
        mark_stand_down(live, now + 15 * 60_000).await;
        live_update(live, "standing_down", None, None, Some(detail.clone())).await;
        println!("budget     : {detail} — released without consuming an attempt");
        return Ok(false);
    }
    live_update(live, "error", Some(fp), None, Some(detail.clone())).await;
    let outcome = store.record_issue_failure(
        fp,
        now,
        u64::from(config.max_attempts.max(1)),
        now + 30 * 60_000,
        &detail,
        "runner",
    )?;
    let retry = outcome.disposition == shanhaijing::FailureDisposition::Retry;
    trace::record(
        store,
        fp,
        &config.repo,
        trace::Decision {
            stage: "budget",
            decision: if retry { "retry_scheduled" } else { "ignored" },
            reason: "per-issue token cap exceeded",
            details: serde_json::json!({
                "error": detail,
                "spent_tokens": spent,
                "cap_tokens": limit,
                "attempts": outcome.attempts,
                "max_attempts": config.max_attempts,
            }),
        },
    );
    if retry {
        println!(
            "budget     : {detail} — retry scheduled (attempt {}/{})",
            outcome.attempts, config.max_attempts
        );
    } else {
        emit_webhook("ignored", fp, &config.repo, detail.clone());
        println!("budget     : {detail} — attempt cap reached, issue ignored");
    }
    Ok(true)
}

async fn finish_pr_quota(
    store: &shanhaijing::Store,
    run_id: &str,
    fp: &str,
    live: &SharedLive,
) -> anyhow::Result<bool> {
    let now = now_ms();
    let retry_at = now + 60_000;
    let detail = format!("pr_quota: {}", deliver::PrQuotaExceeded);
    store.finish_run(
        run_id,
        &shanhaijing::RunFinish {
            finished_at: now,
            outcome: "pr_quota".into(),
            ci_verdict: None,
            error: Some(detail.clone()),
        },
    )?;
    store.release_issue(fp, &detail, retry_at, now)?;
    mark_stand_down(live, retry_at).await;
    live_update(live, "standing_down", None, None, Some(detail)).await;
    Ok(false)
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
    match fp {
        Some(fingerprint) => {
            state.runs.insert(
                fingerprint.to_string(),
                RunLive {
                    phase: phase.map(str::to_string),
                },
            );
        }
        None => {
            // The single-run loop uses a glob update to mean "no run is
            // active"; a pool finishes one run via `live_end` instead so the
            // other runs stay visible.
            state.runs.clear();
        }
    }
    state.last_error = error;
    state.progress_at_ms = now_ms();
}

/// Remove one finished run without clearing the others. The single-run loop
/// clears via `live_update(None, "idle", …)`; a pool calls this per run.
#[allow(dead_code)]
async fn live_end(live: &SharedLive, fingerprint: &str) {
    let mut state = live.lock().await;
    state.runs.remove(fingerprint);
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
    leases: Arc<std::sync::Mutex<Vec<(String, String)>>>,
) {
    tokio::spawn(async move {
        let Ok(mut term) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        else {
            return;
        };
        term.recv().await;
        let leases = leases
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        for (runner_id, instance_id) in leases {
            let _ = store.release_runner_lease(&runner_id, &instance_id);
        }
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

fn delivery_requires_recovery(
    store: &shanhaijing::Store,
    repo: &str,
    run_id: &str,
    error: &anyhow::Error,
) -> anyhow::Result<bool> {
    if deliver::ownership_uncertain(error) {
        return Ok(true);
    }
    Ok(store
        .pending_deliveries(repo)?
        .iter()
        .any(|delivery| delivery.run_id.as_deref() == Some(run_id)))
}

struct RecoveryClaim<'a> {
    store: &'a shanhaijing::Store,
    claim: shanhaijing::DeliveryClaim,
}

impl Drop for RecoveryClaim<'_> {
    fn drop(&mut self) {
        // No release after uncertain/lost ownership, and no unfenced mutation.
        if deliver::ensure_delivery_owned(self.store, &self.claim).is_ok() {
            if let Err(error) = self.store.release_delivery_claim(&self.claim, now_ms()) {
                eprintln!("delivery    : recovery claim release failed: {error}");
            }
        }
    }
}

fn mark_recovery_ambiguous(
    store: &shanhaijing::Store,
    claim: &shanhaijing::DeliveryClaim,
) -> anyhow::Result<()> {
    deliver::ensure_delivery_owned(store, claim)?;
    if claim.delivery.state != shanhaijing::DeliveryState::Ambiguous {
        store.mark_delivery_owned(
            claim,
            shanhaijing::DeliveryState::Ambiguous,
            None,
            None,
            Some("recovery inconclusive; preserving branch and issue/run evidence"),
            now_ms(),
        )?;
    }
    Ok(())
}

/// Injectable network boundary: tests exercise the actual recovery decisions,
/// ownership checks and transactions without credentials or GitHub requests.
async fn reconcile_delivery_claim<F>(
    store: &shanhaijing::Store,
    claim: &shanhaijing::DeliveryClaim,
    slug: &str,
    max_attempts: u64,
    lookup: impl std::future::Future<Output = anyhow::Result<Option<deliver::RecoveredPullRequest>>>,
    delete: impl FnOnce() -> F,
) -> anyhow::Result<bool>
where
    F: std::future::Future<Output = anyhow::Result<()>>,
{
    let delivery = &claim.delivery;
    let found = deliver::with_delivery_heartbeat(store, claim, lookup).await;
    deliver::ensure_delivery_owned(store, claim)?;
    match found {
        Ok(Some(pr)) if pr.matches(&delivery.head_sha, &delivery.base) => {
            let run = match delivery.run_id.as_deref() {
                Some(id) => store.run_for(&delivery.fingerprint, id)?,
                None => None,
            };
            let now = now_ms();
            let watch = shanhaijing::CiWatchInput {
                run_id: delivery.run_id.clone(),
                fingerprint: delivery.fingerprint.clone(),
                repo: delivery.repo.clone(),
                pr_url: pr.url.clone(),
                slug: slug.to_string(),
                pr_number: pr.number,
                head_sha: delivery.head_sha.clone(),
                root_cause: run.as_ref().and_then(|run| run.root_cause.clone()),
                change_stat: run.as_ref().and_then(|run| run.change_stat.clone()),
                now,
                next_check_at: now,
                max_attempts,
            };
            store.complete_delivery_owned_at(
                claim,
                &pr.url,
                pr.number,
                Some(&watch),
                pr.created_at_ms,
                now_ms(),
            )?;
            Ok(true)
        }
        Ok(None) => {
            // Only this claimed definitive absence permits remote deletion.
            // The Git operation also compares the exact recorded remote SHA.
            deliver::ensure_delivery_owned(store, claim)?;
            if deliver::with_delivery_heartbeat(store, claim, delete())
                .await
                .is_err()
            {
                mark_recovery_ambiguous(store, claim)?;
                return Ok(false);
            }
            deliver::ensure_delivery_owned(store, claim)?;
            store.mark_delivery_owned(
                claim,
                shanhaijing::DeliveryState::Failed,
                None,
                None,
                Some("definitive no PR; exact remote branch absent/removed; issue unchanged"),
                now_ms(),
            )?;
            // Failed clears the claim: do NOT independently mutate issue/run.
            Ok(true)
        }
        Ok(Some(_)) | Err(_) => {
            mark_recovery_ambiguous(store, claim)?;
            Ok(false)
        }
    }
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
    // One claim per invocation: never hold an unrenewed tail of a batch.
    let claim = store
        .claim_pending_deliveries(
            repo_id,
            &format!("recovery:{}", uuid::Uuid::new_v4()),
            now_ms(),
            pipeline::CHECKOUT_TTL_MS,
            1,
        )?
        .into_iter()
        .next();
    let Some(claim) = claim else {
        return Ok(0);
    };
    let owned = RecoveryClaim { store, claim };
    let claim = &owned.claim;
    let delivery = &claim.delivery;
    let setup = (|| -> anyhow::Result<_> {
        let token = secrets
            .github_token
            .as_deref()
            .context("cannot reconcile deliveries without NUWA_GITHUB_TOKEN")?;
        let origin = git_ops::git(repo_dir, &["remote", "get-url", "origin"])?;
        let slug =
            deliver::parse_github_slug(&origin).context("parse GitHub owner/repo from origin")?;
        Ok((token, slug))
    })();
    let (token, slug) = match setup {
        Ok(setup) => setup,
        Err(error) => {
            mark_recovery_ambiguous(store, claim)?;
            return Err(error);
        }
    };
    let reconciled = reconcile_delivery_claim(
        store,
        claim,
        &slug,
        max_attempts,
        deliver::find_pr_by_head(
            token,
            &slug,
            &delivery.branch,
            &delivery.head_sha,
            &delivery.base,
        ),
        || async {
            if !push {
                bail!("remote cleanup disabled; retaining delivery evidence");
            }
            git_ops::delete_remote_branch_if_matches(
                repo_dir,
                &delivery.branch,
                &delivery.head_sha,
                Duration::from_secs(60),
            )
            .await
        },
    )
    .await?;
    Ok(usize::from(reconciled))
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
async fn preflight_repo(
    repo_dir: &Path,
    fetch: bool,
    checkout: &pipeline::CheckoutLease,
) -> anyhow::Result<&'static str> {
    checkout.ensure()?;
    if pipeline::checkout_identity(repo_dir)? != checkout.claim.path {
        bail!("preflight checkout claim belongs to a different checkout");
    }
    let base = resolve_base(repo_dir)?;
    let head = git_ops::current_branch(repo_dir)?;
    let dirty = git_ops::git(
        repo_dir,
        &["status", "--porcelain", "--", ":(exclude)nuwa.repo.json"],
    )?;
    if !dirty.trim().is_empty() {
        bail!("working tree dirty — preserving interrupted repair; manual review required");
    }
    if head != base && head != "HEAD" {
        if head.starts_with("agent/fix/") {
            // Keep the branch/commit and never force away uncommitted evidence.
            checkout.ensure()?;
            git_ops::git(repo_dir, &["checkout", base])?;
            eprintln!("preflight   : recovered checkout from {head} to {base}");
        } else {
            bail!(
                "repo {} is on branch `{}` — runner only works from {base}; reset the checkout",
                repo_dir.display(),
                head
            );
        }
    }

    let expected_tip = git_ops::git(repo_dir, &["rev-parse", "HEAD"])?;
    let has_origin = git_ops::git(repo_dir, &["remote", "get-url", "origin"]).is_ok();
    if has_origin {
        if fetch {
            git_ops::fetch_base(repo_dir, base, Duration::from_secs(120)).await?;
        }
        checkout.ensure()?;
        if git_ops::git(repo_dir, &["rev-parse", "HEAD"])? != expected_tip
            || git_ops::current_branch(repo_dir)? != if head == "HEAD" { "HEAD" } else { base }
            || !git_ops::git(
                repo_dir,
                &["status", "--porcelain", "--", ":(exclude)nuwa.repo.json"],
            )?
            .trim()
            .is_empty()
        {
            bail!("checkout changed during preflight — refusing reset");
        }
        git_ops::reset_to_base(repo_dir, base)?;
    } else if head == "HEAD" {
        checkout.ensure()?;
        if git_ops::git(repo_dir, &["rev-parse", "HEAD"])? != expected_tip
            || git_ops::current_branch(repo_dir)? != "HEAD"
            || !git_ops::git(
                repo_dir,
                &["status", "--porcelain", "--", ":(exclude)nuwa.repo.json"],
            )?
            .trim()
            .is_empty()
        {
            bail!("checkout changed during preflight — refusing reset");
        }
        git_ops::git(repo_dir, &["reset", "--hard", base])?;
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

/// Terminal issue state alone proves neither PR absence nor a branch's tip.
/// Leave both local and remote evidence for explicit, claimed cleanup.
async fn cleanup_terminal_branches(
    _store: &shanhaijing::Store,
    _repo_dir: &Path,
    _push: bool,
) -> anyhow::Result<usize> {
    Ok(0)
}

/// A stale name is not authority to delete either copy of a branch.
async fn remove_stale_branch(
    repo_dir: &Path,
    fingerprint: &str,
    push: bool,
) -> anyhow::Result<bool> {
    let branch = format!("agent/fix/{}", issue::short_fingerprint(fingerprint));
    if git_ops::branch_exists(repo_dir, &branch)? {
        bail!("branch {branch} retained for explicit cleanup (no claimed absence/tip evidence)");
    }
    if push && git_ops::remote_branch_exists(repo_dir, &branch, Duration::from_secs(60)).await? {
        bail!("remote branch {branch} retained for claimed recovery or explicit cleanup");
    }
    Ok(false)
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

/// 明确的 branch-only 推送成功后，以独立验证门和 run ID 作为记忆来源。
fn remember_verified_fix(
    store: &shanhaijing::Store,
    repo: &str,
    fingerprint: &str,
    run_id: &str,
    delivered: &deliver::DeliverReport,
    root_cause: &str,
) -> anyhow::Result<()> {
    store.record_verified_repair_memory(
        &shanhaijing::RepairMemoryEntry {
            repo: repo.to_string(),
            fingerprint: fingerprint.to_string(),
            commit_sha: delivered.commit.clone(),
            pr_url: delivered.pr_url.clone(),
            root_cause: root_cause.to_string(),
            change_stat: delivered.change_stat.clone(),
            files: delivered.risk.files.len() as i64,
            recorded_at: now_ms(),
        },
        "local_verification",
        run_id,
    )?;
    Ok(())
}

/// Test-only single-run entry: the config root and editable checkout are the
/// same directory. Production calls [`work_once_in`] directly.
#[cfg(test)]
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
    let shared_repo = tokio::sync::Mutex::new(());
    work_once_in(
        store,
        trusted,
        repo_dir,
        repo_dir,
        &shared_repo,
        sessions_dir,
        provider,
        max_turns,
        push,
        create_pr,
        daily_pr_limit,
        max_tokens,
        daily_token_limit,
        monthly_token_limit,
        ci_timeout_secs,
        runner_id,
        None,
        live,
        secrets,
    )
    .await
}

/// One pickup attempt. `config_root` supplies the trusted instructions and
/// skills; `repo_dir` is the checkout this run edits. `Ok(true)` = an issue
/// was claimed and driven to a terminal state this pass; `Ok(false)` = nothing
/// pickable.
#[allow(clippy::too_many_arguments)]
async fn work_once_in(
    store: &Arc<shanhaijing::Store>,
    trusted: &mut instructions::TrustedInstructions,
    repo_dir: &Path,
    config_root: &Path,
    shared_repo: &tokio::sync::Mutex<()>,
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
    instance_id: Option<&str>,
    live: SharedLive,
    secrets: &Arc<secrets::Secrets>,
) -> anyhow::Result<bool> {
    let checkout = pipeline::CheckoutLease::acquire(store, repo_dir, runner_id, instance_id)?;
    checkout
        .during(work_once_claimed(
            store,
            trusted,
            repo_dir,
            config_root,
            shared_repo,
            sessions_dir,
            provider,
            max_turns,
            push,
            create_pr,
            daily_pr_limit,
            max_tokens,
            daily_token_limit,
            monthly_token_limit,
            ci_timeout_secs,
            runner_id,
            live,
            secrets,
            &checkout,
        ))
        .await
}

#[allow(clippy::too_many_arguments)]
async fn work_once_claimed(
    store: &Arc<shanhaijing::Store>,
    trusted: &mut instructions::TrustedInstructions,
    repo_dir: &Path,
    config_root: &Path,
    shared_repo: &tokio::sync::Mutex<()>,
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
    checkout: &pipeline::CheckoutLease,
) -> anyhow::Result<bool> {
    let now = now_ms();
    let repo_config = &trusted.instructions;
    // Pending evidence belongs to delivery recovery, not a fresh repair/reset.
    if !store.pending_deliveries(&repo_config.repo)?.is_empty() {
        live_update(
            &live,
            "standing_down",
            None,
            None,
            Some("unfinished delivery requires reconciliation".into()),
        )
        .await;
        return Ok(false);
    }
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
        let slots = store.pr_slots_used(&repo_config.repo, day_start)?;
        if slots >= daily_pr_limit {
            println!(
                "quota      : {slots}/{daily_pr_limit} UTC PR admission slots for {} — standing down until the window rolls",
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
    let preflight = {
        let _guard = shared_repo.lock().await;
        preflight_repo(repo_dir, pickable, checkout).await
    };
    let base = match preflight {
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
    // Names and terminal issue states are not proof of PR absence. Retain
    // either branch copy and stand down for claimed recovery/explicit cleanup.
    let stale_branch = format!("agent/fix/{}", issue::short_fingerprint(&fp));
    if let Err(error) = remove_stale_branch(repo_dir, &fp, push).await {
        let message = bounded_text(&secrets.redact(&format!(
            "branch {stale_branch} retained; explicit cleanup required: {error:#}"
        )));
        let retry_at = now_ms() + 15 * 60_000;
        store.release_issue(&fp, &message, retry_at, now_ms())?;
        mark_stand_down(&live, retry_at).await;
        live_update(&live, "standing_down", None, None, Some(message.clone())).await;
        eprintln!("branch     : {message}");
        return Ok(false);
    }

    let prior_context = build_prior_context(store, &repo_config.repo, &fp, secrets);
    // 问题单在 `start_run` 之前构建：确定性会话目录要在第一次生命周期写入时
    // 就已知。
    let prob = pipeline::issue_from_snapshot(&snapshot, repo_config, prior_context, secrets);
    store.start_run(&shanhaijing::RunStart {
        id: run_id.clone(),
        fingerprint: fp.clone(),
        runner_id: runner_id.to_string(),
        repo: snapshot.repo.clone(),
        started_at: now,
        provider: Some(provider_name.clone()),
        model: Some(model_name.clone()),
    })?;
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
        accumulate_usage(
            &mut progress_usage.lock().unwrap_or_else(|e| e.into_inner()),
            p,
        );
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
    // The future in-process pool swaps this for `RunWorkspace::worktree`;
    // the single-run loop keeps editing the primary checkout.
    let workspace = workspace::RunWorkspace::direct(repo_dir);
    let prepared = match pipeline::prepare_repair(pipeline::RepairRequest {
        repo: config_root.to_path_buf(),
        workspace: Some(workspace.path().to_path_buf()),
        issue: prob,
        sessions_dir: sessions_dir.clone(),
        provider,
        max_turns,
        repo_config: Some(repo_config.clone()),
        owner: Some(trusted.clone()),
        base: Some(git_ops::git(repo_dir, &["rev-parse", "HEAD"])?),
        secrets: Arc::clone(secrets),
        progress,
        faux_script: None,
        run_id: Some(run_id.clone()),
    })
    .await
    {
        Ok(prepared) => prepared,
        Err(error) => {
            checkout.ensure()?;
            // Invalid owner policy/baseline is configuration, not a code bug.
            // Release without attempts or any delivery effects.
            let detail =
                bounded_text(&secrets.redact(&format!("verification preparation: {error:#}")));
            let now = now_ms();
            let retry_at = now + 15 * 60_000;
            store.release_issue(&fp, &detail, retry_at, now)?;
            store.finish_run(
                &run_id,
                &shanhaijing::RunFinish {
                    finished_at: now,
                    outcome: "environment".into(),
                    ci_verdict: None,
                    error: Some(detail.clone()),
                },
            )?;
            mark_stand_down(&live, retry_at).await;
            live_update(&live, "standing_down", None, None, Some(detail)).await;
            return Ok(false);
        }
    };
    println!(
        "verify     : mode {}",
        prepared.issue().verification_mode.as_str()
    );
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
    persist_final_usage(store, &run_id, usage, repair_result.as_ref().ok()).await?;
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
            store.finish_run(
                &run_id,
                &shanhaijing::RunFinish {
                    finished_at: now_ms(),
                    outcome: outcome.into(),
                    ci_verdict: None,
                    error: Some(detail.clone()),
                },
            )?;
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

    // Final usage is durable before checking either individual or repo spend.
    let limits = quota::DeliveryLimits {
        daily_pr_limit,
        max_tokens,
        daily_token_limit,
        monthly_token_limit,
    };
    if let Err(error) =
        limits.check_tokens(store, &repo_config.repo, Some(&run_id), &report, now_ms())
    {
        if let Some(gate) = error.downcast_ref::<quota::TokenGateExceeded>() {
            return finish_token_gate(store, &run_id, &fp, repo_config, *gate, &live, secrets)
                .await;
        }
        return Err(error);
    }
    let spent = report.uncached_tokens.saturating_add(report.cached_tokens);
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
    let public_base_url = env_opt("NUWA_PUBLIC_BASE_URL");
    let delivered = match pipeline::deliver(pipeline::DeliveryRequest {
        repo: workspace.path(),
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
        limits,
        checkout_claim: Some(&checkout.claim),
        verification: prepared.verification(),
    })
    .await
    {
        Ok(d) => d,
        Err(e) => {
            // A recheck may deny delivery after another worker spent tokens.
            // Pending remote evidence remains protected inside the gate handler.
            if let Some(gate) = e.downcast_ref::<quota::TokenGateExceeded>() {
                return finish_token_gate(store, &run_id, &fp, repo_config, *gate, &live, secrets)
                    .await;
            }
            if e.downcast_ref::<deliver::PrQuotaExceeded>().is_some() {
                return finish_pr_quota(store, &run_id, &fp, &live).await;
            }
            // Heartbeat/claim uncertainty or any durable pending intent is not
            // a fresh issue failure. Recovery may already own or complete it.
            if delivery_requires_recovery(store, &repo_config.repo, &run_id, &e)? {
                live_update(
                    &live,
                    "standing_down",
                    Some(&fp),
                    None,
                    Some(
                        "delivery ownership/outcome uncertain; preserving recovery evidence".into(),
                    ),
                )
                .await;
                return Ok(false);
            }
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
    // PR identity/state/watch were committed atomically by deliver. Only the
    // branch-only path still needs the legacy local bookkeeping here.
    if delivered.pr_url.is_none() {
        persist_run(
            store,
            &run_id,
            shanhaijing::RunUpdate {
                phase: Some("ci".into()),
                branch: Some(delivered.branch.clone()),
                commit_sha: Some(delivered.commit.clone()),
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
            Some("branch pushed, no PR"),
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
    }
    if let Some(url) = &delivered.pr_url {
        emit_webhook("pr_opened", &fp, &repo_config.repo, url.clone());
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
                &run_id,
                &delivered,
                &redacted_root_cause,
            )?;
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

    // ---- CI 闸门消费：与 serve 共用持久化 watch 的所有权和观测提交 ----
    live_update(&live, "working", Some(&fp), Some("ci"), None).await;
    let observation = match (
        &delivered.slug,
        delivered.pr_number,
        secrets.github_token.clone(),
    ) {
        (Some(slug), Some(number), Some(token)) => {
            println!(
                "ci gate   : observing delivered head {} (timeout {ci_timeout_secs}s)…",
                &delivered.commit[..12.min(delivered.commit.len())]
            );
            ci_watch::wait_for_delivery(
                store,
                &ci_watch::GitHubCi::new(token),
                slug,
                number,
                &delivered.commit,
                Duration::from_secs(ci_timeout_secs),
            )
            .await
        }
        (_, _, None) => Err(anyhow::anyhow!(
            "NUWA_GITHUB_TOKEN not set — cannot observe CI; durable watch remains waiting"
        )),
        _ => Err(anyhow::anyhow!("PR number unavailable — CI gate skipped")),
    };
    let (outcome, decision, detail, error) = match observation {
        Ok(Some(watch)) => {
            let detail = match watch.state {
                shanhaijing::CiWatchState::Green => "all checks green for delivered head".into(),
                shanhaijing::CiWatchState::Red => format!(
                    "CI red — {}",
                    watch.last_error.as_deref().unwrap_or("check PR status")
                ),
                shanhaijing::CiWatchState::Merged => format!("PR #{} merged", watch.pr_number),
                shanhaijing::CiWatchState::Closed => {
                    format!("PR #{} closed without merge", watch.pr_number)
                }
                shanhaijing::CiWatchState::HeadChanged => format!(
                    "PR head changed — manual follow-up required; {}",
                    watch
                        .last_error
                        .as_deref()
                        .unwrap_or("not following the replacement head")
                ),
                shanhaijing::CiWatchState::Expired => {
                    "CI watch expired — manual follow-up required".into()
                }
                shanhaijing::CiWatchState::Pending => "CI watch remains pending".into(),
            };
            let detail = bounded_text(&secrets.redact(&detail));
            let error = matches!(
                watch.state,
                shanhaijing::CiWatchState::Red
                    | shanhaijing::CiWatchState::HeadChanged
                    | shanhaijing::CiWatchState::Expired
            )
            .then(|| detail.clone());
            (
                if watch.state == shanhaijing::CiWatchState::Red {
                    "ci_red"
                } else {
                    "success"
                },
                watch.state.as_str(),
                detail,
                error,
            )
        }
        Ok(None) => (
            "success",
            "ci_timeout",
            bounded_text(&format!(
                "CI observation timed out after {ci_timeout_secs}s — durable watch continues"
            )),
            None,
        ),
        Err(e) => {
            let detail = bounded_text(&secrets.redact(&format!("CI unavailable: {e:#}")));
            ("success", "unavailable", detail.clone(), Some(detail))
        }
    };
    // The owned watcher publishes the verdict atomically. Never overwrite it,
    // including when serve observes CI while this bounded wait is unavailable.
    store.finish_run(
        &run_id,
        &shanhaijing::RunFinish {
            finished_at: now_ms(),
            outcome: outcome.into(),
            ci_verdict: None,
            error,
        },
    )?;
    trace::record(
        store,
        &fp,
        &repo_config.repo,
        trace::Decision {
            stage: "ci",
            decision,
            reason: &detail,
            details: serde_json::json!({
                "pr": delivered.pr_number,
                "slug": delivered.slug,
                "commit": delivered.commit,
                "timeout_secs": ci_timeout_secs,
            }),
        },
    );
    println!("ci gate   : {detail}");
    if outcome == "ci_red" {
        live_update(&live, "error", Some(&fp), None, Some(detail)).await;
        return Ok(true);
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
    let token = env_store::var("NUWA_GITHUB_TOKEN")
        .or_else(|| env_store::var("GITHUB_TOKEN"))
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

fn startup_env_opt(environment: &BTreeMap<String, String>, name: &str) -> Option<String> {
    environment
        .get(name)
        .filter(|value| !value.is_empty())
        .cloned()
}

fn startup_env_path(environment: &BTreeMap<String, String>) -> PathBuf {
    environment
        .get("NUWA_ENV_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/opt/nuwa/nuwa.env"))
}

fn startup_number<T: std::str::FromStr>(
    environment: &BTreeMap<String, String>,
    key: &str,
    default: T,
) -> (T, &'static str) {
    match environment.get(key).and_then(|raw| raw.parse().ok()) {
        Some(value) => (value, "environment"),
        None => (default, "default"),
    }
}

/// Retain proven file provenance; normalized/clamped inputs are derived from
/// those inputs rather than an independent environment override.
fn set_startup_effective(
    snapshot: &mut config_activation::StartupConfig,
    key: &str,
    value: Option<String>,
    source: &str,
) {
    let source = if source == "environment" {
        match snapshot.values.get(key) {
            Some(old) if old.value != value => "derived",
            Some(old) if old.source == "file" => "file",
            _ => "environment",
        }
    } else {
        source
    };
    snapshot.set_effective(key, value, source);
}

fn record_startup_provider(
    snapshot: &mut config_activation::StartupConfig,
    provider: &repair::ProviderKind,
    cli_provider: bool,
    environment: &BTreeMap<String, String>,
) {
    let (name, model, base) =
        repair::actual_provider_config(provider).unwrap_or_else(|_| match provider {
            // Retain the request, not a resolved model or an invented endpoint.
            repair::ProviderKind::Anthropic { model } => ("anthropic".into(), model.clone(), None),
            _ => (provider_meta(provider).0, None, None),
        });
    let provider_source = if cli_provider {
        "cli"
    } else if environment
        .get("NUWA_PROVIDER")
        .is_some_and(|raw| matches!(raw.as_str(), "anthropic" | "zai" | "faux"))
    {
        "environment"
    } else {
        "default"
    };
    let requested_source = if cli_provider { "cli" } else { "environment" };
    let (model_source, base_source) = match provider {
        repair::ProviderKind::Anthropic { model } => (
            if model.is_some() {
                requested_source
            } else {
                "default"
            },
            "default",
        ),
        repair::ProviderKind::Zai { model, base_url } => (
            if model.is_some() {
                requested_source
            } else {
                "default"
            },
            if base_url.is_some() {
                requested_source
            } else {
                "default"
            },
        ),
        repair::ProviderKind::Faux => ("default", "default"),
    };
    set_startup_effective(snapshot, "NUWA_PROVIDER", Some(name), provider_source);
    set_startup_effective(snapshot, "NUWA_MODEL", model, model_source);
    set_startup_effective(snapshot, "NUWA_BASE_URL", base, base_source);
}

fn persist_runner_startup(
    store: &shanhaijing::Store,
    snapshot: &config_activation::StartupConfig,
    now: i64,
) -> anyhow::Result<()> {
    let runner_id = snapshot
        .runner_id
        .as_deref()
        .context("startup runner id missing")?;
    let instance_id = snapshot
        .runner_instance_id
        .as_deref()
        .context("startup runner instance missing")?;
    let json = serde_json::to_string(snapshot).context("serialize runner startup config")?;
    if !store
        .set_runner_startup_config(runner_id, instance_id, &json, now)
        .context("persist runner startup config")?
    {
        bail!("runner startup config rejected: lease lost or snapshot already set");
    }
    Ok(())
}

/// Empty values are unset; settings saves never mutate this startup store.
fn env_opt(name: &str) -> Option<String> {
    env_store::var(name).filter(|value| !value.is_empty())
}

fn provider_from_environment(environment: &BTreeMap<String, String>) -> repair::ProviderKind {
    match environment
        .get("NUWA_PROVIDER")
        .map(String::as_str)
        .unwrap_or("anthropic")
    {
        "zai" => repair::ProviderKind::Zai {
            base_url: startup_env_opt(environment, "NUWA_BASE_URL"),
            model: startup_env_opt(environment, "NUWA_MODEL"),
        },
        "faux" => repair::ProviderKind::Faux,
        _ => repair::ProviderKind::Anthropic {
            model: startup_env_opt(environment, "NUWA_MODEL"),
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
    let secrets = Arc::new(secrets::Secrets::load());
    // 文件里的问题单是外部输入：与常驻 runner 一样先脱敏。
    let prob = pipeline::redact_issue(
        issue::Issue::load(&cli.issue_path).context("load issue")?,
        &secrets,
    );

    if !cli.repo.join(".git").exists() {
        bail!("{} is not a git repository", cli.repo.display());
    }
    let store = Arc::new(
        shanhaijing::Store::open(&cli.db)
            .with_context(|| format!("open db {}", cli.db.display()))?,
    );
    let owner = format!("fix:{}", uuid::Uuid::new_v4());
    let checkout = pipeline::CheckoutLease::acquire(&store, &cli.repo, &owner, None)?;
    checkout
        .during(fix_with_checkout(cli, prob, secrets, &store, &checkout))
        .await
}

async fn fix_with_checkout(
    cli: Cli,
    prob: issue::Issue,
    secrets: Arc<secrets::Secrets>,
    store: &Arc<shanhaijing::Store>,
    checkout: &pipeline::CheckoutLease,
) -> anyhow::Result<()> {
    let limits = quota::DeliveryLimits::from_environment();
    fix_with_limits(cli, prob, secrets, store, checkout, limits).await
}

async fn fix_with_limits(
    cli: Cli,
    prob: issue::Issue,
    secrets: Arc<secrets::Secrets>,
    store: &Arc<shanhaijing::Store>,
    checkout: &pipeline::CheckoutLease,
    limits: quota::DeliveryLimits,
) -> anyhow::Result<()> {
    println!(
        "== Nuwa fix run ==\nissue     : {}\nrepo      : {}\nprovider  : {}\nmax turns : {}",
        prob.fingerprint,
        cli.repo.display(),
        pipeline::provider_label(&cli.provider),
        cli.max_turns,
    );

    // Standalone fix adopts authoritative bytes BEFORE the model, just like
    // work. A working-file reload after repair can never establish authority.
    let base = git_ops::git(&cli.repo, &["rev-parse", "HEAD"])?;
    let private_owner = tempfile::tempdir().context("standalone owner authority")?;
    let owner = if cli.repo.join("nuwa.repo.json").exists()
        || git_ops::git_raw(&cli.repo, &["show", &format!("{base}:nuwa.repo.json")]).is_ok()
    {
        Some(instructions::TrustedInstructions::load(
            &cli.repo,
            &cli.repo.join("nuwa.repo.json"),
            &base,
            private_owner.path(),
        )?)
    } else {
        None
    };
    let repo_config = owner.as_ref().map(|owner| owner.instructions.clone());
    let repo_id = match &repo_config {
        Some(config) => config.repo.clone(),
        None => {
            let origin = git_ops::git(&cli.repo, &["remote", "get-url", "origin"]).ok();
            origin
                .as_deref()
                .and_then(deliver::parse_github_slug)
                .unwrap_or_else(|| checkout.claim.path.clone())
        }
    };
    if !store.pending_deliveries(&repo_id)?.is_empty() {
        bail!("unfinished delivery requires reconciliation before standalone repair");
    }
    limits.check_admission(store, &repo_id, cli.create_pr, now_ms())?;
    checkout.ensure()?;
    let run_id = uuid::Uuid::new_v4().to_string();
    let (provider, model) = provider_meta(&cli.provider);
    store.start_standalone_run(&shanhaijing::RunStart {
        id: run_id.clone(),
        fingerprint: prob.fingerprint.clone(),
        runner_id: format!("fix:{run_id}"),
        repo: repo_id.clone(),
        started_at: now_ms(),
        provider: Some(provider),
        model: Some(model),
    })?;
    let final_usage = Arc::new(std::sync::Mutex::new(repair::RepairProgress {
        turns: 0,
        uncached_tokens: 0,
        cached_tokens: 0,
    }));
    let progress_usage = Arc::clone(&final_usage);
    let progress: pipeline::ProgressSink = Arc::new(move |progress| {
        accumulate_usage(
            &mut progress_usage.lock().unwrap_or_else(|e| e.into_inner()),
            progress,
        );
    });
    let repair_result = async {
        let prepared = pipeline::prepare_repair(pipeline::RepairRequest {
            repo: cli.repo.clone(),
            workspace: None,
            issue: prob,
            sessions_dir: cli.sessions_dir.clone(),
            provider: cli.provider,
            max_turns: cli.max_turns,
            repo_config,
            owner,
            base: Some(base),
            secrets: Arc::clone(&secrets),
            progress: Some(progress),
            faux_script: None,
            run_id: Some(run_id.clone()),
        })
        .await?;
        store.update_run(
            &run_id,
            &shanhaijing::RunUpdate {
                phase: Some("repair".into()),
                session_dir: Some(prepared.session_dir().to_string_lossy().into_owned()),
                ..Default::default()
            },
        )?;
        let report = prepared.execute().await.context("repair run")?;
        Ok::<_, anyhow::Error>((prepared, report))
    }
    .await;
    let usage = *final_usage.lock().unwrap_or_else(|e| e.into_inner());
    persist_final_usage(
        store,
        &run_id,
        usage,
        repair_result.as_ref().ok().map(|(_, report)| report),
    )
    .await?;
    let (prepared, report) = match repair_result {
        Ok(result) => result,
        Err(error) => {
            finish_standalone_run(store, &run_id, "repair_failed", Some(&error), &secrets)?;
            return Err(error);
        }
    };
    store
        .update_run(
            &run_id,
            &shanhaijing::RunUpdate {
                phase: Some("verify".into()),
                harness_run_id: Some(report.run_id.clone()),
                root_cause: Some(bounded_text(&secrets.redact(&report.root_cause_text))),
                session_dir: Some(report.session_dir.clone()),
                compactions: Some(i64::from(report.context.compactions)),
                context_window: Some(report.context.model_context_window as i64),
                messages: Some(report.context.messages as i64),
                ..Default::default()
            },
        )
        .context("persist standalone repair evidence")?;

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
        finish_standalone_run(store, &run_id, "no_deliver", None, &secrets)?;
        println!("\n--no-deliver: stopping before the delivery gate");
        return Ok(());
    }

    let delivered = pipeline::deliver(pipeline::DeliveryRequest {
        repo: &cli.repo,
        issue: prepared.issue(),
        report: &report,
        sessions_dir: &cli.sessions_dir,
        push: cli.push,
        create_pr: cli.create_pr,
        public_base_url: None,
        draft: false,
        secrets: &secrets,
        store: Some(store),
        run_id: Some(&run_id),
        repo_id: &repo_id,
        limits,
        checkout_claim: Some(&checkout.claim),
        verification: prepared.verification(),
    })
    .await;
    let delivered = match delivered {
        Ok(delivered) => delivered,
        Err(error) => {
            // A successful PR completion may precede a checkout cleanup error.
            // Never replace that durable success with a failure/quota outcome.
            let run = store
                .run_by_id(&run_id)?
                .context("standalone run missing")?;
            if run.pr_url.is_some() {
                finish_standalone_run(store, &run_id, "delivered", None, &secrets)?;
            } else if !delivery_requires_recovery(store, &repo_id, &run_id, &error)? {
                let outcome = if error.downcast_ref::<quota::TokenGateExceeded>().is_some() {
                    "budget_exceeded"
                } else if error.downcast_ref::<deliver::PrQuotaExceeded>().is_some() {
                    "pr_quota"
                } else {
                    "delivery_failed"
                };
                finish_standalone_run(store, &run_id, outcome, Some(&error), &secrets)?;
            }
            // Otherwise keep the run active alongside pending delivery evidence.
            return Err(error);
        }
    };
    let run = store
        .run_by_id(&run_id)?
        .context("standalone run missing")?;
    if run.finished_at.is_none() {
        // Local/branch-only delivery has no PR-completion transaction to record
        // this evidence. Do not invent/overwrite GitHub's creation timestamp.
        store.update_run(
            &run_id,
            &shanhaijing::RunUpdate {
                branch: Some(delivered.branch.clone()),
                commit_sha: Some(delivered.commit.clone()),
                pr_url: delivered.pr_url.clone(),
                verification_ok: Some(true),
                verification_detail: Some(secrets.redact(&delivered.verification_detail)),
                change_stat: Some(secrets.redact(&delivered.change_stat)),
                ..Default::default()
            },
        )?;
        finish_standalone_run(store, &run_id, "delivered", None, &secrets)?;
    }
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

fn finish_standalone_run(
    store: &shanhaijing::Store,
    run_id: &str,
    outcome: &str,
    error: Option<&anyhow::Error>,
    secrets: &secrets::Secrets,
) -> anyhow::Result<()> {
    let run = store.run_by_id(run_id)?.context("standalone run missing")?;
    if run.finished_at.is_none() {
        store.finish_run(
            run_id,
            &shanhaijing::RunFinish {
                finished_at: now_ms(),
                outcome: outcome.into(),
                ci_verdict: None,
                error: error
                    .map(|error| bounded_text(&secrets.redact(&format!("{outcome}: {error:#}")))),
            },
        )?;
    }
    Ok(())
}
pub const NUWA_BUILD_MARKER: &str = "MARKER-XYZ-9L";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instructions::RepoInstructions;
    use crate::issue::FailingCheck;

    fn ingest_payload() -> serde_json::Value {
        serde_json::json!({
            "repo": "example.com/batch5",
            "language": "go",
            "title": "panic: boom",
            "stack_trace": "panic: boom\ngoroutine 1 [running]:\nexample.com/batch5.F()\n\t/app/a.go:1 +0x1"
        })
    }

    fn observed_ingest_payload() -> serde_json::Value {
        let mut payload = ingest_payload();
        payload["observed_at"] = 150.into();
        payload["environment"] = "production".into();
        payload["release_sha"] = "A".repeat(40).into();
        payload["source_event_id"] = "incident-1".into();
        payload
    }

    fn ingest_file(dir: &Path, raw: &str, source: Option<&str>) -> anyhow::Result<()> {
        let input = dir.join("signal.jsonl");
        std::fs::write(&input, raw).expect("input");
        let mut args = vec![
            "--signal".into(),
            input.display().to_string(),
            "--db".into(),
            dir.join("ingest.db").display().to_string(),
        ];
        if let Some(source) = source {
            args.extend(["--trust-observations".into(), source.into()]);
        }
        run_ingest(args)
    }

    fn ingest_terminal(store: &shanhaijing::Store, fp: &str, terminal: shanhaijing::State) {
        for state in [
            shanhaijing::State::Triggered,
            shanhaijing::State::Fixing,
            shanhaijing::State::PrOpen,
            terminal,
        ] {
            store.set_state(fp, state, 200, None).expect("transition");
        }
    }

    #[test]
    fn ingest_cli_accepts_legacy_payload() {
        let dir = tempfile::tempdir().expect("tempdir");
        ingest_file(dir.path(), &ingest_payload().to_string(), None).expect("legacy ingest");
        let store = shanhaijing::Store::open(dir.path().join("ingest.db")).expect("store");
        let issue = store.list_issues().expect("issues").remove(0);
        assert_eq!(issue.state, "new");
        assert_eq!(issue.severity, "medium");
        assert_eq!(issue.occurrences, 1);
        let observations = store
            .observations_for(&issue.fingerprint, 100)
            .expect("receipts");
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].time_status, "missing");
        assert_eq!(observations[0].trusted_source, None);
    }

    #[test]
    fn ingest_cli_persists_optional_metadata_without_trust() {
        let dir = tempfile::tempdir().expect("tempdir");
        ingest_file(dir.path(), &observed_ingest_payload().to_string(), None).expect("ingest");
        let store = shanhaijing::Store::open(dir.path().join("ingest.db")).expect("store");
        let issue = store.list_issues().expect("issues").remove(0);
        let observations = store
            .observations_for(&issue.fingerprint, 100)
            .expect("receipts");
        let observation = &observations[0];
        assert_eq!(observation.observed_at_ms, Some(150));
        assert_eq!(observation.environment.as_deref(), Some("production"));
        assert_eq!(observation.release_sha, Some("a".repeat(40)));
        assert_eq!(observation.source_event_id.as_deref(), Some("incident-1"));
        assert_eq!(observation.time_status, "untrusted");
        assert_eq!(observation.trusted_source, None);
    }

    #[test]
    fn ingest_cli_explicit_trust_preserves_terminal_state_for_delayed_observation() {
        for terminal in [shanhaijing::State::Merged, shanhaijing::State::Rejected] {
            let dir = tempfile::tempdir().expect("tempdir");
            ingest_file(dir.path(), &ingest_payload().to_string(), None).expect("seed");
            let store = shanhaijing::Store::open(dir.path().join("ingest.db")).expect("store");
            let fp = store.list_issues().expect("issues")[0].fingerprint.clone();
            ingest_terminal(&store, &fp, terminal);
            let mut payload = observed_ingest_payload();
            ingest_file(dir.path(), &payload.to_string(), Some("owner-import"))
                .expect("trusted import");
            let observations = store.observations_for(&fp, 100).expect("receipts");
            assert_eq!(observations[0].disposition, "delayed");
            assert_eq!(observations[0].time_status, "trusted");
            assert_eq!(
                observations[0].trusted_source.as_deref(),
                Some("owner-import")
            );
            assert_eq!(
                store.list_issues().expect("issues")[0].state,
                terminal.as_str()
            );
            payload["source_event_id"] = "incident-2".into();
            let signal = serde_json::from_value(payload).expect("signal");
            let line = report_one_with_source(&store, &signal, now_ms(), Some("owner-import"))
                .expect("delayed response");
            assert!(line.starts_with("delayed"));
            assert!(line.contains(&fp));
            assert!(line.contains("occurrences=3"));
        }
    }

    #[test]
    fn ingest_cli_duplicate_and_conflicting_event_disclose_partial_receipts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let payload = observed_ingest_payload();
        let mut conflict = payload.clone();
        conflict["environment"] = "staging".into();
        let raw = format!("{payload}\n{payload}\n{conflict}\n");
        let error = ingest_file(dir.path(), &raw, Some("owner-import")).expect_err("conflict");
        let message = format!("{error:#}");
        assert!(message.contains("after 2/3 signals; earlier receipts remain committed"));
        assert!(message.contains("invalid signal observation"));
        assert!(!message.contains("staging"));
        let store = shanhaijing::Store::open(dir.path().join("ingest.db")).expect("store");
        let issue = store.list_issues().expect("issues").remove(0);
        assert_eq!(issue.occurrences, 2);
        let observations = store
            .observations_for(&issue.fingerprint, 100)
            .expect("receipts");
        assert_eq!(observations.len(), 2);
        assert_eq!(observations[0].disposition, "duplicate");
        assert_eq!(observations[0].duplicate_of, Some(observations[1].id));
        let signal = serde_json::from_value(payload).expect("signal");
        let line = report_one_with_source(&store, &signal, now_ms(), Some("owner-import"))
            .expect("duplicate response");
        assert!(line.starts_with("duplicate"));
        assert!(line.contains(&issue.fingerprint));
        assert!(line.contains("occurrences=3"));
    }

    #[test]
    fn ingest_cli_forged_authority_cannot_suppress_regression_or_claim_duplicate() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut payload = observed_ingest_payload();
        ingest_file(dir.path(), &payload.to_string(), Some("owner-import")).expect("seed");
        let store = shanhaijing::Store::open(dir.path().join("ingest.db")).expect("store");
        let fp = store.list_issues().expect("issues")[0].fingerprint.clone();
        ingest_terminal(&store, &fp, shanhaijing::State::Rejected);
        payload["trusted_source"] = "owner-import".into();
        payload["authority"] = serde_json::json!({"trusted": true, "source": "owner-import"});
        ingest_file(dir.path(), &payload.to_string(), None).expect("untrusted import");
        let issue = store.list_issues().expect("issues").remove(0);
        assert_eq!(issue.state, "new");
        assert_eq!(issue.occurrences, 2);
        let observations = store.observations_for(&fp, 100).expect("receipts");
        assert_eq!(observations[0].disposition, "regression");
        assert_eq!(observations[0].time_status, "untrusted");
        assert_eq!(observations[0].trusted_source, None);
        assert_eq!(observations[0].duplicate_of, None);
    }

    #[test]
    fn ingest_cli_prevalidates_all_metadata_and_source_before_committing() {
        let dir = tempfile::tempdir().expect("tempdir");
        for (field, value) in [
            ("environment", " ".to_string()),
            ("release_sha", "BATCH5_PRIVATE_METADATA\n".to_string()),
            (
                "source_event_id",
                "x".repeat(shanhaijing::MAX_SOURCE_EVENT_ID_BYTES + 1),
            ),
        ] {
            let mut bad = observed_ingest_payload();
            bad[field] = value.into();
            let raw = format!("{}\n{bad}", ingest_payload());
            let error = ingest_file(dir.path(), &raw, None).expect_err("invalid metadata");
            assert_eq!(format!("{error:#}"), "invalid signal observation");
        }
        for source in [
            " ".to_string(),
            "private\n".to_string(),
            "x".repeat(shanhaijing::MAX_TRUSTED_SOURCE_BYTES + 1),
        ] {
            let error = ingest_file(dir.path(), &ingest_payload().to_string(), Some(&source))
                .expect_err("invalid source");
            assert_eq!(format!("{error:#}"), "invalid signal observation");
        }
        let store = shanhaijing::Store::open(dir.path().join("ingest.db")).expect("store");
        assert!(store.list_issues().expect("no partial receipts").is_empty());
        assert!(run_ingest(vec!["--trust-observations".into()]).is_err());
    }

    #[test]
    fn ingest_parse_errors_never_reflect_payload_or_serde_values() {
        let mut bad_type = ingest_payload();
        bad_type["observed_at"] = "BATCH5_PRIVATE_METADATA".into();
        for raw in [
            "BATCH5_PRIVATE_METADATA: malformed JSON".to_string(),
            bad_type.to_string(),
            format!("{}\n{bad_type}", ingest_payload()),
        ] {
            let error = parse_signals(&raw).expect_err("invalid input");
            let message = format!("{error:#}");
            assert!(message.starts_with("invalid Signal JSON/JSONL at line "));
            assert!(!message.contains("BATCH5_PRIVATE_METADATA"));
        }
    }

    #[test]
    fn fix_cli_uses_shared_default_journal_and_accepts_db_override() {
        let args = || {
            vec![
                "--issue".into(),
                "issue.json".into(),
                "--repo".into(),
                "repo".into(),
            ]
        };
        assert_eq!(
            parse_args(args()).expect("defaults").db,
            PathBuf::from("shanhaijing.db")
        );
        let mut explicit = args();
        explicit.extend(["--db".into(), "custom.db".into()]);
        assert_eq!(
            parse_args(explicit).expect("override").db,
            PathBuf::from("custom.db")
        );
        let mut missing = args();
        missing.push("--db".into());
        assert!(parse_args(missing).is_err());
    }

    #[test]
    fn standalone_checkout_is_exclusive_without_fabricating_a_runner() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).expect("repo");
        let db = dir.path().join("journal.db");
        let first = Arc::new(shanhaijing::Store::open(&db).expect("first"));
        let second = Arc::new(shanhaijing::Store::open(&db).expect("second"));
        let lease =
            pipeline::CheckoutLease::acquire(&first, &repo, "fix-one", None).expect("claim");
        assert!(first.list_runners().expect("runners").is_empty());
        let alias = repo.join("..").join("repo");
        assert_eq!(
            pipeline::checkout_identity(&alias).expect("alias"),
            lease.claim.path
        );
        assert!(pipeline::CheckoutLease::acquire(&second, &alias, "fix-two", None).is_err());
        drop(lease);
        let successor =
            pipeline::CheckoutLease::acquire(&second, &alias, "fix-two", None).expect("released");
        successor.ensure().expect("owned");
        assert!(second.list_runners().expect("runners").is_empty());
    }

    #[test]
    fn runner_checkout_requires_the_actual_live_instance() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(shanhaijing::Store::open(":memory:").expect("store"));
        let now = now_ms();
        let runner = shanhaijing::RunnerUpsert {
            id: "runner".into(),
            repo: "acme/app".into(),
            host: "test".into(),
            pid: None,
            started_at: now,
            heartbeat_at: now,
            status: "idle".into(),
            provider: None,
            model: None,
            daily_pr_limit: None,
            repo_path: Some(pipeline::checkout_identity(dir.path()).expect("identity")),
            service_unit: None,
            config_json: "{}".into(),
        };
        assert_eq!(
            store
                .try_acquire_runner_lease(&runner, "original", now, 60_000)
                .expect("runner"),
            shanhaijing::RunnerLeaseOutcome::Acquired
        );
        assert!(pipeline::CheckoutLease::acquire(&store, dir.path(), "runner", None).is_err());
        assert!(
            pipeline::CheckoutLease::acquire(&store, dir.path(), "runner", Some("other")).is_err()
        );
        let checkout =
            pipeline::CheckoutLease::acquire(&store, dir.path(), "runner", Some("original"))
                .expect("actual instance");
        drop(checkout);
        store
            .release_runner_lease("runner", "original")
            .expect("release runner");
        assert_eq!(
            store
                .try_acquire_runner_lease(&runner, "successor", now_ms(), 60_000)
                .expect("successor"),
            shanhaijing::RunnerLeaseOutcome::Acquired
        );
        assert!(
            pipeline::CheckoutLease::acquire(&store, dir.path(), "runner", Some("original"))
                .is_err()
        );
        pipeline::CheckoutLease::acquire(&store, dir.path(), "runner", Some("successor"))
            .expect("successor checkout");
    }

    fn recovery_fixture() -> (shanhaijing::Store, shanhaijing::DeliveryClaim) {
        let store = shanhaijing::Store::open(":memory:").expect("store");
        let now = now_ms();
        store
            .report_signal(
                &shanhaijing::Signal {
                    repo: "acme/app".into(),
                    language: "go".into(),
                    title: "panic".into(),
                    severity: "high".into(),
                    message: None,
                    log_window: Vec::new(),
                    stack_trace: "panic: boom\nacme/app.Foo()\n /app/foo.go:17".into(),
                    ..Default::default()
                },
                now,
            )
            .expect("signal");
        let fp = store.list_issues().expect("issues")[0].fingerprint.clone();
        store
            .set_state(&fp, shanhaijing::State::Triggered, now, None)
            .expect("triggered");
        store
            .set_state(&fp, shanhaijing::State::Fixing, now, None)
            .expect("fixing");
        store
            .start_run(&shanhaijing::RunStart {
                id: "recovery-run".into(),
                fingerprint: fp.clone(),
                runner_id: "original".into(),
                repo: "acme/app".into(),
                started_at: now,
                provider: None,
                model: None,
            })
            .expect("run");
        let claim = store
            .claim_delivery(
                &shanhaijing::DeliveryStart {
                    id: "delivery".into(),
                    run_id: Some("recovery-run".into()),
                    fingerprint: fp,
                    repo: "acme/app".into(),
                    branch: "agent/fix/test".into(),
                    base: "main".into(),
                    head_sha: "verified-sha".into(),
                    now,
                },
                "original",
                60_000,
            )
            .expect("claim")
            .expect("owned");
        assert!(store
            .reserve_pr_slot_owned(&claim, 5, now)
            .expect("reserve"));
        (store, claim)
    }

    fn recovered_pr(head: &str, base: &str) -> deliver::RecoveredPullRequest {
        deliver::RecoveredPullRequest {
            url: "https://github.com/acme/app/pull/7".into(),
            number: 7,
            head_sha: head.into(),
            base_ref: base.into(),
            created_at_ms: None,
        }
    }

    #[tokio::test]
    async fn post_push_token_denial_preserves_issue_run_and_recovery_reservation() {
        for gate in [
            quota::TokenGateExceeded::Individual { spent: 2, limit: 1 },
            quota::TokenGateExceeded::Daily { spent: 2, limit: 1 },
            quota::TokenGateExceeded::Monthly { spent: 2, limit: 1 },
        ] {
            let (store, claim) = recovery_fixture();
            store
                .mark_delivery_owned(
                    &claim,
                    shanhaijing::DeliveryState::Pushed,
                    None,
                    None,
                    Some("remote tip verified"),
                    now_ms(),
                )
                .expect("pushed");
            let before = store.list_issues().expect("issues").remove(0);
            let config: RepoInstructions = serde_json::from_value(serde_json::json!({
                "repo": "acme/app", "failing_check": {"command": "unused"},
            }))
            .expect("config");
            let live = Arc::new(tokio::sync::Mutex::new(RunnerLive::new()));
            assert!(!finish_token_gate(
                &store,
                "recovery-run",
                &before.fingerprint,
                &config,
                gate,
                &live,
                &secrets::Secrets::default(),
            )
            .await
            .expect("stand down"));
            let after = store.list_issues().expect("issues").remove(0);
            assert_eq!(
                serde_json::to_value(after).expect("after"),
                serde_json::to_value(before).expect("before")
            );
            let run = store
                .run_by_id("recovery-run")
                .expect("lookup")
                .expect("run");
            assert!(run.finished_at.is_none());
            assert!(run.outcome.is_none());
            assert_eq!(
                store.pending_deliveries("acme/app").expect("pending")[0].state,
                shanhaijing::DeliveryState::Pushed
            );
            assert!(store.delivery_claim_valid(&claim, now_ms()).expect("owned"));
            let now = now_ms();
            assert_eq!(
                store
                    .pr_slots_used("acme/app", now - now.rem_euclid(86_400_000))
                    .expect("slots"),
                1
            );
            assert_eq!(live.lock().await.status, "standing_down");
        }
    }

    #[tokio::test]
    async fn work_pr_quota_releases_without_consuming_an_attempt() {
        let (store, claim) = recovery_fixture();
        store
            .mark_delivery_owned(
                &claim,
                shanhaijing::DeliveryState::Failed,
                None,
                None,
                Some("quota denied before push"),
                now_ms(),
            )
            .expect("definitive denial");
        let fp = claim.delivery.fingerprint.clone();
        let live = Arc::new(tokio::sync::Mutex::new(RunnerLive::new()));
        assert!(!finish_pr_quota(&store, "recovery-run", &fp, &live)
            .await
            .expect("quota outcome"));
        let issue = store.list_issues().expect("issues").remove(0);
        assert_eq!(issue.state, "new");
        assert_eq!(issue.attempts, 0);
        assert!(issue.owner_runner_id.is_none());
        assert_eq!(
            store
                .count_ready_for_repo(now_ms(), "acme/app")
                .expect("cooldown"),
            0
        );
        let run = store
            .run_by_id("recovery-run")
            .expect("lookup")
            .expect("run");
        assert_eq!(run.outcome.as_deref(), Some("pr_quota"));
        assert_eq!(live.lock().await.status, "standing_down");
    }

    #[tokio::test]
    async fn recovery_atomically_claims_and_adopts_exact_pr_once() {
        let (store, original) = recovery_fixture();
        let created_at = now_ms() - 86_400_000;
        assert!(store
            .release_delivery_claim(&original, now_ms())
            .expect("release"));
        let claims = store
            .claim_pending_deliveries("acme/app", "recovery", now_ms(), 60_000, 1)
            .expect("claim");
        let claim = &claims[0];
        assert!(store
            .claim_pending_deliveries("acme/app", "competitor", now_ms(), 60_000, 1)
            .expect("competitor")
            .is_empty());
        assert!(reconcile_delivery_claim(
            &store,
            claim,
            "acme/app",
            3,
            async {
                let mut pr = recovered_pr("verified-sha", "main");
                pr.created_at_ms = Some(created_at);
                Ok(Some(pr))
            },
            || async { panic!("exact existing PR must never trigger deletion") }
        )
        .await
        .expect("adopt"));
        let issue = &store.list_issues().expect("issues")[0];
        assert_eq!(issue.state, "pr_open");
        let run = store
            .run_for(&issue.fingerprint, "recovery-run")
            .expect("run")
            .expect("exists");
        assert_eq!(run.commit_sha.as_deref(), Some("verified-sha"));
        assert_eq!(run.pr_opened_at, Some(created_at));
        assert_eq!(
            run.pr_url.as_deref(),
            Some("https://github.com/acme/app/pull/7")
        );
        let watch = store
            .ci_watch_for(&issue.fingerprint)
            .expect("watch")
            .expect("exists");
        assert_eq!(watch.head_sha, "verified-sha");
        assert!(store
            .pending_deliveries("acme/app")
            .expect("pending")
            .is_empty());
        assert!(reconcile_delivery_claim(
            &store,
            claim,
            "acme/app",
            3,
            async { panic!("terminal claim cannot perform another lookup") },
            || async { panic!("terminal claim cannot delete") }
        )
        .await
        .is_err());
    }

    #[tokio::test]
    async fn recovery_preserves_unknown_mismatched_or_changed_remote_evidence() {
        for scenario in 0..4 {
            let (store, claim) = recovery_fixture();
            let deleted = std::sync::atomic::AtomicBool::new(false);
            assert!(!reconcile_delivery_claim(
                &store,
                &claim,
                "acme/app",
                3,
                async {
                    match scenario {
                        0 => Err(anyhow::anyhow!("lookup unavailable")),
                        1 => Ok(Some(recovered_pr("different-sha", "main"))),
                        2 => Ok(Some(recovered_pr("verified-sha", "different-base"))),
                        _ => Ok(None),
                    }
                },
                || async {
                    assert_eq!(scenario, 3, "only definitive absence can request deletion");
                    deleted.store(true, std::sync::atomic::Ordering::SeqCst);
                    bail!("remote SHA mismatch; branch retained")
                }
            )
            .await
            .expect("inconclusive"));
            assert_eq!(
                deleted.load(std::sync::atomic::Ordering::SeqCst),
                scenario == 3
            );
            assert_eq!(
                store.pending_deliveries("acme/app").expect("pending")[0].state,
                shanhaijing::DeliveryState::Ambiguous
            );
            let issue = &store.list_issues().expect("issues")[0];
            assert_eq!(issue.state, "fixing");
            assert_eq!(issue.attempts, 0);
            assert!(store
                .ci_watch_for(&issue.fingerprint)
                .expect("watch")
                .is_none());
        }
    }

    #[tokio::test]
    async fn recovery_lost_claim_does_not_adopt_or_delete_or_overwrite_successor() {
        let (store, original) = recovery_fixture();
        let successor = std::sync::Mutex::new(None);
        assert!(reconcile_delivery_claim(
            &store,
            &original,
            "acme/app",
            3,
            async {
                assert!(store.release_delivery_claim(&original, now_ms())?);
                *successor.lock().expect("lock") = store
                    .claim_pending_deliveries("acme/app", "successor", now_ms(), 60_000, 1)?
                    .pop();
                Ok(Some(recovered_pr("verified-sha", "main")))
            },
            || async { panic!("lost ownership cannot delete") }
        )
        .await
        .is_err());
        let next = successor.lock().expect("lock").take().expect("successor");
        deliver::ensure_delivery_owned(&store, &next).expect("successor still owns");
        let issue = &store.list_issues().expect("issues")[0];
        assert_eq!(issue.state, "fixing");
        assert!(delivery_requires_recovery(
            &store,
            "acme/app",
            "recovery-run",
            &anyhow::anyhow!("transport failed")
        )
        .expect("preserve"));
        assert!(store
            .run_for(&issue.fingerprint, "recovery-run")
            .expect("run")
            .expect("exists")
            .finished_at
            .is_none());
    }

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
        let id = default_runner_id("example.com/spike-target", "synthetic-host");
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
            ..Default::default()
        };
        store.report_signal(&signal, now).expect("report");
        let delivery = store
            .claim_delivery(
                &shanhaijing::DeliveryStart {
                    id: "prior-standalone".into(),
                    run_id: None,
                    fingerprint: "prior-standalone".into(),
                    repo: signal.repo.clone(),
                    branch: "agent/fix/prior".into(),
                    base: "main".into(),
                    head_sha: "prior-head".into(),
                    now,
                },
                "prior-owner",
                60_000,
            )
            .expect("claim")
            .expect("owned");
        assert!(store
            .reserve_pr_slot_owned(&delivery, 1, now)
            .expect("admit"));
        store
            .complete_delivery_owned(
                &delivery,
                "https://github.com/acme/spike/pull/1",
                1,
                None,
                now,
            )
            .expect("confirmed");
        let day_start = now - now.rem_euclid(86_400_000);
        assert_eq!(
            store.pr_slots_used(&signal.repo, day_start).expect("slots"),
            1
        );
        assert_eq!(
            store
                .prs_opened_since(&signal.repo, day_start)
                .expect("historical metric"),
            0
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
            denied_read_paths: Vec::new(),
            verification_policy: Default::default(),
            trusted_verification: None,
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
            ..Default::default()
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
            denied_read_paths: Vec::new(),
            verification_policy: Default::default(),
            trusted_verification: None,
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
            "denied_read_paths": cfg.denied_read_paths,
            "verification_policy": match cfg.verification_policy {
                instructions::VerificationPolicy::Compatibility => "compatibility",
                instructions::VerificationPolicy::Strict => "strict",
            },
            "trusted_verification": cfg.trusted_verification.as_ref().map(|spec| serde_json::json!({
                "check": check(&spec.check), "protected_inputs": spec.protected_inputs,
            })),
            "max_attempts": cfg.max_attempts, "draft_first_n": cfg.draft_first_n,
        });
        let path = repo.join("nuwa.repo.json");
        std::fs::write(&path, value.to_string()).expect("test instructions");
        instructions::TrustedInstructions::load(
            repo,
            &path,
            "main",
            &repo.parent().expect("fixture parent").join("test-trusted"),
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

    fn quota_work_fixture() -> (
        tempfile::TempDir,
        PathBuf,
        Arc<shanhaijing::Store>,
        RepoInstructions,
    ) {
        let dir = tempfile::tempdir().expect("fixture");
        let repo = dir.path().join("repo");
        init_git_repo(&repo);
        std::fs::write(repo.join("calc.go"),
            "package main\nfunc Average(xs []float64) float64 {\nvar sum float64\nfor i := 0; i <= len(xs); i++ {\nsum += xs[i]\n}\nreturn sum / float64(len(xs))\n}\n"
        ).expect("source");
        git(&repo, &["add", "calc.go"]);
        git(&repo, &["commit", "-m", "seed faux repair"]);
        let config: RepoInstructions = serde_json::from_value(serde_json::json!({
            "repo": "example.com/quota-work", "failing_check": {"command": fixed_loop_check()},
        }))
        .expect("config");
        let store = Arc::new(shanhaijing::Store::open(":memory:").expect("store"));
        store.report_signal(&shanhaijing::Signal {
            repo: config.repo.clone(), language: "go".into(), title: "panic in Average".into(),
            severity: "high".into(),
            stack_trace: "panic: boom\ngoroutine 1 [running]:\nexample.com/quota-work.Average()\n\t/app/calc.go:17 +0x1".into(),
            ..Default::default()
        }, now_ms()).expect("signal");
        (dir, repo, store, config)
    }

    fn standalone_cli_fixture(
        max_turns: u32,
        no_deliver: bool,
    ) -> (
        tempfile::TempDir,
        Cli,
        issue::Issue,
        Arc<shanhaijing::Store>,
        String,
    ) {
        let (dir, repo, store, config) = quota_work_fixture();
        test_trusted_instructions(&repo, &config);
        let snapshot = store.ready_issue(now_ms()).expect("ready").expect("issue");
        let prob = pipeline::issue_from_snapshot(
            &snapshot,
            &config,
            String::new(),
            &secrets::Secrets::default(),
        );
        let cli = Cli {
            issue_path: dir.path().join("unused.json"),
            db: dir.path().join("unused.db"),
            sessions_dir: dir.path().join("cli-sessions"),
            repo,
            provider: repair::ProviderKind::Faux,
            max_turns,
            push: false,
            create_pr: false,
            no_deliver,
        };
        (dir, cli, prob, store, config.repo)
    }

    async fn run_standalone_fixture(
        cli: Cli,
        prob: issue::Issue,
        store: &Arc<shanhaijing::Store>,
        limits: quota::DeliveryLimits,
    ) -> anyhow::Result<()> {
        let checkout = pipeline::CheckoutLease::acquire(store, &cli.repo, "cli-test", None)?;
        checkout
            .during(fix_with_limits(
                cli,
                prob,
                Arc::new(secrets::Secrets::default()),
                store,
                &checkout,
                limits,
            ))
            .await
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn standalone_cli_persists_failed_usage_without_claiming_known_issue() {
        let (_dir, cli, prob, store, repo) = standalone_cli_fixture(1, true);
        store
            .start_run(&shanhaijing::RunStart {
                id: "existing-owner".into(),
                fingerprint: prob.fingerprint.clone(),
                runner_id: "existing-runner".into(),
                repo: repo.clone(),
                started_at: now_ms(),
                provider: None,
                model: None,
            })
            .expect("existing claim");
        let before = store.list_issues().expect("before").remove(0);
        let error =
            run_standalone_fixture(cli, prob.clone(), &store, quota::DeliveryLimits::default())
                .await
                .expect_err("one faux turn fails");
        let run = store
            .runs_for(&prob.fingerprint, 10)
            .expect("runs")
            .into_iter()
            .find(|run| run.runner_id.starts_with("fix:"))
            .expect("CLI run");
        assert!(
            uuid::Uuid::parse_str(&run.id).is_ok(),
            "real standalone UUID"
        );
        assert_eq!(run.repo, repo);
        assert_eq!(run.provider.as_deref(), Some("faux"));
        assert_eq!(run.model.as_deref(), Some("faux"));
        assert_eq!(run.outcome.as_deref(), Some("repair_failed"));
        assert!(run
            .error
            .as_deref()
            .expect("real reason")
            .contains(&format!("{error:#}")));
        assert_eq!(run.turns, 1);
        assert!(run.tokens_uncached > 0);
        assert_eq!(
            store.tokens_used_since(Some(&repo), 0).expect("spent"),
            (run.tokens_uncached + run.tokens_cached) as u64
        );
        let after = store.list_issues().expect("after").remove(0);
        assert_eq!(
            serde_json::to_value(after).expect("after"),
            serde_json::to_value(before).expect("before")
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn standalone_cli_keeps_usage_for_no_deliver_token_denial_and_local_delivery() {
        for outcome in ["no_deliver", "budget_exceeded", "delivered"] {
            let (_dir, cli, prob, store, repo) = standalone_cli_fixture(5, outcome == "no_deliver");
            let result = run_standalone_fixture(
                cli,
                prob.clone(),
                &store,
                quota::DeliveryLimits {
                    daily_pr_limit: 0, // Branch-only/no-deliver runs need no PR slot.
                    max_tokens: if outcome == "budget_exceeded" {
                        0
                    } else {
                        u64::MAX
                    },
                    ..Default::default()
                },
            )
            .await;
            assert_eq!(result.is_ok(), outcome != "budget_exceeded", "{result:?}");
            let run = store
                .latest_run(&prob.fingerprint)
                .expect("latest")
                .expect("run");
            assert_eq!(run.outcome.as_deref(), Some(outcome));
            assert!(run.tokens_uncached > 0);
            assert_eq!(
                store.tokens_used_since(Some(&repo), 0).expect("spent once"),
                (run.tokens_uncached + run.tokens_cached) as u64
            );
            let issue = store.list_issues().expect("issues").remove(0);
            assert_eq!(issue.state, "new");
            assert_eq!(issue.attempts, 0);
            assert!(issue.owner_runner_id.is_none());
            if outcome == "delivered" {
                assert!(run.branch.is_some());
                assert!(run.commit_sha.is_some());
                assert_eq!(run.verification_ok, Some(true));
            }
        }
    }

    #[tokio::test]
    async fn standalone_cli_rejects_exhausted_quotas_before_model_preparation() {
        for scope in ["daily", "monthly", "pr"] {
            let (_dir, mut cli, prob, store, repo) = standalone_cli_fixture(5, false);
            let mut limits = quota::DeliveryLimits::default();
            if scope == "pr" {
                cli.create_pr = true;
                limits.daily_pr_limit = 1;
                let now = now_ms();
                let claim = store
                    .claim_delivery(
                        &shanhaijing::DeliveryStart {
                            id: "prior-pr".into(),
                            run_id: None,
                            fingerprint: prob.fingerprint.clone(),
                            repo: repo.clone(),
                            branch: "agent/fix/prior".into(),
                            base: "main".into(),
                            head_sha: "prior-sha".into(),
                            now,
                        },
                        "prior",
                        60_000,
                    )
                    .expect("claim")
                    .expect("owned");
                assert!(store
                    .reserve_pr_slot_owned(&claim, 1, now)
                    .expect("reserve"));
                store
                    .complete_delivery_owned(
                        &claim,
                        "https://github.com/acme/app/pull/1",
                        1,
                        None,
                        now,
                    )
                    .expect("confirmed canonical slot without a run");
            } else {
                store
                    .start_standalone_run(&shanhaijing::RunStart {
                        id: "prior-failure".into(),
                        fingerprint: "not-an-issue".into(),
                        runner_id: "cli".into(),
                        repo: repo.clone(),
                        started_at: now_ms(),
                        provider: None,
                        model: None,
                    })
                    .expect("prior run");
                store
                    .update_run(
                        "prior-failure",
                        &shanhaijing::RunUpdate {
                            tokens_uncached: Some(30),
                            tokens_cached: Some(20),
                            ..Default::default()
                        },
                    )
                    .expect("usage");
                store
                    .finish_run(
                        "prior-failure",
                        &shanhaijing::RunFinish {
                            finished_at: now_ms(),
                            outcome: "repair_failed".into(),
                            ci_verdict: None,
                            error: None,
                        },
                    )
                    .expect("failed usage counts");
                if scope == "daily" {
                    limits.daily_token_limit = 50;
                } else {
                    limits.monthly_token_limit = 50;
                }
            }
            let before = store.list_issues().expect("before");
            let sessions = cli.sessions_dir.clone();
            let error = run_standalone_fixture(cli, prob.clone(), &store, limits)
                .await
                .expect_err("exact threshold is exhausted before spending");
            if scope == "pr" {
                assert!(error.downcast_ref::<deliver::PrQuotaExceeded>().is_some());
            } else {
                assert!(format!("{error:#}").contains(scope));
            }
            assert!(!sessions.exists(), "model preparation never started");
            assert!(store
                .runs_for(&prob.fingerprint, 10)
                .expect("no new run")
                .is_empty());
            assert_eq!(
                serde_json::to_value(store.list_issues().expect("after")).expect("after"),
                serde_json::to_value(before).expect("before")
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn final_token_gate_releases_faux_work_after_initial_admission() {
        for monthly in [false, true] {
            let (dir, repo, store, config) = quota_work_fixture();
            let original = git_output(&repo, &["rev-parse", "HEAD"]);
            assert_eq!(
                store
                    .tokens_used_since(Some(&config.repo), 0)
                    .expect("unspent"),
                0
            );
            let live = Arc::new(tokio::sync::Mutex::new(RunnerLive::new()));
            let worked = work_once(
                &store,
                &mut test_trusted_instructions(&repo, &config),
                &repo,
                dir.path().join("sessions"),
                repair::ProviderKind::Faux,
                5,
                false,
                false,
                5,
                1_000_000,
                if monthly { 5_000_000 } else { 1 },
                if monthly { 1 } else { 50_000_000 },
                600,
                "quota-test",
                Arc::clone(&live),
                &Arc::new(secrets::Secrets::default()),
            )
            .await
            .expect("work");
            assert!(!worked, "aggregate quota denial stands down");
            let issue = store.list_issues().expect("issues").remove(0);
            assert_eq!(issue.state, "new");
            assert_eq!(issue.attempts, 0, "aggregate spend is not an issue fault");
            let run = store
                .latest_run(&issue.fingerprint)
                .expect("latest")
                .expect("run");
            assert_eq!(run.outcome.as_deref(), Some("budget_exceeded"));
            assert_eq!(run.turns, 3, "real faux repair reached the final gate");
            assert!(run.tokens_uncached + run.tokens_cached > 1);
            assert!(run
                .error
                .as_deref()
                .expect("truthful reason")
                .contains(if monthly { "monthly" } else { "daily" }));
            assert_eq!(
                store
                    .tokens_used_since(Some(&config.repo), 0)
                    .expect("spend"),
                (run.tokens_uncached + run.tokens_cached) as u64
            );
            assert!(
                repo.join("fixed.marker").exists(),
                "repair actually executed"
            );
            assert_eq!(git_output(&repo, &["rev-parse", "HEAD"]), original);
            assert_eq!(git_output(&repo, &["branch", "--list", "agent/fix/*"]), "");
            assert_eq!(live.lock().await.status, "standing_down");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn failed_faux_work_persists_usage_before_classifying_result() {
        let (dir, repo, store, config) = quota_work_fixture();
        work_once(
            &store,
            &mut test_trusted_instructions(&repo, &config),
            &repo,
            dir.path().join("sessions"),
            repair::ProviderKind::Faux,
            1,
            false,
            false,
            5,
            1_000_000,
            5_000_000,
            50_000_000,
            600,
            "quota-test",
            Arc::new(tokio::sync::Mutex::new(RunnerLive::new())),
            &Arc::new(secrets::Secrets::default()),
        )
        .await
        .expect("failed repair is a business outcome");
        let issue = store.list_issues().expect("issues").remove(0);
        let run = store
            .latest_run(&issue.fingerprint)
            .expect("latest")
            .expect("run");
        assert_eq!(run.outcome.as_deref(), Some("repair_failed"));
        assert_eq!(run.turns, 1);
        assert!(
            run.tokens_uncached > 0,
            "the failed turn still consumed tokens"
        );
        assert_eq!(
            store
                .tokens_used_since(Some(&config.repo), 0)
                .expect("spend"),
            (run.tokens_uncached + run.tokens_cached) as u64
        );
        assert_eq!(issue.attempts, 1);
    }

    #[tokio::test]
    async fn final_usage_persistence_is_monotone_and_never_ignores_write_failure() {
        let (_dir, _repo, store, config) = quota_work_fixture();
        let fp = store.list_issues().expect("issues")[0].fingerprint.clone();
        store
            .start_run(&shanhaijing::RunStart {
                id: "usage".into(),
                fingerprint: fp.clone(),
                runner_id: "test".into(),
                repo: config.repo,
                started_at: now_ms(),
                provider: None,
                model: None,
            })
            .expect("run");
        let report = repair::RepairReport {
            run_id: "harness".into(),
            root_cause_text: String::new(),
            turns: 2,
            uncached_tokens: 20,
            cached_tokens: 50,
            session_dir: String::new(),
            context: Default::default(),
        };
        let mut progress = repair::RepairProgress {
            turns: 3,
            uncached_tokens: 100,
            cached_tokens: 30,
        };
        accumulate_usage(
            &mut progress,
            repair::RepairProgress {
                turns: 1,
                uncached_tokens: 1,
                cached_tokens: 40,
            },
        );
        assert_eq!(
            (
                progress.turns,
                progress.uncached_tokens,
                progress.cached_tokens
            ),
            (3, 100, 40)
        );
        persist_final_usage(&store, "usage", progress, Some(&report))
            .await
            .expect("final usage");
        persist_final_usage(
            &store,
            "usage",
            repair::RepairProgress {
                turns: 1,
                uncached_tokens: 1,
                cached_tokens: 1,
            },
            None,
        )
        .await
        .expect("late lower snapshot");
        let run = store.run_for(&fp, "usage").expect("lookup").expect("run");
        assert_eq!(
            (run.turns, run.tokens_uncached, run.tokens_cached),
            (3, 100, 50)
        );
        store
            .finish_run(
                "usage",
                &shanhaijing::RunFinish {
                    finished_at: now_ms(),
                    outcome: "repair_failed".into(),
                    ci_verdict: None,
                    error: None,
                },
            )
            .expect("finish");
        for id in ["usage", "missing"] {
            let error = persist_final_usage(&store, id, progress, None)
                .await
                .expect_err("write fails");
            assert!(format!("{error:#}").contains("persist final usage"));
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
    async fn strict_owner_configuration_fault_releases_work_without_consuming_an_attempt() {
        let dir = tempfile::tempdir().expect("fixture");
        let repo = dir.path().join("repo");
        init_git_repo(&repo);
        let config: RepoInstructions = serde_json::from_value(serde_json::json!({
            "repo": "example.com/strict", "failing_check": {"command": "echo ok"},
            "verification_policy": "strict",
        }))
        .expect("config");
        let mut trusted = test_trusted_instructions(&repo, &config);
        let store = Arc::new(shanhaijing::Store::open(":memory:").expect("store"));
        store.report_signal(&shanhaijing::Signal {
            repo: config.repo.clone(), language: "go".into(), title: "panic".into(),
            severity: "high".into(), message: None, log_window: Vec::new(),
            stack_trace: "panic: boom\ngoroutine 1 [running]:\nexample.com/strict.F()\n\t/app/f.go:1 +0x1".into(),
            ..Default::default()
        }, now_ms()).expect("signal");
        let tip = git_output(&repo, &["rev-parse", "HEAD"]);
        let live = Arc::new(tokio::sync::Mutex::new(RunnerLive::new()));
        let worked = work_once(
            &store,
            &mut trusted,
            &repo,
            dir.path().join("sessions"),
            repair::ProviderKind::Faux,
            1,
            false,
            false,
            10,
            100_000,
            1_000_000,
            1_000_000,
            1,
            "strict-test",
            live,
            &Arc::new(secrets::Secrets::default()),
        )
        .await
        .expect("configuration stand-down");
        assert!(!worked);
        let rows = store.list_issues().expect("issues");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].attempts, 0);
        assert_eq!(rows[0].state, "new");
        assert_eq!(git_output(&repo, &["rev-parse", "HEAD"]), tip);
        assert!(
            git_output(&repo, &["for-each-ref", "refs/heads/agent/fix/"])
                .trim()
                .is_empty()
        );
        assert!(
            !dir.path().join("sessions").exists(),
            "no model/session writes"
        );
    }

    #[tokio::test]
    async fn preflight_recovers_from_agent_branch() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        init_git_repo(&repo);
        git(&repo, &["checkout", "-b", "agent/fix/abc"]);

        let store = Arc::new(shanhaijing::Store::open(":memory:").expect("store"));
        let checkout =
            pipeline::CheckoutLease::acquire(&store, &repo, "test", None).expect("checkout");
        let base = checkout
            .during(preflight_repo(&repo, true, &checkout))
            .await
            .expect("preflight");
        assert_eq!(base, "main");
        assert_eq!(
            git_output(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
            "main"
        );
    }

    #[tokio::test]
    async fn preflight_preserves_dirty_or_unowned_checkout() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        init_git_repo(&repo);
        git(&repo, &["checkout", "-b", "agent/fix/evidence"]);
        std::fs::write(repo.join("evidence.txt"), "interrupted repair").expect("evidence");
        let store = Arc::new(shanhaijing::Store::open(":memory:").expect("store"));
        let checkout =
            pipeline::CheckoutLease::acquire(&store, &repo, "test", None).expect("claim");
        assert!(preflight_repo(&repo, false, &checkout).await.is_err());
        assert!(store
            .release_checkout_claim(&checkout.claim, now_ms())
            .expect("release"));
        assert!(preflight_repo(&repo, false, &checkout).await.is_err());
        assert_eq!(
            git_ops::current_branch(&repo).expect("branch"),
            "agent/fix/evidence"
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("evidence.txt")).expect("preserved"),
            "interrupted repair"
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

        let store = Arc::new(shanhaijing::Store::open(":memory:").expect("store"));
        let checkout =
            pipeline::CheckoutLease::acquire(&store, &work, "test", None).expect("checkout");
        checkout
            .during(preflight_repo(&work, true, &checkout))
            .await
            .expect("preflight");
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
                    ..Default::default()
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
            denied_read_paths: Vec::new(),
            verification_policy: Default::default(),
            trusted_verification: None,
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
                    ..Default::default()
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
                ..Default::default()
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
    async fn cleanup_terminal_branches_retains_without_claimed_tip_evidence() {
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
                ..Default::default()
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
        assert_eq!(removed, 0, "terminal state is not deletion authority");
        for (branch, target) in states {
            let exists = git_ops::branch_exists(&repo, &branch).expect("exists");
            assert!(exists, "{branch} ({target}) must be preserved");
        }
    }

    #[tokio::test]
    async fn remove_stale_branch_retains_unclaimed_local_branch() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        init_git_repo(&repo);
        git(&repo, &["branch", "agent/fix/fp"]);
        assert!(git_ops::branch_exists(&repo, "agent/fix/fp").expect("exists"));
        remove_stale_branch(&repo, "fp", false)
            .await
            .expect_err("unclaimed branch retained");
        assert!(git_ops::branch_exists(&repo, "agent/fix/fp").expect("preserved"));
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
            ..Default::default()
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
            denied_read_paths: Vec::new(),
            verification_policy: Default::default(),
            trusted_verification: None,
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
            ..Default::default()
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
            denied_read_paths: Vec::new(),
            verification_policy: Default::default(),
            trusted_verification: None,
            max_attempts: 2,
            draft_first_n: 0,
        };
        let live: SharedLive = Arc::new(tokio::sync::Mutex::new(RunnerLive::new()));

        let original = std::fs::read(repo.join("calc.go")).expect("original fixture");
        for (step, expected) in ["new", "new", "ignored"].into_iter().enumerate() {
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
            if step == 1 {
                // The dirty retry must preserve the first attempt, not silently
                // clean it up or consume the second attempt. Simulate an owner
                // saving the evidence and restoring this synthetic checkout.
                assert_eq!(store.attempts_for(&fp).expect("attempts"), 1);
                assert!(std::fs::read_to_string(repo.join("calc.go"))
                    .expect("preserved repair")
                    .contains("i < len(xs)"));
                std::fs::rename(repo.join("fixed.marker"), dir.path().join("saved.marker"))
                    .expect("save evidence outside checkout");
                std::fs::write(repo.join("calc.go"), &original).expect("owner restores fixture");
            }
            store
                .set_cooldown(&fp, 0)
                .expect("retry immediately in test");
        }
        assert_eq!(store.attempts_for(&fp).expect("attempts"), 2);
        assert!(dir.path().join("saved.marker").is_file());
    }

    #[tokio::test]
    async fn failure_recovery_without_pr_leaves_issue_and_run_unchanged() {
        let (store, claim) = recovery_fixture();
        let deleted = std::sync::atomic::AtomicBool::new(false);
        assert!(reconcile_delivery_claim(
            &store,
            &claim,
            "acme/app",
            2,
            async { Ok(None) },
            || async {
                deliver::ensure_delivery_owned(&store, &claim)?;
                deleted.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            }
        )
        .await
        .expect("failed delivery recorded"));
        assert!(deleted.load(std::sync::atomic::Ordering::SeqCst));
        assert!(!store
            .delivery_claim_valid(&claim, now_ms())
            .expect("terminal clears claim"));
        assert!(store
            .pending_deliveries("acme/app")
            .expect("pending")
            .is_empty());
        let issue = store.list_issues().expect("issues").pop().expect("issue");
        assert_eq!(issue.state, "fixing");
        assert_eq!(issue.attempts, 0);
        let run = store
            .run_for(&issue.fingerprint, "recovery-run")
            .expect("run")
            .expect("exists");
        assert!(run.finished_at.is_none());
        assert!(run.pr_url.is_none());
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
            ..Default::default()
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
            denied_read_paths: Vec::new(),
            verification_policy: Default::default(),
            trusted_verification: None,
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
            ..Default::default()
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
            .record_verified_repair_memory(
                &shanhaijing::RepairMemoryEntry {
                    repo: "example.com/spike".into(),
                    fingerprint: "other-fp".into(),
                    commit_sha: "0123456789abcdef0123456789abcdef01234567".into(),
                    pr_url: Some("https://github.com/acme/spike/pull/7".into()),
                    root_cause: "ROOT CAUSE: nil map guard".into(),
                    change_stat: " calc.go | 2 +-".into(),
                    files: 1,
                    recorded_at: now,
                },
                "local_verification",
                "other-memory-run",
            )
            .expect("memory");
        store
            .record_verified_repair_memory(
                &shanhaijing::RepairMemoryEntry {
                    repo: "example.com/spike".into(),
                    fingerprint: fp.clone(),
                    commit_sha: "feedface".into(),
                    pr_url: None,
                    root_cause: "ROOT CAUSE: current issue memory must be excluded".into(),
                    change_stat: String::new(),
                    files: 1,
                    recorded_at: now,
                },
                "local_verification",
                "current-memory-run",
            )
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

    fn work_test_environment(dir: &Path) -> BTreeMap<String, String> {
        BTreeMap::from([
            (
                "NUWA_ENV_FILE".into(),
                dir.join("nuwa.env").display().to_string(),
            ),
            ("HOSTNAME".into(), "synthetic-host".into()),
        ])
    }

    async fn run_test_work(
        args: Vec<String>,
        environment: BTreeMap<String, String>,
    ) -> anyhow::Result<()> {
        run_work_with_environment(
            args,
            environment,
            uuid::Uuid::new_v4().to_string(),
            now_ms(),
            Arc::new(secrets::Secrets::default()),
        )
        .await
    }

    // Process-level evidence: the `run_work` engine must register the runner,
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
            ..Default::default()
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
                "--service-unit".to_string(),
                "nuwa-synthetic-work".to_string(),
                "--daily-pr-limit".to_string(),
                "7".to_string(),
                "--daily-token-limit".to_string(),
                "7000000".to_string(),
                "--monthly-token-limit".to_string(),
                "70000000".to_string(),
                "--ci-timeout".to_string(),
                "45".to_string(),
            ]
        };

        run_test_work(
            args(&db, &repo, &sessions),
            work_test_environment(dir.path()),
        )
        .await
        .expect("first run");

        let store = Store::open(&db).expect("reopen");
        let runners = store.list_runners().expect("runners");
        assert!(
            runners.iter().any(|r| r.id == "test:runner"),
            "runner must be registered"
        );
        let row = runners
            .iter()
            .find(|r| r.id == "test:runner")
            .expect("runner");
        let startup_json = row
            .startup_config_json
            .as_deref()
            .expect("startup evidence");
        let startup: config_activation::StartupConfig =
            serde_json::from_str(startup_json).expect("snapshot");
        assert_eq!(startup.runner_id.as_deref(), Some("test:runner"));
        assert_eq!(startup.started_at_ms, row.started_at);
        assert!(uuid::Uuid::parse_str(&startup.process_instance_id).is_ok());
        assert!(startup.runner_instance_id.is_some());
        for (key, expected) in [
            ("NUWA_PROVIDER", "faux"),
            ("NUWA_RUNNER_ID", "test:runner"),
            ("NUWA_MAX_TURNS", "5"),
            ("NUWA_INTERVAL", "5"),
            ("NUWA_MAX_TOKENS", "1000000"),
            ("NUWA_SERVICE_UNIT", "nuwa-synthetic-work"),
            ("NUWA_DAILY_PR_LIMIT", "7"),
            ("NUWA_DAILY_TOKEN_LIMIT", "7000000"),
            ("NUWA_MONTHLY_TOKEN_LIMIT", "70000000"),
            ("NUWA_CI_TIMEOUT", "45"),
        ] {
            assert_eq!(
                startup.values[key].value.as_deref(),
                Some(expected),
                "{key}"
            );
            assert_eq!(startup.values[key].source, "cli", "{key}");
        }
        let (_, model, base) =
            repair::actual_provider_config(&repair::ProviderKind::Faux).expect("faux");
        let base = base.map(|base| reqwest::Url::parse(&base).expect("base URL").to_string());
        assert_eq!(startup.values["NUWA_MODEL"].value, model);
        assert_eq!(startup.values["NUWA_BASE_URL"].value, base);
        assert_eq!(startup.values["NUWA_MODEL"].source, "default");
        std::fs::write(dir.path().join("nuwa.env"), "NUWA_MAX_TURNS=99\n").expect("changed config");
        store
            .heartbeat_runner(
                "test:runner",
                &shanhaijing::RunnerHeartbeat {
                    heartbeat_at: now_ms(),
                    status: "idle".into(),
                    current_fingerprint: None,
                    phase: None,
                    config_json: Some("{\"max_turns\":99}".into()),
                    last_error: None,
                },
            )
            .expect("heartbeat after config refresh");
        assert_eq!(
            store.list_runners().expect("runners")[0]
                .startup_config_json
                .as_deref(),
            Some(startup_json)
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

        run_test_work(
            args(&db, &repo, &sessions),
            work_test_environment(dir.path()),
        )
        .await
        .expect("second run");

        let store = Store::open(&db).expect("reopen2");
        let run_b = store.latest_run(&fp_b).expect("latest b").expect("run b");
        assert_eq!(run_b.outcome.as_deref(), Some("interrupted"));
        assert!(run_b.finished_at.is_some());
        let restarted: config_activation::StartupConfig = serde_json::from_str(
            store.list_runners().expect("runners")[0]
                .startup_config_json
                .as_deref()
                .expect("new startup"),
        )
        .expect("snapshot");
        assert_ne!(restarted.process_instance_id, startup.process_instance_id);
        assert_ne!(restarted.runner_instance_id, startup.runner_instance_id);
    }

    #[test]
    fn work_startup_preserves_unknown_anthropic_model_requests() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = shanhaijing::Store::open(dir.path().join("startup.db")).expect("store");
        let mut environment = work_test_environment(dir.path());
        environment.insert("NUWA_PROVIDER".into(), "anthropic".into());
        environment.insert(
            "NUWA_MODEL".into(),
            "unknown-environment-anthropic-model".into(),
        );
        let env_path = startup_env_path(&environment);
        for (cli_provider, requested, source) in [
            (true, "unknown-cli-anthropic-model", "cli"),
            (false, "unknown-environment-anthropic-model", "environment"),
        ] {
            let provider = if cli_provider {
                repair::ProviderKind::Anthropic {
                    model: Some(requested.into()),
                }
            } else {
                provider_from_environment(&environment)
            };
            assert!(repair::actual_provider_config(&provider).is_err());
            let mut snapshot = config_activation::StartupConfig::capture(
                &store,
                &env_path,
                "work",
                uuid::Uuid::new_v4().to_string(),
                now_ms(),
                &environment,
            );
            record_startup_provider(&mut snapshot, &provider, cli_provider, &environment);
            let json = serde_json::to_string(&snapshot).expect("serialize snapshot");
            let restored = serde_json::from_str(&json).expect("restore snapshot");
            let saved = config_activation::saved_config(&store, &env_path);
            let snapshot =
                config_activation::project_process(Some(restored), &saved, "online", None)
                    .snapshot
                    .expect("unknown model request remains valid snapshot evidence");
            assert_eq!(
                snapshot.values["NUWA_PROVIDER"].value.as_deref(),
                Some("anthropic")
            );
            assert_eq!(snapshot.values["NUWA_PROVIDER"].source, source);
            assert_eq!(
                snapshot.values["NUWA_MODEL"].value.as_deref(),
                Some(requested)
            );
            assert_eq!(snapshot.values["NUWA_MODEL"].source, source);
            assert!(snapshot.values["NUWA_MODEL"].reason.is_none());
            assert!(snapshot.values["NUWA_BASE_URL"].value.is_none());
        }
    }

    #[tokio::test]
    async fn work_startup_records_actual_defaults_and_provider_specific_precedence() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).expect("repo");
        std::fs::write(
            repo.join("nuwa.repo.json"),
            serde_json::json!({
                "repo": "example.com/startup", "failing_check": {"command": "echo unused"},
                "forbidden_paths": [], "notes": ""
            })
            .to_string(),
        )
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
        let db = dir.path().join("startup.db");
        let mut environment = work_test_environment(dir.path());
        for (key, value) in [
            ("NUWA_PROVIDER", "zai"),
            ("NUWA_MODEL", "environment-model"),
            ("NUWA_BASE_URL", "https://environment.example.test/v4"),
            ("NUWA_MAX_TURNS", "invalid"),
            ("NUWA_INTERVAL", ""),
            ("NUWA_CI_TIMEOUT", "-1"),
            ("NUWA_SERVICE_UNIT", ""),
        ] {
            environment.insert(key.into(), value.into());
        }
        let cases = [
            (
                vec!["--provider", "anthropic"],
                repair::ProviderKind::Anthropic { model: None },
                "default",
            ),
            (
                vec!["--provider", "zai"],
                repair::ProviderKind::Zai {
                    model: None,
                    base_url: None,
                },
                "default",
            ),
            (
                vec!["--provider", "faux"],
                repair::ProviderKind::Faux,
                "default",
            ),
            (
                vec![
                    "--provider",
                    "zai",
                    "--model",
                    "discarded",
                    "--provider",
                    "anthropic",
                ],
                repair::ProviderKind::Anthropic { model: None },
                "default",
            ),
            (
                vec![
                    "--provider",
                    "zai",
                    "--model",
                    "cli-model",
                    "--base-url",
                    "https://cli.example.test/v4",
                ],
                repair::ProviderKind::Zai {
                    model: Some("cli-model".into()),
                    base_url: Some("https://cli.example.test/v4".into()),
                },
                "cli",
            ),
            (
                vec![],
                provider_from_environment(&environment),
                "environment",
            ),
        ];
        for (flags, provider, model_source) in cases {
            let mut args = vec![
                "--db".into(),
                db.display().to_string(),
                "--instructions".into(),
                repo.join("nuwa.repo.json").display().to_string(),
                "--sessions".into(),
                dir.path().join("sessions").display().to_string(),
            ];
            let provider_source = if flags.is_empty() {
                "environment"
            } else {
                "cli"
            };
            args.extend(flags.into_iter().map(str::to_string));
            run_test_work(args, environment.clone())
                .await
                .expect("empty work run");
            let store = shanhaijing::Store::open(&db).expect("store");
            let rows = store.list_runners().expect("runners");
            let snapshot: config_activation::StartupConfig =
                serde_json::from_str(rows[0].startup_config_json.as_deref().expect("startup"))
                    .expect("snapshot");
            let (name, model, base) =
                repair::actual_provider_config(&provider).expect("actual provider");
            let base = base.map(|base| reqwest::Url::parse(&base).expect("base URL").to_string());
            assert_eq!(
                snapshot.values["NUWA_PROVIDER"].value.as_deref(),
                Some(name.as_str())
            );
            assert_eq!(snapshot.values["NUWA_PROVIDER"].source, provider_source);
            assert_eq!(snapshot.values["NUWA_MODEL"].value, model);
            assert_eq!(snapshot.values["NUWA_MODEL"].source, model_source);
            assert_eq!(snapshot.values["NUWA_BASE_URL"].value, base);
            for (key, expected) in [
                ("NUWA_MAX_TURNS", "30"),
                ("NUWA_INTERVAL", "30"),
                ("NUWA_CI_TIMEOUT", "600"),
                ("NUWA_DAILY_PR_LIMIT", "5"),
                ("NUWA_MAX_TOKENS", "1500000"),
                ("NUWA_DAILY_TOKEN_LIMIT", "5000000"),
                ("NUWA_MONTHLY_TOKEN_LIMIT", "50000000"),
                ("NUWA_SERVICE_UNIT", "nuwa-work"),
            ] {
                assert_eq!(
                    snapshot.values[key].value.as_deref(),
                    Some(expected),
                    "{key}"
                );
                assert_eq!(snapshot.values[key].source, "default", "{key}");
            }
            let expected_id = default_runner_id("example.com/startup", "synthetic-host");
            assert_eq!(snapshot.runner_id.as_deref(), Some(expected_id.as_str()));
            assert_eq!(snapshot.values["NUWA_RUNNER_ID"].source, "derived");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pool_runs_two_issues_in_one_process() {
        use shanhaijing::Store;

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

        let db = dir.path().join("pool.db");
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
            ..Default::default()
        };
        let sig_b = shanhaijing::Signal {
            title: "panic in Pipeline".into(),
            log_window: Vec::new(),
            stack_trace: "panic: boom\ngoroutine 1 [running]:\nexample.com/spike.Pipeline()\n\t/app/calc.go:25 +0x1".into(),
            ..sig_a.clone()
        };
        store.report_signal(&sig_a, now).expect("a");
        store.report_signal(&sig_b, now + 1).expect("b");
        let fingerprints: Vec<String> = store
            .list_issues()
            .expect("issues")
            .into_iter()
            .map(|issue| issue.fingerprint)
            .collect();
        assert_eq!(fingerprints.len(), 2);
        let mut environment = work_test_environment(dir.path());
        for (key, value) in [
            ("NUWA_PROVIDER", "faux"),
            ("NUWA_RUNNER_ID", "test:runner"),
            ("NUWA_POOL", "2"),
            ("NUWA_INTERVAL", "5"),
            ("NUWA_MAX_TURNS", "5"),
            ("NUWA_MAX_TOKENS", "1000000"),
        ] {
            environment.insert(key.into(), value.into());
        }
        let revision = uuid::Uuid::new_v4().to_string();
        environment.insert(config_activation::REVISION_KEY.into(), revision.clone());
        let env_path = dir.path().join("nuwa.env");
        let mut saved_environment = environment.clone();
        // The path is a read-only startup input, not a managed env-file key.
        saved_environment.remove("NUWA_ENV_FILE");
        std::fs::write(&env_path, settings::render_env(&saved_environment)).expect("saved env");
        let env_id = config_activation::env_identity(&env_path).expect("env identity");
        store
            .with_config_save(&env_id, |tx| tx.record_saved(&revision, now, &[]))
            .expect("saved revision");
        drop(store);

        run_test_work(
            vec![
                "--db".into(),
                db.display().to_string(),
                "--instructions".into(),
                repo.join("nuwa.repo.json").display().to_string(),
                "--sessions".into(),
                sessions.display().to_string(),
            ],
            environment,
        )
        .await
        .expect("pool run");

        let store = Store::open(&db).expect("reopen");
        let runners = store.list_runners().expect("runners");
        let snapshots: Vec<config_activation::StartupConfig> = runners
            .iter()
            .map(|runner| {
                serde_json::from_str(runner.startup_config_json.as_deref().expect("startup"))
                    .expect("snapshot")
            })
            .collect();
        assert_eq!(snapshots.len(), 2);
        assert_eq!(
            snapshots[0].process_instance_id,
            snapshots[1].process_instance_id
        );
        assert_eq!(snapshots[0].started_at_ms, snapshots[1].started_at_ms);
        assert_eq!(
            snapshots[0].loaded_revision.as_deref(),
            Some(revision.as_str())
        );
        assert_eq!(snapshots[0].loaded_revision, snapshots[1].loaded_revision);
        assert_eq!(snapshots[0].revision_reason, snapshots[1].revision_reason);
        assert_ne!(snapshots[0].runner_id, snapshots[1].runner_id);
        assert_ne!(
            snapshots[0].runner_instance_id,
            snapshots[1].runner_instance_id
        );
        for snapshot in &snapshots {
            assert_eq!(snapshot.values["NUWA_RUNNER_ID"].value, snapshot.runner_id);
        }
        let ids: BTreeSet<String> = runners.into_iter().map(|runner| runner.id).collect();
        assert!(ids.contains("test:runner"), "{ids:?}");
        assert!(ids.contains("test:runner-2"), "{ids:?}");
        for fingerprint in fingerprints {
            let run = store
                .latest_run(&fingerprint)
                .expect("latest")
                .expect("run");
            assert_eq!(run.outcome.as_deref(), Some("success"), "{fingerprint}");
        }
    }

    #[tokio::test]
    async fn runner_live_aggregates_multiple_runs_without_picking_one() {
        let live: SharedLive = Arc::new(tokio::sync::Mutex::new(RunnerLive::new()));
        live_update(&live, "working", Some("fp1"), Some("repair"), None).await;
        let (status, fingerprint, phase, _, _) = live_snapshot(&live).await;
        assert_eq!(status, "working");
        assert_eq!(fingerprint.as_deref(), Some("fp1"));
        assert_eq!(phase.as_deref(), Some("repair"));

        live_update(&live, "working", Some("fp2"), Some("deliver"), None).await;
        let (status, fingerprint, phase, _, _) = live_snapshot(&live).await;
        assert_eq!(status, "working");
        assert!(
            fingerprint.is_none(),
            "two active runs must not arbitrarily pick one"
        );
        assert!(phase.is_none());

        live_end(&live, "fp1").await;
        let (_, fingerprint, phase, _, _) = live_snapshot(&live).await;
        assert_eq!(fingerprint.as_deref(), Some("fp2"));
        assert_eq!(phase.as_deref(), Some("deliver"));

        live_update(&live, "idle", None, None, None).await;
        let (status, fingerprint, phase, _, _) = live_snapshot(&live).await;
        assert_eq!(status, "idle");
        assert!(fingerprint.is_none());
        assert!(phase.is_none());
    }
}
