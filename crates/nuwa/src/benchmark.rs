//! 修复基准：固定种子场景 → 真实修复/验证/交付 → 记分。
//!
//! 两个模式：
//! - `faux`（默认）：场景自带确定性脚本，CI/本地随时可跑，衡量流水线与验证门；
//! - `live`：真实 provider，衡量模型修复质量（需要对应 API key）。
//!
//! 指标刻意不只看“通过率”：`claimed_fixed && !repaired` 计为误报，验收门必须
//! 能拦住“声称修好但检查仍是红的”。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{bail, Context};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};

use crate::issue::{FailingCheck, Issue, VerificationMode};
use crate::repair::ProviderKind;
use crate::{git_ops, pipeline, provider_meta, secrets, verify};
use rpi_ai::providers::faux::FauxScript;
use shanhaijing::{BenchmarkFinish, BenchmarkResultInput, BenchmarkRunRow, BenchmarkStart, Store};

#[derive(RustEmbed)]
#[folder = "assets/benchmarks/"]
struct BenchmarkAssets;

#[derive(Debug, Clone, Deserialize)]
pub struct Scenario {
    pub name: String,
    pub title: String,
    pub description: String,
    pub language: String,
    pub failing_check: FailingCheck,
    /// Windows 需要使用不同启动器时覆盖检查命令（例如 python3 → python）。
    #[serde(default)]
    pub command_windows: Option<String>,
    #[serde(default)]
    pub writes: Vec<ScenarioWrite>,
    /// The configured check already passes on the base commit: the bug is only
    /// reachable at runtime, so the reproducer carries the proof.
    #[serde(default)]
    pub base_green: bool,
    /// Platform-specific reproducer the faux agent writes alongside the fix.
    #[serde(default)]
    pub reproducer: Option<ScenarioReproducer>,
    #[serde(default, rename = "final")]
    pub final_message: String,
    #[serde(default)]
    pub expect_fixed: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ScenarioReproducer {
    pub unix_path: String,
    pub unix_content: String,
    pub windows_path: String,
    pub windows_content: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ScenarioWrite {
    pub path: String,
    pub content: String,
}

/// 控制台场景目录的只读元数据（不暴露脚本正文）。
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct ScenarioMeta {
    pub name: String,
    pub title: String,
    pub description: String,
    pub language: String,
    pub failing_check: String,
    pub expect_fixed: bool,
}

pub fn catalog() -> anyhow::Result<Vec<ScenarioMeta>> {
    Ok(load_scenarios()?
        .into_iter()
        .map(|scenario| ScenarioMeta {
            name: scenario.name,
            title: scenario.title,
            description: scenario.description,
            language: scenario.language,
            failing_check: scenario.failing_check.command,
            expect_fixed: scenario.expect_fixed,
        })
        .collect())
}

fn load_scenarios() -> anyhow::Result<Vec<Scenario>> {
    let mut names: Vec<String> = BenchmarkAssets::iter()
        .filter(|name| name.ends_with("/scenario.json"))
        .map(|name| name.to_string())
        .collect();
    names.sort();
    if names.is_empty() {
        bail!("no benchmark scenarios embedded");
    }
    let mut scenarios = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for name in names {
        let file = BenchmarkAssets::get(&name)
            .with_context(|| format!("embedded scenario {name} disappeared"))?;
        let scenario: Scenario = serde_json::from_slice(file.data.as_ref())
            .with_context(|| format!("parse benchmark scenario {name}"))?;
        if scenario.name.trim().is_empty() {
            bail!("{name}: scenario name must not be empty");
        }
        if !seen.insert(scenario.name.clone()) {
            bail!("duplicate benchmark scenario `{}`", scenario.name);
        }
        scenarios.push(scenario);
    }
    scenarios.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(scenarios)
}

struct RunOptions {
    db_path: PathBuf,
    provider: ProviderKind,
    max_turns: u32,
    scenarios: Vec<String>,
    run_id: Option<String>,
    json: bool,
}

pub async fn run(args: Vec<String>) -> anyhow::Result<()> {
    let mut args = args;
    if matches!(
        args.first().map(String::as_str),
        Some("list") | Some("scenarios")
    ) {
        let catalog = catalog()?;
        println!(
            "{:<22} {:<8} {:<8} title",
            "scenario", "language", "expected"
        );
        for scenario in catalog {
            println!(
                "{:<22} {:<8} {:<8} {}",
                scenario.name,
                scenario.language,
                if scenario.expect_fixed {
                    "fixed"
                } else {
                    "not-fixed"
                },
                scenario.title
            );
        }
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("run") {
        args.remove(0);
    }
    let options = parse_options(args)?;
    let mut scenarios = load_scenarios()?;
    if !options.scenarios.is_empty() {
        scenarios.retain(|scenario| options.scenarios.iter().any(|name| name == &scenario.name));
        if scenarios.is_empty() {
            bail!("no scenario matched {:?}", options.scenarios);
        }
    }
    let provider_name = provider_meta(&options.provider);
    let mode = if matches!(options.provider, ProviderKind::Faux) {
        "faux"
    } else {
        "live"
    };
    let secrets = Arc::new(secrets::Secrets::load());
    let store = Store::open(&options.db_path)
        .with_context(|| format!("open db {}", options.db_path.display()))?;
    let run_id = options
        .run_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let started = crate::now_ms();
    store
        .start_benchmark_run(&BenchmarkStart {
            id: run_id.clone(),
            started_at: started,
            provider: provider_name.0.clone(),
            model: Some(provider_name.1.clone()),
            mode: mode.to_string(),
            scenarios: scenarios.len(),
        })
        .context("record benchmark start")?;

    let run_started = Instant::now();
    let mut results = Vec::new();
    for scenario in &scenarios {
        let result = run_one(
            &run_id,
            scenario,
            &options.provider,
            options.max_turns,
            &secrets,
        )
        .await;
        if let Err(error) = store.insert_benchmark_result(&result) {
            let _ = store.finish_benchmark_run(
                &run_id,
                &BenchmarkFinish {
                    finished_at: crate::now_ms(),
                    status: "failed".to_string(),
                    repaired: 0,
                    claimed: 0,
                    false_positive: 0,
                    delivery_ok: 0,
                    turns: 0,
                    tokens_uncached: 0,
                    tokens_cached: 0,
                    duration_ms: run_started.elapsed().as_millis() as i64,
                    error: Some(format!("store write failed: {error}")),
                },
            );
            return Err(error.into());
        }
        println!(
            "{:<22} repaired={:<5} claimed={:<5} delivered={:<5} turns={:<3} tokens={:<7} {}ms{}",
            result.scenario,
            result.repaired,
            result.claimed_fixed,
            result.delivery_ok,
            result.turns,
            result.tokens_uncached + result.tokens_cached,
            result.duration_ms,
            result
                .error
                .as_deref()
                .map(|error| format!("  error: {error}"))
                .unwrap_or_default(),
        );
        results.push(result);
    }

    let repaired = results.iter().filter(|result| result.repaired).count();
    let claimed = results.iter().filter(|result| result.claimed_fixed).count();
    let false_positive = results
        .iter()
        .filter(|result| result.claimed_fixed && !result.repaired)
        .count();
    let delivery_ok = results.iter().filter(|result| result.delivery_ok).count();
    let errored = results
        .iter()
        .filter(|result| result.error.is_some())
        .count();
    let status = if errored == results.len() {
        "failed"
    } else {
        "completed"
    };
    let aggregate_error =
        (errored > 0).then(|| format!("{errored}/{} scenarios errored", results.len()));
    store
        .finish_benchmark_run(
            &run_id,
            &BenchmarkFinish {
                finished_at: crate::now_ms(),
                status: status.to_string(),
                repaired: repaired as i64,
                claimed: claimed as i64,
                false_positive: false_positive as i64,
                delivery_ok: delivery_ok as i64,
                turns: results.iter().map(|result| result.turns).sum(),
                tokens_uncached: results.iter().map(|result| result.tokens_uncached).sum(),
                tokens_cached: results.iter().map(|result| result.tokens_cached).sum(),
                duration_ms: run_started.elapsed().as_millis() as i64,
                error: aggregate_error.clone(),
            },
        )
        .context("record benchmark finish")?;

    let run = store.get_benchmark_run(&run_id)?.map(|row| run_json(&row));
    if options.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "run": run,
                "results": results,
            }))?
        );
    } else {
        println!(
            "\nbenchmark  : {run_id}\nprovider   : {} ({mode})\nscenarios  : {}\nrepaired   : {repaired}\nclaimed    : {claimed}\nfalse-pos  : {false_positive}\ndelivered  : {delivery_ok}\nstatus     : {status}{}",
            provider_name.1,
            results.len(),
            aggregate_error
                .map(|error| format!("\nerror      : {error}"))
                .unwrap_or_default(),
        );
    }
    if status == "failed" {
        bail!("benchmark failed: every scenario errored");
    }
    Ok(())
}

