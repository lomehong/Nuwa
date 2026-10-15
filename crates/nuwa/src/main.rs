mod ci;
mod deliver;
mod guard;
mod instructions;
mod issue;
mod repair;

use std::path::{Path, PathBuf};

use anyhow::{bail, Context};

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
        Some("serve") => run_serve(argv.collect()).await,
        Some("status") => run_status(argv.collect()),
        Some("sync") => run_sync(argv.collect()).await,
        Some("reopen") => run_reopen(argv.collect()),
        other => bail!(
            "usage: nuwa fix --issue <path> --repo <path> [--provider anthropic|faux] [--model <id>] [--max-turns N] [--push] [--pr] [--no-deliver]\n            nuwa ingest --signal <path> [--db <path>]\n            nuwa work --db <path> --instructions <path> [--provider anthropic|faux] [--push] [--pr] [--loop --interval N]\n            nuwa serve --db <path> [--listen 127.0.0.1:7321]   (HTTP ingest for 青鸟 HTTPSink)\n            nuwa status [--db <path>]\n            nuwa sync --db <path>   (pr_open 问题单与 GitHub 对账：merged→merged, closed→rejected)\n            nuwa reopen --db <path> --fingerprint <fp>   (owner reviewed an escalation)\n        (got {other:?})"
        ),
    }
}

