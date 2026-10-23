//! `GET/PUT /api/v1/settings` — schema, masked values and validated
//! write-back for the env file (docs/09 §9.2 settings, §13 config).

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use shanhaijing::{EventRow, RunRow, RunnerRow, Store};

use crate::api::common::{
    configured_units, parse_json_body, restart_lock, schedule_restart, valid_systemd_unit,
};
use crate::api::{ApiError, ErrorBody, ErrorDetail};
use crate::serve::AppState;
use crate::settings::{self, SettingSpec};

/// Network budget for the `POST /settings/test` probes (docs/09 §9.2).
const TEST_TIMEOUT: Duration = Duration::from_secs(10);
const GITHUB_RATE_LIMIT_URL: &str = "https://api.github.com/rate_limit";
const ZAI_DEFAULT_BASE_URL: &str = "https://open.bigmodel.cn/api/coding/paas/v4";
const ZAI_DEFAULT_MODEL: &str = "glm-5.3-flash";
const ANTHROPIC_DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Audit rows scanned for `restart_pending` / `last_changes` /
/// `updated_at_ms`. `__config__` only carries settings events, so this covers
/// years of edits.
const CHANGE_SCAN_LIMIT: u32 = 500;
/// `last_changes` depth (docs/09 §9.2 settings).
const LAST_CHANGES_LIMIT: usize = 20;

/// `config_json` fields a runner reports that map 1:1 to an env key. Secrets
/// never participate (docs/09 §13.2).
const RUNTIME_FIELDS: &[(&str, &str)] = &[
    ("NUWA_INTERVAL", "interval_secs"),
    ("NUWA_CI_TIMEOUT", "ci_timeout_secs"),
    ("NUWA_MAX_TURNS", "max_turns"),
    ("NUWA_MAX_TOKENS", "max_tokens"),
];

#[derive(Debug, Serialize)]
pub struct SettingsResponse {
    pub schema: Vec<SettingDto>,
    pub values: BTreeMap<String, SettingValueDto>,
    pub env_path: String,
    pub env_exists: bool,
    /// `None` when the file is missing; `n/a` on non-Unix hosts.
    pub env_permissions: Option<String>,
    pub needs_migration: bool,
    pub restart_pending: bool,
    pub effective_diff: BTreeMap<String, DiffDto>,
    /// Names only — values of unrecognized keys are never returned.
    pub unknown_keys: Vec<String>,
    pub last_changes: Vec<LastChangeDto>,
}

#[derive(Debug, Serialize)]
pub struct SettingDto {
    pub key: &'static str,
    pub label: &'static str,
    pub group: &'static str,
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub default: Option<&'static str>,
    pub options: &'static [&'static str],
    pub secret: bool,
    pub scope: &'static str,
    pub requires_restart: bool,
    pub read_only: bool,
    pub help: &'static str,
    /// Inclusive numeric bounds for `int` keys; the console echoes them so the
    /// browser can reject an out-of-range value before the PUT (docs/09 §9.2).
    pub min: Option<u64>,
    pub max: Option<u64>,
}

impl From<&SettingSpec> for SettingDto {
    fn from(spec: &SettingSpec) -> Self {
        Self {
            key: spec.key,
            label: spec.label,
            group: spec.group,
            kind: spec.kind,
            default: spec.default,
            options: spec.options,
            secret: spec.secret,
            scope: spec.scope,
            requires_restart: spec.requires_restart,
            read_only: spec.read_only,
            help: spec.help,
            min: spec.min,
            max: spec.max,
        }
    }
}

/// Non-secret keys echo their value; secrets only the presence, last four
/// characters and the newest `config_changed` timestamp.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum SettingValueDto {
    Secret {
        set: bool,
        last4: Option<String>,
        updated_at_ms: i64,
    },
    Plain {
        value: String,
        source: &'static str,
    },
}

#[derive(Debug, Serialize)]
pub struct DiffDto {
    /// File value, or the schema default when the key is absent.
    pub file: String,
    pub runtime: String,
    /// `file` when the compared value comes from the env file, `default` when
    /// the key is absent and the schema default is shown instead.
    pub source: &'static str,
}

#[derive(Debug, Serialize)]
pub struct LastChangeDto {
    pub key: String,
    pub ts_ms: i64,
    pub actor: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct PutSettingsRequest {
    #[serde(default)]
    pub values: BTreeMap<String, Value>,
}

#[derive(Debug, Serialize)]
pub struct PutSettingsResponse {
    pub changed: Vec<String>,
    pub restart_required: bool,
}

#[derive(Debug, Default, Deserialize)]
pub struct TestSettingsRequest {
    #[serde(default)]
    pub target: Option<String>,
    /// Unsaved candidate values; an explicit entry wins over the environment
    /// so the console tests exactly what the operator typed (docs/09 §9.2).
    #[serde(default)]
    pub values: BTreeMap<String, Value>,
}

#[derive(Debug, Serialize)]
pub struct TestSettingsResponse {
    pub ok: bool,
    pub detail: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct RestartRequest {
    #[serde(default)]
    pub force: bool,
}

/// One in-flight run, enough for the console to render the 409 guard dialog.
#[derive(Debug, Serialize)]
struct ActiveRunSummary {
    id: String,
    fingerprint: String,
    runner_id: String,
    repo: String,
    phase: String,
    started_at_ms: i64,
}

/// The documented error envelope plus the `active_runs` list a guarded
/// restart returns (docs/09 §9.2).
#[derive(Serialize)]
struct RestartConflictBody {
    error: ErrorDetail,
    active_runs: Vec<ActiveRunSummary>,
}

/// `GET /api/v1/settings` — schema plus the effective configuration.
pub async fn get(State(state): State<AppState>) -> Response {
    let path = state.env_path.clone();
    let started_at_ms = state.started_at_ms;
    let store = Arc::clone(&state.store);
    match tokio::task::spawn_blocking(move || read_settings(&store, &path, started_at_ms)).await {
        Ok(Ok(body)) => Json(body).into_response(),
        Ok(Err(error)) => {
            eprintln!("settings: {error}");
            internal_error("failed to read settings").into_response()
        }
        Err(error) => {
            eprintln!("settings: blocking task failed: {error}");
            internal_error("failed to read settings").into_response()
        }
    }
}

/// `PUT /api/v1/settings` — validate the whole body, migrate lowercase keys,
/// write atomically and journal one `config_changed` per changed key.
pub async fn put(
    State(state): State<AppState>,
    payload: Result<Json<PutSettingsRequest>, JsonRejection>,
) -> Response {
    let request = match parse_json_body(payload) {
        Ok(request) => request,
        Err(error) => return error.into_response(),
    };
    let path = state.env_path.clone();
    let store = Arc::clone(&state.store);
    let now = crate::now_ms();
    match tokio::task::spawn_blocking(move || write_settings(&store, &path, &request.values, now))
        .await
    {
        Ok(Ok(body)) => Json(body).into_response(),
        Ok(Err(error)) => error.into_response(),
        Err(error) => {
            eprintln!("settings: blocking task failed: {error}");
            internal_error("failed to write settings").into_response()
        }
    }
}

fn internal_error(message: &'static str) -> ApiError {
    ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", message)
}

// ---- POST /settings/test ---------------------------------------------------

/// `POST /api/v1/settings/test` — probe the GitHub token or the LLM endpoint
/// with unsaved candidate values (docs/09 §9.2). Nothing is persisted and no
/// value is ever logged.
pub async fn test(payload: Result<Json<TestSettingsRequest>, JsonRejection>) -> Response {
    let request = match parse_json_body(payload) {
        Ok(request) => request,
        Err(error) => return error.into_response(),
    };
    let target = request.target.as_deref().map(str::trim).unwrap_or("");
    if !matches!(target, "github" | "llm") {
        return field_error("target", "must be github or llm").into_response();
    }
    if let Err(error) = validate_values(&request.values) {
        return error.into_response();
    }
    let result = match target {
        "github" => probe_github(&request.values).await,
        _ => probe_llm(&request.values).await,
    };
    match result {
        Ok(response) => Json(response).into_response(),
        Err(error) => error.into_response(),
    }
}

/// Every `values` key is validated before any probe: unknown keys and
/// non-scalar values are a 422 for the same reason as PUT — silently
/// ignoring a mistyped key or an object payload would make the probe test a
/// different value than the operator typed (docs/09 §9.2).
fn validate_values(values: &BTreeMap<String, Value>) -> Result<(), ApiError> {
    let mut fields = BTreeMap::new();
    for (key, value) in values {
        if settings::setting_spec(key).is_none() {
            fields.insert(key.clone(), "unknown key".to_string());
        } else if value_text(value).is_none() {
            fields.insert(
                key.clone(),
                "must be a string, number or boolean".to_string(),
            );
        }
    }
    if fields.is_empty() {
        Ok(())
    } else {
        Err(ApiError::validation(fields))
    }
}

/// Env-file scalars as text. The console may send typed JSON (a number for an
/// int field, a boolean for a flag), so those are converted; objects, arrays
/// and null have no env representation.
fn value_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        Value::Null | Value::Array(_) | Value::Object(_) => None,
    }
}

fn field_error(field: &str, message: impl Into<String>) -> ApiError {
    let mut fields = BTreeMap::new();
    fields.insert(field.to_string(), message.into());
    ApiError::validation(fields)
}

/// An explicit `values` entry wins over the environment, even when empty: the
/// operator is testing the value on screen, not the running process default.
/// Credentials and the GitHub token use this rule; the LLM model/base/provider
/// fall through on empty instead (see [`resolve_llm_setting`]).
fn provided_value(values: &BTreeMap<String, Value>, key: &str) -> Result<Option<String>, ApiError> {
    match values.get(key) {
        Some(value) => match value_text(value) {
            Some(text) => Ok(Some(text.trim().to_string())),
            None => Err(field_error(key, "must be a string, number or boolean")),
        },
        None => Ok(None),
    }
}

