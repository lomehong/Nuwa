//! Helpers shared by the read endpoints.

use std::collections::BTreeMap;

use shanhaijing::{EventQuery, IssueRow, RunnerRow, ShanResult, Store};

/// Poll interval assumed when a runner's `config_json` has no usable
/// `interval_secs` (docs/09 §8.4).
pub(crate) const DEFAULT_INTERVAL_SECS: u64 = 30;
/// Stuck window floor: `stuck_after = max(3 × interval, 300s)` (docs/09 §8.4).
pub(crate) const STUCK_AFTER_FLOOR_MS: i64 = 300_000;
/// `NUWA_INTERVAL` is documented at ≤ 86400s, so `3 × interval` tops out at
/// 72h; clamp there so a corrupt config cannot disable stuck detection.
pub(crate) const STUCK_AFTER_CEILING_MS: i64 = 259_200_000;
pub(crate) const DAY_MS: i64 = 86_400_000;

/// Funnel keys; every key is present even when its count is zero.
pub(crate) const STATES: [&str; 7] = [
    "new",
    "triggered",
    "fixing",
    "pr_open",
    "merged",
    "rejected",
    "escalated",
];

const REASON_REPAIR_FAILED: (&str, &str) = ("repair_failed", "修复 runner 失败，需要复核");
const REASON_DELIVERY_FAILED: (&str, &str) = ("delivery_failed", "验证/交付失败，需要复核");
const REASON_BUDGET_EXCEEDED: (&str, &str) = ("budget_exceeded", "token 预算超限，需要复核");
const REASON_CI_RED: (&str, &str) = ("ci_red", "CI 红灯，PR 已关闭");
const REASON_PR_CLOSED: (&str, &str) = ("pr_closed", "PR 未合并被关闭");
const REASON_STUCK: (&str, &str) = ("stuck", "疑似卡单，runner 心跳已过期");
const REASON_CI_TIMEOUT: (&str, &str) = ("ci_timeout", "CI 超时，需要人工查看");

/// The shared "needs attention" set: escalated, stuck, rejected, or pr_open
/// with a `ci_timeout` event in the last 24h (docs/09 §9.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AttentionKind {
    Escalated,
    Stuck,
    Rejected,
    CiTimeout,
}

impl AttentionKind {
    /// Overview attention order: escalated > stuck > rejected > ci_timeout.
    pub(crate) fn rank(self) -> u8 {
        match self {
            Self::Escalated => 0,
            Self::Stuck => 1,
            Self::Rejected => 2,
            Self::CiTimeout => 3,
        }
    }

    /// Stable `reason_code`/`reason_text` for the overview attention queue.
    /// Escalations and rejections carry a phase prefix in their newest event
    /// detail (docs/09 §8.3); a rejected issue without one is a PR closed by
    /// `sync`.
    pub(crate) fn reason(
        self,
        store: &Store,
        issue: &IssueRow,
    ) -> ShanResult<(&'static str, &'static str)> {
        Ok(match self {
            Self::Escalated | Self::Rejected => {
                let detail = store
                    .latest_event(&issue.fingerprint)?
                    .map(|event| event.detail)
                    .unwrap_or_default();
                reason_for_detail(&detail, &issue.state)
            }
            Self::Stuck => REASON_STUCK,
            Self::CiTimeout => REASON_CI_TIMEOUT,
        })
    }
}

/// Classify an issue into the attention set. `stuck` must be the result of
/// [`issue_is_stuck`] for the same issue; it is only consulted for
/// `triggered`/`fixing` rows.
pub(crate) fn attention_kind(
    store: &Store,
    issue: &IssueRow,
    stuck: bool,
    now: i64,
) -> ShanResult<Option<AttentionKind>> {
    Ok(match issue.state.as_str() {
        "escalated" => Some(AttentionKind::Escalated),
        "rejected" => Some(AttentionKind::Rejected),
        "triggered" | "fixing" if stuck => Some(AttentionKind::Stuck),
        "pr_open" if ci_timeout_within_day(store, &issue.fingerprint, now)? => {
            Some(AttentionKind::CiTimeout)
        }
        _ => None,
    })
}

/// Owner runner id → configured poll interval, used to size each issue's
/// stuck window. Runners with absent/malformed configs keep the 30s default.
pub(crate) fn runner_intervals(runners: &[RunnerRow]) -> BTreeMap<&str, u64> {
    runners
        .iter()
        .map(|runner| {
            (
                runner.id.as_str(),
                runner_interval_secs(&runner.config_json),
            )
        })
        .collect()
}

/// Read `interval_secs` from a runner's effective `config_json`; anything
/// missing, malformed or non-numeric falls back to the 30s default.
pub(crate) fn runner_interval_secs(config_json: &str) -> u64 {
    serde_json::from_str::<serde_json::Value>(config_json)
        .ok()
        .and_then(|config| {
            config
                .get("interval_secs")
                .and_then(serde_json::Value::as_u64)
        })
        .unwrap_or(DEFAULT_INTERVAL_SECS)
}

