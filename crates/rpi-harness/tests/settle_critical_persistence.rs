//! Nuwa 本地补丁(2026-10-21): settle 关键持久化失败必须终止 run 的回归测试。
//!
//! 旧行为：settle 观察者里 assistant 提交(`append_provisioned`)或 `tool_started`
//! 写入失败只 `tracing::warn!` 并继续，工具随后照常执行——破坏"先持久化后执行"
//! （崩溃后无法恢复"哪个工具的副作用未知"）。修复后：关键持久化失败会记录错误、
//! 取消 run 令牌（工具在 prepare 阶段被中止），run 以 `HarnessError::io` 失败返回。
//!
//! 注入方式：一个包装 `InMemorySessionStorage` 的 `FailingStorage`，按模式让
//! assistant 消息 entry 或 `tool_started` record 的 append 失败；其余写入（帧进度、
//! operation 记录、user prompt 等可丢弃/无关持久化）全部放行。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use rpi_agent::AgentMessage;
use rpi_ai::providers::faux::{FauxProvider, FauxScript};
use rpi_ai::Provider;
use rpi_harness::agent_harness::{AgentHarness, AgentLane};
use rpi_harness::error::{SessionError, SessionResult};
use rpi_harness::session::memory::{InMemorySessionStorage, SystemClock};
use rpi_harness::session::types::{
    BranchBounds, Entry, LanePointer, LaneRecord, LogItem, LogOptions, OperationStartedRecord,
    ProvisionedEntry, ProvisionedKind, RecordQuery, SessionMetadata, SessionStats, SessionStorage,
    EntryQuery,
};
use rpi_harness::session::{DefaultIdGenerator, Session};
use rpi_harness::types::{AgentHarnessOptions, HarnessTool, RetryPolicy};
use rpi_tools::{
    create_bash_tool, create_read_tool, create_write_tool, ExecutionToolContext, FileSystem,
    InMemoryExecutionEnv,
};

#[derive(PartialEq, Clone, Copy)]
enum FailMode {
    /// assistant 消息 entry 的 append 失败（settle_assistant 的关键提交）。
    AssistantEntry,
    /// `tool_started` record 的 append 失败（start_tool 的关键写入）。
    ToolStartedRecord,
}