/// Shared by `ingest` (file) and `serve` (HTTP body): one JSON object or
/// JSONL — the 青鸟 FileSink/HTTPSink wire formats.
fn parse_signals(raw: &str) -> anyhow::Result<Vec<shanhaijing::Signal>> {
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
    store.set_state(&fp, shanhaijing::State::New, now_ms(), Some("reopened by owner"))?;
    store.set_cooldown(&fp, 0)?;
    println!("reopened   : {fp} — ready for pickup");
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
fn report_one(
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
    let mut db_path = std::path::PathBuf::from("shanhaijing.db");
    let mut instructions_path = None;
    let mut sessions_dir = PathBuf::from("spike/sessions");
    let mut provider = repair::ProviderKind::Anthropic { model: None };
    let mut max_turns = 30u32;
    let mut push = false;
    let mut create_pr = false;
    let mut loop_run = false;
    let mut interval_secs = 30u64;
    let mut daily_pr_limit = 5u64;
    let mut max_tokens: u64 = std::env::var("NUWA_MAX_TOKENS_PER_ISSUE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1_500_000);
    let mut ci_timeout_secs = 600u64;
    let mut it = args.into_iter();
    while let Some(flag) = it.next() {
        let mut value =
            |name: &str| it.next().with_context(|| format!("missing value for {name}"));
        match flag.as_str() {
            "--db" => db_path = PathBuf::from(value("--db")?),
            "--instructions" => instructions_path = Some(PathBuf::from(value("--instructions")?)),
            "--sessions" => sessions_dir = PathBuf::from(value("--sessions")?),
            "--provider" => match value("--provider")?.as_str() {
                "anthropic" => provider = repair::ProviderKind::Anthropic { model: None },
                "faux" => provider = repair::ProviderKind::Faux,
                "zai" => provider = repair::ProviderKind::Zai { base_url: None, model: None },
                other => bail!("unknown provider {other}"),
            },
            "--base-url" => match &mut provider {
                repair::ProviderKind::Zai { base_url, .. } => {
                    *base_url = Some(value("--base-url")?)
                }
                _ => bail!("--base-url only applies to --provider zai"),
            },
            "--model" => {
                let model = value("--model")?;
                match &mut provider {
                    repair::ProviderKind::Anthropic { model: m } => *m = Some(model),
                    repair::ProviderKind::Zai { model: m, .. } => *m = Some(model),
                    repair::ProviderKind::Faux => bail!("--model does not apply to faux"),
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
            other => bail!("unknown work flag {other}"),
        }
    }
    let instructions_path = instructions_path.context("--instructions <path> is required")?;

    let store = shanhaijing::Store::open(&db_path)
        .with_context(|| format!("open db {}", db_path.display()))?;
    let (repo_config, repo_dir) = instructions::RepoInstructions::load(&instructions_path)?;

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

/// One pickup attempt. `Ok(true)` = an issue was claimed and driven to a
/// terminal state this pass; `Ok(false)` = nothing pickable.
#[allow(clippy::too_many_arguments)]
async fn work_once(
    store: &shanhaijing::Store,
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
) -> anyhow::Result<bool> {
    let now = now_ms();

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
            return Ok(false);
        }
    }

    // ---- repo preflight: a dirty or off-main checkout wastes a run ----
    // These are environment faults (a previous run left the checkout behind),
    // not issue faults — bail loudly instead of claiming and escalating.
    let head = deliver::git(repo_dir, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    if head != "main" && head != "master" {
        bail!(
            "repo {} is on branch `{}` — runner only works from main/master; reset the checkout",
            repo_dir.display(),
            head
        );
    }
    let dirty = deliver::git(
        repo_dir,
        &["status", "--porcelain", "--", ":(exclude)nuwa.repo.json"],
    )?;
    if !dirty.trim().is_empty() {
        bail!(
            "repo {} has an unclean working tree — reset before running:\n{}",
            repo_dir.display(),
            dirty.trim()
        );
    }

    // ---- claim (atomic: N runners can never share an issue) ----
    let Some(snapshot) =
        store.claim_ready_issue(now, now + shanhaijing::DEFAULT_COOLDOWN_MS)?
    else {
        println!("nothing ready for pickup");
        return Ok(false);
    };
    if snapshot.repo != repo_config.repo {
        bail!(
            "repo mismatch: issue `{}` belongs to `{}`, instructions are for `{}` — refusing (ADR-004)",
            snapshot.fingerprint,
            snapshot.repo,
            repo_config.repo
        );
    }
    let fp = snapshot.fingerprint.clone();
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
    store.set_state(&fp, shanhaijing::State::Fixing, now, Some("repair run started"))?;

    // ---- repair ----
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
    let guards = std::sync::Arc::new(guard::Guards::new(repo_config.forbidden_paths.clone()));
    let job = repair::RepairJob {
        repo: repo_dir.to_path_buf(),
        issue: prob,
        sessions_dir,
        provider,
        max_turns,
        guards,
        notes: repo_config.notes.clone(),
    };

    let report = match repair::run(&job).await {
        Ok(r) => r,
        Err(e) => {
            let detail = format!("repair failed: {e:#}");
            store.set_state(&fp, shanhaijing::State::Escalated, now_ms(), Some(detail.as_str()))?;
            bail!("repair failed — issue escalated: {e:#}");
        }
    };
    let cause = report
        .root_cause_text
        .lines()
        .find(|l| l.trim_start().starts_with("ROOT CAUSE"))
        .unwrap_or("(no ROOT CAUSE line)");
    println!("root cause : {cause}");

    // ---- budget gate: per-issue token ceiling ----
    // The turn cap bounds the loop mid-run (RPI_MAX_TURNS_PER_RUN); this gate
    // is the spend ceiling — an over-budget run never reaches delivery.
    let spent = report.uncached_tokens + report.cached_tokens;
    if spent > max_tokens {
        let detail = format!("token budget exceeded: {spent} > {max_tokens}");
        store.set_state(&fp, shanhaijing::State::Escalated, now_ms(), Some(detail.as_str()))?;
        bail!("{detail} — issue escalated");
    }
    println!("tokens     : {spent} (cap {max_tokens})");

    // ---- deliver ----
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
            let detail = format!("delivery failed: {e:#}");
            store.set_state(&fp, shanhaijing::State::Escalated, now_ms(), Some(detail.as_str()))?;
            bail!("delivery failed — issue escalated: {e:#}");
        }
    };

    store.set_state(
        &fp,
        shanhaijing::State::PrOpen,
        now_ms(),
        Some(delivered.pr_url.as_deref().unwrap_or("branch pushed, no PR")),
    )?;
    if let Some(url) = &delivered.pr_url {
        store.set_pr(&fp, url, now_ms())?;
    }

    // ---- CI 闸门消费（docs/03 末段）：红 → 自动关 PR + rejected；绿/超时 → 留给人工合并 ----
    if create_pr {
        if let (Some(slug), Some(number)) = (&delivered.slug, delivered.pr_number) {
            let token = std::env::var("NUWA_GITHUB_TOKEN")
                .or_else(|_| std::env::var("GITHUB_TOKEN"))
                .context("NUWA_GITHUB_TOKEN not set — cannot poll CI checks")?;
            println!(
                "ci gate   : polling checks for {} (timeout {ci_timeout_secs}s)…",
                &delivered.commit[..12.min(delivered.commit.len())]
            );
            let checks = ci::wait_for_ci(&token, slug, &delivered.commit, ci_timeout_secs).await;
            match checks {
                Ok(ci::Verdict::Success) => {
                    println!("ci gate   : all checks green — PR open, awaiting human merge");
                }
                Ok(ci::Verdict::Failure) => {
                    ci::close_pr(&token, slug, number).await?;
                    store.set_state(
                        &fp,
                        shanhaijing::State::Rejected,
                        now_ms(),
                        Some(&format!("CI red — PR #{number} closed, back to owner")),
                    )?;
                    bail!("CI red — PR #{number} closed, issue rejected (regression signal will reopen it)");
                }
                Ok(ci::Verdict::TimedOut) => {
                    println!(
                        "ci gate   : checks unfinished in {ci_timeout_secs}s — PR left open, check manually"
                    );
                }
                Err(e) => {
                    // Checks unreadable (fine-grained PAT missing Actions/Checks
                    // read on this repo): the PR stays open, the issue stays
                    // pr_open — merging remains the human decision.
                    println!(
                        "ci gate   : checks unavailable ({e:#}) — PR left open; \
                         grant the PAT Actions:Read-only + Contents:Read-only on this repo"
                    );
                }
            }
        }
    }

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
    for row in store.list_issues()? {
        if row.state != "pr_open" {
            continue;
        }
        let Some(url) = &row.pr_url else { continue };
        let Some((slug, number)) = ci::parse_pr_url(url) else {
            println!("skip       : {} — unparsable PR url {url}", row.fingerprint);
            continue;
        };
        let state = ci::pr_merge_state(&token, &slug, number).await?;
        match state.as_str() {
            "merged" => {
                store.set_state(&row.fingerprint, shanhaijing::State::Merged, now, Some(url))?;
                println!("merged     : {} → {url}", row.fingerprint);
            }
            "closed" => {
                store.set_state(
                    &row.fingerprint,
                    shanhaijing::State::Rejected,
                    now,
                    Some("PR closed without merge"),
                )?;
                println!("rejected   : {} (PR #{number} closed without merge)", row.fingerprint);
            }
            _ => println!("still open : {} — PR #{number}", row.fingerprint),
        }
    }
    Ok(())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// HTTP ingest 服务端 — the online half of the 青鸟 path (HTTPSink → here).
/// POST /ingest with one envelope JSON or JSONL body; GET /health for checks.
/// v0 binds localhost by default: the collector runs on the same trust domain.
async fn run_serve(args: Vec<String>) -> anyhow::Result<()> {
    use axum::extract::State;
    use axum::http::StatusCode;
    use axum::routing::{get, post};
    use std::sync::Arc;

    let mut db_path = std::path::PathBuf::from("shanhaijing.db");
    let mut listen = String::from("127.0.0.1:7321");
    let mut it = args.into_iter();
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--db" => db_path = PathBuf::from(it.next().context("--db needs a value")?),
            "--listen" => listen = it.next().context("--listen needs a value")?,
            other => bail!("unknown serve flag {other}"),
        }
    }
    let store = Arc::new(
        shanhaijing::Store::open(&db_path)
            .with_context(|| format!("open db {}", db_path.display()))?,
    );

    let app = axum::Router::new()
        .route(
            "/ingest",
            // Bytes, not String: the String extractor enforces a text/plain
            // Content-Type and would 415 the collector's application/json.
            post(|State(store): State<Arc<shanhaijing::Store>>, body: axum::body::Bytes| async move {
                let raw = match std::str::from_utf8(&body) {
                    Ok(s) => s,
                    Err(e) => return (StatusCode::BAD_REQUEST, format!("bad utf8: {e}")),
                };
                let signals = match parse_signals(raw) {
                    Ok(s) if !s.is_empty() => s,
                    Ok(_) => return (StatusCode::BAD_REQUEST, "empty payload".to_string()),
                    Err(e) => return (StatusCode::BAD_REQUEST, format!("bad payload: {e:#}")),
                };
                let now = now_ms();
                let mut lines = Vec::with_capacity(signals.len());
                for signal in &signals {
                    match report_one(&store, signal, now) {
                        Ok(line) => lines.push(line),
                        Err(e) => {
                            return (StatusCode::INTERNAL_SERVER_ERROR, format!("store: {e:#}"))
                        }
                    }
                }
                (StatusCode::OK, lines.join("\n"))
            }),
        )
        .route(
            "/health",
            get(|| async { "nuwa serve ok" }),
        )
        .with_state(store);

    let listener = tokio::net::TcpListener::bind(&listen)
        .await
        .with_context(|| format!("bind {listen}"))?;
    println!("serve      : listening on http://{listen} (POST /ingest, GET /health)");
    axum::serve(listener, app).await.context("serve")
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