/// `stuck_after = max(3 × interval, 300s)` (docs/09 §8.4), saturated and
/// clamped so a corrupt interval can neither overflow nor disable detection.
pub(crate) fn stuck_after_ms(interval_secs: u64) -> i64 {
    let window_ms = interval_secs.saturating_mul(3).saturating_mul(1_000);
    let window_ms = i64::try_from(window_ms).unwrap_or(i64::MAX);
    window_ms.clamp(STUCK_AFTER_FLOOR_MS, STUCK_AFTER_CEILING_MS)
}

/// Stuck window for one issue, sized by its owner runner's poll interval.
pub(crate) fn issue_stuck_after_ms(
    issue: &IssueRow,
    runner_intervals: &BTreeMap<&str, u64>,
) -> i64 {
    let interval_secs = issue
        .owner_runner_id
        .as_deref()
        .and_then(|owner| runner_intervals.get(owner).copied())
        .unwrap_or(DEFAULT_INTERVAL_SECS);
    stuck_after_ms(interval_secs)
}

/// The per-issue stuck flag shared by the list, detail and overview: only
/// `triggered`/`fixing` rows can be stuck, and the window follows the owner
/// runner's configured interval.
pub(crate) fn issue_is_stuck(
    store: &Store,
    issue: &IssueRow,
    runner_intervals: &BTreeMap<&str, u64>,
    now: i64,
) -> ShanResult<bool> {
    if !matches!(issue.state.as_str(), "triggered" | "fixing") {
        return Ok(false);
    }
    store.stuck(
        &issue.fingerprint,
        now,
        issue_stuck_after_ms(issue, runner_intervals),
    )
}

/// Map the newest event detail to a stable reason. Escalation details carry
/// a phase prefix (`repair_failed: …`, docs/09 §8.3); a rejected issue
/// without one is a PR closed by `sync`.
pub(crate) fn reason_for_detail(detail: &str, state: &str) -> (&'static str, &'static str) {
    for (prefix, reason) in [
        ("repair_failed:", REASON_REPAIR_FAILED),
        ("delivery_failed:", REASON_DELIVERY_FAILED),
        ("budget_exceeded:", REASON_BUDGET_EXCEEDED),
        ("ci_red:", REASON_CI_RED),
    ] {
        if detail.starts_with(prefix) {
            return reason;
        }
    }
    if state == "rejected" {
        REASON_PR_CLOSED
    } else {
        // Legacy escalations without a phase prefix still mean the repair
        // pipeline gave up; repair_failed is the generic bucket.
        REASON_REPAIR_FAILED
    }
}

fn ci_timeout_within_day(store: &Store, fingerprint: &str, now: i64) -> ShanResult<bool> {
    let query = EventQuery {
        kinds: vec!["ci_timeout".to_string()],
        fingerprint: Some(fingerprint.to_string()),
        from_ms: Some(now - DAY_MS),
        ..EventQuery::default()
    };
    Ok(!store.list_events(&query, 1)?.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reason_for_detail_maps_every_documented_prefix() {
        let cases = [
            (
                "repair_failed: boom",
                "escalated",
                "repair_failed",
                "修复 runner 失败，需要复核",
            ),
            (
                "delivery_failed: verify failed",
                "escalated",
                "delivery_failed",
                "验证/交付失败，需要复核",
            ),
            (
                "budget_exceeded: 900 > 800",
                "escalated",
                "budget_exceeded",
                "token 预算超限，需要复核",
            ),
            (
                "ci_red: CI red — PR #7 closed",
                "rejected",
                "ci_red",
                "CI 红灯，PR 已关闭",
            ),
        ];
        for (detail, state, code, text) in cases {
            assert_eq!(
                reason_for_detail(detail, state),
                (code, text),
                "detail {detail:?}"
            );
        }
    }

    #[test]
    fn reason_for_detail_requires_the_prefix_and_falls_back() {
        // A prefix must start the detail, not merely appear inside it.
        for detail in [
            "repair failed: repair_failed: boom",
            "repair_failedness",
            "repair_failed",
        ] {
            assert_eq!(
                reason_for_detail(detail, "escalated"),
                REASON_REPAIR_FAILED,
                "detail {detail:?}"
            );
        }
        // Rejections without a phase prefix come from `sync` closing a PR.
        assert_eq!(reason_for_detail("", "rejected"), REASON_PR_CLOSED);
        assert_eq!(
            reason_for_detail("PR closed without merge", "rejected"),
            REASON_PR_CLOSED
        );
    }

    #[test]
    fn stuck_window_follows_the_runner_interval_with_a_300s_floor() {
        assert_eq!(stuck_after_ms(30), STUCK_AFTER_FLOOR_MS);
        assert_eq!(stuck_after_ms(600), 1_800_000);
        assert_eq!(stuck_after_ms(u64::MAX), STUCK_AFTER_CEILING_MS);
        assert_eq!(
            runner_interval_secs(r#"{"interval_secs":600}"#),
            600,
            "the configured interval is honoured"
        );
        for config in ["{}", "", "not json", r#"{"interval_secs":"600"}"#, "[]"] {
            assert_eq!(
                runner_interval_secs(config),
                DEFAULT_INTERVAL_SECS,
                "config {config:?} falls back to the default"
            );
        }
    }
}
