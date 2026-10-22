//! 决策 trace：把「为什么做这个决定」写成结构化事件，和运行指标分开。
//!
//! 事件 kind 固定为 `decision`，detail 是 JSON：
//! `{"stage": "...", "decision": "...", "reason": "...", "details": {...}}`。
//! 失败不阻断主流程——trace 是证据，不是控制流。

pub struct Decision<'a> {
    /// claim / preflight / repair / budget / deliver / ci
    pub stage: &'a str,
    pub decision: &'a str,
    pub reason: &'a str,
    pub details: serde_json::Value,
}

pub fn record(store: &shanhaijing::Store, fingerprint: &str, repo: &str, decision: Decision<'_>) {
    let detail = serde_json::json!({
        "stage": decision.stage,
        "decision": decision.decision,
        "reason": decision.reason,
        "details": decision.details,
    })
    .to_string();
    let _ = store.record_event(
        fingerprint,
        crate::now_ms(),
        Some(repo),
        "runner",
        "decision",
        &detail,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decision_is_recorded_as_structured_json() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = shanhaijing::Store::open(dir.path().join("db")).expect("store");
        record(
            &store,
            "fp",
            "example.com/app",
            Decision {
                stage: "repair",
                decision: "started",
                reason: "claimed",
                details: serde_json::json!({"skills": ["rust-cargo@1"]}),
            },
        );
        let events = store.events_for("fp", 10).expect("events");
        let decision = events
            .iter()
            .find(|event| event.kind == "decision")
            .expect("decision event");
        let payload: serde_json::Value =
            serde_json::from_str(&decision.detail).expect("decision detail is JSON");
        assert_eq!(payload["stage"], "repair");
        assert_eq!(payload["decision"], "started");
        assert_eq!(payload["details"]["skills"][0], "rust-cargo@1");
    }
}
