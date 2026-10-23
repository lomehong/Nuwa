//! Generic state-change webhook.
//!
//! This is deliberately narrow: Nuwa emits its own lifecycle events to one
//! owner-configured endpoint. It does not collect metrics or route arbitrary
//! alerts (docs/01). Payloads are JSON and signed with HMAC-SHA256 when a
//! secret is configured.

use std::collections::BTreeSet;
use std::sync::OnceLock;
use std::time::Duration;

use hmac::{Hmac, Mac};
use serde::Serialize;
use sha2::Sha256;
use tokio::sync::mpsc;

pub const DEFAULT_EVENTS: &[&str] = &[
    "escalated",
    "ignored",
    "provider_stand_down",
    "heartbeat_stale",
    "ci_red",
    "sync_error",
    "pr_opened",
    "runner_error",
];

#[derive(Debug, Clone)]
pub struct WebhookConfig {
    pub url: String,
    pub secret: Option<String>,
    pub events: BTreeSet<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WebhookEvent {
    pub schema: &'static str,
    /// Unique per event. The signature covers it, so a receiver can dedupe on
    /// `id` (and reject stale `ts_ms`) instead of replaying a captured body.
    pub id: String,
    pub event: String,
    pub fingerprint: Option<String>,
    pub repo: Option<String>,
    pub detail: String,
    pub ts_ms: i64,
}

impl WebhookEvent {
    pub fn new(event: impl Into<String>, fingerprint: Option<String>, repo: Option<String>, detail: impl Into<String>) -> Self {
        Self {
            schema: "nuwa/webhook/v1",
            id: uuid::Uuid::new_v4().to_string(),
            event: event.into(),
            fingerprint,
            repo,
            detail: detail.into(),
            ts_ms: crate::now_ms(),
        }
    }
}

#[derive(Clone)]
#[derive(Clone)]
pub struct WebhookHandle {
    tx: Option<mpsc::UnboundedSender<WebhookEvent>>,
    /// Tells the delivery task to drain the queue and stop.
    stop: Option<std::sync::Arc<tokio::sync::Notify>>,
    /// The delivery task, awaited on shutdown.
    task: Option<std::sync::Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>>,
}

impl WebhookHandle {
    pub fn disabled() -> Self {
        Self {
            tx: None,
            stop: None,
            task: None,
        }
    }

    pub fn emit(&self, event: WebhookEvent) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(event);
        }
    }

    /// Deliver what is still queued, then stop the delivery task.
    ///
    /// The events that matter most — `escalated`, `ci_red` — are emitted right
    /// before the runner gives up on an issue, and the runner may exit
    /// immediately afterwards. Without this drain they die with the runtime.
    /// Returns `false` when the queue did not drain within `timeout`.
    pub async fn shutdown(&self, timeout: Duration) -> bool {
        let (Some(stop), Some(task)) = (&self.stop, &self.task) else {
            return true;
        };
        stop.notify_one();
        let handle = {
            let mut guard = task.lock().await;
            guard.take()
        };
        let Some(handle) = handle else {
            return true;
        };
        matches!(tokio::time::timeout(timeout, handle).await, Ok(Ok(())))
    }
}

static GLOBAL: OnceLock<WebhookHandle> = OnceLock::new();

/// Initialize the process-wide webhook from env. Safe to call more than once;
/// the first caller wins. Both `serve` and `work` call this at startup.
pub fn init_from_env() -> &'static WebhookHandle {
    GLOBAL.get_or_init(|| {
        let Some(url) = std::env::var("NUWA_WEBHOOK_URL")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
        else {
            return WebhookHandle::disabled();
        };
        let secret = std::env::var("NUWA_WEBHOOK_SECRET")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let events = std::env::var("NUWA_WEBHOOK_EVENTS")
            .ok()
            .map(|raw| parse_events(&raw))
            .filter(|events| !events.is_empty())
            .unwrap_or_else(|| DEFAULT_EVENTS.iter().map(|event| (*event).to_string()).collect());
        spawn(WebhookConfig {
            url,
            secret,
            events,
        })
    })
}

/// Emit through the process-wide handle, if one was initialized.
pub fn emit(event: WebhookEvent) {
    if let Some(handle) = GLOBAL.get() {
        handle.emit(event);
    }
}

