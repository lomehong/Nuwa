mod deliver;
mod guard;
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

fn parse_args() -> anyhow::Result<Cli> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("fix") => {}
        other => bail!("usage: nuwa fix --issue <path> --repo <path> --sessions <dir> [--provider anthropic|faux] [--model <id>] [--max-turns N] [--push] [--pr] [--no-deliver] (got {other:?})"),
    }
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
    let cli = parse_args()?;
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