fn run_json(run: &BenchmarkRunRow) -> serde_json::Value {
    serde_json::json!({
        "id": run.id,
        "started_at_ms": run.started_at,
        "finished_at_ms": run.finished_at,
        "provider": run.provider,
        "model": run.model,
        "mode": run.mode,
        "status": run.status,
        "scenarios": run.scenarios,
        "repaired": run.repaired,
        "claimed": run.claimed,
        "false_positive": run.false_positive,
        "delivery_ok": run.delivery_ok,
        "turns": run.turns,
        "tokens_uncached": run.tokens_uncached,
        "tokens_cached": run.tokens_cached,
        "duration_ms": run.duration_ms,
        "error": run.error,
    })
}

fn parse_options(args: Vec<String>) -> anyhow::Result<RunOptions> {
    let mut db_path = PathBuf::from("shanhaijing.db");
    let mut provider_name: Option<String> = None;
    let mut model: Option<String> = None;
    let mut max_turns = 12u32;
    let mut scenarios = Vec::new();
    let mut run_id = None;
    let mut json = false;
    let mut it = args.into_iter();
    while let Some(flag) = it.next() {
        let mut value = |name: &str| it.next().with_context(|| format!("{name} needs a value"));
        match flag.as_str() {
            "--db" => db_path = PathBuf::from(value("--db")?),
            "--provider" => provider_name = Some(value("--provider")?),
            "--model" => model = Some(value("--model")?),
            "--max-turns" => {
                max_turns = value("--max-turns")?
                    .parse()
                    .context("--max-turns must be a positive integer")?;
                if max_turns == 0 {
                    bail!("--max-turns must be >= 1");
                }
            }
            "--scenario" => {
                for name in value("--scenario")?.split(',') {
                    let name = name.trim();
                    if !name.is_empty() {
                        scenarios.push(name.to_string());
                    }
                }
            }
            "--run-id" => run_id = Some(value("--run-id")?),
            "--json" => json = true,
            other => bail!("unknown benchmark flag {other}"),
        }
    }
    let provider = match provider_name.as_deref() {
        // A benchmark must be reproducible by default: never silently inherit
        // the runner's live provider (and its spend) from the environment.
        None | Some("faux") => {
            if model.is_some() {
                bail!("--model does not apply to the faux provider");
            }
            ProviderKind::Faux
        }
        Some("zai") => ProviderKind::Zai {
            base_url: crate::env_store::var("NUWA_BASE_URL").filter(|value| !value.is_empty()),
            model,
        },
        Some("anthropic") => ProviderKind::Anthropic { model },
        Some(other) => bail!("unknown provider {other} (faux|zai|anthropic)"),
    };
    Ok(RunOptions {
        db_path,
        provider,
        max_turns,
        scenarios,
        run_id,
        json,
    })
}