fn env_value(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Candidate value, else process env, else `default`; an explicit empty entry
/// still wins (credentials must never fall back to the environment).
fn resolve_value(
    values: &BTreeMap<String, Value>,
    key: &str,
    default: &str,
) -> Result<String, ApiError> {
    if let Some(value) = provided_value(values, key)? {
        return Ok(value);
    }
    Ok(env_value(key).unwrap_or_else(|| default.to_string()))
}

/// Candidate value, else `env`, else `default`, where an empty candidate means
/// "unset". The runtime filters empty `NUWA_PROVIDER` / `NUWA_MODEL` /
/// `NUWA_BASE_URL` (`main.rs::env_d` / `env_provider_default`), so the console
/// must fall through to the provider default instead of rejecting a 422.
/// `env` is injected so the rule is testable without touching the process
/// environment.
fn resolve_llm_setting(
    values: &BTreeMap<String, Value>,
    key: &str,
    env: Option<String>,
    default: &str,
) -> Result<String, ApiError> {
    let provided = provided_value(values, key)?.filter(|value| !value.is_empty());
    Ok(provided
        .or_else(|| env.filter(|value| !value.is_empty()))
        .unwrap_or_else(|| default.to_string()))
}

fn test_http_client() -> Result<reqwest::Client, ApiError> {
    reqwest::Client::builder()
        .timeout(TEST_TIMEOUT)
        .build()
        .map_err(|error| ApiError::upstream(format!("HTTP client setup failed: {error}")))
}

/// Send one probe with a hard timeout; non-2xx is a 502, never a body echo
/// (provider error payloads can quote the request).
async fn send_probe(service: &str, request: reqwest::RequestBuilder) -> Result<Value, ApiError> {
    let response = match tokio::time::timeout(TEST_TIMEOUT, request.send()).await {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => {
            return Err(ApiError::upstream(format!(
                "{service} is unreachable: {error}"
            )))
        }
        Err(_) => return Err(ApiError::upstream(format!("{service} check timed out"))),
    };
    let status = response.status();
    if !status.is_success() {
        return Err(ApiError::upstream(format!(
            "{service} request failed ({status})"
        )));
    }
    Ok(response.json().await.unwrap_or_default())
}

/// `GET /rate_limit` proves the token itself; it spends no API quota.
async fn probe_github(values: &BTreeMap<String, Value>) -> Result<TestSettingsResponse, ApiError> {
    let token = match provided_value(values, "NUWA_GITHUB_TOKEN")? {
        Some(token) => token,
        None => env_value("NUWA_GITHUB_TOKEN")
            .or_else(|| env_value("GITHUB_TOKEN"))
            .unwrap_or_default(),
    };
    if token.is_empty() {
        return Err(field_error("NUWA_GITHUB_TOKEN", "token is required"));
    }
    let request = test_http_client()?
        .get(GITHUB_RATE_LIMIT_URL)
        .bearer_auth(&token)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "nuwa");
    let payload = send_probe("GitHub", request).await?;
    let rate = payload.get("rate");
    let remaining = rate
        .and_then(|rate| rate.get("remaining"))
        .and_then(Value::as_u64);
    let limit = rate
        .and_then(|rate| rate.get("limit"))
        .and_then(Value::as_u64);
    let detail = match (remaining, limit) {
        (Some(remaining), Some(limit)) => {
            format!("token accepted; {remaining}/{limit} requests remaining")
        }
        _ => "token accepted".to_string(),
    };
    Ok(TestSettingsResponse { ok: true, detail })
}

async fn probe_llm(values: &BTreeMap<String, Value>) -> Result<TestSettingsResponse, ApiError> {
    let provider = resolve_llm_setting(
        values,
        "NUWA_PROVIDER",
        env_value("NUWA_PROVIDER"),
        "anthropic",
    )?;
    match provider.as_str() {
        "faux" => Ok(TestSettingsResponse {
            ok: true,
            detail: "faux provider needs no network".to_string(),
        }),
        "zai" => probe_zai(values).await,
        "anthropic" => probe_anthropic(values).await,
        other => Err(field_error(
            "NUWA_PROVIDER",
            format!("must be one of zai|anthropic|faux, got {other:?}"),
        )),
    }
}

fn llm_key(values: &BTreeMap<String, Value>, key: &str) -> Result<String, ApiError> {
    let value = resolve_value(values, key, "")?;
    if value.is_empty() {
        return Err(field_error(key, "API key is required"));
    }
    Ok(value)
}

fn llm_model(values: &BTreeMap<String, Value>, default: &str) -> Result<String, ApiError> {
    resolve_llm_setting(values, "NUWA_MODEL", env_value("NUWA_MODEL"), default)
}

fn llm_base(values: &BTreeMap<String, Value>, default: &str) -> Result<String, ApiError> {
    let base = resolve_llm_setting(values, "NUWA_BASE_URL", env_value("NUWA_BASE_URL"), default)?;
    if !valid_http_url(&base) {
        return Err(field_error("NUWA_BASE_URL", "must be an http(s) URL"));
    }
    Ok(base.trim_end_matches('/').to_string())
}

/// The credential a probe sends, and the host it may be sent to.
///
/// The running process's key may only travel to the base URL the process is
/// configured with. As soon as the request names a different host, the key
/// has to be supplied on screen — otherwise a console user could point the
/// probe at their own host and read the running key off the wire.
fn probe_credential(
    values: &BTreeMap<String, Value>,
    key: &str,
    requested_base: Option<&str>,
    process_base: &str,
) -> Result<String, ApiError> {
    if let Some(value) = provided_value(values, key)?.filter(|value| !value.is_empty()) {
        return Ok(value);
    }
    let process_base = process_base.trim_end_matches('/');
    if let Some(requested) = requested_base {
        if requested.trim_end_matches('/') != process_base {
            return Err(field_error(
                key,
                "required when probing another base URL — the running process's key is never sent to a different host",
            ));
        }
    }
    let value = env_value(key).unwrap_or_default();
    if value.is_empty() {
        return Err(field_error(key, "API key is required"));
    }
    Ok(value)
}

/// Resolve the anthropic probe target exactly like `repair::run`: the public
/// API host is fixed (the runtime ignores `NUWA_BASE_URL` for anthropic), and
/// the model must come from the vendored catalog — an absent or unknown id
/// falls back to the catalog's first entry.
fn resolve_anthropic_probe(
    values: &BTreeMap<String, Value>,
    env_model: Option<String>,
) -> Result<(&'static str, String), ApiError> {
    let requested = resolve_llm_setting(values, "NUWA_MODEL", env_model, "")?;
    let catalog = rpi_ai::providers::anthropic::models::anthropic_models();
    let model = catalog
        .iter()
        .find(|model| model.id == requested)
        .or_else(|| catalog.first())
        .map(|model| model.id.clone())
        .unwrap_or_else(|| "claude-opus-4-8".to_string());
    Ok((ANTHROPIC_DEFAULT_BASE_URL, model))
}

/// Minimal non-streaming completion (`max_tokens = 1`) — the cheapest proof
/// the key, base URL and model work together (docs/09 §9.2).
async fn probe_zai(values: &BTreeMap<String, Value>) -> Result<TestSettingsResponse, ApiError> {
    let model = llm_model(values, ZAI_DEFAULT_MODEL)?;
    let base = llm_base(values, ZAI_DEFAULT_BASE_URL)?;
    let process_base =
        env_value("NUWA_BASE_URL").unwrap_or_else(|| ZAI_DEFAULT_BASE_URL.to_string());
    let requested_base = provided_value(values, "NUWA_BASE_URL")?
        .map(|value| value.trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty());
    let key = probe_credential(
        values,
        "ZAI_API_KEY",
        requested_base.as_deref(),
        &process_base,
    )?;
    let request = test_http_client()?
        .post(format!("{base}/chat/completions"))
        .bearer_auth(key)
        .json(&serde_json::json!({
            "model": model,
            "max_tokens": 1,
            "stream": false,
            "messages": [{ "role": "user", "content": "ping" }],
        }));
    send_probe("LLM (zai)", request).await?;
    Ok(TestSettingsResponse {
        ok: true,
        detail: format!("zai model {model} responded"),
    })
}

async fn probe_anthropic(
    values: &BTreeMap<String, Value>,
) -> Result<TestSettingsResponse, ApiError> {
    let key = llm_key(values, "ANTHROPIC_API_KEY")?;
    let (base, model) = resolve_anthropic_probe(values, env_value("NUWA_MODEL"))?;
    let request = test_http_client()?
        .post(format!("{base}/v1/messages"))
        .header("x-api-key", key)
        .header("anthropic-version", ANTHROPIC_VERSION)
        .json(&serde_json::json!({
            "model": model,
            "max_tokens": 1,
            "stream": false,
            "messages": [{ "role": "user", "content": "ping" }],
        }));
    send_probe("LLM (anthropic)", request).await?;
    Ok(TestSettingsResponse {
        ok: true,
        detail: format!("anthropic model {model} responded"),
    })
}

// ---- POST /settings/restart ------------------------------------------------

