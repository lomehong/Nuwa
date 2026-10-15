//! Repair run: 问题单 → harness（rpi-harness crash-resumable loop）→ 修改 + 自验.
//!
//! One issue = one JSONL session under `sessions/<fingerprint>/`, so a crashed
//! run is resumable and every turn is auditable after the fact.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

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
    /// Repo-instruction notes appended to the runner system prompt.
    pub notes: String,
}

pub struct RepairReport {
    pub run_id: String,
    pub root_cause_text: String,
    pub turns: u32,
    pub uncached_tokens: u64,
    pub cached_tokens: u64,
}

pub async fn run(job: &RepairJob) -> anyhow::Result<RepairReport> {
    // Turn budget (rpi-harness RunBudget reads this per run). Token-level caps
    // land with the 运行器池 scheduler; the turn cap bounds runaway loops now.
    std::env::set_var("RPI_MAX_TURNS_PER_RUN", job.max_turns.to_string());

    let repo = std::path::absolute(&job.repo)
        .with_context(|| format!("resolve repo {}", job.repo.display()))?;
    let repo_str = repo.to_string_lossy().into_owned();

    // ---- env + tool belt ----
    let env = Arc::new(OsExecutionEnv::with_cwd(repo.clone()));
    let env_dyn: Arc<dyn rpi_tools::ExecutionEnv> = env.clone();
    let mut_env: Arc<dyn rpi_tools::MutatingEnv> = env.clone();
    let ctx = ExecutionToolContext::new(env_dyn, Some(mut_env));

    let tools: Vec<Arc<dyn AgentTool>> = vec![
        create_read_tool(&ctx, None),
        create_edit_tool(&ctx),
        create_write_tool(&ctx),
        create_bash_tool(&ctx, None),
        create_grep_tool(&ctx, None),
        create_find_tool(&ctx, None),
        create_ls_tool(&ctx, None),
    ];
    let active_tool_names: Vec<String> = tools.iter().map(|t| t.schema().name.clone()).collect();
    let harness_tools: Vec<HarnessTool> = tools.into_iter().map(HarnessTool::new).collect();

    // ---- durable session (crash-resumable) ----
    let sessions_dir = job.sessions_dir.join(job.issue.short_fingerprint());
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
            // guards, session persistence and delivery. It does NOT prove LLM
            // repair quality — that needs the anthropic path with a real key.
            let script = FauxScript::new()
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
                .with_text(
                    "ROOT CAUSE: Average iterated with i <= len(xs), indexing one past the last element — index out of range panic on every non-empty window.\n\
                     CHANGE: bounded the loop at i < len(xs) so the sum covers exactly the slice.\n\
                     VERIFICATION: go test ./... — all tests pass.",
                );
            let provider = FauxProvider::new(script);
            let model = provider.default_model().clone();
            (model, vec![provider as Arc<dyn Provider>])
        }
        ProviderKind::Zai { base_url, model } => {
            let key = std::env::var("ZAI_API_KEY")
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
            let key = std::env::var("ANTHROPIC_API_KEY")
                .context("ANTHROPIC_API_KEY not set (or use --provider faux for the offline path)")?;
            let http = rpi_ai::http::build_client("https://api.anthropic.com", None, None)
                .unwrap_or_else(|_| reqwest::Client::new());
            let provider = AnthropicProvider::new(Some(key), http);
            let catalog = anthropic_models();
            let picked = catalog
                .iter()
                .find(|m| Some(m.id.as_str()) == model.as_deref())
                .or_else(|| catalog.first())
                .with_context(|| "anthropic model catalog is empty")?
                .clone();
            (picked, vec![Arc::new(provider) as Arc<dyn Provider>])
        }
    };

    // ---- event drain (console visibility) ----
    let (broadcast, mut rx) = BroadcastEmitter::new(1024);
    let emitter: Arc<dyn rpi_agent::AgentEmitter> = Arc::new(broadcast);
    let turns = Arc::new(AtomicU32::new(0));
    let turn_counter = Arc::clone(&turns);
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
                    AgentEvent::TurnEnd { .. } => {
                        eprintln!("  -- turn end --");
                        turn_counter.fetch_add(1, Ordering::Relaxed);
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
        ..AgentHarnessOptions::default()
    };
    let harness = AgentHarness::create(options)
        .await
        .context("harness create")?;
    let result = harness
        .prompt_text(&job.issue.user_prompt(), vec![])
        .await
        .context("repair run")?;
    let stats = harness.get_stats().await.ok();
    harness.close().await.ok();
    drain.abort();

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
        HarnessRunOutcome::Failed { error, .. } => bail!("run failed: {error:?}"),
        HarnessRunOutcome::Suspended { .. } => bail!("run suspended on a provider deferred handle"),
    };

    Ok(RepairReport {
        run_id: result.run_id,
        root_cause_text,
        turns: turns.load(Ordering::Relaxed),
        uncached_tokens: stats.as_ref().map(|s| s.uncached_tokens as u64).unwrap_or(0),
        cached_tokens: stats.as_ref().map(|s| s.cached_tokens as u64).unwrap_or(0),
    })
}
