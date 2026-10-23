//! Repair run: 问题单 → harness（rpi-harness crash-resumable loop）→ 修改 + 自验.
//!
//! One issue = one JSONL session under `sessions/<fingerprint>/`, so a crashed
//! run is resumable and every turn is auditable after the fact.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use rpi_agent::events::{AgentEvent, BroadcastEmitter};
use rpi_agent::AgentTool;
use rpi_ai::providers::anthropic::models::anthropic_models;
use rpi_ai::providers::anthropic::AnthropicProvider;
use rpi_ai::providers::faux::{FauxProvider, FauxScript};
use rpi_ai::providers::openai_completions::OpenAiCompletionsProvider;
use rpi_ai::Provider;
use rpi_harness::agent_harness::{AgentHarness, AgentLane, HarnessRunOutcome};
use rpi_harness::session::jsonl::{
    JsonlSessionCreateOptions, JsonlSessionRepo, JsonlSessionRepoOptions,
};
use rpi_harness::session::memory::SystemClock;
use rpi_harness::session::session::DefaultIdGenerator;
use rpi_harness::session::types::SessionStorage;
use rpi_harness::session::Session;
use rpi_harness::types::{AgentHarnessOptions, HarnessTool};
use rpi_tools::{
    create_bash_tool, create_edit_tool, create_find_tool, create_grep_tool, create_ls_tool,
    create_read_tool, create_write_tool, ExecutionToolContext, OsExecutionEnv,
};

use crate::guard::Guards;
use crate::issue::Issue;

#[derive(Debug, Clone)]
pub enum ProviderKind {
    Anthropic { model: Option<String> },
    Faux,
    /// OpenAI-completions-compatible endpoint (智谱 coding plan 等)。
    /// Base URL keeps its `/v4`-style version segment; the provider appends
    /// `/chat/completions` (rpi-ai chat_completions_url rules).
    Zai {
        base_url: Option<String>,
        model: Option<String>,
    },
}

pub struct RepairJob {
    pub repo: PathBuf,
    pub issue: Issue,
    pub sessions_dir: PathBuf,
    pub provider: ProviderKind,
    pub max_turns: u32,
    pub guards: Arc<Guards>,
    /// Credentials read at startup and scrubbed from the process environment.
    pub secrets: Arc<crate::secrets::Secrets>,
    /// Repo-instruction notes appended to the runner system prompt.
    pub notes: String,
    /// Live progress sink, invoked on every completed turn that carries usage.
    pub progress: Option<Arc<dyn Fn(RepairProgress) + Send + Sync>>,
    /// 基准场景提供的确定性脚本；`None` 时使用内置自证脚本。
    pub faux_script: Option<FauxScript>,
    /// The work-loop run id. When present, the session lives in a per-run
    /// subdirectory so two repairs of the same fingerprint never share one —
    /// otherwise the transcript endpoint's "newest JSONL in the dir" resolves an
    /// older run's request to the latest run's file (wrong evidence). `None`
    /// for the one-shot `nuwa fix` path, which keeps the flat layout.
    pub run_id: Option<String>,
}

/// Live progress snapshot handed to `RepairJob::progress` after each turn.
#[derive(Debug, Clone, Copy)]
pub struct RepairProgress {
    pub turns: u32,
    pub uncached_tokens: u64,
    pub cached_tokens: u64,
}

pub struct RepairReport {
    pub run_id: String,
    pub root_cause_text: String,
    pub turns: u32,
    pub uncached_tokens: u64,
    pub cached_tokens: u64,
    /// Absolute per-issue session directory (JSONL transcripts).
    // Read by the work-loop run telemetry (M1b Task 4).
    pub session_dir: String,
    pub context: ContextBudget,
}

/// 上下文预算的可观测部分。逐条消息的淘汰与压缩后的摘要长度 harness 没有
/// 暴露，这里只报告能作为证据的计数（压缩次数、模型窗口、最终消息数）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ContextBudget {
    /// 模型上下文窗口（token）；0 = 未知。
    pub model_context_window: u64,
    /// run 期间的自动压缩次数。
    pub compactions: u32,
    /// run 结束时会话里的消息数。
    pub messages: u64,
}

