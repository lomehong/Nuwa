//! Helpers shared by the read endpoints.

use std::collections::BTreeMap;
use std::io::Read;
use std::process::{ExitStatus, Stdio};
use std::time::{Duration, Instant};

use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use axum::Json;
use serde::de::DeserializeOwned;
use serde::Serialize;
use shanhaijing::{EventQuery, IssueRow, RunnerRow, ShanResult, Store};

use crate::api::ApiError;

/// Poll interval assumed when a runner's `config_json` has no usable
/// `interval_secs` (docs/09 §8.4).
pub(crate) const DEFAULT_INTERVAL_SECS: u64 = 30;
/// Stuck window floor: `stuck_after = max(3 × interval, floor)` (docs/09
/// §8.4). `NUWA_STUCK_AFTER_SECS` overrides the floor; 300s is the default.
pub(crate) const DEFAULT_STUCK_FLOOR_SECS: u64 = 300;
/// Documented `NUWA_STUCK_AFTER_SECS` range (docs/09 §8.4).
pub(crate) const STUCK_FLOOR_MIN_SECS: u64 = 60;
pub(crate) const STUCK_FLOOR_MAX_SECS: u64 = 86_400;
pub(crate) const STUCK_AFTER_FLOOR_MS: i64 = DEFAULT_STUCK_FLOOR_SECS as i64 * 1_000;
/// `NUWA_INTERVAL` is documented at ≤ 86400s, so `3 × interval` tops out at
/// 72h; clamp there so a corrupt config cannot disable stuck detection.
pub(crate) const STUCK_AFTER_CEILING_MS: i64 = 259_200_000;
pub(crate) const DAY_MS: i64 = 86_400_000;

/// A runner is offline once its heartbeat is older than this (docs/09 §9.2).
pub(crate) const RUNNER_OFFLINE_MS: i64 = 90_000;

/// Global read-endpoint pagination contract (docs/09 §10).
pub(crate) const DEFAULT_PAGE_SIZE: u64 = 50;
pub(crate) const MAX_PAGE_SIZE: u64 = 200;

/// `{page, page_size, total}` meta shared by the paginated list endpoints.
#[derive(Debug, Serialize)]
pub struct PageMeta {
    pub page: u64,
    pub page_size: u64,
    pub total: usize,
}

pub(crate) fn non_empty(raw: Option<&str>) -> Option<String> {
    raw.filter(|value| !value.is_empty()).map(str::to_string)
}

/// `1|true` / `0|false` query flags; anything else is a field error.
pub(crate) fn parse_flag(
    field: &str,
    raw: Option<&str>,
    fields: &mut BTreeMap<String, String>,
) -> bool {
    match raw.map(str::trim).filter(|value| !value.is_empty()) {
        None => false,
        Some("1" | "true") => true,
        Some("0" | "false") => false,
        Some(other) => {
            fields.insert(field.to_string(), format!("must be 0 or 1, got {other}"));
            false
        }
    }
}

pub(crate) fn parse_page_number(
    field: &str,
    raw: Option<&str>,
    default: u64,
    max: Option<u64>,
    fields: &mut BTreeMap<String, String>,
) -> u64 {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return default;
    };
    match raw.parse::<u64>() {
        Ok(value) if value >= 1 && max.map_or(true, |max| value <= max) => value,
        Ok(_) => {
            let message = match max {
                Some(max) => format!("must be between 1 and {max}"),
                None => "must be a positive integer".to_string(),
            };
            fields.insert(field.to_string(), message);
            default
        }
        Err(_) => {
            fields.insert(field.to_string(), "must be a positive integer".to_string());
            default
        }
    }
}

/// Parse an epoch-ms query bound; invalid numbers become a field error.
pub(crate) fn parse_ms(
    field: &str,
    raw: Option<&str>,
    fields: &mut BTreeMap<String, String>,
) -> Option<i64> {
    let raw = non_empty(raw)?;
    match raw.trim().parse::<i64>() {
        Ok(value) => Some(value),
        Err(_) => {
            fields.insert(
                field.to_string(),
                "must be an integer epoch milliseconds value".to_string(),
            );
            None
        }
    }
}

