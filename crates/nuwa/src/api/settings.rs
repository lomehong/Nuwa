//! `GET/PUT /api/v1/settings` — schema, masked values and validated
//! write-back for the env file (docs/09 §9.2 settings, §13 config).

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use shanhaijing::{EventRow, RunnerRow, Store};

use crate::api::common::{parse_json_body, valid_systemd_unit};
use crate::api::ApiError;
use crate::serve::AppState;
use crate::settings::{self, SettingSpec};

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
    changes.sort_by(|a, b| b.ts.cmp(&a.ts));

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
            let diff_source = if source == "default" { "default" } else { "file" };
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

fn write_settings(
    store: &Store,
    path: &Path,
    updates: &BTreeMap<String, Value>,
    now: i64,
) -> Result<PutSettingsResponse, ApiError> {
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
type ValidatedUpdates =
    Result<(BTreeMap<String, String>, Vec<String>), BTreeMap<String, String>>;

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
    use std::collections::BTreeSet;
    use std::path::PathBuf;
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{header, Request, StatusCode};
    use axum::response::Response;
    use axum::Router;
    use shanhaijing::{RunnerUpsert, Store};
    use tower::ServiceExt;

    use super::last_four;
    use crate::serve::{build_app, AppState};

    /// docs/09 §13.1, in table order.
    const CANONICAL_KEYS: &[&str] = &[
        "NUWA_GITHUB_TOKEN",
        "ZAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "NUWA_INGEST_TOKEN",
        "NUWA_UI_TOKEN",
        "NUWA_PROVIDER",
        "NUWA_MODEL",
        "NUWA_BASE_URL",
        "NUWA_INTERVAL",
        "NUWA_CI_TIMEOUT",
        "NUWA_DAILY_PR_LIMIT",
        "NUWA_MAX_TURNS",
        "NUWA_MAX_TOKENS",
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
            serve_unit: "nuwa-serve".to_string(),
            runner_units: vec!["nuwa-work".to_string()],
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
}
