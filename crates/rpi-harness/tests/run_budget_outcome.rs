//! Nuwa 本地补丁(2026-10-21): 预算耗尽误报 Completed 的回归测试。
//!
//! 达到 run budget（RPI_MAX_TURNS_PER_RUN）停止时，最后一条 assistant 往往还
//! 停在 ToolUse；旧实现只追加 runBudget 通知，outcome 仍被 derive 成 Completed。
//! nuwa 侧 (crates/nuwa/src/repair.rs) 把 Completed 当修复成功、Aborted 当失败
//! bail——预算耗尽必须被识别为失败。修复后：budget_stop 触发且最后一条 assistant
//! 不是正常完成 (stop_reason != Stop) 时，outcome 为 Aborted。
//!
//! RunBudget 只从环境变量构造（`RPI_MAX_TURNS_PER_RUN`），环境变量是进程级的，
//! 所以本文件独立成一个测试二进制，内部只有这一个用它的测试，避免污染并行测试。

use std::sync::Arc;

use rpi_ai::providers::faux::{FauxProvider, FauxScript};
use rpi_ai::Provider;
use rpi_harness::agent_harness::{AgentHarness, AgentLane, HarnessRunOutcome};
use rpi_harness::run_budget::MAX_TURNS_ENV;
use rpi_harness::session::memory::{InMemorySessionStorage, SystemClock};
use rpi_harness::session::types::SessionMetadata;
use rpi_harness::session::{DefaultIdGenerator, Session};
use rpi_harness::types::{AgentHarnessOptions, HarnessTool, RetryPolicy};
use rpi_tools::{
    create_read_tool, create_write_tool, ExecutionToolContext, FileSystem, InMemoryExecutionEnv,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn budget_exhausted_run_with_pending_tool_use_is_not_completed() {
    // 环境变量是进程级的：先保存旧值，测试结束恢复。
    let previous = std::env::var(MAX_TURNS_ENV).ok();
    std::env::set_var(MAX_TURNS_ENV, "1");

    let result = run_with_budget().await;

    match previous {
        Some(v) => std::env::set_var(MAX_TURNS_ENV, v),
        None => std::env::remove_var(MAX_TURNS_ENV),
    }

    let (outcome, env) = result;
    // 预算耗尽且工作未完成（最后一条 assistant 还在请求工具）→ 绝不能是 Completed。
    assert!(
        !matches!(outcome, HarnessRunOutcome::Completed { .. }),
        "budget-exhausted run must not report Completed, got: {outcome:?}"
    );
    // 语义上应为 Aborted：nuwa repair.rs 对 Aborted 直接 bail（"budget or abort"）。
    match &outcome {
        HarnessRunOutcome::Aborted { final_message, .. } => {
            assert_eq!(
                final_message.stop_reason,
                rpi_ai::types::StopReason::ToolUse,
                "the run was cut off mid-work, after the budget turn's tool calls"
            );
        }
        other => panic!("expected Aborted on budget exhaustion, got {other:?}"),
    }
    // 预算 turn 的工具确实执行过（stop 发生在该 turn 的工具结果之后）。
    assert!(
        env.read_text_file("/a.txt", None).await.is_ok(),
        "the budget turn's tool should have run before the stop"
    );
}

/// max_turns=1：第一轮 assistant 请求 write 工具，工具跑完后预算触发停止，
/// 第二轮（正常收尾文本）永远不会发生。
async fn run_with_budget() -> (HarnessRunOutcome, Arc<InMemoryExecutionEnv>) {
    let provider = FauxProvider::new(
        FauxScript::new()
            .with_tool_call(
                "write",
                serde_json::json!({ "path": "a.txt", "content": "budget turn" }),
            )
            .with_text("This final turn must never be reached."),
    );
    let model = provider.default_model().clone();

    let env = Arc::new(InMemoryExecutionEnv::new());
    let env_dyn: Arc<dyn rpi_tools::ExecutionEnv> = env.clone();
    let mut_env: Arc<dyn rpi_tools::MutatingEnv> = env.clone();
    let ctx = ExecutionToolContext::new(env_dyn, Some(mut_env));
    let tools: Vec<HarnessTool> = vec![create_read_tool(&ctx, None), create_write_tool(&ctx)]
        .into_iter()
        .map(HarnessTool::new)
        .collect();
    let active: Vec<String> = tools.iter().map(|t| t.tool.schema().name.clone()).collect();

    let metadata = SessionMetadata {
        id: "budget".into(),
        created_at: 0,
        parent_session_id: None,
    };
    let storage = Arc::new(InMemorySessionStorage::new(
        metadata,
        Arc::new(SystemClock),
        Arc::new(DefaultIdGenerator::new()),
    ));
    let session = Session::new(storage, None);

    let options = AgentHarnessOptions {
        model,
        thinking_level: Default::default(),
        active_tool_names: active,
        tools,
        system_prompt: None,
        resources: Default::default(),
        stream_options: Default::default(),
        retry: RetryPolicy::default(),
        compaction: Default::default(),
        steering_mode: Default::default(),
        follow_up_mode: Default::default(),
        tool_execution: Default::default(),
        drive: Default::default(),
        session,
        models: vec![provider.clone() as Arc<dyn Provider>],
        to_provider_messages: None,
        entry_projectors: Default::default(),
        agent_emitter: None,
        before_tool_call: None,
        after_tool_call: None,
        transform_context: None,
        entry_transforms: Vec::new(),
        provider_hooks: None,
        allow_existing_session: false,
    };

    let harness = AgentHarness::create(options).await.expect("create harness");
    let result = harness
        .prompt_text("work until the budget stops you", vec![])
        .await
        .expect("budget stop still resolves the run (as Aborted), not an io error");
    (result.outcome, env)
}