/// Funnel keys; every key is present even when its count is zero.
pub(crate) const STATES: [&str; 8] = [
    "new",
    "triggered",
    "fixing",
    "pr_open",
    "merged",
    "rejected",
    "escalated",
    "ignored",
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

/// Operator floor override for the stuck window (`NUWA_STUCK_AFTER_SECS`,
/// docs/09 §8.4). Unset or malformed values keep the 300s default; parsed
/// values clamp to the documented 60..=86400 range.
pub(crate) fn stuck_floor_ms() -> i64 {
    match std::env::var("NUWA_STUCK_AFTER_SECS")
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .map(|secs| secs.clamp(STUCK_FLOOR_MIN_SECS, STUCK_FLOOR_MAX_SECS))
    {
        Some(secs) => i64::try_from(secs).unwrap_or(i64::MAX).saturating_mul(1_000),
        None => STUCK_AFTER_FLOOR_MS,
    }
}

/// `stuck_after = max(3 × interval, floor)` (docs/09 §8.4), saturated and
/// clamped so a corrupt interval can neither overflow nor disable detection.
pub(crate) fn stuck_after_ms(interval_secs: u64, floor_ms: i64) -> i64 {
    let window_ms = interval_secs.saturating_mul(3).saturating_mul(1_000);
    let window_ms = i64::try_from(window_ms).unwrap_or(i64::MAX);
    window_ms.max(floor_ms).min(STUCK_AFTER_CEILING_MS)
}

/// Stuck window for one issue, sized by its owner runner's poll interval and
/// raised to the operator floor.
pub(crate) fn issue_stuck_after_ms(
    issue: &IssueRow,
    runner_intervals: &BTreeMap<&str, u64>,
    floor_ms: i64,
) -> i64 {
    let interval_secs = issue
        .owner_runner_id
        .as_deref()
        .and_then(|owner| runner_intervals.get(owner).copied())
        .unwrap_or(DEFAULT_INTERVAL_SECS);
    stuck_after_ms(interval_secs, floor_ms)
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
        issue_stuck_after_ms(issue, runner_intervals, stuck_floor_ms()),
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

/// systemd unit whitelist (docs/09 §13): letters, digits and `@ . _ : -`.
/// A leading `-` is rejected: `systemctl` treats it as an option terminator
/// convention, so `-nuwa-work` could be parsed as a flag rather than a unit.
/// Values are always passed as one `Command` argument, never shell text.
pub(crate) fn valid_systemd_unit(unit: &str) -> bool {
    !unit.is_empty()
        && !unit.starts_with('-')
        && unit
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '@' | '.' | '_' | ':' | '-'))
}

/// Serve unit followed by the runner units, deduplicated in order. Blank
/// entries are skipped: "no unit configured", not an invalid unit.
pub(crate) fn configured_units(serve_unit: &str, runner_units: &[String]) -> Vec<String> {
    let mut units: Vec<String> = Vec::new();
    for unit in std::iter::once(serve_unit).chain(runner_units.iter().map(String::as_str)) {
        if unit.is_empty() || units.iter().any(|existing| existing == unit) {
            continue;
        }
        units.push(unit.to_string());
    }
    units
}

/// Budget for one systemd helper process. A hung `systemd-run`/`systemctl`
/// must not pin a console worker past the documented response window.
pub(crate) const SYSTEMD_TIMEOUT: Duration = Duration::from_secs(3);
/// How often a bounded wait re-checks the child.
const SYSTEMD_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Process-wide restart mutex: one schedule in flight at a time, shared by
/// `POST /settings/restart` and `POST /runners/{id}/restart` (docs/09 §9.2).
/// `try_lock` keeps a concurrent request a 409 instead of a queue.
pub(crate) fn restart_lock() -> &'static tokio::sync::Mutex<()> {
    static RESTART_LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> =
        std::sync::OnceLock::new();
    RESTART_LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// Serializes restart tests: one test holds [`restart_lock`] to prove the
/// 409, so every other restart test waits its turn instead of racing it.
#[cfg(test)]
pub(crate) async fn restart_test_guard() -> tokio::sync::MutexGuard<'static, ()> {
    static RESTART_TEST_LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> =
        std::sync::OnceLock::new();
    RESTART_TEST_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await
}

/// Result of a bounded subprocess run. `stdout` is empty unless the caller
/// asked to capture it.
#[derive(Debug)]
pub(crate) struct BoundedOutput {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
}

/// Run one helper process without a shell, waiting at most [`SYSTEMD_TIMEOUT`].
/// A child that outlives the budget is killed and reported as
/// `TimedOut`, which callers map to a 501 (`systemd-run`) or `n/a`
/// (`systemctl is-active`).
pub(crate) fn run_bounded(
    program: &str,
    args: &[&str],
    capture_stdout: bool,
) -> std::io::Result<BoundedOutput> {
    run_bounded_with(program, args, capture_stdout, SYSTEMD_TIMEOUT)
}