async fn run_one(
    run_id: &str,
    scenario: &Scenario,
    provider: &ProviderKind,
    max_turns: u32,
    secrets: &secrets::Secrets,
) -> BenchmarkResultInput {
    let started = Instant::now();
    let mut result = BenchmarkResultInput {
        run_id: run_id.to_string(),
        scenario: scenario.name.clone(),
        language: scenario.language.clone(),
        expected_fixed: scenario.expect_fixed,
        repaired: false,
        claimed_fixed: false,
        delivery_ok: false,
        turns: 0,
        tokens_uncached: 0,
        tokens_cached: 0,
        duration_ms: 0,
        root_cause: None,
        final_report: None,
        error: None,
    };
    match run_one_inner(run_id, scenario, provider, max_turns, secrets, &mut result).await {
        Ok(()) => {}
        Err(error) => {
            result.error = Some(bounded(&secrets.redact(&format!("{error:#}")), 4_000));
        }
    }
    result.duration_ms = started.elapsed().as_millis() as i64;
    result
}

async fn run_one_inner(
    run_id: &str,
    scenario: &Scenario,
    provider: &ProviderKind,
    max_turns: u32,
    secrets: &secrets::Secrets,
    result: &mut BenchmarkResultInput,
) -> anyhow::Result<()> {
    let dir = tempfile::tempdir().context("create scenario dir")?;
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo)?;
    materialize(scenario, &repo)?;
    init_repo(&repo)?;
    let sessions = dir.path().join("sessions");

    let check = scenario_check(scenario);
    let base = verify::probe_check(&repo, "main", &check, secrets)
        .await
        .context("run failing check on base")?;
    if scenario.base_green {
        if !base.ok {
            bail!("scenario invalid: base_green requires failing_check to pass on the base commit");
        }
    } else if base.ok {
        bail!("scenario invalid: failing_check is green on the base commit");
    }

    let issue = Issue {
        fingerprint: format!("benchmark/{}", scenario.name),
        title: scenario.title.clone(),
        language: scenario.language.clone(),
        occurrences: 1,
        severity: "medium".to_string(),
        first_seen: String::new(),
        last_seen: String::new(),
        stack_trace: scenario.description.clone(),
        log_window: Vec::new(),
        prior_context: String::new(),
        failing_check: check.clone(),
        reproduce_check: None,
        forbidden_paths: Vec::new(),
        verification_mode: if scenario.base_green {
            VerificationMode::Reproducer
        } else {
            VerificationMode::default()
        },
    };
    let faux_script = matches!(provider, ProviderKind::Faux).then(|| scenario_script(scenario));
    let prepared = pipeline::prepare_repair(pipeline::RepairRequest {
        repo: repo.clone(),
        workspace: None,
        issue: issue.clone(),
        sessions_dir: sessions.clone(),
        provider: provider.clone(),
        max_turns,
        repo_config: None,
        secrets: Arc::new(secrets.clone()),
        progress: None,
        faux_script,
        run_id: Some(format!("benchmark-{run_id}-{}", scenario.name)),
    })?;
    let report = prepared.execute().await.context("repair run")?;
    result.turns = i64::from(report.turns);
    result.tokens_uncached = report.uncached_tokens as i64;
    result.tokens_cached = report.cached_tokens as i64;
    result.root_cause = Some(bounded(&secrets.redact(&report.root_cause_text), 8_000));
    result.final_report = Some(bounded(&secrets.redact(&report.root_cause_text), 8_000));
    result.claimed_fixed = claimed_fixed(&report.root_cause_text);

    // A base-green scenario is already green on the configured check; only the
    // reproducer can prove the runtime bug was fixed. Use the same helper the
    // delivery gate uses so scoring and gating cannot drift.
    let post = if scenario.base_green {
        verify::check_reproducer_working_tree(&repo, secrets).await
    } else {
        verify::check_working_tree(&repo, &check, secrets).await
    };
    match post {
        Ok(output) => result.repaired = output.ok,
        Err(error) => {
            result.error = Some(bounded(
                &secrets.redact(&format!("post-repair check failed to run: {error:#}")),
                4_000,
            ));
        }
    }

    // Always attempt delivery: a false claim should be blocked by the gate, and
    // that block is itself part of what the benchmark measures.
    let delivery = pipeline::deliver(pipeline::DeliveryRequest {
        repo: &repo,
        issue: &issue,
        report: &report,
        sessions_dir: &sessions,
        push: false,
        create_pr: false,
        public_base_url: None,
        draft: false,
        secrets,
        store: None,
        run_id: None,
        repo_id: "",
    })
    .await;
    match delivery {
        Ok(_) => result.delivery_ok = true,
        Err(error) => {
            if result.repaired {
                result.error = Some(bounded(
                    &secrets.redact(&format!("delivery gate failed: {error:#}")),
                    4_000,
                ));
            }
        }
    }
    Ok(())
}