/// `POST /api/v1/settings/restart` — schedule `systemctl restart` for the
/// serve unit plus every runner unit (docs/09 §9.2, §13.3). In-flight runs
/// need `force`; invalid units never reach systemd.
pub async fn restart(
    State(state): State<AppState>,
    payload: Result<Json<RestartRequest>, JsonRejection>,
) -> Response {
    let request = match parse_json_body(payload) {
        Ok(request) => request,
        Err(error) => return error.into_response(),
    };
    // One restart schedule at a time, across both restart endpoints. The
    // guard lives to the end of this handler, i.e. until `systemd-run` has
    // returned; a concurrent request is a 409 before any audit row or
    // subprocess (docs/09 §9.2).
    let Ok(_restart) = restart_lock().try_lock() else {
        return ApiError::conflict("a restart is already being scheduled").into_response();
    };
    let store = Arc::clone(&state.store);
    let active = match tokio::task::spawn_blocking(move || store.active_runs()).await {
        Ok(Ok(runs)) => runs,
        Ok(Err(error)) => {
            eprintln!("settings restart: active run lookup failed: {error}");
            return internal_error("failed to read active runs").into_response();
        }
        Err(error) => {
            eprintln!("settings restart: blocking task failed: {error}");
            return internal_error("failed to read active runs").into_response();
        }
    };
    let active_runs = active_run_summaries(&active);
    if !request.force && !active_runs.is_empty() {
        let error = ErrorBody::from(ApiError::conflict(format!(
            "{} active run(s) in flight; retry with force=true",
            active_runs.len()
        )))
        .error;
        return (
            StatusCode::CONFLICT,
            Json(RestartConflictBody { error, active_runs }),
        )
            .into_response();
    }

    let units = configured_units(&state.serve_unit, &state.runner_units);
    if units.is_empty() {
        return ApiError::not_supported(
            "no systemd units configured; restart the services manually",
        )
        .into_response();
    }
    if let Some(unit) = units.iter().find(|unit| !valid_systemd_unit(unit)) {
        let mut fields = BTreeMap::new();
        fields.insert(
            "units".to_string(),
            format!("invalid systemd unit {unit:?}"),
        );
        return ApiError::validation(fields).into_response();
    }

    let forced = request.force && !active.is_empty();
    let detail = if forced {
        format!("{} (forced)", units.join(","))
    } else {
        units.join(",")
    };
    // Audit first: a slow database write must not consume the 2-second timer
    // window and cut off the 202. A failed audit is logged and never blocks
    // the restart, matching the settings PUT policy.
    let scheduled_units = units.clone();
    let store = Arc::clone(&state.store);
    let now = crate::now_ms();
    let scheduled = tokio::task::spawn_blocking(move || {
        audit_then_schedule(&store, now, &detail, || schedule_restart(&scheduled_units))
    })
    .await;
    match scheduled {
        Ok(true) => StatusCode::ACCEPTED.into_response(),
        Ok(false) => ApiError::not_supported(restart_unavailable_message(&units)).into_response(),
        Err(error) => {
            eprintln!("settings restart: blocking task failed: {error}");
            internal_error("failed to schedule restart").into_response()
        }
    }
}

/// Write `restart_requested` BEFORE scheduling `systemd-run` (docs/09 §9.2).
/// `schedule` reports whether systemd-run accepted the timer; the audit stays
/// even when it did not, because the attempt happened.
fn audit_then_schedule(
    store: &Store,
    now: i64,
    detail: &str,
    schedule: impl FnOnce() -> bool,
) -> bool {
    if let Err(error) = store.record_event(
        "__system__",
        now,
        None,
        "owner",
        "restart_requested",
        detail,
    ) {
        eprintln!("settings restart: audit failed: {error}");
    }
    schedule()
}

/// Manual fallback shown by the systemd-run 501 (docs/09 §9.2).
fn restart_unavailable_message(units: &[String]) -> String {
    format!(
        "systemd-run is unavailable; restart the units manually: systemctl restart {}",
        units.join(" ")
    )
}

fn active_run_summaries(runs: &[RunRow]) -> Vec<ActiveRunSummary> {
    runs.iter()
        .map(|run| ActiveRunSummary {
            id: run.id.clone(),
            fingerprint: run.fingerprint.clone(),
            runner_id: run.runner_id.clone(),
            repo: run.repo.clone(),
            phase: run.phase.clone(),
            started_at_ms: run.started_at,
        })
        .collect()
}

fn read_settings(
    store: &Store,
    path: &Path,
    started_at_ms: i64,
) -> Result<SettingsResponse, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => Some(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("read {}: {error}", path.display())),
    };
    let env_exists = text.is_some();
    let (map, migrated) =
        settings::normalize_known_keys(&settings::parse_env(text.as_deref().unwrap_or("")));

    let events = store
        .events_for("__config__", CHANGE_SCAN_LIMIT)
        .map_err(|error| format!("events: {error}"))?;
    let mut changes: Vec<&EventRow> = events
        .iter()
        .filter(|event| event.kind == "config_changed")
        .collect();
    changes.sort_by_key(|a| std::cmp::Reverse(a.ts));

    let mut values = BTreeMap::new();
    // Values plus the diff source label (`file` / `default`) for
    // `effective_diff`.
    let mut plain: BTreeMap<String, (String, &'static str)> = BTreeMap::new();
    for spec in settings::SETTINGS_SCHEMA {
        if spec.secret {
            let raw = map.get(spec.key).map(String::as_str).unwrap_or("");
            let updated_at_ms = changes
                .iter()
                .find(|event| event.detail == spec.key)
                .map(|event| event.ts)
                .unwrap_or(0);
            values.insert(
                spec.key.to_string(),
                SettingValueDto::Secret {
                    set: !raw.is_empty(),
                    last4: last_four(raw),
                    updated_at_ms,
                },
            );
        } else {
            let (value, source) = if spec.key == "NUWA_ENV_FILE" {
                // The effective path may come from the process environment,
                // not from a line in the file itself.
                (path.display().to_string(), "env")
            } else if let Some(value) = map.get(spec.key) {
                (value.clone(), "env")
            } else {
                (spec.default.unwrap_or("").to_string(), "default")
            };
            let diff_source = if source == "default" {
                "default"
            } else {
                "file"
            };
            plain.insert(spec.key.to_string(), (value.clone(), diff_source));
            values.insert(
                spec.key.to_string(),
                SettingValueDto::Plain { value, source },
            );
        }
    }

    let unknown_keys = map
        .keys()
        .filter(|key| settings::setting_spec(key).is_none())
        .cloned()
        .collect();
    let runners = store
        .list_runners()
        .map_err(|error| format!("runners: {error}"))?;

    Ok(SettingsResponse {
        schema: settings::SETTINGS_SCHEMA
            .iter()
            .map(SettingDto::from)
            .collect(),
        values,
        env_path: path.display().to_string(),
        env_exists,
        env_permissions: env_permissions(path),
        needs_migration: !migrated.is_empty(),
        restart_pending: changes
            .first()
            .is_some_and(|event| event.ts > started_at_ms),
        effective_diff: effective_diff(&runners, &plain),
        unknown_keys,
        last_changes: changes
            .iter()
            .take(LAST_CHANGES_LIMIT)
            .map(|event| LastChangeDto {
                key: event.detail.clone(),
                ts_ms: event.ts,
                actor: event.actor.clone(),
            })
            .collect(),
    })
}

/// Serializes the whole settings read-modify-write. Two concurrent PUTs that
/// each read the old file and write back their own change would otherwise lose
/// one update (last writer wins) and race on the temp file. A blocking mutex is
/// fine here: `write_settings` already runs on the blocking pool.
static SETTINGS_WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn write_settings(
    store: &Store,
    path: &Path,
    updates: &BTreeMap<String, Value>,
    now: i64,
) -> Result<PutSettingsResponse, ApiError> {
    // Hold the lock across read → validate → backup → write → audit so the file
    // is never a torn merge of two requests. A poisoned lock (a prior panic)
    // must not wedge settings forever.
    let _write_guard = SETTINGS_WRITE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            eprintln!("settings: read {}: {error}", path.display());
            return Err(internal_error("failed to write settings"));
        }
    };
    let (map, migrated) = settings::normalize_known_keys(&settings::parse_env(&text));
    let (new_map, changed) = validate_updates(&map, updates).map_err(ApiError::validation)?;

    // Write only the lines that actually change (plus lowercase keys being
    // migrated), so comments, order, unknown keys and untouched values stay
    // byte for byte (docs/09 §13.2).
    let mut write_map: BTreeMap<String, String> = BTreeMap::new();
    for key in &migrated {
        if let Some(value) = map.get(key) {
            write_map.insert(key.clone(), value.clone());
        }
    }
    for key in &changed {
        if let Some(value) = new_map.get(key) {
            write_map.insert(key.clone(), value.clone());
        }
    }

    if !write_map.is_empty() {
        // Validate everything first: an invalid field never touches the file.
        // A `.bak` also covers an ordinary PUT, not just migration.
        settings::backup_env_file(path).map_err(|error| {
            eprintln!("settings: {error}");
            internal_error("failed to write settings")
        })?;
        let updated = settings::update_env_text(&text, &write_map);
        settings::write_env_atomic(path, &updated).map_err(|error| {
            eprintln!("settings: write failed: {error}");
            internal_error("failed to write settings")
        })?;
    }

    if !migrated.is_empty() {
        // One audit row for the lowercase -> uppercase rewrite (detail = key
        // list, docs/09 §13.2), alongside the per-key config_changed rows.
        if let Err(error) = store.record_event(
            "__config__",
            now,
            None,
            "owner",
            "config_migrated",
            &migrated.join(","),
        ) {
            eprintln!("settings: migration audit failed: {error}");
        }
    }
    for key in &changed {
        // The file is the source of truth; a failed audit row must not turn a
        // successful save into an error.
        if let Err(error) =
            store.record_event("__config__", now, None, "owner", "config_changed", key)
        {
            eprintln!("settings: audit failed for {key}: {error}");
        }
    }

    let restart_required = changed
        .iter()
        .any(|key| settings::setting_spec(key).is_some_and(|spec| spec.requires_restart));
    Ok(PutSettingsResponse {
        changed,
        restart_required,
    })
}

/// New map + changed keys, or the 422 field map.
type ValidatedUpdates = Result<(BTreeMap<String, String>, Vec<String>), BTreeMap<String, String>>;