struct FailingStorage {
    inner: Arc<dyn SessionStorage>,
    mode: FailMode,
    hits: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl SessionStorage for FailingStorage {
    fn metadata(&self) -> SessionMetadata {
        self.inner.metadata()
    }
    async fn get_lanes(&self) -> SessionResult<Vec<LanePointer>> {
        self.inner.get_lanes().await
    }
    async fn create_lane(&self, lane: &str, at: Option<&str>) -> SessionResult<()> {
        self.inner.create_lane(lane, at).await
    }
    async fn move_lane(&self, lane: &str, to: Option<&str>) -> SessionResult<()> {
        self.inner.move_lane(lane, to).await
    }
    async fn append_entry(&self, entry: ProvisionedEntry, lane: &str) -> SessionResult<Entry> {
        let is_assistant = matches!(
            &entry.kind,
            ProvisionedKind::Message {
                message: AgentMessage::Assistant(_),
                ..
            }
        );
        if self.mode == FailMode::AssistantEntry && is_assistant {
            self.hits.fetch_add(1, Ordering::SeqCst);
            return Err(SessionError::storage(
                "injected failure: assistant entry append",
            ));
        }
        self.inner.append_entry(entry, lane).await
    }
    async fn append_record(&self, record: LaneRecord) -> SessionResult<LaneRecord> {
        if self.mode == FailMode::ToolStartedRecord
            && matches!(record, LaneRecord::ToolStarted(_))
        {
            self.hits.fetch_add(1, Ordering::SeqCst);
            return Err(SessionError::storage(
                "injected failure: tool_started record append",
            ));
        }
        self.inner.append_record(record).await
    }
    async fn get_entry(&self, id: &str) -> SessionResult<Option<Entry>> {
        self.inner.get_entry(id).await
    }
    async fn find_entries(&self, query: &EntryQuery) -> SessionResult<Vec<Entry>> {
        self.inner.find_entries(query).await
    }
    async fn find_entries_on_branch(
        &self,
        query: &EntryQuery,
        bounds: &BranchBounds,
        start: &str,
    ) -> SessionResult<Vec<Entry>> {
        self.inner.find_entries_on_branch(query, bounds, start).await
    }
    async fn find_records(&self, query: &RecordQuery) -> SessionResult<Vec<LaneRecord>> {
        self.inner.find_records(query).await
    }
    async fn find_open_operations(
        &self,
        lane: &str,
        limit: Option<usize>,
    ) -> SessionResult<Vec<OperationStartedRecord>> {
        self.inner.find_open_operations(lane, limit).await
    }
    async fn get_log(&self, options: &LogOptions) -> SessionResult<Vec<LogItem>> {
        self.inner.get_log(options).await
    }
    async fn get_name(&self) -> SessionResult<Option<String>> {
        self.inner.get_name().await
    }
    async fn set_name(&self, name: Option<&str>) -> SessionResult<()> {
        self.inner.set_name(name).await
    }
    async fn get_label(&self, id: &str) -> SessionResult<Option<String>> {
        self.inner.get_label(id).await
    }
    async fn set_label(&self, id: &str, label: Option<&str>) -> SessionResult<()> {
        self.inner.set_label(id, label).await
    }
    async fn get_stats(&self) -> SessionResult<SessionStats> {
        self.inner.get_stats().await
    }
}

/// 与 harness_run_e2e 相同的搭建，但 session storage 换成注入失败的包装。
async fn harness_with_failing_storage(
    mode: FailMode,
) -> (AgentHarness, Arc<InMemoryExecutionEnv>, Arc<AtomicUsize>) {
    let provider = FauxProvider::new(
        FauxScript::new()
            .with_tool_call(
                "write",
                serde_json::json!({ "path": "out.txt", "content": "must never be written" }),
            )
            .with_text("Done."),
    );
    let model = provider.default_model().clone();

    let env = Arc::new(InMemoryExecutionEnv::new());
    let env_dyn: Arc<dyn rpi_tools::ExecutionEnv> = env.clone();
    let mut_env: Arc<dyn rpi_tools::MutatingEnv> = env.clone();
    let ctx = ExecutionToolContext::new(env_dyn, Some(mut_env));
    let tools: Vec<HarnessTool> = vec![
        create_read_tool(&ctx, None),
        create_write_tool(&ctx),
        create_bash_tool(&ctx, None),
    ]
    .into_iter()
    .map(HarnessTool::new)
    .collect();
    let active: Vec<String> = tools.iter().map(|t| t.tool.schema().name.clone()).collect();

    let metadata = SessionMetadata {
        id: "settle-fail".into(),
        created_at: 0,
        parent_session_id: None,
    };
    let inner = Arc::new(InMemorySessionStorage::new(
        metadata,
        Arc::new(SystemClock),
        Arc::new(DefaultIdGenerator::new()),
    ));
    let hits = Arc::new(AtomicUsize::new(0));
    let storage: Arc<dyn SessionStorage> = Arc::new(FailingStorage {
        inner,
        mode,
        hits: Arc::clone(&hits),
    });
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
    (harness, env, hits)
}

/// 共享断言：run 必须以错误终止，注入的失败确实发生过，且工具从未执行
/// （in-memory env 中不存在 out.txt）。
async fn assert_run_failed_and_tool_never_ran(
    mode: FailMode,
    expected_error_part: &str,
) {
    let (harness, env, hits) = harness_with_failing_storage(mode).await;

    let error = harness
        .prompt_text("Write the file.", vec![])
        .await
        .expect_err("a critical settle persistence failure must fail the run");
    let message = error.to_string();
    assert!(
        message.contains(expected_error_part),
        "error should name the failed critical write, got: {message}"
    );
    assert!(
        hits.load(Ordering::SeqCst) > 0,
        "the injected failure was never hit — the test proves nothing"
    );
    // 工具未执行："先持久化后执行" 守住了。
    assert!(
        env.read_text_file("/out.txt", None).await.is_err(),
        "the write tool must not run after a critical persistence failure"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn assistant_commit_failure_fails_run_and_tool_never_executes() {
    assert_run_failed_and_tool_never_ran(
        FailMode::AssistantEntry,
        "settled assistant message commit failed",
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tool_started_failure_fails_run_and_tool_never_executes() {
    assert_run_failed_and_tool_never_ran(FailMode::ToolStartedRecord, "tool_started record failed")
        .await;
}