/// A claim is not verification evidence. Only one exact, explicit outcome
/// counts; duplicate (even identical), unknown and missing outcomes fail closed.
fn claimed_fixed(final_message: &str) -> bool {
    let mut outcomes = final_message
        .lines()
        .filter_map(|line| line.trim().strip_prefix("OUTCOME:").map(str::trim));
    outcomes.next() == Some("fixed") && outcomes.next().is_none()
}

fn scenario_check(scenario: &Scenario) -> FailingCheck {
    let mut check = scenario.failing_check.clone();
    if cfg!(windows) {
        if let Some(command) = &scenario.command_windows {
            check.command = command.clone();
        }
    }
    check
}

fn scenario_script(scenario: &Scenario) -> FauxScript {
    let mut script = FauxScript::new();
    for write in &scenario.writes {
        script = script.with_tool_call(
            "write",
            serde_json::json!({"path": write.path, "content": write.content}),
        );
    }
    if let Some(reproducer) = &scenario.reproducer {
        let (path, content) = if cfg!(windows) {
            (&reproducer.windows_path, &reproducer.windows_content)
        } else {
            (&reproducer.unix_path, &reproducer.unix_content)
        };
        script = script.with_tool_call(
            "write",
            serde_json::json!({"path": path, "content": content}),
        );
    }
    if !scenario.final_message.trim().is_empty() {
        script = script.with_text(scenario.final_message.clone());
    }
    script
}