/// Validate the whole write before anything is persisted. Returns the new map
/// and the changed keys, or a 422 field map.
fn validate_updates(
    old: &BTreeMap<String, String>,
    updates: &BTreeMap<String, Value>,
) -> ValidatedUpdates {
    let mut new = old.clone();
    let mut changed = Vec::new();
    let mut fields = BTreeMap::new();

    for (key, raw) in updates {
        let Some(spec) = settings::setting_spec(key) else {
            fields.insert(key.clone(), "unknown key".to_string());
            continue;
        };
        if spec.read_only {
            fields.insert(key.clone(), "read-only key".to_string());
            continue;
        }
        let raw_text = match raw {
            Value::String(text) => text.clone(),
            Value::Number(number) => number.to_string(),
            _ => {
                fields.insert(key.clone(), "must be a string".to_string());
                continue;
            }
        };
        // `KEY=VALUE\n` cannot carry raw line breaks or control characters:
        // they would inject extra keys and bypass the changed/audit contract.
        if !raw_text.is_empty() && raw_text.chars().any(|c| c.is_ascii_control()) {
            fields.insert(
                key.clone(),
                "must not contain line breaks or control characters".to_string(),
            );
            continue;
        }
        // systemd `EnvironmentFile=` strips surrounding quotes and processes
        // backslash escapes, while nuwa's own `parse_env` reads the value raw.
        // A value containing `"` or `\` would therefore load differently at
        // runtime than it validates/displays here — e.g. a UI token saved as
        // `"alpha"` becomes `alpha` under systemd, locking the owner out. No
        // legitimate value for these keys (tokens, URLs, numbers, identifiers,
        // unit names) needs either character, so reject rather than corrupt.
        if raw_text.contains('"') || raw_text.contains('\\') {
            fields.insert(
                key.clone(),
                "must not contain quote or backslash characters".to_string(),
            );
            continue;
        }
        let value = raw_text.trim();

        // An empty secret keeps the current value (docs/09 §9.2 settings).
        if spec.secret {
            if value.is_empty() {
                continue;
            }
            if old.get(key).map(String::as_str) != Some(value) {
                changed.push(key.clone());
            }
            new.insert(key.clone(), value.to_string());
            continue;
        }

        let current = old
            .get(key)
            .map(String::as_str)
            .or(spec.default)
            .unwrap_or("");
        match validate_value(spec, value) {
            Ok(value) if value != current => {
                changed.push(key.clone());
                new.insert(key.clone(), value);
            }
            Ok(_) => {}
            Err(message) => {
                fields.insert(key.clone(), message);
            }
        }
    }

    if fields.is_empty() {
        Ok((new, changed))
    } else {
        Err(fields)
    }
}

fn validate_value(spec: &SettingSpec, value: &str) -> Result<String, String> {
    match spec.kind {
        "int" => {
            let parsed: u64 = value.parse().map_err(|_| int_message(spec))?;
            let (min, max) = (spec.min.unwrap_or(0), spec.max.unwrap_or(u64::MAX));
            if parsed < min || parsed > max {
                return Err(int_message(spec));
            }
            Ok(parsed.to_string())
        }
        "enum" => {
            if spec.options.contains(&value) {
                Ok(value.to_string())
            } else {
                Err(format!("must be one of {}", spec.options.join("|")))
            }
        }
        "url" => {
            if value.is_empty() || valid_http_url(value) {
                Ok(value.to_string())
            } else {
                Err("must be an http(s) URL".to_string())
            }
        }
        "csv" => validate_runner_units(value),
        "string" => validate_string_value(spec.key, value),
        _ => Err("this key cannot be written".to_string()),
    }
}

fn int_message(spec: &SettingSpec) -> String {
    match (spec.min, spec.max) {
        (Some(min), Some(max)) => format!("must be an integer between {min} and {max}"),
        _ => "must be a positive integer".to_string(),
    }
}

fn valid_http_url(value: &str) -> bool {
    if value.chars().any(char::is_whitespace) {
        return false;
    }
    match reqwest::Url::parse(value) {
        Ok(url) => matches!(url.scheme(), "http" | "https") && url.host_str().is_some(),
        Err(_) => false,
    }
}

fn validate_runner_units(value: &str) -> Result<String, String> {
    if value.is_empty() {
        return Ok(String::new());
    }
    let units: Vec<&str> = value
        .split(',')
        .map(str::trim)
        .filter(|unit| !unit.is_empty())
        .collect();
    if units.is_empty() {
        return Ok(String::new());
    }
    if let Some(unit) = units.iter().find(|unit| !valid_systemd_unit(unit)) {
        return Err(format!("invalid systemd unit {unit:?}"));
    }
    Ok(units.join(","))
}

fn validate_string_value(key: &str, value: &str) -> Result<String, String> {
    match key {
        "NUWA_RUNNER_ID" if !value.is_empty() => {
            let valid = value.chars().count() <= 128
                && value
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'));
            if valid {
                Ok(value.to_string())
            } else {
                Err("must match [A-Za-z0-9._:-] and be at most 128 characters".to_string())
            }
        }
        "NUWA_SERVICE_UNIT" | "NUWA_SERVE_UNIT" if !value.is_empty() => {
            if valid_systemd_unit(value) {
                Ok(value.to_string())
            } else {
                Err("must be a systemd unit (letters, digits, @ . _ : -)".to_string())
            }
        }
        _ => Ok(value.to_string()),
    }
}

fn last_four(value: &str) -> Option<String> {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= 4 {
        // Echoing a short secret in full would defeat the masking.
        return None;
    }
    Some(chars[chars.len() - 4..].iter().collect())
}

fn env_permissions(path: &Path) -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .ok()
            .map(|metadata| format!("{:o}", metadata.permissions().mode() & 0o777))
    }
    #[cfg(not(unix))]
    {
        std::fs::metadata(path).ok().map(|_| "n/a".to_string())
    }
}