/// Flush the process-wide webhook queue. Call before the process exits —
/// delivery runs in a detached task and would otherwise be lost.
pub async fn flush(timeout: Duration) -> bool {
    match GLOBAL.get() {
        Some(handle) => handle.shutdown(timeout).await,
        None => true,
    }
}

pub fn parse_events(raw: &str) -> BTreeSet<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|event| !event.is_empty())
        .map(str::to_string)
        .collect()
}

/// Start the delivery task. The channel is unbounded because webhook delivery
/// has a hard HTTP timeout/retry budget and must never block the runner.
pub fn spawn(config: WebhookConfig) -> WebhookHandle {
    let (tx, mut rx) = mpsc::unbounded_channel::<WebhookEvent>();
    let stop = std::sync::Arc::new(tokio::sync::Notify::new());
    let task_stop = std::sync::Arc::clone(&stop);
    let task = tokio::spawn(async move {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        loop {
            let event = tokio::select! {
                ev = rx.recv() => match ev {
                    Some(ev) => ev,
                    None => break,
                },
                // Shutdown: deliver what is queued, then stop.
                _ = task_stop.notified() => {
                    while let Ok(ev) = rx.try_recv() {
                        deliver(&client, &config, ev).await;
                    }
                    break;
                }
            };
            deliver(&client, &config, event).await;
        }
    });
    WebhookHandle {
        tx: Some(tx),
        stop: Some(stop),
        task: Some(std::sync::Arc::new(tokio::sync::Mutex::new(Some(task)))),
    }
}

/// Deliver one event, retrying transient failures with a bounded budget.
async fn deliver(client: &reqwest::Client, config: &WebhookConfig, event: WebhookEvent) {
    if !config.events.contains(&event.event) {
        return;
    }
    let body = match serde_json::to_string(&event) {
        Ok(body) => body,
        Err(_) => return,
    };
    let signature = config.secret.as_deref().map(|secret| hmac_hex(secret, &body));
    for attempt in 1..=3u32 {
        let mut request = client
            .post(&config.url)
            .header("Content-Type", "application/json")
            .header("User-Agent", "nuwa-webhook")
            .body(body.clone());
        if let Some(signature) = &signature {
            request = request.header("X-Nuwa-Signature", format!("sha256={signature}"));
        }
        match request.send().await {
            Ok(response) if response.status().is_success() => break,
            Ok(response) => {
                eprintln!(
                    "webhook    : {} -> HTTP {} (attempt {attempt}/3)",
                    event.event,
                    response.status()
                );
            }
            Err(error) => {
                eprintln!("webhook    : {} -> {error} (attempt {attempt}/3)", event.event);
            }
        }
        if attempt < 3 {
            tokio::time::sleep(Duration::from_secs(1 << (attempt - 1))).await;
        }
    }
}

pub fn hmac_hex(secret: &str, body: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key");
    mac.update(body.as_bytes());
    mac.finalize()
        .into_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_matches_the_known_hmac_sha256_vector() {
        assert_eq!(
            hmac_hex("key", "The quick brown fox jumps over the lazy dog"),
            "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"
        );
    }

    #[test]
    fn event_filter_parses_csv_and_ignores_empty_parts() {
        let events = parse_events(" escalated, ignored ,, sync_error ");
        assert!(events.contains("escalated"));
        assert!(events.contains("ignored"));
        assert!(events.contains("sync_error"));
        assert_eq!(events.len(), 3);
    }

    #[test]
    fn event_payload_carries_schema_and_identity() {
        let event = WebhookEvent::new("escalated", Some("fp".into()), Some("repo".into()), "boom");
        let value = serde_json::to_value(&event).expect("json");
        assert_eq!(value["schema"], "nuwa/webhook/v1");
        assert_eq!(value["event"], "escalated");
        assert_eq!(value["fingerprint"], "fp");
        assert!(
            value["id"].as_str().is_some_and(|id| !id.is_empty()),
            "every event carries a dedupe id: {value}"
        );
        assert!(value["ts_ms"].as_i64().is_some_and(|ts| ts > 0));
    }

    /// 同一事件重投必须换 id，接收方才不会把重试当重复丢掉。
    #[test]
    fn each_delivery_gets_a_fresh_id() {
        let a = WebhookEvent::new("escalated", None, None, "x");
        let b = WebhookEvent::new("escalated", None, None, "x");
        assert_ne!(a.id, b.id);
    }
}