fn materialize(scenario: &Scenario, dest: &Path) -> anyhow::Result<()> {
    let prefix = format!("{}/repo/", scenario.name);
    let mut files = 0usize;
    for name in BenchmarkAssets::iter() {
        let Some(relative) = name.strip_prefix(&prefix) else {
            continue;
        };
        if relative
            .split('/')
            .any(|part| part == ".." || part.is_empty())
        {
            bail!(
                "scenario {} has an unsafe file path `{relative}`",
                scenario.name
            );
        }
        let file = BenchmarkAssets::get(&name)
            .with_context(|| format!("embedded file {name} disappeared"))?;
        let path = dest.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, file.data.as_ref())
            .with_context(|| format!("write scenario file {}", path.display()))?;
        files += 1;
    }
    if files == 0 {
        bail!("scenario {} embeds no repo files", scenario.name);
    }
    Ok(())
}

fn init_repo(repo: &Path) -> anyhow::Result<()> {
    git_ops::git(repo, &["init", "-b", "main"])?;
    git_ops::git(repo, &["add", "-A"])?;
    git_ops::git(
        repo,
        &[
            "-c",
            "user.name=nuwa-benchmark",
            "-c",
            "user.email=benchmark@local",
            "commit",
            "-m",
            "benchmark seed",
        ],
    )?;
    Ok(())
}