/// Compare the latest runner-reported `config_json` against the file values.
/// Runners are considered newest heartbeat first: the freshest runtime value
/// wins, and a key already listed is not repeated by an older runner.
fn effective_diff(
    runners: &[RunnerRow],
    values: &BTreeMap<String, (String, &'static str)>,
) -> BTreeMap<String, DiffDto> {
    let mut diff = BTreeMap::new();
    let mut runners: Vec<&RunnerRow> = runners.iter().collect();
    runners.sort_by(|a, b| {
        b.heartbeat_at
            .cmp(&a.heartbeat_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    for runner in runners {
        let Ok(config) = serde_json::from_str::<Value>(&runner.config_json) else {
            continue;
        };
        for (key, field) in RUNTIME_FIELDS {
            if diff.contains_key(*key) {
                continue;
            }
            let Some(runtime) = config.get(*field).and_then(Value::as_u64) else {
                continue;
            };
            let Some((file, source)) = values.get(*key) else {
                continue;
            };
            let differs = match file.trim().parse::<u64>() {
                Ok(number) => number != runtime,
                Err(_) => file != &runtime.to_string(),
            };
            if differs {
                diff.insert(
                    (*key).to_string(),
                    DiffDto {
                        file: file.clone(),
                        runtime: runtime.to_string(),
                        source,
                    },
                );
            }
        }
    }
    diff
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::PathBuf;
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{header, Request, StatusCode};
    use axum::response::Response;
    use axum::Router;
    use serde_json::Value;
    use shanhaijing::{ReportOutcome, RunStart, RunnerUpsert, Signal, Store};
    use tower::ServiceExt;

    use super::{
        audit_then_schedule, last_four, llm_key, probe_credential, resolve_anthropic_probe,
        resolve_llm_setting, restart_unavailable_message,
    };
    use crate::api::common::restart_test_guard;
    use crate::serve::{build_app, AppState};

    /// docs/09 §13.1, in table order.
    const CANONICAL_KEYS: &[&str] = &[
        "NUWA_GITHUB_TOKEN",
        "ZAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "NUWA_INGEST_TOKEN",
        "NUWA_UI_TOKEN",
        "NUWA_WEBHOOK_URL",
        "NUWA_WEBHOOK_SECRET",
        "NUWA_WEBHOOK_EVENTS",
        "NUWA_PROVIDER",
        "NUWA_MODEL",
        "NUWA_BASE_URL",
        "NUWA_PUBLIC_BASE_URL",
        "NUWA_INTERVAL",
        "NUWA_CI_TIMEOUT",
        "NUWA_SYNC_INTERVAL",
        "NUWA_DAILY_PR_LIMIT",
        "NUWA_MAX_TURNS",
        "NUWA_MAX_TOKENS",
        "NUWA_DAILY_TOKEN_LIMIT",
        "NUWA_MONTHLY_TOKEN_LIMIT",
        "NUWA_RUNNER_ID",
        "NUWA_SERVICE_UNIT",
        "NUWA_SERVE_UNIT",
        "NUWA_RUNNER_UNITS",
        "NUWA_COOKIE_SECURE",
        "NUWA_SESSION_TTL_SECS",
        "NUWA_STUCK_AFTER_SECS",
        "NUWA_EVENT_RETENTION_DAYS",
        "NUWA_ENV_FILE",
    ];

    fn test_app_with(
        env_text: Option<&str>,
        started_at_ms: i64,
    ) -> (tempfile::TempDir, Router, Arc<Store>, PathBuf) {
        test_app_custom(env_text, started_at_ms, "nuwa-serve", &["nuwa-work"])
    }

    fn test_app_custom(
        env_text: Option<&str>,
        started_at_ms: i64,
        serve_unit: &str,
        runner_units: &[&str],
    ) -> (tempfile::TempDir, Router, Arc<Store>, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let env_path = dir.path().join("nuwa.env");
        if let Some(text) = env_text {
            std::fs::write(&env_path, text).expect("write env");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&env_path, std::fs::Permissions::from_mode(0o600))
                    .expect("chmod");
            }
        }
        let db_path = dir.path().join("s.db");
        let store = Arc::new(Store::open(&db_path).expect("open"));
        let state = AppState {
            store: Arc::clone(&store),
            ui_token: Arc::new(None),
            ingest_token: Arc::new(None),
            session: Arc::new(crate::session::SessionManager::new("", 86_400_000)),
            limiter: Arc::new(crate::session::LoginLimiter::new()),
            started_at_ms,
            env_path: env_path.clone(),
            db_path,
            cookie_secure: false,
            cookie_secure_never: false,
            serve_unit: serve_unit.to_string(),
            runner_units: runner_units
                .iter()
                .map(|unit| (*unit).to_string())
                .collect(),
        };
        let app = build_app(state);
        (dir, app, store, env_path)
    }

    fn test_app(env_text: Option<&str>) -> (tempfile::TempDir, Router, Arc<Store>, PathBuf) {
        test_app_with(env_text, crate::now_ms())
    }

    async fn get(app: &Router, uri: &str) -> Response {
        app.clone()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response")
    }

    fn set_cookie_value(response: &Response, name: &str) -> String {
        for value in response.headers().get_all(header::SET_COOKIE) {
            let raw = value.to_str().expect("ascii Set-Cookie");
            if let Some(rest) = raw.strip_prefix(&format!("{name}=")) {
                return rest.split(';').next().expect("cookie value").to_string();
            }
        }
        panic!("missing {name} Set-Cookie");
    }

    /// Local mode is implicitly authenticated, so `GET /auth/me` issues the
    /// readable CSRF cookie without a login round-trip.
    async fn csrf_token(app: &Router) -> String {
        let response = get(app, "/api/v1/auth/me").await;
        assert_eq!(response.status(), StatusCode::OK);
        set_cookie_value(&response, "nuwa_csrf")
    }

    async fn put_json(app: &Router, body: &str, csrf: Option<&str>) -> Response {
        let mut request = Request::builder()
            .method("PUT")
            .uri("/api/v1/settings")
            .header(header::HOST, "localhost:7321")
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(token) = csrf {
            request = request
                .header(header::COOKIE, format!("nuwa_csrf={token}"))
                .header("x-nuwa-csrf", token);
        }
        app.clone()
            .oneshot(request.body(Body::from(body.to_string())).expect("request"))
            .await
            .expect("response")
    }

    async fn post_json(app: &Router, uri: &str, csrf: Option<&str>, body: &str) -> Response {
        let mut request = Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::HOST, "localhost:7321")
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(token) = csrf {
            request = request
                .header(header::COOKIE, format!("nuwa_csrf={token}"))
                .header("x-nuwa-csrf", token);
        }
        app.clone()
            .oneshot(request.body(Body::from(body.to_string())).expect("request"))
            .await
            .expect("response")
    }

    async fn json_body(response: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        serde_json::from_slice(&bytes).expect("json body")
    }

    #[test]
    fn schema_covers_docs_keys_with_metadata() {
        let schema = crate::settings::SETTINGS_SCHEMA;
        let keys: Vec<&str> = schema.iter().map(|spec| spec.key).collect();
        assert_eq!(keys, CANONICAL_KEYS);
        assert_eq!(
            keys,
            crate::settings::ENV_KEYS.to_vec(),
            "ENV_KEYS and the API schema must not drift"
        );
        for spec in schema {
            assert!(!spec.label.is_empty(), "{}", spec.key);
            assert!(!spec.help.is_empty(), "{}", spec.key);
            assert!(
                matches!(
                    spec.group,
                    "credentials"
                        | "model"
                        | "runner"
                        | "service"
                        | "security"
                        | "ops"
                        | "advanced"
                ),
                "{}: {}",
                spec.key,
                spec.group
            );
            assert!(
                matches!(
                    spec.kind,
                    "secret" | "string" | "int" | "enum" | "url" | "csv" | "path"
                ),
                "{}: {}",
                spec.key,
                spec.kind
            );
            assert!(
                matches!(spec.scope, "both" | "work" | "serve"),
                "{}: {}",
                spec.key,
                spec.scope
            );
            assert!(spec.requires_restart, "{}", spec.key);
            assert_eq!(
                spec.secret,
                crate::settings::SECRET_KEYS.contains(&spec.key),
                "{}: secret flag must match SECRET_KEYS",
                spec.key
            );
            if spec.kind == "enum" {
                assert!(!spec.options.is_empty(), "{}", spec.key);
            }
            if spec.secret {
                assert!(spec.default.is_none(), "{}", spec.key);
            }
            if spec.kind == "int" {
                assert!(spec.min.is_some() && spec.max.is_some(), "{}", spec.key);
            }
        }
        let read_only: Vec<&str> = schema
            .iter()
            .filter(|spec| spec.read_only)
            .map(|spec| spec.key)
            .collect();
        assert_eq!(read_only, vec!["NUWA_ENV_FILE"]);
    }

    #[tokio::test]
    async fn schema_exposes_numeric_bounds() {
        let (_dir, app, _store, _env_path) = test_app(None);
        let value = json_body(get(&app, "/api/v1/settings").await).await;
        let schema = value["schema"].as_array().expect("schema");
        let interval = schema
            .iter()
            .find(|entry| entry["key"] == "NUWA_INTERVAL")
            .expect("NUWA_INTERVAL");
        assert_eq!(interval["min"], 1);
        assert_eq!(interval["max"], 86_400);
        let github = schema
            .iter()
            .find(|entry| entry["key"] == "NUWA_GITHUB_TOKEN")
            .expect("github");
        assert!(github["min"].is_null());
        assert!(github["max"].is_null());
    }

    #[tokio::test]
    async fn get_returns_schema_masked_values_and_metadata() {
        let file = "# managed by test\n\
                    NUWA_GITHUB_TOKEN=ghp_secretvalue1234\n\
                    NUWA_INGEST_TOKEN=abcd\n\
                    nuwa_interval=45\n\
                    NUWA_MODEL=custom-model\n\
                    LEGACY_KEY=keep-me\n";
        let (_dir, app, _store, env_path) = test_app(Some(file));

        let response = get(&app, "/api/v1/settings").await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "GET /api/v1/settings must be wired"
        );
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|v| v.to_str().ok()),
            Some("no-store")
        );
        let value = json_body(response).await;

        let schema = value["schema"].as_array().expect("schema array");
        let keys: Vec<&str> = schema
            .iter()
            .map(|spec| spec["key"].as_str().expect("key"))
            .collect();
        assert_eq!(keys, CANONICAL_KEYS);
        let provider = schema
            .iter()
            .find(|spec| spec["key"] == "NUWA_PROVIDER")
            .expect("provider schema");
        assert_eq!(provider["label"], "LLM Provider");
        assert_eq!(provider["group"], "model");
        assert_eq!(provider["type"], "enum");
        assert_eq!(provider["default"], "anthropic");
        assert_eq!(
            provider["options"],
            serde_json::json!(["zai", "anthropic", "faux"])
        );
        assert_eq!(provider["secret"], false);
        assert_eq!(provider["scope"], "work");
        assert_eq!(provider["requires_restart"], true);
        assert_eq!(provider["read_only"], false);
        assert!(provider["help"].is_string());
        let env_file = schema
            .iter()
            .find(|spec| spec["key"] == "NUWA_ENV_FILE")
            .expect("env file schema");
        assert_eq!(env_file["type"], "path");
        assert_eq!(env_file["read_only"], true);
        assert_eq!(env_file["scope"], "both");

        let github = &value["values"]["NUWA_GITHUB_TOKEN"];
        assert_eq!(github["set"], true);
        assert_eq!(github["last4"], "1234");
        assert_eq!(github["updated_at_ms"], 0);
        assert!(
            github.get("value").is_none(),
            "secrets must not echo the value: {github}"
        );
        let anthropic = &value["values"]["ANTHROPIC_API_KEY"];
        assert_eq!(anthropic["set"], false);
        assert!(anthropic["last4"].is_null());
        let short = &value["values"]["NUWA_INGEST_TOKEN"];
        assert_eq!(short["set"], true);
        assert!(
            short["last4"].is_null(),
            "a secret of four characters or fewer must not echo its tail: {short}"
        );

        assert_eq!(value["values"]["NUWA_INTERVAL"]["value"], "45");
        assert_eq!(value["values"]["NUWA_INTERVAL"]["source"], "env");
        assert_eq!(value["values"]["NUWA_PROVIDER"]["value"], "anthropic");
        assert_eq!(value["values"]["NUWA_PROVIDER"]["source"], "default");
        assert_eq!(value["values"]["NUWA_DAILY_PR_LIMIT"]["value"], "5");
        assert_eq!(value["values"]["NUWA_DAILY_PR_LIMIT"]["source"], "default");
        assert_eq!(value["values"]["NUWA_MODEL"]["value"], "custom-model");
        assert_eq!(
            value["values"]["NUWA_ENV_FILE"]["value"],
            env_path.display().to_string()
        );

        assert_eq!(value["env_path"], env_path.display().to_string());
        assert_eq!(value["env_exists"], true);
        #[cfg(unix)]
        assert_eq!(value["env_permissions"], "600");
        #[cfg(not(unix))]
        assert_eq!(value["env_permissions"], "n/a");
        assert_eq!(value["needs_migration"], true);
        assert_eq!(value["unknown_keys"], serde_json::json!(["LEGACY_KEY"]));
        assert_eq!(value["restart_pending"], false);
        assert!(value["effective_diff"]
            .as_object()
            .expect("diff object")
            .is_empty());
        assert!(value["last_changes"]
            .as_array()
            .expect("changes array")
            .is_empty());

        let raw = serde_json::to_string(&value).expect("json");
        assert!(
            !raw.contains("ghp_secretvalue1234"),
            "secret value leaked into the response"
        );
        assert!(
            !raw.contains("keep-me"),
            "unknown key values must never be returned"
        );
    }

    #[tokio::test]
    async fn put_requires_csrf_and_writes_uppercase_with_audit() {
        let file = "NUWA_INTERVAL=30\n\
                    nuwa_provider=zai\n\
                    NUWA_GITHUB_TOKEN=old-token-9999\n\
                    LEGACY_KEY=keep-me\n";
        let (_dir, app, store, env_path) = test_app(Some(file));

        let response = put_json(&app, r#"{"values":{"NUWA_INTERVAL":"60"}}"#, None).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            std::fs::read_to_string(&env_path).expect("read env"),
            file,
            "a CSRF rejection must not write"
        );

        let csrf = csrf_token(&app).await;
        let response = put_json(
            &app,
            r#"{"values":{
                "NUWA_INTERVAL":"60",
                "NUWA_MODEL":"glm-5.3-flash",
                "NUWA_GITHUB_TOKEN":"",
                "NUWA_UI_TOKEN":"new-ui-token"
            }}"#,
            Some(&csrf),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let value = json_body(response).await;
        assert_eq!(
            value["changed"],
            serde_json::json!(["NUWA_INTERVAL", "NUWA_MODEL", "NUWA_UI_TOKEN"])
        );
        assert_eq!(value["restart_required"], true);

        let text = std::fs::read_to_string(&env_path).expect("read env");
        assert!(text.contains("NUWA_INTERVAL=60"), "{text}");
        assert!(text.contains("NUWA_MODEL=glm-5.3-flash"), "{text}");
        assert!(text.contains("NUWA_UI_TOKEN=new-ui-token"), "{text}");
        assert!(
            text.contains("NUWA_GITHUB_TOKEN=old-token-9999"),
            "an empty secret keeps the current value: {text}"
        );
        assert!(
            text.contains("NUWA_PROVIDER=zai"),
            "historical lowercase keys are migrated before writing: {text}"
        );
        assert!(text.contains("LEGACY_KEY=keep-me"), "{text}");
        assert!(!text.contains("nuwa_provider"), "{text}");

        let backup =
            std::fs::read_to_string(env_path.with_extension("env.bak")).expect("migration backup");
        assert!(backup.contains("nuwa_provider=zai"), "{backup}");

        let events = store.events_for("__config__", 10).expect("events");
        let changed_events: Vec<_> = events
            .iter()
            .filter(|event| event.kind == "config_changed")
            .collect();
        assert_eq!(
            changed_events.len(),
            3,
            "one config_changed per changed key"
        );
        let details: BTreeSet<&str> = changed_events
            .iter()
            .map(|event| event.detail.as_str())
            .collect();
        assert_eq!(
            details,
            BTreeSet::from(["NUWA_INTERVAL", "NUWA_MODEL", "NUWA_UI_TOKEN"])
        );
        for event in &changed_events {
            assert_eq!(event.actor.as_deref(), Some("owner"));
            assert!(event.repo.is_empty(), "config events are repo-less");
        }

        let migrated_events: Vec<_> = events
            .iter()
            .filter(|event| event.kind == "config_migrated")
            .collect();
        assert_eq!(migrated_events.len(), 1, "one config_migrated per PUT");
        assert_eq!(migrated_events[0].detail, "NUWA_PROVIDER");
        assert_eq!(migrated_events[0].actor.as_deref(), Some("owner"));
        assert!(migrated_events[0].repo.is_empty());
    }

    #[tokio::test]
    async fn put_preserves_comments_order_and_unknown_keys() {
        let file = "# operator note\n\
                    NUWA_GITHUB_TOKEN=old-token-9999\n\
                    \n\
                    # runner tuning\n\
                    NUWA_INTERVAL=30\n\
                    LEGACY_KEY=keep-me\n\
                    NUWA_MODEL=custom-model\n";
        let (_dir, app, _store, env_path) = test_app(Some(file));
        let csrf = csrf_token(&app).await;

        let response = put_json(
            &app,
            r#"{"values":{"NUWA_INTERVAL":"60","NUWA_MAX_TURNS":"50"}}"#,
            Some(&csrf),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let value = json_body(response).await;
        assert_eq!(
            value["changed"],
            serde_json::json!(["NUWA_INTERVAL", "NUWA_MAX_TURNS"])
        );

        assert_eq!(
            std::fs::read_to_string(&env_path).expect("read env"),
            "# operator note\n\
             NUWA_GITHUB_TOKEN=old-token-9999\n\
             \n\
             # runner tuning\n\
             NUWA_INTERVAL=60\n\
             LEGACY_KEY=keep-me\n\
             NUWA_MODEL=custom-model\n\
             NUWA_MAX_TURNS=50\n",
            "only the updated line and the appended key may change"
        );

        let backup = std::fs::read_to_string(env_path.with_extension("env.bak"))
            .expect("ordinary PUT backup");
        assert_eq!(backup, file, "the pre-PUT file is preserved in .bak");
    }

    #[tokio::test]
    async fn put_rejects_line_breaking_values_without_touching_the_file() {
        let file = "NUWA_MODEL=custom-model\n\
                    NUWA_GITHUB_TOKEN=old-token-9999\n\
                    LEGACY_KEY=keep-me\n";
        let (_dir, app, store, env_path) = test_app(Some(file));
        let csrf = csrf_token(&app).await;

        let response = put_json(
            &app,
            r#"{"values":{
                "NUWA_MODEL":"custom-model\nNUWA_PROVIDER=faux",
                "NUWA_GITHUB_TOKEN":"token\nLEGACY_KEY=injected",
                "NUWA_RUNNER_ID":"runner\u0000id"
            }}"#,
            Some(&csrf),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let value = json_body(response).await;
        assert_eq!(value["error"]["code"], "validation_failed");
        for key in ["NUWA_MODEL", "NUWA_GITHUB_TOKEN", "NUWA_RUNNER_ID"] {
            assert!(value["error"]["fields"][key].is_string(), "{key}: {value}");
        }

        assert_eq!(
            std::fs::read(&env_path).expect("read env"),
            file.as_bytes(),
            "a rejected PUT must leave the file byte-identical"
        );
        assert!(
            !env_path.with_extension("env.bak").exists(),
            "validation runs before the backup and write"
        );
        assert!(
            store
                .events_for("__config__", 10)
                .expect("events")
                .is_empty(),
            "a rejected PUT must not journal any config event"
        );
    }

    #[tokio::test]
    async fn put_rejects_quote_and_backslash_values_that_systemd_would_rewrite() {
        // systemd EnvironmentFile strips surrounding quotes and unescapes
        // backslashes; nuwa's parse_env reads raw. A token saved as `"alpha"`
        // would load as `alpha` at runtime and lock the owner out, so reject.
        let file = "NUWA_UI_TOKEN=good-token-1234\n";
        let (_dir, app, _store, env_path) = test_app(Some(file));
        let csrf = csrf_token(&app).await;

        let response = put_json(
            &app,
            r#"{"values":{
                "NUWA_UI_TOKEN":"\"quoted-token\"",
                "NUWA_GITHUB_TOKEN":"back\\slash"
            }}"#,
            Some(&csrf),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let value = json_body(response).await;
        assert_eq!(value["error"]["code"], "validation_failed");
        for key in ["NUWA_UI_TOKEN", "NUWA_GITHUB_TOKEN"] {
            assert!(value["error"]["fields"][key].is_string(), "{key}: {value}");
        }
        assert_eq!(
            std::fs::read(&env_path).expect("read env"),
            file.as_bytes(),
            "a rejected PUT must leave the file byte-identical"
        );
    }

    #[tokio::test]
    async fn put_rejects_unknown_and_read_only_keys_with_422() {
        let (_dir, app, _store, env_path) = test_app(Some("NUWA_INTERVAL=30\n"));
        let csrf = csrf_token(&app).await;

        let response = put_json(
            &app,
            r#"{"values":{
                "NUWA_INTERVAL":"60",
                "NOPE":"x",
                "NUWA_ENV_FILE":"/tmp/evil.env"
            }}"#,
            Some(&csrf),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let value = json_body(response).await;
        assert_eq!(value["error"]["code"], "validation_failed");
        assert!(value["error"]["fields"]["NOPE"].is_string(), "{value}");
        assert!(
            value["error"]["fields"]["NUWA_ENV_FILE"].is_string(),
            "{value}"
        );
        assert_eq!(
            std::fs::read_to_string(&env_path).expect("read env"),
            "NUWA_INTERVAL=30\n",
            "validation is all-or-nothing"
        );
    }

    #[tokio::test]
    async fn put_validates_numeric_enum_and_url_fields() {
        let (_dir, app, _store, env_path) = test_app(None);
        let csrf = csrf_token(&app).await;

        let response = put_json(
            &app,
            r#"{"values":{
                "NUWA_INTERVAL":"abc",
                "NUWA_DAILY_PR_LIMIT":"0",
                "NUWA_MAX_TURNS":"1001",
                "NUWA_PROVIDER":"bogus",
                "NUWA_BASE_URL":"ftp://example.test",
                "NUWA_COOKIE_SECURE":"sometimes"
            }}"#,
            Some(&csrf),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let value = json_body(response).await;
        let fields = value["error"]["fields"].as_object().expect("field map");
        for key in [
            "NUWA_INTERVAL",
            "NUWA_DAILY_PR_LIMIT",
            "NUWA_MAX_TURNS",
            "NUWA_PROVIDER",
            "NUWA_BASE_URL",
            "NUWA_COOKIE_SECURE",
        ] {
            assert!(fields.get(key).is_some_and(|v| v.is_string()), "{key}");
        }
        assert!(
            !env_path.exists(),
            "an invalid write must not create the env file"
        );
    }

    #[tokio::test]
    async fn put_creates_a_missing_env_file_from_defaults() {
        let (_dir, app, _store, env_path) = test_app(None);

        let response = get(&app, "/api/v1/settings").await;
        let before = json_body(response).await;
        assert_eq!(before["env_exists"], false);
        assert!(before["env_permissions"].is_null());
        assert_eq!(before["values"]["NUWA_INTERVAL"]["value"], "30");
        assert_eq!(before["values"]["NUWA_INTERVAL"]["source"], "default");
        assert_eq!(before["values"]["NUWA_MODEL"]["value"], "");
        assert_eq!(before["values"]["NUWA_MODEL"]["source"], "default");

        let csrf = csrf_token(&app).await;
        let response = put_json(
            &app,
            r#"{"values":{
                "NUWA_INTERVAL":"120",
                "NUWA_BASE_URL":"https://example.test/v1"
            }}"#,
            Some(&csrf),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let value = json_body(response).await;
        assert_eq!(
            value["changed"],
            serde_json::json!(["NUWA_BASE_URL", "NUWA_INTERVAL"])
        );
        assert_eq!(value["restart_required"], true);
        assert!(env_path.exists(), "saving must create the env file");
        let text = std::fs::read_to_string(&env_path).expect("read env");
        assert!(text.contains("NUWA_INTERVAL=120"), "{text}");
        assert!(
            text.contains("NUWA_BASE_URL=https://example.test/v1"),
            "{text}"
        );
        assert!(!text.contains("nuwa_interval"), "{text}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&env_path)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "the atomic writer chmods the env file");
        }

        let response = get(&app, "/api/v1/settings").await;
        let after = json_body(response).await;
        assert_eq!(after["env_exists"], true);
        assert_eq!(after["needs_migration"], false);
        assert_eq!(after["values"]["NUWA_INTERVAL"]["value"], "120");
        assert_eq!(after["values"]["NUWA_INTERVAL"]["source"], "env");
        assert_eq!(after["values"]["NUWA_BASE_URL"]["source"], "env");
    }

    #[tokio::test]
    async fn restart_pending_uses_config_changed_events_after_start() {
        let now = crate::now_ms();
        let (_dir, app, store, _env_path) =
            test_app_with(Some("NUWA_GITHUB_TOKEN=token-abcd\n"), now - 1_000);

        let response = get(&app, "/api/v1/settings").await;
        let before = json_body(response).await;
        assert_eq!(before["restart_pending"], false);

        store
            .record_event(
                "__config__",
                now,
                None,
                "owner",
                "config_changed",
                "NUWA_INTERVAL",
            )
            .expect("config event");
        store
            .record_event(
                "__config__",
                now,
                None,
                "owner",
                "config_changed",
                "NUWA_GITHUB_TOKEN",
            )
            .expect("secret config event");

        let response = get(&app, "/api/v1/settings").await;
        let after = json_body(response).await;
        assert_eq!(after["restart_pending"], true);
        let changes = after["last_changes"].as_array().expect("changes");
        assert_eq!(changes.len(), 2);
        let keys: BTreeSet<&str> = changes
            .iter()
            .map(|change| change["key"].as_str().expect("key"))
            .collect();
        assert_eq!(keys, BTreeSet::from(["NUWA_INTERVAL", "NUWA_GITHUB_TOKEN"]));
        for change in changes {
            assert_eq!(change["ts_ms"], now);
            assert_eq!(change["actor"], "owner");
        }
        assert_eq!(
            after["values"]["NUWA_GITHUB_TOKEN"]["updated_at_ms"], now,
            "secret update time comes from the latest config_changed audit row"
        );
        assert_eq!(after["values"]["NUWA_GITHUB_TOKEN"]["last4"], "abcd");
    }

    #[test]
    fn last_four_hides_short_secrets_and_handles_multibyte_values() {
        assert_eq!(last_four(""), None);
        assert_eq!(last_four("abcd"), None);
        assert_eq!(last_four("密钥测试"), None);
        assert_eq!(last_four("abcde"), Some("bcde".to_string()));
        assert_eq!(last_four("密钥测试值"), Some("钥测试值".to_string()));
    }

    #[test]
    fn empty_llm_model_base_and_provider_mean_unset() {
        let mut values = BTreeMap::new();
        values.insert("NUWA_MODEL".to_string(), Value::String("  ".to_string()));
        values.insert("NUWA_BASE_URL".to_string(), Value::String(String::new()));
        values.insert("NUWA_PROVIDER".to_string(), Value::String(String::new()));

        // Empty/blank candidate -> environment value.
        assert_eq!(
            resolve_llm_setting(
                &values,
                "NUWA_MODEL",
                Some("env-model".to_string()),
                "default-model",
            )
            .expect("model"),
            "env-model"
        );
        // Empty candidate and empty env -> provider default.
        assert_eq!(
            resolve_llm_setting(
                &values,
                "NUWA_BASE_URL",
                Some(String::new()),
                "https://d.test"
            )
            .expect("base"),
            "https://d.test"
        );
        assert_eq!(
            resolve_llm_setting(&values, "NUWA_PROVIDER", None, "anthropic").expect("provider"),
            "anthropic"
        );
        // A non-empty candidate always wins over env and default.
        values.insert(
            "NUWA_MODEL".to_string(),
            Value::String("typed-model".to_string()),
        );
        assert_eq!(
            resolve_llm_setting(
                &values,
                "NUWA_MODEL",
                Some("env-model".to_string()),
                "default-model",
            )
            .expect("model"),
            "typed-model"
        );
        // Empty secrets stay an error: they never fall back to the environment.
        values.insert("ZAI_API_KEY".to_string(), Value::String(String::new()));
        let error = llm_key(&values, "ZAI_API_KEY").expect_err("an empty secret must 422");
        assert_eq!(error.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            error
                .fields
                .as_ref()
                .and_then(|fields| fields.get("ZAI_API_KEY"))
                .map(String::as_str),
            Some("API key is required")
        );
    }

    #[test]
    fn probe_credential_refuses_the_process_key_for_another_host() {
        // No key on screen, but the request names another host: the running
        // process's key must not be used — that is the exfiltration path.
        let mut values = BTreeMap::new();
        values.insert(
            "NUWA_BASE_URL".to_string(),
            Value::String("https://attacker.example.test".to_string()),
        );
        let error = probe_credential(
            &values,
            "ZAI_API_KEY",
            Some("https://attacker.example.test"),
            "https://open.bigmodel.cn/api/coding/paas/v4",
        )
        .expect_err("another host must not borrow the process key");
        assert_eq!(error.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            error
                .fields
                .as_ref()
                .and_then(|fields| fields.get("ZAI_API_KEY"))
                .map(String::as_str)
                .unwrap_or_default()
                .contains("never sent to a different host"),
            "{error:?}"
        );

        // A key typed on screen always wins, whatever the host.
        let mut values = BTreeMap::new();
        values.insert(
            "ZAI_API_KEY".to_string(),
            Value::String("pasted-key".to_string()),
        );
        let key = probe_credential(
            &values,
            "ZAI_API_KEY",
            Some("https://attacker.example.test"),
            "https://open.bigmodel.cn/api/coding/paas/v4",
        )
        .expect("pasted key");
        assert_eq!(key, "pasted-key");
    }

    #[test]
    fn anthropic_probe_ignores_base_override_and_uses_the_catalog() {
        let catalog = rpi_ai::providers::anthropic::models::anthropic_models();
        let first = catalog.first().expect("anthropic catalog").id.clone();
        let known = catalog.last().expect("anthropic catalog").id.clone();

        // An empty value falls through to the environment; an unknown id then
        // falls back to the catalog head, and NUWA_BASE_URL is never used.
        let mut values = BTreeMap::new();
        values.insert(
            "NUWA_BASE_URL".to_string(),
            Value::String("https://proxy.example.test".to_string()),
        );
        values.insert("NUWA_MODEL".to_string(), Value::String(String::new()));
        let (base, model) =
            resolve_anthropic_probe(&values, Some("not-in-catalog".to_string())).expect("resolve");
        assert_eq!(base, "https://api.anthropic.com");
        assert_eq!(model, first);

        // A catalog model typed into the console wins over the env value.
        values.insert("NUWA_MODEL".to_string(), Value::String(known.clone()));
        let (base, model) =
            resolve_anthropic_probe(&values, Some("env-model".to_string())).expect("resolve");
        assert_eq!(base, "https://api.anthropic.com");
        assert_eq!(model, known);

        // Nothing configured at all: catalog head, fixed base.
        let (base, model) = resolve_anthropic_probe(&BTreeMap::new(), None).expect("resolve");
        assert_eq!(base, "https://api.anthropic.com");
        assert_eq!(model, first);
    }

    #[test]
    fn restart_audit_is_written_before_systemd_run() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        let now = crate::now_ms();
        let scheduled = audit_then_schedule(&store, now, "nuwa-serve,nuwa-work", || {
            let events = store.events_for("__system__", 10).expect("events");
            assert!(
                events.iter().any(|event| event.kind == "restart_requested"),
                "the audit row must exist before systemd-run is attempted"
            );
            false
        });
        assert!(
            !scheduled,
            "the 501 path keeps the audit but reports failure"
        );
        let events = store.events_for("__system__", 10).expect("events");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "restart_requested");
        assert_eq!(events[0].detail, "nuwa-serve,nuwa-work");
        assert_eq!(events[0].actor.as_deref(), Some("owner"));
        assert_eq!(events[0].ts, now);
    }

    #[test]
    fn restart_unavailable_message_names_the_manual_command() {
        let message =
            restart_unavailable_message(&["nuwa-serve".to_string(), "nuwa-work".to_string()]);
        assert!(
            message.contains("systemctl restart nuwa-serve nuwa-work"),
            "{message}"
        );
    }

    #[tokio::test]
    async fn effective_diff_lists_only_differing_non_secret_keys() {
        let now = crate::now_ms();
        let (_dir, app, store, _env_path) = test_app_with(Some("NUWA_INTERVAL=60\n"), now);

        store
            .upsert_runner(&RunnerUpsert {
                id: "runner-live".to_string(),
                repo: "example.com/app".to_string(),
                host: "test-host".to_string(),
                pid: Some(42),
                started_at: now - 60_000,
                heartbeat_at: now - 1_000,
                status: "idle".to_string(),
                provider: Some("zai".to_string()),
                model: Some("glm-5.3-flash".to_string()),
                daily_pr_limit: Some(5),
                repo_path: None,
                service_unit: None,
                config_json: r#"{"interval_secs":30,"ci_timeout_secs":600,"max_turns":50,"max_tokens":1500000}"#
                    .to_string(),
            })
            .expect("upsert runner");

        let response = get(&app, "/api/v1/settings").await;
        let value = json_body(response).await;
        assert_eq!(
            value["effective_diff"],
            serde_json::json!({
                "NUWA_INTERVAL": { "file": "60", "runtime": "30", "source": "file" },
                "NUWA_MAX_TURNS": { "file": "30", "runtime": "50", "source": "default" }
            }),
            "only differing non-secret keys are listed; absent keys are labelled default"
        );
        let diff = value["effective_diff"].as_object().expect("diff");
        assert!(
            !diff
                .keys()
                .any(|key| key.contains("TOKEN") || key.ends_with("API_KEY")),
            "secrets never participate in effective_diff"
        );
    }

    // ---- settings test / restart ------------------------------------------

    #[tokio::test]
    async fn settings_test_and_restart_require_csrf() {
        let (_dir, app, _store, _env_path) = test_app(None);

        for uri in ["/api/v1/settings/test", "/api/v1/settings/restart"] {
            let response = post_json(&app, uri, None, "{}").await;
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{uri}");
            let value = json_body(response).await;
            assert_eq!(value["error"]["code"], "forbidden");
        }
    }

    #[tokio::test]
    async fn settings_test_rejects_invalid_targets_and_empty_credentials_without_network() {
        let (_dir, app, _store, _env_path) = test_app(None);
        let csrf = csrf_token(&app).await;

        // A syntactically valid body with a mistyped field is a 422.
        let response = post_json(
            &app,
            "/api/v1/settings/test",
            Some(&csrf),
            r#"{"target":"bogus"}"#,
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let value = json_body(response).await;
        assert_eq!(value["error"]["code"], "validation_failed");
        assert!(value["error"]["fields"]["target"].is_string(), "{value}");

        let response = post_json(&app, "/api/v1/settings/test", Some(&csrf), "{}").await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let value = json_body(response).await;
        assert!(value["error"]["fields"]["target"].is_string(), "{value}");

        // An explicitly empty token never falls back to the environment and
        // never reaches the network.
        let response = post_json(
            &app,
            "/api/v1/settings/test",
            Some(&csrf),
            r#"{"target":"github","values":{"NUWA_GITHUB_TOKEN":""}}"#,
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let value = json_body(response).await;
        assert_eq!(value["error"]["code"], "validation_failed");
        assert!(
            value["error"]["fields"]["NUWA_GITHUB_TOKEN"].is_string(),
            "{value}"
        );

        let response = post_json(
            &app,
            "/api/v1/settings/test",
            Some(&csrf),
            r#"{"target":"llm","values":{"NUWA_PROVIDER":"zai","ZAI_API_KEY":""}}"#,
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let value = json_body(response).await;
        assert!(
            value["error"]["fields"]["ZAI_API_KEY"].is_string(),
            "{value}"
        );

        let response = post_json(
            &app,
            "/api/v1/settings/test",
            Some(&csrf),
            r#"{"target":"llm","values":{"NUWA_PROVIDER":"anthropic","ANTHROPIC_API_KEY":""}}"#,
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let value = json_body(response).await;
        assert!(
            value["error"]["fields"]["ANTHROPIC_API_KEY"].is_string(),
            "{value}"
        );

        // faux is the offline provider: it succeeds without a key or network.
        let response = post_json(
            &app,
            "/api/v1/settings/test",
            Some(&csrf),
            r#"{"target":"llm","values":{"NUWA_PROVIDER":"faux"}}"#,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let value = json_body(response).await;
        assert_eq!(value["ok"], true);
        assert!(value["detail"].as_str().is_some_and(|d| !d.is_empty()));
    }

    #[tokio::test]
    async fn settings_test_rejects_unknown_values_keys_without_network() {
        let (_dir, app, _store, _env_path) = test_app(None);
        let csrf = csrf_token(&app).await;

        let response = post_json(
            &app,
            "/api/v1/settings/test",
            Some(&csrf),
            r#"{"target":"llm","values":{
                "NUWA_PROVIDER":"faux",
                "NOPE":"x",
                "ANTHROPIC_KEY":"typo"
            }}"#,
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let value = json_body(response).await;
        assert_eq!(value["error"]["code"], "validation_failed");
        assert!(value["error"]["fields"]["NOPE"].is_string(), "{value}");
        assert!(
            value["error"]["fields"]["ANTHROPIC_KEY"].is_string(),
            "{value}"
        );

        // Known keys and empty model/base are accepted (empty means unset);
        // a boolean candidate is an env scalar too.
        let response = post_json(
            &app,
            "/api/v1/settings/test",
            Some(&csrf),
            r#"{"target":"llm","values":{
                "NUWA_PROVIDER":"faux",
                "NUWA_MODEL":"",
                "NUWA_BASE_URL":"",
                "NUWA_COOKIE_SECURE":true
            }}"#,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let value = json_body(response).await;
        assert_eq!(value["ok"], true);
    }

    #[tokio::test]
    async fn settings_test_rejects_non_scalar_values_for_known_keys() {
        let (_dir, app, _store, _env_path) = test_app(None);
        let csrf = csrf_token(&app).await;

        // Every supplied key is type-checked, not only unknown names: the
        // probe must never silently test a different value than typed.
        let response = post_json(
            &app,
            "/api/v1/settings/test",
            Some(&csrf),
            r#"{"target":"llm","values":{
                "NUWA_PROVIDER":"faux",
                "NUWA_INTERVAL":{"nested":true},
                "NUWA_MODEL":["m"],
                "NUWA_BASE_URL":null
            }}"#,
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let value = json_body(response).await;
        assert_eq!(value["error"]["code"], "validation_failed");
        for key in ["NUWA_INTERVAL", "NUWA_MODEL", "NUWA_BASE_URL"] {
            assert!(value["error"]["fields"][key].is_string(), "{key}: {value}");
        }
        assert!(
            value["error"]["fields"]["NUWA_PROVIDER"].is_null(),
            "a valid scalar must not be flagged: {value}"
        );
    }

    #[tokio::test]
    async fn restart_is_guarded_by_active_runs() {
        let _serial = restart_test_guard().await;
        let now = crate::now_ms();
        let (_dir, app, store, _env_path) = test_app(None);
        let signal = Signal {
            repo: "example.com/app".to_string(),
            language: "go".to_string(),
            title: "restart guard".to_string(),
            severity: "high".to_string(),
            message: Some("boom".to_string()),
            log_window: Vec::new(),
            stack_trace: "panic: restart guard\ngoroutine 1 [running]:\nexample.com/app.F()\n\t/app/app.go:1 +0x1"
                .to_string(),
        };
        let fingerprint = match store.report_signal(&signal, now).expect("signal") {
            ReportOutcome::NewIssue { fingerprint } => fingerprint,
            other => panic!("must be a new issue, got {other:?}"),
        };
        store
            .start_run(&RunStart {
                id: "run-active".to_string(),
                fingerprint: fingerprint.clone(),
                runner_id: "runner-1".to_string(),
                repo: "example.com/app".to_string(),
                started_at: now,
                provider: None,
                model: None,
            })
            .expect("start run");
        let csrf = csrf_token(&app).await;

        let response = post_json(&app, "/api/v1/settings/restart", Some(&csrf), "{}").await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let value = json_body(response).await;
        assert_eq!(value["error"]["code"], "conflict");
        let active = value["active_runs"]
            .as_array()
            .expect("a guarded restart lists its active runs");
        assert_eq!(active.len(), 1);
        assert_eq!(active[0]["id"], "run-active");
        assert_eq!(active[0]["fingerprint"], fingerprint);
        assert_eq!(active[0]["runner_id"], "runner-1");
        assert!(
            store
                .events_for("__system__", 10)
                .expect("events")
                .is_empty(),
            "a guarded restart must not journal restart_requested"
        );
    }

    #[tokio::test]
    async fn restart_with_invalid_units_is_422() {
        let _serial = restart_test_guard().await;
        for (serve_unit, runner_units) in [
            ("nuwa serve", vec!["nuwa-work"]),
            ("nuwa-serve", vec!["nuwa-work", "../etc/passwd"]),
        ] {
            let (_dir, app, store, _env_path) =
                test_app_custom(None, crate::now_ms(), serve_unit, &runner_units);
            let csrf = csrf_token(&app).await;

            let response = post_json(&app, "/api/v1/settings/restart", Some(&csrf), "{}").await;
            assert_eq!(
                response.status(),
                StatusCode::UNPROCESSABLE_ENTITY,
                "{serve_unit} {runner_units:?}"
            );
            let value = json_body(response).await;
            assert_eq!(value["error"]["code"], "validation_failed");
            assert!(
                value["error"]["fields"]["units"].is_string(),
                "an invalid unit must name the field: {value}"
            );
            assert!(
                store
                    .events_for("__system__", 10)
                    .expect("events")
                    .is_empty(),
                "a rejected restart must not journal restart_requested"
            );
        }
    }

    #[tokio::test]
    async fn restart_without_configured_units_is_501() {
        let _serial = restart_test_guard().await;
        let (_dir, app, store, _env_path) = test_app_custom(None, crate::now_ms(), "", &[]);
        let csrf = csrf_token(&app).await;

        let response = post_json(&app, "/api/v1/settings/restart", Some(&csrf), "{}").await;
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
        let value = json_body(response).await;
        assert_eq!(value["error"]["code"], "not_supported");
        assert!(
            value["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("manually")),
            "the 501 must carry the manual fallback hint: {value}"
        );
        assert!(
            store
                .events_for("__system__", 10)
                .expect("events")
                .is_empty(),
            "a restart rejected before scheduling is audit-free"
        );
    }
}