fn run_bounded_with(
    program: &str,
    args: &[&str],
    capture_stdout: bool,
    timeout: Duration,
) -> std::io::Result<BoundedOutput> {
    let mut child = std::process::Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(if capture_stdout {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stderr(Stdio::null())
        .spawn()?;
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            // Best effort: kill a hung helper so it cannot leak, and reap it
            // so no zombie is left behind.
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("{program} did not exit within {}ms", timeout.as_millis()),
            ));
        }
        std::thread::sleep(SYSTEMD_POLL_INTERVAL);
    };
    let mut stdout = Vec::new();
    if let Some(mut pipe) = child.stdout.take() {
        // The child has exited, so the pipe reaches EOF instead of blocking.
        pipe.read_to_end(&mut stdout)?;
    }
    Ok(BoundedOutput { status, stdout })
}

/// `systemd-run --on-active=2 systemctl restart <units...>`: the systemd
/// manager performs the restart after the HTTP response is written, so
/// restarting `nuwa-serve` cannot cut off its own reply. Units stay separate
/// arguments; nothing is ever concatenated into shell text. A spawn failure
/// or a helper that hangs past the budget is a `false` (→ 501), never a hung
/// console request.
pub(crate) fn schedule_restart(units: &[String]) -> bool {
    let mut args: Vec<&str> = vec!["--on-active=2", "systemctl", "restart"];
    args.extend(units.iter().map(String::as_str));
    run_bounded("systemd-run", &args, false)
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Keep extractor failures inside the JSON error contract: syntax errors stay
/// 400, well-formed bodies with wrong field types become 422 field errors.
pub(crate) fn parse_json_body<T: DeserializeOwned>(
    payload: Result<Json<T>, JsonRejection>,
) -> Result<T, ApiError> {
    match payload {
        Ok(Json(body)) => Ok(body),
        Err(rejection) => Err(rejection_error(rejection)),
    }
}

fn rejection_error(rejection: JsonRejection) -> ApiError {
    let status = rejection.status();
    let message = rejection.body_text();
    match status {
        StatusCode::UNSUPPORTED_MEDIA_TYPE => {
            ApiError::new(status, "unsupported_media_type", message)
        }
        StatusCode::UNPROCESSABLE_ENTITY => {
            let mut fields = BTreeMap::new();
            fields.insert("body".to_string(), message);
            ApiError::validation(fields)
        }
        _ => ApiError::invalid_request(message),
    }
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
    fn systemd_unit_whitelist_rejects_option_like_names() {
        for unit in ["nuwa-work", "nuwa-work@1.service", "a.b_c:d", "nuwa-work-2"] {
            assert!(valid_systemd_unit(unit), "{unit}");
        }
        for unit in ["-nuwa-work", "--now", "", "nuwa work", "../etc/passwd"] {
            assert!(!valid_systemd_unit(unit), "{unit:?}");
        }
    }

    #[test]
    fn bounded_subprocess_captures_stdout_and_reports_spawn_failure() {
        // `git` is a hard test prerequisite in this workspace.
        let output = run_bounded("git", &["--version"], true).expect("git --version");
        assert!(output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("git version"),
            "{:?}",
            String::from_utf8_lossy(&output.stdout)
        );

        let error = run_bounded("nuwa-test-no-such-program", &[], false)
            .expect_err("a missing program must be an error");
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn bounded_subprocess_kills_a_hung_child() {
        #[cfg(unix)]
        let (program, args) = ("sleep", &["10"][..]);
        #[cfg(windows)]
        let (program, args) = ("ping", &["-n", "20", "127.0.0.1"][..]);
        let started = Instant::now();
        let error = run_bounded_with(program, args, false, Duration::from_millis(150))
            .expect_err("a slow child must be killed");
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the wait must be bounded, took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn stuck_window_follows_the_runner_interval_and_floor_override() {
        // The documented default floor is 300s.
        assert_eq!(stuck_after_ms(30, STUCK_AFTER_FLOOR_MS), 300_000);
        assert_eq!(stuck_after_ms(600, STUCK_AFTER_FLOOR_MS), 1_800_000);
        // A `NUWA_STUCK_AFTER_SECS` floor raises short-interval windows...
        assert_eq!(stuck_after_ms(30, 600_000), 600_000);
        // ...but a slower owner interval still wins over a lower floor.
        assert_eq!(stuck_after_ms(600, 300_000), 1_800_000);
        // Both ends stay clamped.
        assert_eq!(
            stuck_after_ms(u64::MAX, STUCK_AFTER_FLOOR_MS),
            STUCK_AFTER_CEILING_MS
        );
        assert_eq!(
            stuck_after_ms(30, STUCK_AFTER_CEILING_MS * 2),
            STUCK_AFTER_CEILING_MS
        );
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
