mod deliver;
mod guard;
mod instructions;
mod issue;
mod repair;

use std::path::PathBuf;

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
                other => bail!("unknown provider {other} (anthropic|faux)"),
            },
            "--model" => {
                let model = value("--model")?;
                cli.provider = repair::ProviderKind::Anthropic { model: Some(model) };
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
        other => bail!(
            "usage: nuwa fix --issue <path> --repo <path> [--provider anthropic|faux] [--model <id>] [--max-turns N] [--push] [--pr] [--no-deliver]\n            nuwa ingest --signal <path> [--db <path>]\n            nuwa work --db <path> --instructions <path> [--provider anthropic|faux] [--push] [--pr]\n        (got {other:?})"
        ),
    }
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

    // A collector file is either one JSON object or JSONL (青鸟 FileSink).
    let signals: Vec<shanhaijing::Signal> = match serde_json::from_str::<shanhaijing::Signal>(&raw) {
        Ok(signal) => vec![signal],
        Err(first_error) => raw
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|line| {
                serde_json::from_str(line).with_context(|| {
                    format!("not a Signal object ({first_error}) and line is not JSONL: {line:.80}")
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?,
    };
    if signals.is_empty() {
        bail!("no signals in {}", signal_path.display());
    }
    for signal in &signals {
        report_one(&store, signal, now)?;
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

fn report_one(
    store: &shanhaijing::Store,
    signal: &shanhaijing::Signal,
    now: i64,
) -> anyhow::Result<()> {
    match store.report_signal(signal, now)? {
        shanhaijing::ReportOutcome::NewIssue { fingerprint } => {
            println!("new issue  : {fingerprint} — ready for pickup");
        }
        shanhaijing::ReportOutcome::Deduped {
            fingerprint,
            occurrences,
        } => {
            println!("dedup      : {fingerprint} (occurrences={occurrences})");
        }
        shanhaijing::ReportOutcome::CooldownActive {
            fingerprint,
            retry_after_ms,
            ..
        } => {
            println!("cooldown   : {fingerprint} — retry in {retry_after_ms}ms");
        }
        shanhaijing::ReportOutcome::Regression {
            fingerprint,
            occurrences,
        } => {
            println!("REGRESSION : {fingerprint} reopened (occurrences={occurrences})");
        }
    }
    Ok(())
}

/// 应龙领单 — claim the highest-priority ready issue from 山海经 and drive it
/// to a verified branch/PR. Failure anywhere after the claim escalates the
/// issue (docs/07 A-1 回炉升级; attempt limits are A-3 runner-pool work).
async fn run_work(args: Vec<String>) -> anyhow::Result<()> {
    let mut db_path = std::path::PathBuf::from("shanhaijing.db");
    let mut instructions_path = None;
    let mut sessions_dir = PathBuf::from("spike/sessions");
    let mut provider = repair::ProviderKind::Anthropic { model: None };
    let mut max_turns = 30u32;
    let mut push = false;
    let mut create_pr = false;
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
                other => bail!("unknown provider {other}"),
            },
            "--model" => provider = repair::ProviderKind::Anthropic { model: Some(value("--model")?) },
            "--max-turns" => max_turns = value("--max-turns")?.parse()?,
            "--push" => push = true,
            "--pr" => {
                push = true;
                create_pr = true;
            }
            other => bail!("unknown work flag {other}"),
        }
    }
    let instructions_path = instructions_path.context("--instructions <path> is required")?;

    let store = shanhaijing::Store::open(&db_path)
        .with_context(|| format!("open db {}", db_path.display()))?;
    let (repo_config, repo_dir) = instructions::RepoInstructions::load(&instructions_path)?;
    let now = now_ms();

    // ---- claim ----
    let Some(snapshot) = store.ready_issue(now)? else {
        println!("nothing ready for pickup");
        return Ok(());
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
        },
    );
    store.set_state(&fp, shanhaijing::State::Triggered, now, Some("claimed by runner"))?;
    store.set_cooldown(&fp, now + shanhaijing::DEFAULT_COOLDOWN_MS)?;
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
        repo: repo_dir,
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
    println!(
        "delivered  : branch {} commit {} pr {}",
        delivered.branch,
        &delivered.commit[..12.min(delivered.commit.len())],
        delivered.pr_url.as_deref().unwrap_or("-"),
    );
    Ok(())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
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