fn bounded(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    text.chars().take(max_chars).collect::<String>() + "…"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_one_explicit_fixed_outcome_counts_as_a_claim() {
        for (text, expected) in [
            ("OUTCOME: fixed", true),
            ("VERIFICATION: failed\nOUTCOME: not_fixed", false),
            ("VERIFICATION: not run\nOUTCOME: unverified", false),
            ("VERIFICATION: pass", false),
            ("fixed all bugs", false),
            ("OUTCOME: fixed\nOUTCOME: not_fixed", false),
            ("OUTCOME: unverified\nOUTCOME: fixed", false),
            ("OUTCOME: fixed\nOUTCOME: fixed", false),
            ("OUTCOME: fixed\nOUTCOME: unknown", false),
            ("VERIFICATION: pass\nOUTCOME: unknown", false),
            ("OUTCOME: fixed maybe", false),
            ("The sample says OUTCOME: fixed", false),
            ("VERIFICATION: failed\nOUTCOME: fixed", true),
            (
                "ROOT CAUSE: x\r\nCHANGE: y\r\nVERIFICATION: pass\r\nOUTCOME: fixed\r\n",
                true,
            ),
        ] {
            assert_eq!(claimed_fixed(text), expected, "{text:?}");
        }
    }

    #[test]
    fn builtin_scenarios_parse_and_have_fixtures() {
        let scenarios = load_scenarios().expect("scenarios");
        assert!(scenarios.len() >= 10, "expected a meaningful seed set");
        for language in ["go", "node", "python", "rust"] {
            assert!(
                scenarios
                    .iter()
                    .any(|scenario| scenario.language == language),
                "missing a {language} scenario"
            );
        }
        assert!(scenarios.iter().any(|scenario| scenario.expect_fixed));
        assert!(
            scenarios.iter().any(|scenario| !scenario.expect_fixed),
            "negative control is required to measure false positives"
        );
        assert!(
            scenarios.iter().any(|scenario| scenario.base_green),
            "a runtime-only scenario is required to exercise the reproducer path"
        );
        for scenario in scenarios.iter().filter(|scenario| scenario.base_green) {
            assert!(
                scenario.reproducer.is_some(),
                "{} needs a platform reproducer",
                scenario.name
            );
        }
        for scenario in &scenarios {
            assert!(
                claimed_fixed(&scenario.final_message),
                "{} must explicitly claim fixed, including the negative control",
                scenario.name
            );
            assert!(
                BenchmarkAssets::iter()
                    .any(|name| name.starts_with(&format!("{}/repo/", scenario.name))),
                "{} needs repo fixtures",
                scenario.name
            );
            if matches!(
                scenario.name.as_str(),
                "node-empty-average" | "node-noop-claim"
            ) {
                assert!(
                    scenario.failing_check.command.contains("node"),
                    "node scenarios must run node"
                );
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn faux_scenario_repairs_and_delivers() {
        let scenarios = load_scenarios().expect("scenarios");
        let scenario = scenarios
            .iter()
            .find(|scenario| scenario.name == "node-empty-average")
            .expect("node scenario");
        let secrets = secrets::Secrets::default();
        let mut result = BenchmarkResultInput {
            run_id: "test".into(),
            scenario: scenario.name.clone(),
            language: scenario.language.clone(),
            expected_fixed: scenario.expect_fixed,
            repaired: false,
            claimed_fixed: false,
            delivery_ok: false,
            turns: 0,
            tokens_uncached: 0,
            tokens_cached: 0,
            duration_ms: 0,
            root_cause: None,
            final_report: None,
            error: None,
        };
        run_one_inner(
            "test",
            scenario,
            &ProviderKind::Faux,
            12,
            &secrets,
            &mut result,
        )
        .await
        .expect("scenario run");
        assert!(result.repaired, "faux script must fix the node scenario");
        assert!(result.claimed_fixed);
        assert!(
            result.delivery_ok,
            "green branch must pass the delivery gate"
        );
        assert!(result.error.is_none(), "{:?}", result.error);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn faux_runtime_only_scenario_uses_reproducer() {
        let scenario = load_scenarios()
            .expect("scenarios")
            .into_iter()
            .find(|scenario| scenario.name == "go-main-panic")
            .expect("runtime-only scenario");
        let secrets = secrets::Secrets::default();
        let mut result = BenchmarkResultInput {
            run_id: "test-runtime-only".into(),
            scenario: scenario.name.clone(),
            language: scenario.language.clone(),
            expected_fixed: scenario.expect_fixed,
            repaired: false,
            claimed_fixed: false,
            delivery_ok: false,
            turns: 0,
            tokens_uncached: 0,
            tokens_cached: 0,
            duration_ms: 0,
            root_cause: None,
            final_report: None,
            error: None,
        };
        run_one_inner(
            "test",
            &scenario,
            &ProviderKind::Faux,
            12,
            &secrets,
            &mut result,
        )
        .await
        .expect("scenario run");
        assert!(
            result.repaired,
            "reproducer must be green after the fix: {:?}",
            result.error
        );
        assert!(result.claimed_fixed);
        assert!(
            result.delivery_ok,
            "red→green reproducer must pass the gate: {:?}",
            result.error
        );
        assert!(result.error.is_none(), "{:?}", result.error);
    }

    async fn run_named_scenario(name: &str) -> BenchmarkResultInput {
        let scenario = load_scenarios()
            .expect("scenarios")
            .into_iter()
            .find(|scenario| scenario.name == name)
            .unwrap_or_else(|| panic!("missing scenario {name}"));
        let mut result = BenchmarkResultInput {
            run_id: format!("test-{name}"),
            scenario: scenario.name.clone(),
            language: scenario.language.clone(),
            expected_fixed: scenario.expect_fixed,
            repaired: false,
            claimed_fixed: false,
            delivery_ok: false,
            turns: 0,
            tokens_uncached: 0,
            tokens_cached: 0,
            duration_ms: 0,
            root_cause: None,
            final_report: None,
            error: None,
        };
        run_one_inner(
            "test",
            &scenario,
            &ProviderKind::Faux,
            12,
            &secrets::Secrets::default(),
            &mut result,
        )
        .await
        .expect("scenario run");
        result
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn faux_build_failure_repairs() {
        let result = run_named_scenario("go-build-failure").await;
        assert!(result.repaired, "{:?}", result.error);
        assert!(result.claimed_fixed);
        assert!(result.delivery_ok, "{:?}", result.error);
        assert!(result.error.is_none(), "{:?}", result.error);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn faux_multi_file_repairs() {
        let result = run_named_scenario("go-multi-file").await;
        assert!(result.repaired, "{:?}", result.error);
        assert!(result.claimed_fixed);
        assert!(result.delivery_ok, "{:?}", result.error);
        assert!(result.error.is_none(), "{:?}", result.error);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn faux_rust_runtime_only_uses_reproducer() {
        let result = run_named_scenario("rust-main-panic").await;
        assert!(result.repaired, "{:?}", result.error);
        assert!(result.claimed_fixed);
        assert!(result.delivery_ok, "{:?}", result.error);
        assert!(result.error.is_none(), "{:?}", result.error);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn honest_failure_is_not_a_fixed_claim() {
        let mut scenario = load_scenarios()
            .expect("scenarios")
            .into_iter()
            .find(|scenario| scenario.name == "node-noop-claim")
            .expect("negative scenario");
        scenario.final_message = "ROOT CAUSE: wrong operator\nCHANGE: none\nVERIFICATION: node --test — failed\nOUTCOME: not_fixed".into();
        let result = run_one(
            "honest",
            &scenario,
            &ProviderKind::Faux,
            12,
            &secrets::Secrets::default(),
        )
        .await;
        assert!(result.error.is_none(), "{:?}", result.error);
        assert!(
            !result.claimed_fixed,
            "failed verification is not a success claim"
        );
        assert!(!result.repaired);
        assert!(!result.delivery_ok);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn negative_control_is_blocked_by_the_gate() {
        let scenarios = load_scenarios().expect("scenarios");
        let scenario = scenarios
            .iter()
            .find(|scenario| scenario.name == "node-noop-claim")
            .expect("negative scenario");
        let secrets = secrets::Secrets::default();
        let mut result = BenchmarkResultInput {
            run_id: "test".into(),
            scenario: scenario.name.clone(),
            language: scenario.language.clone(),
            expected_fixed: scenario.expect_fixed,
            repaired: false,
            claimed_fixed: false,
            delivery_ok: false,
            turns: 0,
            tokens_uncached: 0,
            tokens_cached: 0,
            duration_ms: 0,
            root_cause: None,
            final_report: None,
            error: None,
        };
        run_one_inner(
            "test",
            scenario,
            &ProviderKind::Faux,
            12,
            &secrets,
            &mut result,
        )
        .await
        .expect("scenario run");
        assert!(!result.repaired, "no-op must not count as repaired");
        assert!(
            result.claimed_fixed,
            "scenario must exercise the claim path"
        );
        assert!(
            !result.delivery_ok,
            "delivery gate must block the false claim"
        );
    }
}
