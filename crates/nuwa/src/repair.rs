//! Repair run: 问题单 → harness（rpi-harness crash-resumable loop）→ 修改 + 自验.
//!
//! One issue = one JSONL session under `sessions/<fingerprint>/`, so a crashed
//! run is resumable and every turn is auditable after the fact.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use anyhow::{bail, Context};
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
    Anthropic {
        model: Option<String>,
    },
    Faux,
    /// OpenAI-completions-compatible endpoint (智谱 coding plan 等)。
    /// Base URL keeps its `/v4`-style version segment; the provider appends
    /// `/chat/completions` (rpi-ai chat_completions_url rules).
    Zai {
        base_url: Option<String>,
        model: Option<String>,
    },
}

const ZAI_DEFAULT_BASE_URL: &str = "https://open.bigmodel.cn/api/coding/paas/v4";
const ZAI_DEFAULT_MODEL: &str = "glm-5.3-flash";

fn zai_config(base_url: &Option<String>, model: &Option<String>) -> (String, String) {
    (
        base_url
            .clone()
            .unwrap_or_else(|| ZAI_DEFAULT_BASE_URL.into()),
        model.clone().unwrap_or_else(|| ZAI_DEFAULT_MODEL.into()),
    )
}

/// Actual provider/model/endpoint, without credentials or a network provider.
/// Keep the legacy display labels (`provider_meta`) separate from this evidence.
pub fn actual_provider_config(
    provider: &ProviderKind,
) -> anyhow::Result<(String, Option<String>, Option<String>)> {
    match provider {
        ProviderKind::Faux => {
            let provider = FauxProvider::new(FauxScript::new());
            let model = provider.default_model();
            Ok((
                "faux".into(),
                Some(model.id.clone()),
                Some(model.base_url.clone()),
            ))
        }
        ProviderKind::Anthropic { model } => {
            let picked = pick_anthropic_model(&anthropic_models(), model.as_deref())?;
            Ok(("anthropic".into(), Some(picked.id), Some(picked.base_url)))
        }
        ProviderKind::Zai { base_url, model } => {
            let (base, model) = zai_config(base_url, model);
            Ok(("zai".into(), Some(model), Some(base)))
        }
    }
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

// prompt_text joins its emitters; draining to Empty afterwards captures the final usage without a detached task.
async fn collect_run_events<P, R, F, U>(
    prompt: P,
    mut rx: tokio::sync::broadcast::Receiver<AgentEvent>,
    mut record_usage: F,
    progress: Option<&(dyn Fn(RepairProgress) + Send + Sync)>,
) -> (R, anyhow::Result<RepairProgress>)
where
    P: std::future::Future<Output = R>,
    F: FnMut(rpi_ai::types::Usage) -> U,
    U: std::future::Future<Output = anyhow::Result<()>>,
{
    use tokio::sync::broadcast::error::{RecvError, TryRecvError};
    let (done, mut completed) = tokio::sync::oneshot::channel();
    let drain = async {
        let mut finished = false;
        let mut error = None;
        let mut totals = RepairProgress {
            turns: 0,
            uncached_tokens: 0,
            cached_tokens: 0,
        };
        loop {
            let event = if finished {
                match rx.try_recv() {
                    Ok(event) => Ok(event),
                    Err(TryRecvError::Empty | TryRecvError::Closed) => break,
                    Err(TryRecvError::Lagged(n)) => Err(RecvError::Lagged(n)),
                }
            } else {
                tokio::select! {
                    biased;
                    _ = &mut completed => {
                        finished = true;
                        continue;
                    }
                    event = rx.recv() => event,
                }
            };
            match event {
                Ok(AgentEvent::ToolExecutionStart {
                    tool_name, args, ..
                }) => {
                    eprintln!("  [tool] {tool_name} {args}");
                }
                Ok(AgentEvent::ToolExecutionEnd {
                    tool_name,
                    is_error,
                    ..
                }) => {
                    eprintln!("  [tool→] {tool_name} err={is_error}");
                }
                Ok(AgentEvent::TurnEnd { message, .. }) => {
                    eprintln!("  -- turn end --");
                    totals.turns += 1;
                    if let rpi_agent::AgentMessage::Assistant(assistant) = message {
                        let usage = assistant.usage;
                        // Output is billable too; harness SessionStats' uncached
                        // bucket only includes input + cache_write.
                        totals.uncached_tokens +=
                            (usage.input + usage.cache_write + usage.output).max(0) as u64;
                        totals.cached_tokens += usage.cache_read.max(0) as u64;
                        if let Err(failure) = record_usage(usage).await {
                            error.get_or_insert(failure.context("record usage"));
                        }
                        // Publish spend even if persistence failed, and continue
                        // draining so a failed run cannot silently discard its tail.
                        if let Some(callback) = progress {
                            callback(totals);
                        }
                    }
                }
                Ok(AgentEvent::RetryScheduled {
                    attempt,
                    max_retries,
                    error,
                    ..
                }) => {
                    eprintln!("  [retry {attempt}/{max_retries}] {error}");
                }
                Ok(_) => {}
                Err(RecvError::Lagged(n)) => {
                    error.get_or_insert_with(|| {
                        anyhow::anyhow!("repair events lagged by {n}; usage may be incomplete")
                    });
                }
                Err(RecvError::Closed) => break,
            }
        }
        match error {
            Some(error) => Err(error.context(format!(
                "repair usage accounting failed (observed uncached={}, cached={})",
                totals.uncached_tokens, totals.cached_tokens
            ))),
            None => Ok(totals),
        }
    };
    tokio::join!(
        async {
            let result = prompt.await;
            let _ = done.send(());
            result
        },
        drain
    )
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
             VERIFICATION: go test ./... — all tests pass.\n\
             OUTCOME: fixed",
        )
}