/// A failure that came from the model provider, not from the issue.
///
/// The runner must tell the two apart: a provider outage is nobody's fault
/// and must not consume an attempt, while a failing check is the issue's
/// fault and must. Classifying by error text alone could not — the target
/// repository's own test output ("connection refused") ends up in the same
/// message and used to be read as a provider outage.
#[derive(Debug)]
pub struct ProviderFailure(pub String);

impl std::fmt::Display for ProviderFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "provider failure: {}", self.0)
    }
}

impl std::error::Error for ProviderFailure {}

/// Give the event drain a bounded window to pick up the events that are
/// still in flight — the final `TurnEnd` usually is — so the last turn's
/// token usage is not lost when the drain is aborted.
async fn settle_drain(turns: &Arc<AtomicU32>) {
    let deadline = Instant::now() + Duration::from_millis(300);
    let mut previous = turns.load(Ordering::Relaxed);
    let mut stable = 0;
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
        let current = turns.load(Ordering::Relaxed);
        if current == previous {
            stable += 1;
            if stable >= 2 {
                return;
            }
        } else {
            stable = 0;
            previous = current;
        }
    }
}

/// Resolve the requested Anthropic model. A typo must fail loudly instead of
/// silently burning tokens on the catalog's first entry.
fn pick_anthropic_model(
    catalog: &[rpi_ai::Model],
    requested: Option<&str>,
) -> anyhow::Result<rpi_ai::Model> {
    match requested {
        None => catalog
            .first()
            .cloned()
            .context("anthropic model catalog is empty"),
        Some(requested) => catalog
            .iter()
            .find(|model| model.id == requested)
            .cloned()
            .with_context(|| {
                format!(
                    "unknown Anthropic model `{requested}`; available: {}",
                    catalog
                        .iter()
                        .map(|model| model.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }),
    }
}

/// 会话目录布局：每个 fingerprint 一个目录；常驻 runner 再按 run id 分桶，
/// 使同指纹的重试不共用一份 JSONL。一次性 CLI 保持平铺。
pub fn session_dir_for(
    sessions_dir: &std::path::Path,
    issue: &Issue,
    run_id: Option<&str>,
) -> std::path::PathBuf {
    let base = sessions_dir.join(issue.short_fingerprint());
    match run_id {
        Some(run_id) => base.join(run_id),
        None => base,
    }
}

/// The offline smoke script: it does NOT prove LLM repair quality — that needs
/// a real provider. It exercises tools, guards, persistence and delivery.
fn default_faux_script() -> FauxScript {
    FauxScript::new()
        .with_tool_call(
            "edit",
            serde_json::json!({
                "path": "calc.go",
                "edits": [{
                    "oldText": "for i := 0; i <= len(xs); i++ {",
                    "newText": "for i := 0; i < len(xs); i++ {"
                }]
            }),
        )
        // The offline proof also leaves a marker so the delivery gate's
        // red→green check is language- and shell-agnostic.
        .with_tool_call(
            "write",
            serde_json::json!({
                "path": "fixed.marker",
                "content": "fixed\n"
            }),
        )
        .with_text(
            "ROOT CAUSE: Average iterated with i <= len(xs), indexing one past the last element — index out of range panic on every non-empty window.\n\
             CHANGE: bounded the loop at i < len(xs) so the sum covers exactly the slice.\n\
             VERIFICATION: go test ./... — all tests pass.",
        )
}

pub async fn run(job: &RepairJob) -> anyhow::Result<RepairReport> {    // Turn budget (rpi-harness RunBudget reads this per run). Token-level caps
    // land with the 运行器池 scheduler; the turn cap bounds runaway loops now.
    std::env::set_var("RPI_MAX_TURNS_PER_RUN", job.max_turns.to_string());

    let progress = job.progress.clone();
    let uncached = Arc::new(AtomicU64::new(0));
    let cached = Arc::new(AtomicU64::new(0));

    let repo = std::path::absolute(&job.repo)
        .with_context(|| format!("resolve repo {}", job.repo.display()))?;
    let repo_str = repo.to_string_lossy().into_owned();

    // ---- env + tool belt ----
    let env = Arc::new(OsExecutionEnv::with_cwd(repo.clone()));
    let env_dyn: Arc<dyn rpi_tools::ExecutionEnv> = env.clone();
    let mut_env: Arc<dyn rpi_tools::MutatingEnv> = env.clone();
    let ctx = ExecutionToolContext::new(env_dyn, Some(mut_env));

    // Child processes get an explicit environment whitelist. The work process
    // removed its credentials at startup; this is the second layer that keeps
    // unrelated host secrets out of build/test commands.
    let allowlist: std::collections::HashMap<String, String> =
        crate::secrets::agent_env_allowlist().into_iter().collect();
    let prepare: rpi_tools::BashPrepare =
        Arc::new(move |exec: &mut rpi_tools::BashExecution| {
            exec.inherit_env = false;
            exec.env = allowlist.clone();
            Box::pin(async { Ok(()) })
        });
    let tools: Vec<Arc<dyn AgentTool>> = vec![
        create_read_tool(&ctx, None),
        create_edit_tool(&ctx),
        create_write_tool(&ctx),
        // 模型忘传 timeout 时（如 go test 等 stdin / 交互 prompt）命令会挂死
        // 整个 run——300s 兜底，模型显式传的超时仍然优先（Q6，2026-10-19
        // mindloop 实战：go test 挂死 50 分钟无事件）。
        create_bash_tool(
            &ctx,
            Some(rpi_tools::BashToolOptions {
                default_timeout: Some(300.0),
                prepare: Some(prepare),
                ..Default::default()
            }),
        ),
        create_grep_tool(&ctx, None),
        create_find_tool(&ctx, None),
        create_ls_tool(&ctx, None),
    ];
    let active_tool_names: Vec<String> = tools.iter().map(|t| t.schema().name.clone()).collect();
    let harness_tools: Vec<HarnessTool> = tools.into_iter().map(HarnessTool::new).collect();

    // ---- durable session (crash-resumable) ----
    // Per-run subdirectory when the work loop supplies a run id, so retries of
    // the same fingerprint do not pile their JSONL into one shared directory
    // (the transcript endpoint resolves "newest JSONL in the run's dir").
    let sessions_dir = session_dir_for(&job.sessions_dir, &job.issue, job.run_id.as_deref());
    let sessions_dir = std::path::absolute(&sessions_dir)
        .with_context(|| format!("resolve {}", sessions_dir.display()))?;
    std::fs::create_dir_all(&sessions_dir)
        .with_context(|| format!("create {}", sessions_dir.display()))?;
    let fs: Arc<dyn rpi_tools::FileSystem> = env.clone();
    let store = JsonlSessionRepo::with_env_cwd(JsonlSessionRepoOptions {
        fs,
        sessions_root: sessions_dir.to_string_lossy().into_owned(),
        clock: Arc::new(SystemClock),
        ids: Arc::new(DefaultIdGenerator::new()),
    });
    let storage = store
        .create_typed(&JsonlSessionCreateOptions {
            id: None,
            parent_session_id: None,
            cwd: repo_str.clone(),
            metadata: None,
        })
        .await
        .context("create session")?;
    let session = Session::new(Arc::new(storage) as Arc<dyn SessionStorage>, None);

    // ---- provider + model ----
    let (model, providers): (rpi_ai::Model, Vec<Arc<dyn Provider>>) = match &job.provider {
        ProviderKind::Faux => {
            // Deterministic offline proof of the pipeline: the scripted "model"
            // performs the real edit via the real edit tool, exercising tools,
            // guards, session persistence and delivery. Benchmark scenarios
            // inject their own script; the built-in one stays the smoke test.
            let script = job.faux_script.clone().unwrap_or_else(default_faux_script);
            let provider = FauxProvider::new(script);
            let model = provider.default_model().clone();
            (model, vec![provider as Arc<dyn Provider>])
        }
        ProviderKind::Zai { base_url, model } => {
            let key = job
                .secrets
                .zai_key
                .clone()
                .context("ZAI_API_KEY not set (智谱 coding plan 的 API key)")?;
            let base = base_url.clone().unwrap_or_else(|| {
                "https://open.bigmodel.cn/api/coding/paas/v4".to_string()
            });
            let model_id = model.clone().unwrap_or_else(|| "glm-5.3-flash".to_string());
            let provider_id = "zai-coding-cn";
            let mut picked = rpi_ai::Model::new(
                model_id.clone(),
                model_id.clone(),
                rpi_ai::Api::OpenaiCompletions,
                provider_id,
                base,
            );
            // Model::new zeroes these; OpenAI-compat omits max_tokens at 0
            // (server default), but compaction math needs a real window.
            picked.context_window = 128_000;
            let http = rpi_ai::http::build_client(&picked.base_url, None, None)
                .unwrap_or_else(|_| reqwest::Client::new());
            let provider = OpenAiCompletionsProvider::with_models_without_env_api_key(
                provider_id,
                Some(key),
                http,
                vec![picked.clone()],
            );
            (picked, vec![Arc::new(provider) as Arc<dyn Provider>])
        }
        ProviderKind::Anthropic { model } => {
            let key = job
                .secrets
                .anthropic_key
                .clone()
                .context("ANTHROPIC_API_KEY not set (or use --provider faux for the offline path)")?;
            let http = rpi_ai::http::build_client("https://api.anthropic.com", None, None)
                .unwrap_or_else(|_| reqwest::Client::new());
            let provider = AnthropicProvider::new(Some(key), http);
            let picked = pick_anthropic_model(&anthropic_models(), model.as_deref())?;
            (picked, vec![Arc::new(provider) as Arc<dyn Provider>])
        }
    };

    // ---- events ----
    let model_context_window = model.context_window;
    let (broadcast, mut rx) = BroadcastEmitter::new(1024);
    let emitter: Arc<dyn rpi_agent::AgentEmitter> = Arc::new(broadcast);
    let turns = Arc::new(AtomicU32::new(0));

    // ---- harness + run ----
    let notes = job.notes.trim();
    let system_prompt = if notes.is_empty() {
        job.issue.system_prompt(&repo_str)
    } else {
        format!(
            "{}\n\nREPO NOTES:\n{}",
            job.issue.system_prompt(&repo_str),
            notes
        )
    };
    let options = AgentHarnessOptions {
        model,
        active_tool_names,
        tools: harness_tools,
        system_prompt: Some(system_prompt),
        session,
        models: providers,
        agent_emitter: Some(emitter),
        before_tool_call: Some(crate::guard::before_tool_hook(Arc::clone(&job.guards))),
        // LLM 请求快速失败：单请求 120s（正常首 chunk 秒级）、至多重试 1 次。
        // 挂死的 provider 流不再拖住整个 run 数十分钟——失败会走验证门/
        // escalate 路径，由下一轮 claim 自然重试（Q6，2026-10-19）。
        stream_options: rpi_harness::types::AgentHarnessStreamOptions {
            timeout: Some(std::time::Duration::from_secs(120)),
            max_retries: Some(1),
            ..Default::default()
        },
        ..AgentHarnessOptions::default()
    };
    let harness = Arc::new(AgentHarness::create(options).await.context("harness create")?);

    // 压缩是上下文预算里唯一有事件的一环：每次自动压缩的 guard 会发
    // active=true/false 两个事件，只数开始那一次。
    let compactions = Arc::new(AtomicU32::new(0));
    let _compaction_subscription = {
        let counter = Arc::clone(&compactions);
        harness
            .events()
            .on::<rpi_harness::events::CompactionProgressEvent, _>(move |event| {
                if event.active && !event.manual {
                    counter.fetch_add(1, Ordering::Relaxed);
                }
            })
    };

    // ---- event drain: console visibility + usage write-back ----
    // Upstream rpi leaves `AgentLane::record_usage` unwired (no production
    // caller), so session stats never see tokens. The drain observes each
    // TurnEnd's assistant usage and feeds it back — this is the nuwa-side
    // wiring that makes the token budget gate real.
    let turn_counter = Arc::clone(&turns);
    let drain_harness = harness.clone();
    let drain = tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(ev) => match ev {
                    AgentEvent::ToolExecutionStart { tool_name, ref args, .. } => {
                        eprintln!("  [tool] {tool_name} {args}");
                    }
                    AgentEvent::ToolExecutionEnd { tool_name, is_error, .. } => {
                        eprintln!("  [tool→] {tool_name} err={is_error}");
                    }
                    AgentEvent::TurnEnd { ref message, .. } => {
                        eprintln!("  -- turn end --");
                        let turns = turn_counter.fetch_add(1, Ordering::Relaxed) + 1;
                        if let rpi_agent::AgentMessage::Assistant(assistant) = message {
                            let usage = assistant.usage.clone();
                            // Output tokens are billable and belong to the
                            // uncached bucket: a repair run spends most of its
                            // tokens writing the patch, and leaving them out
                            // made every budget gate (per issue, per day, per
                            // month) undercount the real spend.
                            let uncached_delta =
                                (usage.input + usage.cache_write + usage.output).max(0) as u64;
                            let cached_delta = usage.cache_read.max(0) as u64;
                            let uncached_total =
                                uncached.fetch_add(uncached_delta, Ordering::Relaxed)
                                    + uncached_delta;
                            let cached_total =
                                cached.fetch_add(cached_delta, Ordering::Relaxed) + cached_delta;
                            let _ = drain_harness.record_usage(usage, None, None).await;
                            if let Some(cb) = &progress {
                                cb(RepairProgress {
                                    turns,
                                    uncached_tokens: uncached_total,
                                    cached_tokens: cached_total,
                                });
                            }
                        }
                    }
                    AgentEvent::RetryScheduled { attempt, max_retries, ref error, .. } => {
                        eprintln!("  [retry {attempt}/{max_retries}] {error}");
                    }
                    _ => {}
                },
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    eprintln!("  [events] lagged by {n}");
                }
                Err(_) => break,
            }
        }
    });

    let result = harness.prompt_text(&job.issue.user_prompt(), vec![]).await;
    let stats = harness.get_stats().await.ok();
    // The drain task holds an Arc of the harness and the harness holds the
    // event emitter, so neither ends on its own: every exit path has to
    // abort it, or each failed run leaks a task (and its harness) for the
    // lifetime of the runner process.
    settle_drain(&turns).await;
    harness.close().await.ok();
    drain.abort();
    let result = match result {
        Ok(result) => result,
        // The prompt call is the provider call: a failure here is the
        // provider's, not the issue's.
        Err(e) => {
            return Err(anyhow::Error::new(ProviderFailure(format!("{e:#}")))
                .context("repair run"));
        }
    };

    let root_cause_text = match result.outcome {
        HarnessRunOutcome::Completed { final_message, .. } => {
            let mut text = String::new();
            for block in &final_message.content {
                if let rpi_ai::types::Content::Text(t) = block {
                    text.push_str(&t.text);
                }
            }
            text
        }
        HarnessRunOutcome::Aborted { .. } => bail!("run aborted (budget or abort)"),
        // The harness only fails on a provider-side error (tool failures are
        // recorded as tool results, they do not fail the run).
        HarnessRunOutcome::Failed { error, .. } => {
            return Err(anyhow::Error::new(ProviderFailure(format!(
                "run failed: {error:?}"
            ))));
        }
        HarnessRunOutcome::Suspended { .. } => {
            return Err(anyhow::Error::new(ProviderFailure(
                "run suspended on a provider deferred handle".to_string(),
            )));
        }
    };

    Ok(RepairReport {
        run_id: result.run_id,
        root_cause_text,
        turns: turns.load(Ordering::Relaxed),
        uncached_tokens: stats.as_ref().map(|s| s.uncached_tokens as u64).unwrap_or(0),
        cached_tokens: stats.as_ref().map(|s| s.cached_tokens as u64).unwrap_or(0),
        session_dir: sessions_dir.to_string_lossy().into_owned(),
        context: ContextBudget {
            model_context_window,
            compactions: compactions.load(Ordering::Relaxed),
            messages: stats.as_ref().map(|s| s.message_count).unwrap_or(0),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn issue() -> Issue {
        Issue {
            fingerprint: "fp".into(),
            title: "panic in Average".into(),
            language: "go".into(),
            occurrences: 1,
            severity: "high".into(),
            first_seen: String::new(),
            last_seen: String::new(),
            stack_trace: "panic: boom".into(),
            log_window: Vec::new(),
            prior_context: String::new(),
            failing_check: crate::issue::FailingCheck {
                command: "echo ok".into(),
                target: None,
                timeout_secs: 600,
            },
            reproduce_check: None,
            forbidden_paths: Vec::new(),
            verification_mode: Default::default(),
        }
    }

    #[test]
    fn unknown_anthropic_model_is_rejected() {
        let catalog = anthropic_models();
        let selected = pick_anthropic_model(&catalog, None).expect("default");
        assert_eq!(selected.id, catalog[0].id);

        let err = pick_anthropic_model(&catalog, Some("definitely-not-a-model"))
            .expect_err("unknown model");
        let text = format!("{err:#}");
        assert!(text.contains("unknown Anthropic model"), "{text}");
        assert!(text.contains("available:"), "{text}");
    }

    // The harness calls `block_in_place` (rpi-harness agent_harness.rs), which
    // panics on the default current-thread test runtime.
    #[tokio::test(flavor = "multi_thread")]
    async fn faux_run_reports_progress_and_session_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).expect("repo");
        let source = repo.join("calc.go");
        std::fs::write(
            &source,
            "package main\n\nfunc Average(xs []float64) float64 {\n\
             \tvar sum float64\n\
             \tfor i := 0; i <= len(xs); i++ {\n\
             \t\tsum += xs[i]\n\
             \t}\n\
             \treturn sum / float64(len(xs))\n\
             }\n",
        )
        .expect("write");

        let seen = Arc::new(Mutex::new(Vec::<RepairProgress>::new()));
        let seen_cb = Arc::clone(&seen);
        let job = RepairJob {
            repo: repo.clone(),
            issue: issue(),
            sessions_dir: dir.path().join("sessions"),
            provider: ProviderKind::Faux,
            max_turns: 5,
            guards: Arc::new(Guards::new(Vec::new())),
            secrets: Arc::new(crate::secrets::Secrets::default()),
            notes: String::new(),
            progress: Some(Arc::new(move |p| {
                seen_cb.lock().expect("lock").push(p);
            })),
            faux_script: None,
            run_id: None,
        };

        let report = run(&job).await.expect("run");
        assert!(!report.session_dir.is_empty());
        assert!(
            report.context.model_context_window > 0,
            "faux model must expose a context window"
        );
        assert_eq!(report.context.compactions, 0, "short run needs no compaction");
        assert!(report.context.messages > 0, "final session must count messages");
        let events = seen.lock().expect("lock");
        assert!(!events.is_empty(), "progress callback must fire at least once");
        assert!(events.last().expect("last").turns >= 1);

        let fixed = std::fs::read_to_string(&source).expect("read");
        assert!(fixed.contains("i < len(xs)"), "faux edit must land");
    }
}
