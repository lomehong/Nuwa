//! Nuwa 本地补丁(2026-10-21): compaction 摘要请求必须继承调用方 pin 的 stream
//! options（timeout / max_retries 等）的回归测试。
//!
//! 旧实现的 `build_summary_options` 重建默认 `SimpleStreamOptions`，丢掉了
//! harness 钉住的限制（nuwa pin 了 120s 超时 / 1 次重试），摘要请求会退回
//! provider 默认 600s。这里用一个记录 `stream_simple` 实际收到 options 的
//! capture provider，断言 `generate_summary_with_usage` 把 pin 的值透传下去；
//! `stream_options: None` 时保持旧默认行为不变。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rpi_agent::message::AgentMessage;
use rpi_ai::providers::faux::{FauxProvider, FauxScript};
use rpi_ai::types::{AssistantMessage, AssistantMessageEvent, DoneReason, StopReason, UserContent, UserMessage};
use rpi_ai::{
    event_stream::create_assistant_message_event_stream, AssistantMessageEventStream, Content,
    Context, Model, Provider, SimpleStreamOptions,
};
use rpi_harness::compaction::{generate_summary_with_usage, CompactionLlmOptions};
use rpi_harness::types::AgentHarnessStreamOptions;
use tokio_util::sync::CancellationToken;

/// 记录每次 `stream_simple` 收到的 `SimpleStreamOptions`，并回一段固定摘要文本。
struct CaptureProvider {
    model: Model,
    captured: Arc<Mutex<Vec<SimpleStreamOptions>>>,
}

impl CaptureProvider {
    fn new(model: Model) -> Arc<Self> {
        Arc::new(Self {
            model,
            captured: Arc::new(Mutex::new(Vec::new())),
        })
    }
}

#[async_trait::async_trait]
impl Provider for CaptureProvider {
    fn id(&self) -> &str {
        "capture"
    }

    fn models(&self) -> &[Model] {
        std::slice::from_ref(&self.model)
    }

    async fn stream_simple(
        &self,
        model: &Model,
        _ctx: &Context,
        opts: &SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        self.captured.lock().unwrap().push(opts.clone());
        let (mut producer, stream) = create_assistant_message_event_stream();
        let mut message =
            AssistantMessage::empty(model.api.clone(), "capture", model.id.clone(), 0);
        message.content = vec![Content::text("## Goal\nsummary")];
        message.stop_reason = StopReason::Stop;
        producer.push(AssistantMessageEvent::Start {
            partial: Arc::new(message.clone()),
        });
        producer.push(AssistantMessageEvent::Done {
            reason: DoneReason::Stop,
            message,
        });
        producer.close();
        stream
    }
}

fn history() -> Vec<AgentMessage> {
    vec![AgentMessage::User(UserMessage::new(
        UserContent::Text("some history to summarize".to_string()),
        0,
    ))]
}

#[tokio::test]
async fn summary_request_carries_pinned_timeout_and_max_retries() {
    let model = FauxProvider::new(FauxScript::new()).default_model().clone();
    let provider = CaptureProvider::new(model.clone());
    let opts = CompactionLlmOptions {
        provider: Arc::clone(&provider) as Arc<dyn Provider>,
        model,
        api_key: None,
        signal: CancellationToken::new(),
        thinking_level: None,
        retry: None,
        custom_instructions: None,
        // nuwa 侧 pin 的形状：120s 超时 + 1 次重试。
        stream_options: Some(AgentHarnessStreamOptions {
            timeout: Some(Duration::from_secs(120)),
            max_retries: Some(1),
            ..Default::default()
        }),
    };

    let (text, _usage) = generate_summary_with_usage(&history(), 2000, None, &opts)
        .await
        .expect("summary generation succeeds");
    assert!(text.contains("summary"), "text was: {text}");

    let captured = provider.captured.lock().unwrap();
    assert_eq!(captured.len(), 1, "exactly one summarization request");
    assert_eq!(
        captured[0].timeout,
        Some(Duration::from_secs(120)),
        "summary request must inherit the pinned timeout, not the provider default"
    );
    assert_eq!(
        captured[0].max_retries,
        Some(1),
        "summary request must inherit the pinned max_retries"
    );
    // build_summary_options 自己负责的字段不被 merge 破坏：
    assert!(captured[0].max_tokens.is_some(), "max_tokens still set");
}

#[tokio::test]
async fn summary_request_keeps_defaults_without_pinned_options() {
    let model = FauxProvider::new(FauxScript::new()).default_model().clone();
    let provider = CaptureProvider::new(model.clone());
    let opts = CompactionLlmOptions {
        provider: Arc::clone(&provider) as Arc<dyn Provider>,
        model,
        api_key: None,
        signal: CancellationToken::new(),
        thinking_level: None,
        retry: None,
        custom_instructions: None,
        stream_options: None,
    };

    let (_text, _usage) = generate_summary_with_usage(&history(), 2000, None, &opts)
        .await
        .expect("summary generation succeeds");

    let captured = provider.captured.lock().unwrap();
    assert_eq!(captured.len(), 1);
    // None = 旧默认行为：与 `SimpleStreamOptions::default()` 完全一致（600s
    // provider 默认超时、无重试 pin），不引入任何调用方限制。
    let default_opts = SimpleStreamOptions::default();
    assert_eq!(captured[0].timeout, default_opts.timeout);
    assert_eq!(captured[0].max_retries, default_opts.max_retries);
}