pub async fn run(job: &RepairJob) -> anyhow::Result<RepairReport> {
    // The turn cap is a per-run harness option, not the process-global
    // RPI_MAX_TURNS_PER_RUN: concurrent repairs in one process must not
    // overwrite each other's limit (A-3 pre-refactor).

    let mut guards = job
        .guards
        .rooted_for(&job.repo)
        .context("root repair guards")?;
    let sessions_root = std::path::absolute(&job.sessions_dir).context("resolve session root")?;
    guards.deny_private_path(&sessions_root)?;
    let guards = Arc::new(guards);
    let policy = guards.policy()?;
    let repo = policy.root().to_path_buf();
    let repo_str = repo.to_string_lossy().into_owned();

    // ---- env + tool belt ----
    let runner_env = Arc::new(OsExecutionEnv::with_cwd(repo.clone()));
    let env = Arc::new(crate::guard::local_env::LocalExecutionEnv::new(
        Arc::clone(&runner_env),
        policy,
    ));
    let env_dyn: Arc<dyn rpi_tools::ExecutionEnv> = env.clone();
    let mut_env: Arc<dyn rpi_tools::MutatingEnv> = env.clone();
    let ctx = ExecutionToolContext::new(env_dyn, Some(mut_env));

    // Child processes get an explicit environment whitelist. The work process
    // removed its credentials at startup; this is the second layer that keeps
    // unrelated host secrets out of build/test commands.
    let allowlist: std::collections::HashMap<String, String> =
        crate::secrets::agent_env_allowlist().into_iter().collect();
    let prepare: rpi_tools::BashPrepare = Arc::new(move |exec: &mut rpi_tools::BashExecution| {
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
    // The runner owns transcripts, not the agent's rooted local file context.
    let fs: Arc<dyn rpi_tools::FileSystem> = runner_env;
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
            let (base, model_id) = zai_config(base_url, model);
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
            let key = job.secrets.anthropic_key.clone().context(
                "ANTHROPIC_API_KEY not set (or use --provider faux for the offline path)",
            )?;
            let picked = pick_anthropic_model(&anthropic_models(), model.as_deref())?;
            let http = rpi_ai::http::build_client(&picked.base_url, None, None)
                .unwrap_or_else(|_| reqwest::Client::new());
            let provider = AnthropicProvider::new(Some(key), http);
            (picked, vec![Arc::new(provider) as Arc<dyn Provider>])
        }
    };

    // ---- events ----
    let model_context_window = model.context_window;
    let (broadcast, rx) = BroadcastEmitter::new(1024);
    let emitter: Arc<dyn rpi_agent::AgentEmitter> = Arc::new(broadcast);

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
        before_tool_call: Some(crate::guard::before_tool_hook(guards)),
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
    let harness = AgentHarness::create(options)
        .await
        .context("harness create")?;
    // Per-run cap: several harnesses may run in one process, so the limit
    // must not go through the process-global RPI_MAX_TURNS_PER_RUN.
    harness.set_max_turns(job.max_turns);
    let harness = Arc::new(harness);

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
    let (result, usage) = collect_run_events(
        harness.prompt_text(&job.issue.user_prompt(), vec![]),
        rx,
        |usage| async {
            harness
                .record_usage(usage, None, None)
                .await
                .map(|_| ())
                .context("harness usage write-back")
        },
        job.progress.as_deref(),
    )
    .await;
    // Never snapshot stats or close the harness while a usage write is pending.
    // Retain errors until after cleanup, including on prompt failure.
    let stats = harness
        .get_stats()
        .await
        .context("read final session stats");
    let closed = harness.close().await.context("close repair harness");
    let usage = usage?;
    closed?;
    let result = match result {
        Ok(result) => result,
        // The prompt call is the provider call: a failure here is the
        // provider's, not the issue's.
        Err(e) => {
            return Err(anyhow::Error::new(ProviderFailure(format!("{e:#}"))).context("repair run"));
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

    let stats = stats?;
    Ok(RepairReport {
        run_id: result.run_id,
        root_cause_text,
        turns: usage.turns,
        // SessionStats excludes output tokens; never let it lower the spend
        // already published by progress (including the fully drained last turn).
        uncached_tokens: usage
            .uncached_tokens
            .max(stats.uncached_tokens.max(0) as u64),
        cached_tokens: usage.cached_tokens.max(stats.cached_tokens.max(0) as u64),
        session_dir: sessions_dir.to_string_lossy().into_owned(),
        context: ContextBudget {
            model_context_window,
            compactions: compactions.load(Ordering::Relaxed),
            messages: stats.message_count,
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

    fn usage_event(input: i64, output: i64, cache_read: i64, cache_write: i64) -> AgentEvent {
        let mut message = rpi_ai::providers::faux::faux_assistant_message(
            "done",
            rpi_ai::types::StopReason::Stop,
        );
        message.usage.input = input;
        message.usage.output = output;
        message.usage.cache_read = cache_read;
        message.usage.cache_write = cache_write;
        message.usage.total_tokens = input + output + cache_read + cache_write;
        AgentEvent::TurnEnd {
            message: rpi_agent::AgentMessage::Assistant(Box::new(message)),
            tool_results: Vec::new(),
        }
    }

    #[tokio::test]
    async fn delayed_tail_usage_is_joined_on_success_and_failure() {
        for fail in [false, true] {
            let (emitter, rx) = BroadcastEmitter::new(16);
            let (release, wait) = tokio::sync::oneshot::channel();
            let mut wait = Some(wait);
            let owner = Arc::new(());
            let weak = Arc::downgrade(&owner);
            let recorded = Arc::new(Mutex::new(Vec::new()));
            let records = Arc::clone(&recorded);
            let seen = Mutex::new(Vec::new());
            let progress = |p| seen.lock().expect("progress").push(p);
            let prompt = async {
                emitter.try_emit(usage_event(10, 3, 4, 2));
                emitter.try_emit(usage_event(20, 7, 8, 5));
                if fail {
                    anyhow::bail!("prompt failed")
                }
                Ok(())
            };
            let mut collection = Box::pin(collect_run_events(
                prompt,
                rx,
                move |usage: rpi_ai::types::Usage| {
                    let _keep_alive = &owner;
                    let wait = if usage.input == 20 { wait.take() } else { None };
                    let records = Arc::clone(&records);
                    async move {
                        if let Some(wait) = wait {
                            wait.await.expect("release tail");
                        }
                        records.lock().expect("records").push(usage);
                        Ok(())
                    }
                },
                Some(&progress),
            ));
            // The prompt is already done, but the final usage write is explicitly
            // held. No elapsed-time heuristic can prove that this drain is done.
            assert!(futures::poll!(collection.as_mut()).is_pending());
            assert_eq!(recorded.lock().expect("records").len(), 1);
            release.send(()).expect("release");
            let (result, totals) = collection.await;
            assert_eq!(result.is_err(), fail);
            let totals = totals.expect("usage");
            assert_eq!(totals.turns, 2);
            assert_eq!(totals.uncached_tokens, 47);
            assert_eq!(totals.cached_tokens, 12);
            assert_eq!(recorded.lock().expect("records").len(), 2);
            let seen = seen.lock().expect("progress");
            assert_eq!(seen.len(), 2);
            assert_eq!(seen[1].uncached_tokens, 47);
            assert_eq!(seen[1].cached_tokens, 12);
            assert!(weak.upgrade().is_none(), "drain retained its owner");
        }
    }

    #[tokio::test]
    async fn usage_write_failure_is_visible_and_does_not_discard_tail() {
        let (emitter, rx) = BroadcastEmitter::new(16);
        let seen = Mutex::new(Vec::new());
        let progress = |p| seen.lock().expect("progress").push(p);
        let (_, result) = collect_run_events(
            async {
                emitter.try_emit(usage_event(10, 3, 4, 2));
                emitter.try_emit(usage_event(20, 7, 8, 5));
            },
            rx,
            |usage| async move {
                if usage.input == 10 {
                    anyhow::bail!("usage disk full")
                }
                Ok(())
            },
            Some(&progress),
        )
        .await;
        let error = result.expect_err("usage failure cannot become zero spend");
        assert!(format!("{error:#}").contains("usage disk full"));
        let seen = seen.lock().expect("progress");
        assert_eq!(seen.len(), 2, "drain must continue after a write failure");
        assert_eq!(seen[1].uncached_tokens, 47);
        assert_eq!(seen[1].cached_tokens, 12);
    }

    #[tokio::test]
    async fn lost_events_fail_accounting_but_drain_the_remaining_usage() {
        let (emitter, rx) = BroadcastEmitter::new(1);
        let seen = Mutex::new(Vec::new());
        let progress = |p| seen.lock().expect("progress").push(p);
        let (_, result) = collect_run_events(
            async {
                emitter.try_emit(usage_event(10, 3, 4, 2));
                emitter.try_emit(usage_event(20, 7, 8, 5));
            },
            rx,
            |_| async { Ok(()) },
            Some(&progress),
        )
        .await;
        let error = result.expect_err("lost events must not silently undercount");
        assert!(format!("{error:#}").contains("lagged by 1"));
        let seen = seen.lock().expect("progress");
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].uncached_tokens, 32);
        assert_eq!(seen[0].cached_tokens, 8);
    }

    #[tokio::test]
    async fn cancelled_collection_does_not_detach_a_drain_task() {
        let (emitter, rx) = BroadcastEmitter::new(16);
        let owner = Arc::new(());
        let weak = Arc::downgrade(&owner);
        emitter.try_emit(usage_event(10, 3, 4, 2));
        let mut collection = Box::pin(collect_run_events(
            std::future::pending::<()>(),
            rx,
            move |_| {
                let _keep_alive = &owner;
                std::future::pending::<anyhow::Result<()>>()
            },
            None,
        ));
        assert!(futures::poll!(collection.as_mut()).is_pending());
        drop(collection);
        assert!(
            weak.upgrade().is_none(),
            "cancelled drain retained its owner"
        );
    }

    #[test]
    fn actual_provider_defaults_match_catalog_and_construction() {
        let (name, model, base) = actual_provider_config(&ProviderKind::Anthropic { model: None })
            .expect("anthropic defaults");
        let catalog = anthropic_models();
        assert_eq!(name, "anthropic");
        assert_eq!(model.as_deref(), Some(catalog[0].id.as_str()));
        assert_eq!(base.as_deref(), Some(catalog[0].base_url.as_str()));

        let (_, model, base) = actual_provider_config(&ProviderKind::Faux).expect("faux defaults");
        let faux = FauxProvider::new(FauxScript::new());
        assert_eq!(model.as_deref(), Some(faux.default_model().id.as_str()));
        assert_eq!(
            base.as_deref(),
            Some(faux.default_model().base_url.as_str())
        );

        let (_, model, base) = actual_provider_config(&ProviderKind::Zai {
            base_url: None,
            model: None,
        })
        .expect("zai defaults");
        let (expected_base, expected_model) = zai_config(&None, &None);
        assert_eq!(model, Some(expected_model));
        assert_eq!(base, Some(expected_base));
        let custom = ProviderKind::Zai {
            base_url: Some("https://example.test/v4".into()),
            model: Some("synthetic-model".into()),
        };
        assert_eq!(
            actual_provider_config(&custom).expect("custom"),
            (
                "zai".into(),
                Some("synthetic-model".into()),
                Some("https://example.test/v4".into()),
            )
        );
        assert!(actual_provider_config(&ProviderKind::Anthropic {
            model: Some("not-a-catalog-model".into()),
        })
        .is_err());
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn faux_failure_persists_tail_usage_and_releases_progress() {
        use rpi_ai::providers::faux::{faux_assistant_message, FauxStep};
        let dir = tempfile::tempdir().expect("tempdir");
        let script = FauxScript::new().with_tool_call(
            "write",
            serde_json::json!({"path": "attempt.txt", "content": "attempt"}),
        );
        let mut failure = faux_assistant_message("cannot repair", rpi_ai::types::StopReason::Error);
        failure.error_message = Some("invalid request".into());
        script.append_responses(vec![FauxStep::message(failure)]);
        let seen = Arc::new(Mutex::new(Vec::<RepairProgress>::new()));
        let seen_cb = Arc::clone(&seen);
        let owner = Arc::new(());
        let weak = Arc::downgrade(&owner);
        let job = RepairJob {
            repo: dir.path().to_path_buf(),
            issue: issue(),
            sessions_dir: dir.path().join("sessions"),
            provider: ProviderKind::Faux,
            max_turns: 5,
            guards: Arc::new(Guards::new(Vec::new())),
            secrets: Arc::new(crate::secrets::Secrets::default()),
            notes: String::new(),
            progress: Some(Arc::new(move |p| {
                let _keep_alive = &owner;
                seen_cb.lock().expect("progress").push(p);
            })),
            faux_script: Some(script),
            run_id: None,
        };
        let error = run(&job).await.err().expect("provider failure");
        assert!(
            error.downcast_ref::<ProviderFailure>().is_some(),
            "{error:#}"
        );
        let session_dir = session_dir_for(&job.sessions_dir, &job.issue, None);
        let transcript = std::fs::read_dir(session_dir)
            .expect("sessions")
            .flat_map(|entry| {
                std::fs::read_dir(entry.expect("cwd bucket").path()).expect("cwd sessions")
            })
            .map(|entry| entry.expect("entry").path())
            .find(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
            .expect("transcript");
        let transcript = std::fs::read_to_string(transcript).expect("read transcript");
        let usages: Vec<rpi_ai::types::Usage> = transcript
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("jsonl"))
            .filter(|line| line["kind"] == "record" && line["type"] == "usage")
            .map(|line| serde_json::from_value(line["usage"].clone()).expect("usage"))
            .collect();
        assert_eq!(usages.len(), 2, "failure must persist both turns' usage");
        let seen = seen.lock().expect("progress");
        assert_eq!(seen.len(), 2);
        assert!(seen[1].uncached_tokens > seen[0].uncached_tokens);
        let persisted: i64 = usages
            .iter()
            .map(|usage| usage.input + usage.output + usage.cache_write)
            .sum();
        assert_eq!(seen[1].uncached_tokens, persisted as u64);
        drop(job);
        assert!(weak.upgrade().is_none(), "failed run retained progress");
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
        assert!(report.root_cause_text.ends_with("OUTCOME: fixed"));
        assert!(
            report.context.model_context_window > 0,
            "faux model must expose a context window"
        );
        assert_eq!(
            report.context.compactions, 0,
            "short run needs no compaction"
        );
        assert!(
            report.context.messages > 0,
            "final session must count messages"
        );
        let events = seen.lock().expect("lock");
        assert!(
            !events.is_empty(),
            "progress callback must fire at least once"
        );
        let last = events.last().expect("last");
        assert_eq!(last.turns, 3);
        assert!(
            last.uncached_tokens > 0,
            "faux supplies real usage estimates"
        );
        assert_eq!(report.turns, last.turns);
        assert_eq!(report.uncached_tokens, last.uncached_tokens);
        assert_eq!(report.cached_tokens, last.cached_tokens);

        let fixed = std::fs::read_to_string(&source).expect("read");
        assert!(fixed.contains("i < len(xs)"), "faux edit must land");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn per_run_turn_caps_do_not_race() {
        let root = tempfile::tempdir().expect("tempdir");
        let make_job = |name: &str, max_turns: u32, seen: Arc<Mutex<Vec<RepairProgress>>>| {
            let repo = root.path().join(name);
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
            let seen_cb = Arc::clone(&seen);
            RepairJob {
                repo,
                issue: issue(),
                sessions_dir: root.path().join(format!("sessions-{name}")),
                provider: ProviderKind::Faux,
                max_turns,
                guards: Arc::new(Guards::new(Vec::new())),
                secrets: Arc::new(crate::secrets::Secrets::default()),
                notes: String::new(),
                progress: Some(Arc::new(move |progress| {
                    seen_cb.lock().expect("progress").push(progress);
                })),
                faux_script: None,
                run_id: Some(name.into()),
            }
        };
        let low_seen = Arc::new(Mutex::new(Vec::new()));
        let high_seen = Arc::new(Mutex::new(Vec::new()));
        let low = make_job("low", 1, Arc::clone(&low_seen));
        let high = make_job("high", 5, Arc::clone(&high_seen));

        let (low_result, high_result) = tokio::join!(run(&low), run(&high));

        let low_error = match low_result {
            Ok(_) => panic!("cap 1 must abort after the first turn"),
            Err(error) => error,
        };
        assert!(
            format!("{low_error:#}").contains("aborted"),
            "unexpected low-cap outcome: {low_error:#}"
        );
        let high_report = high_result.expect("cap 5 must finish the 3-turn script");
        assert_eq!(high_report.turns, 3);
        assert_eq!(
            low_seen
                .lock()
                .expect("progress")
                .last()
                .expect("low progress")
                .turns,
            1,
            "the cap-1 run must stop at its own limit"
        );
        assert_eq!(
            high_seen
                .lock()
                .expect("progress")
                .last()
                .expect("high progress")
                .turns,
            3,
            "the cap-5 run must keep its own limit"
        );
    }
}
