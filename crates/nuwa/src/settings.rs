//! 设置页 — `/opt/nuwa/nuwa.env` 的分区掩码编辑器（存储模型 A，docs/09 §5）。
//!
//! env 文件为唯一事实源（systemd EnvironmentFile）。GET 渲染现值（密钥掩码，
//! 留空保持不变）；POST 校验（数值/白名单）→ 写回（chmod 600）→ 对每个变更
//! key 写 config_changed 事件（只记 key 不记值）→ 提示重启生效。重启走
//! `systemd-run` 编排（serve 先响应，2 秒后由 systemd 执行重启）。

use crate::ui::esc;
use crate::Shared;

use axum::extract::{Form, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// Every key the console knows (docs/09 §13.1). Uppercase is canonical; the
/// startup migration rewrites historical lowercase spellings.
pub const ENV_KEYS: &[&str] = &[
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

pub(crate) const SECRET_KEYS: &[&str] = &[
    "NUWA_GITHUB_TOKEN",
    "ZAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "NUWA_INGEST_TOKEN",
    "NUWA_UI_TOKEN",
];

/// One canonical setting (docs/09 §9.2 settings schema, §13.1 key table).
/// Static so the API can serve the schema without allocating or copying.
#[derive(Debug, Clone, Copy)]
pub struct SettingSpec {
    pub key: &'static str,
    pub label: &'static str,
    pub group: &'static str,
    /// `secret` | `string` | `int` | `enum` | `url` | `csv` | `path`.
    pub kind: &'static str,
    /// `None` for keys without a default (the secrets).
    pub default: Option<&'static str>,
    pub options: &'static [&'static str],
    pub secret: bool,
    /// `both` | `work` | `serve`.
    pub scope: &'static str,
    /// The env file is read at process start, so every write needs a restart.
    pub requires_restart: bool,
    pub read_only: bool,
    pub help: &'static str,
    pub min: Option<u64>,
    pub max: Option<u64>,
}

// One call per schema row; grouping the arguments into a struct would make
// the table harder to scan than the lint is worth.
#[allow(clippy::too_many_arguments)]
const fn spec(
    key: &'static str,
    label: &'static str,
    group: &'static str,
    kind: &'static str,
    default: Option<&'static str>,
    options: &'static [&'static str],
    secret: bool,
    scope: &'static str,
    help: &'static str,
) -> SettingSpec {
    SettingSpec {
        key,
        label,
        group,
        kind,
        default,
        options,
        secret,
        scope,
        requires_restart: true,
        read_only: false,
        help,
        min: None,
        max: None,
    }
}

const fn bounded(mut setting: SettingSpec, min: u64, max: u64) -> SettingSpec {
    setting.min = Some(min);
    setting.max = Some(max);
    setting
}

const fn read_only(mut setting: SettingSpec) -> SettingSpec {
    setting.read_only = true;
    setting
}

/// Canonical key schema from docs/09 §13.1, in table order.
pub const SETTINGS_SCHEMA: &[SettingSpec] = &[
    spec(
        "NUWA_GITHUB_TOKEN",
        "GitHub token",
        "credentials",
        "secret",
        None,
        &[],
        true,
        "both",
        "GitHub 细粒度 PAT：Contents 读写 + Actions 只读",
    ),
    spec(
        "ZAI_API_KEY",
        "Z.AI API key",
        "credentials",
        "secret",
        None,
        &[],
        true,
        "work",
        "智谱 API Key（provider=zai 时使用）",
    ),
    spec(
        "ANTHROPIC_API_KEY",
        "Anthropic API key",
        "credentials",
        "secret",
        None,
        &[],
        true,
        "work",
        "Anthropic API Key（provider=anthropic 时使用）",
    ),
    spec(
        "NUWA_INGEST_TOKEN",
        "Ingest token",
        "credentials",
        "secret",
        None,
        &[],
        true,
        "serve",
        "上报令牌（X-NUWA-Token）；留空 = 本地模式",
    ),
    spec(
        "NUWA_UI_TOKEN",
        "Console token",
        "credentials",
        "secret",
        None,
        &[],
        true,
        "serve",
        "控制台登录令牌；留空则回退到上报令牌",
    ),
    spec(
        "NUWA_PROVIDER",
        "LLM Provider",
        "model",
        "enum",
        Some("anthropic"),
        &["zai", "anthropic", "faux"],
        false,
        "work",
        "修复 runner 使用的模型提供方",
    ),
    spec(
        "NUWA_MODEL",
        "Model",
        "model",
        "string",
        Some(""),
        &[],
        false,
        "work",
        "模型名称，由 provider 决定",
    ),
    spec(
        "NUWA_BASE_URL",
        "Base URL",
        "model",
        "url",
        Some(""),
        &[],
        false,
        "work",
        "OpenAI 兼容端点；必须是 http/https URL",
    ),
    bounded(
        spec(
            "NUWA_INTERVAL",
            "Poll interval",
            "runner",
            "int",
            Some("30"),
            &[],
            false,
            "work",
            "轮询间隔（秒）",
        ),
        1,
        86_400,
    ),
    bounded(
        spec(
            "NUWA_CI_TIMEOUT",
            "CI timeout",
            "runner",
            "int",
            Some("600"),
            &[],
            false,
            "work",
            "CI 等待上限（秒）",
        ),
        1,
        86_400,
    ),
    bounded(
        spec(
            "NUWA_DAILY_PR_LIMIT",
            "Daily PR limit",
            "runner",
            "int",
            Some("5"),
            &[],
            false,
            "work",
            "每日 PR 上限",
        ),
        1,
        100,
    ),
    bounded(
        spec(
            "NUWA_MAX_TURNS",
            "Max turns",
            "runner",
            "int",
            Some("30"),
            &[],
            false,
            "work",
            "单问题单次修复 turn 上限",
        ),
        1,
        1_000,
    ),
    bounded(
        spec(
            "NUWA_MAX_TOKENS",
            "Max tokens",
            "runner",
            "int",
            Some("1500000"),
            &[],
            false,
            "work",
            "单问题 token 上限",
        ),
        1,
        100_000_000,
    ),
    spec(
        "NUWA_RUNNER_ID",
        "Runner ID",
        "runner",
        "string",
        Some("host:repo"),
        &[],
        false,
        "work",
        "runner 身份；[A-Za-z0-9._:-]，≤128 字符",
    ),
    spec(
        "NUWA_SERVICE_UNIT",
        "Service unit",
        "runner",
        "string",
        Some("nuwa-work"),
        &[],
        false,
        "work",
        "work 进程的 systemd unit",
    ),
    spec(
        "NUWA_SERVE_UNIT",
        "Serve unit",
        "service",
        "string",
        Some("nuwa-serve"),
        &[],
        false,
        "serve",
        "serve 进程的 systemd unit",
    ),
    spec(
        "NUWA_RUNNER_UNITS",
        "Runner units",
        "service",
        "csv",
        Some("nuwa-work"),
        &[],
        false,
        "serve",
        "重启时一并重启的 work units（逗号分隔）",
    ),
    spec(
        "NUWA_COOKIE_SECURE",
        "Cookie secure",
        "security",
        "enum",
        Some("auto"),
        &["auto", "always", "never"],
        false,
        "serve",
        "会话 cookie 的 Secure 策略",
    ),
    bounded(
        spec(
            "NUWA_SESSION_TTL_SECS",
            "Session TTL",
            "security",
            "int",
            Some("86400"),
            &[],
            false,
            "serve",
            "控制台会话有效期（秒）",
        ),
        300,
        2_592_000,
    ),
    bounded(
        spec(
            "NUWA_STUCK_AFTER_SECS",
            "Stuck floor",
            "ops",
            "int",
            Some("300"),
            &[],
            false,
            "serve",
            "卡单判定下限（秒）",
        ),
        60,
        86_400,
    ),
    bounded(
        spec(
            "NUWA_EVENT_RETENTION_DAYS",
            "Event retention",
            "ops",
            "int",
            Some("90"),
            &[],
            false,
            "serve",
            "事件保留天数",
        ),
        1,
        3_650,
    ),
    read_only(spec(
        "NUWA_ENV_FILE",
        "Env file",
        "advanced",
        "path",
        Some("/opt/nuwa/nuwa.env"),
        &[],
        false,
        "both",
        "env 文件路径（只读展示）",
    )),
];

pub fn setting_spec(key: &str) -> Option<&'static SettingSpec> {
    SETTINGS_SCHEMA.iter().find(|setting| setting.key == key)
}

pub fn env_file_path() -> std::path::PathBuf {
    std::env::var("NUWA_ENV_FILE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("/opt/nuwa/nuwa.env"))
}

pub fn parse_env(text: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            map.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    map
}

/// 把历史小写键并入规范大写键；大写优先，删除小写重复项。
pub fn normalize_known_keys(map: &BTreeMap<String, String>) -> (BTreeMap<String, String>, Vec<String>) {
    let mut out = map.clone();
    let mut changed = BTreeSet::new();
    for key in ENV_KEYS {
        let lower = key.to_ascii_lowercase();
        if let Some(lower_value) = map.get(&lower).cloned() {
            if !map.contains_key(*key) {
                out.insert((*key).to_string(), lower_value);
            }
            out.remove(&lower);
            changed.insert((*key).to_string());
        }
    }
    (out, changed.into_iter().collect())
}

pub fn migrate_env_file_at(path: &std::path::Path) -> Result<Vec<String>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("read {}: {e}", path.display())),
    };
    let (new, changed) = normalize_known_keys(&parse_env(&text));
    if changed.is_empty() {
        return Ok(changed);
    }
    // 迁移会重排文件（注释/顺序不保留）：先备份，再原子替换。
    let backup = path.with_extension("env.bak");
    std::fs::copy(path, &backup)
        .map_err(|e| format!("backup {} -> {}: {e}", path.display(), backup.display()))?;
    write_env_atomic(path, &render_env(&new))?;
    Ok(changed)
}

/// 原子写：Unix 先写临时文件（chmod 600）再 rename 覆盖；失败时清理临时文件。
pub(crate) fn write_env_atomic(path: &std::path::Path, contents: &str) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let tmp = path.with_extension("env.tmp");
        let write = (|| -> std::io::Result<()> {
            std::fs::write(&tmp, contents)?;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
            std::fs::rename(&tmp, path)
        })();
        if let Err(e) = write {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("write {}: {e}", path.display()));
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, contents).map_err(|e| format!("write {}: {e}", path.display()))
    }
}

pub fn migrate_env_file() -> Result<Vec<String>, String> {
    migrate_env_file_at(&env_file_path())
}

pub fn render_env(map: &BTreeMap<String, String>) -> String {
    let mut out = String::from("# Nuwa env — edited via web console (docs/09 §5)\n");
    for (k, v) in map {
        out.push_str(&format!("{k}={v}\n"));
    }
    out
}

/// Copy `path` to the sibling `.bak` slot before a rewrite. A missing file is
/// a no-op; any other failure aborts the caller, so the original is never
/// lost by a write that could not be backed up.
pub(crate) fn backup_env_file(path: &std::path::Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    let backup = path.with_extension("env.bak");
    std::fs::copy(path, &backup)
        .map(|_| ())
        .map_err(|e| format!("backup {} -> {}: {e}", path.display(), backup.display()))
}

/// Rewrite `text` in place without reordering it (docs/09 §13.2):
///
/// * historical lowercase known keys move to their canonical uppercase
///   spelling on the same line; a lowercase duplicate is dropped when the
///   uppercase spelling is also present (uppercase wins);
/// * every `KEY=VALUE` line matching a key in `updates` gets the new value,
///   keeping its position and line ending;
/// * keys in `updates` that do not appear in the file are appended;
/// * comments, blank lines, unknown keys, malformed lines and CRLF endings
///   are preserved byte for byte.
pub(crate) fn update_env_text(text: &str, updates: &BTreeMap<String, String>) -> String {
    let parsed = parse_env(text);
    let present: BTreeSet<String> = parsed.keys().cloned().collect();
    // Only used for lines we append; existing line endings are kept verbatim.
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut written: BTreeSet<String> = BTreeSet::new();
    let mut out = String::with_capacity(text.len() + updates.len() * 24);

    for line in text.split_inclusive('\n') {
        let (body, ending) = split_line_ending(line);
        let trimmed = body.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            out.push_str(body);
            out.push_str(ending);
            continue;
        }
        let Some((raw_key, _)) = body.split_once('=') else {
            // Malformed lines are preserved; a known key they mention is not
            // seen by parse_env either, so an update for it is appended.
            out.push_str(body);
            out.push_str(ending);
            continue;
        };
        let raw_key = raw_key.trim();
        let key = raw_key.to_ascii_uppercase();
        if !ENV_KEYS.contains(&key.as_str()) {
            out.push_str(body);
            out.push_str(ending);
            continue;
        }
        if raw_key != key && present.contains(&key) {
            // Uppercase spelling wins; drop the historical lowercase duplicate.
            continue;
        }
        if raw_key == key && !updates.contains_key(&key) {
            out.push_str(body);
        } else {
            let value = updates
                .get(&key)
                .or_else(|| parsed.get(&key))
                .or_else(|| parsed.get(raw_key))
                .map(String::as_str)
                .unwrap_or_default();
            out.push_str(&format!("{key}={value}"));
            written.insert(key);
        }
        out.push_str(ending);
    }

    for (key, value) in updates {
        if written.contains(key) || present.contains(key) {
            continue;
        }
        if !out.is_empty() && !out.ends_with('\n') {
            out.push_str(eol);
        }
        out.push_str(&format!("{key}={value}{eol}"));
    }
    out
}

/// Split one `split_inclusive('\n')` segment into its body and line ending.
fn split_line_ending(line: &str) -> (&str, &str) {
    if let Some(body) = line.strip_suffix("\r\n") {
        (body, "\r\n")
    } else if let Some(body) = line.strip_suffix('\n') {
        (body, "\n")
    } else {
        (line, "")
    }
}

pub fn mask(v: Option<&str>) -> String {
    match v {
        Some(v) if !v.is_empty() => {
            // Character-based: a byte slice would panic when the cut point
            // falls inside a multi-byte character.
            let chars: Vec<char> = v.chars().collect();
            let tail: String = chars[chars.len().saturating_sub(4)..].iter().collect();
            format!("••••{tail}")
        }
        _ => "(未设置)".to_string(),
    }
}

pub async fn get_settings(State((_, _)): State<Shared>) -> Response {
    let text = match std::fs::read_to_string(env_file_path()) {
        Ok(t) => t,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("read {}: {e}（本地调试可设 NUWA_ENV_FILE 指向样例文件）", env_file_path().display()),
            )
                .into_response()
        }
    };
    let (map, _) = normalize_known_keys(&parse_env(&text));
    let flash: Option<&str> = None;
    Html(render_settings(&map, flash)).into_response()
}

pub async fn save_settings(
    State((store, _)): State<Shared>,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    let text = match std::fs::read_to_string(env_file_path()) {
        Ok(t) => t,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("read {}: {e}", env_file_path().display()),
            )
                .into_response()
        }
    };
    let (old, _) = normalize_known_keys(&parse_env(&text));

    let (new, changed) = match apply_form(&old, &form) {
        Ok((n, c)) => (n, c),
        Err(e) => return (StatusCode::BAD_REQUEST, format!("校验失败：{e}")).into_response(),
    };

    if let Err(e) = std::fs::write(env_file_path(), render_env(&new)) {
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("write: {e}")).into_response();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(env_file_path(), std::fs::Permissions::from_mode(0o600));
    }

    let now = crate::now_ms();
    for key in &changed {
        let _ = store.record_event("__config__", now, None, "owner", "config_changed", key);
    }

    let summary = if changed.is_empty() {
        "没有变更".to_string()
    } else {
        format!("已保存 {} 项变更（重启服务后生效）：{}", changed.len(), changed.join(", "))
    };
    Html(format!(
        "<!doctype html><meta charset=\"utf-8\"><body style=\"font-family:system-ui;background:#0d1117;color:#e6edf3;padding:24px\">\
         <h2>设置已保存</h2><p>{summary}</p>\
         <p><a href=\"/settings\">← 返回设置</a> · <a href=\"/ui\">← 控制台</a></p>",
    ))
    .into_response()
}

pub async fn restart_services(State((_, _)): State<Shared>) -> Response {
    // systemd-run schedules the restart on the systemd manager itself, so the
    // HTTP response is written before serve/work are restarted.
    let scheduled = std::process::Command::new("systemd-run")
        .args(["--on-active=2", "systemctl", "restart", "nuwa-serve", "nuwa-work"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if scheduled {
        (
            StatusCode::OK,
            "<!doctype html><meta charset=\"utf-8\"><meta http-equiv=\"refresh\" content=\"6;url=/ui\">\
             <body style=\"font-family:system-ui;background:#0d1117;color:#e6edf3;padding:24px\">\
             <p>重启已编排（2 秒后执行），稍后自动返回控制台…</p>",
        )
            .into_response()
    } else {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "systemd-run 不可用（非 systemd 环境？）——请手动 systemctl restart".to_string(),
        )
            .into_response()
    }
}

/// 合并表单到旧配置：密钥留空 = 保持不变；数值校验；provider 白名单。
/// 返回（新 map，变更 key 列表）。
pub fn apply_form(
    old: &BTreeMap<String, String>,
    form: &HashMap<String, String>,
) -> Result<(BTreeMap<String, String>, Vec<String>), String> {
    let mut new = old.clone();
    let mut changed = Vec::new();

    for key in ENV_KEYS {
        let Some(input) = form.get(*key) else { continue };
        let trimmed = input.trim();
        let is_secret = SECRET_KEYS.contains(key);

        if is_secret && trimmed.is_empty() {
            continue; // 密钥留空 = 保持不变
        }

        let value = match *key {
            "NUWA_PROVIDER" => {
                if !matches!(trimmed, "zai" | "anthropic" | "faux") {
                    return Err(format!("NUWA_PROVIDER 非法：{trimmed}（zai|anthropic|faux）"));
                }
                trimmed.to_string()
            }
            "NUWA_INTERVAL" | "NUWA_CI_TIMEOUT" | "NUWA_DAILY_PR_LIMIT" | "NUWA_MAX_TURNS"
            | "NUWA_MAX_TOKENS" => {
                let n: u64 = trimmed
                    .parse()
                    .map_err(|_| format!("{key} 必须是正整数，得到 {trimmed:?}"))?;
                if n == 0 {
                    return Err(format!("{key} 不能为 0"));
                }
                n.to_string()
            }
            _ => trimmed.to_string(),
        };

        if old.get(*key).map(|v| v.as_str()) != Some(value.as_str()) {
            changed.push((*key).to_string());
        }
        new.insert((*key).to_string(), value);
    }

    Ok((new, changed))
}

pub fn render_settings(map: &BTreeMap<String, String>, flash: Option<&str>) -> String {
    let get = |k: &str| map.get(k).map(|v| v.as_str()).unwrap_or("");
    let flash_html = flash
        .map(|m| format!("<div class=\"flash\">{}</div>", esc(m)))
        .unwrap_or_default();

    let secret_row = |key: &str, label: &str| {
        format!(
            "<tr><td>{label}</td><td class=\"mono dim\">{}</td>\
             <td><input type=\"password\" name=\"{key}\" placeholder=\"留空保持不变\" autocomplete=\"new-password\"></td></tr>",
            esc(&mask(Some(get(key))))
        )
    };

    let text_row = |key: &str, label: &str, placeholder: &str| {
        format!(
            "<tr><td>{label}</td><td class=\"mono dim\">{}</td>\
             <td><input type=\"text\" name=\"{key}\" value=\"{}\" placeholder=\"{}\"></td></tr>",
            esc(get(key)),
            esc(get(key)),
            esc(placeholder)
        )
    };

    let num_row = |key: &str, label: &str| {
        format!(
            "<tr><td>{label}</td><td class=\"mono dim\">{}</td>\
             <td><input type=\"number\" name=\"{key}\" value=\"{}\" min=\"1\"></td></tr>",
            esc(get(key)),
            esc(get(key))
        )
    };

    let provider_options = ["zai", "anthropic", "faux"]
        .iter()
        .map(|p| {
            let sel = if get("NUWA_PROVIDER") == *p { " selected" } else { "" };
            format!("<option value=\"{p}\"{sel}>{p}</option>")
        })
        .collect::<String>();

    format!(
        r#"<!doctype html><html lang="zh"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Nuwa · 设置</title><style>
body{{font-family:system-ui,sans-serif;background:#0d1117;color:#e6edf3;margin:24px;max-width:900px;margin-inline:auto}}
h1{{font-size:20px}} h2{{font-size:14px;color:#8b949e;text-transform:uppercase;letter-spacing:.08em;margin:26px 0 8px}}
table{{border-collapse:collapse;width:100%;font-size:14px}}
th,td{{text-align:left;padding:7px 10px;border-bottom:1px solid #21262d;vertical-align:middle}}
th{{color:#8b949e;font-weight:600;width:240px}}
input{{background:#0d1117;color:#e6edf3;border:1px solid #30363d;border-radius:6px;padding:6px 10px;width:100%;font-family:inherit}}
input:focus{{outline:1px solid #1f6feb}}
button{{background:#238636;color:#fff;border:0;border-radius:6px;padding:8px 18px;cursor:pointer}}
button.warn{{background:#da3633}}
.flash{{background:#1f6feb33;border:1px solid #1f6feb;border-radius:8px;padding:10px 14px;margin-bottom:14px}}
.mono{{font-family:ui-monospace,monospace;font-size:12px}} .dim{{color:#8b949e}}
a{{color:#58a6ff}} .row{{display:flex;gap:10px;margin-top:18px}}
</style></head><body>
<h1>Nuwa · 设置</h1>
{flash_html}
<form method="post" action="/settings/save">
<h2>① 凭据</h2>
<table>
{secret_github}{secret_zai}{secret_ingest}{secret_ui}
</table>
<h2>② LLM 修复模型</h2>
<table>
<tr><td>provider</td><td class="mono dim">NUWA_PROVIDER</td><td><select name="NUWA_PROVIDER">{provider_options}</select></td></tr>
{text_row_model}{text_row_base}
</table>
<h2>③ Runner 运行参数</h2>
<table>
{num_interval}{num_ci}{num_limit}{num_turns}{num_tokens}
</table>
<button type="submit">保存设置</button>
</form>
<div class="row">
<form method="post" action="/settings/restart"><button class="warn">重启 serve + work（应用变更）</button></form>
<p class="dim">保存写入 env 文件；重启由 systemd 编排（2 秒后执行），页面将自动返回。</p>
</div>
<p><a href="/ui">← 返回控制台</a></p>
</body></html>"#,
        secret_github = secret_row("NUWA_GITHUB_TOKEN", "GitHub PAT（Contents: RW + Actions: R）"),
        secret_zai = secret_row("ZAI_API_KEY", "智谱 API Key"),
        secret_ingest = secret_row("NUWA_INGEST_TOKEN", "上报令牌（青鸟 X-NUWA-Token）"),
        secret_ui = secret_row("NUWA_UI_TOKEN", "控制台密码（留空 = 同上报令牌）"),
        provider_options = provider_options,
        text_row_model = text_row("NUWA_MODEL", "模型", "glm-5.3-flash"),
        text_row_base = text_row("NUWA_BASE_URL", "Base URL", "https://open.bigmodel.cn/api/coding/paas/v4"),
        num_interval = num_row("NUWA_INTERVAL", "轮询间隔（秒）"),
        num_ci = num_row("NUWA_CI_TIMEOUT", "CI 等待上限（秒）"),
        num_limit = num_row("NUWA_DAILY_PR_LIMIT", "每日 PR 上限"),
        num_turns = num_row("NUWA_MAX_TURNS", "单问题单 turn 上限"),
        num_tokens = num_row("NUWA_MAX_TOKENS", "单问题单 token 上限"),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        apply_form, backup_env_file, mask, migrate_env_file_at, normalize_known_keys, parse_env,
        render_env, render_settings, update_env_text, SECRET_KEYS, SETTINGS_SCHEMA,
    };
    use std::collections::{BTreeMap, HashMap};

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
    }

    #[test]
    fn migration_moves_lowercase_known_keys_to_uppercase() {
        let old = map(&[("nuwa_interval", "30"), ("nuwa_provider", "zai")]);
        let (new, changed) = normalize_known_keys(&old);
        assert_eq!(new.get("NUWA_INTERVAL").map(String::as_str), Some("30"));
        assert_eq!(new.get("NUWA_PROVIDER").map(String::as_str), Some("zai"));
        assert!(!new.contains_key("nuwa_interval"));
        assert!(changed.contains(&"NUWA_INTERVAL".to_string()));
        assert!(changed.contains(&"NUWA_PROVIDER".to_string()));
    }

    #[test]
    fn uppercase_wins_over_lowercase_duplicate() {
        let old = map(&[("NUWA_INTERVAL", "60"), ("nuwa_interval", "30")]);
        let (new, _) = normalize_known_keys(&old);
        assert_eq!(new.get("NUWA_INTERVAL").map(String::as_str), Some("60"));
        assert!(!new.contains_key("nuwa_interval"));
    }

    #[test]
    fn apply_form_reads_and_validates_uppercase_numbers() {
        let old = map(&[("NUWA_INTERVAL", "30")]);
        let form = HashMap::from([("NUWA_INTERVAL".to_string(), "60".to_string())]);
        let (new, changed) = apply_form(&old, &form).expect("valid");
        assert_eq!(new.get("NUWA_INTERVAL").map(String::as_str), Some("60"));
        assert_eq!(changed, vec!["NUWA_INTERVAL".to_string()]);

        let bad = HashMap::from([("NUWA_INTERVAL".to_string(), "0".to_string())]);
        assert!(apply_form(&old, &bad).is_err());
    }

    #[test]
    fn render_env_writes_uppercase_keys() {
        let out = render_env(&map(&[("NUWA_INTERVAL", "60")]));
        assert!(out.contains("NUWA_INTERVAL=60"));
        assert!(!out.contains("nuwa_interval"));
    }

    #[test]
    fn update_env_text_replaces_in_place_and_appends_new_keys() {
        let text = "# operator note\n\
                    NUWA_MODEL=custom-model\n\
                    \n\
                    NUWA_INTERVAL=30\n\
                    LEGACY_KEY=keep-me\n";
        let updates = map(&[("NUWA_INTERVAL", "60"), ("NUWA_MAX_TURNS", "50")]);
        let out = update_env_text(text, &updates);
        assert_eq!(
            out,
            "# operator note\n\
             NUWA_MODEL=custom-model\n\
             \n\
             NUWA_INTERVAL=60\n\
             LEGACY_KEY=keep-me\n\
             NUWA_MAX_TURNS=50\n",
            "comments, order and unknown keys stay; only the target line and the appended key change"
        );
    }

    #[test]
    fn update_env_text_keeps_crlf_and_missing_trailing_newline() {
        let text = "# note\r\nNUWA_INTERVAL=30";
        let updates = map(&[("NUWA_INTERVAL", "60"), ("NUWA_MODEL", "glm")]);
        let out = update_env_text(text, &updates);
        assert_eq!(
            out, "# note\r\nNUWA_INTERVAL=60\r\nNUWA_MODEL=glm\r\n",
            "CRLF is kept and appended keys start on a new line"
        );
    }

    #[test]
    fn update_env_text_migrates_lowercase_keys_in_place() {
        let text = "# note\n\
                    nuwa_interval=45\n\
                    nuwa_provider=zai\n";
        let out = update_env_text(text, &BTreeMap::new());
        assert_eq!(out, "# note\nNUWA_INTERVAL=45\nNUWA_PROVIDER=zai\n");
    }

    #[test]
    fn update_env_text_drops_lowercase_duplicate_when_uppercase_wins() {
        let text = "NUWA_INTERVAL=60\n\
                    nuwa_interval=30\n";
        let out = update_env_text(text, &BTreeMap::new());
        assert_eq!(out, "NUWA_INTERVAL=60\n");
    }

    #[test]
    fn schema_secret_flags_match_the_secret_key_list() {
        for spec in SETTINGS_SCHEMA {
            assert_eq!(
                spec.secret,
                SECRET_KEYS.contains(&spec.key),
                "{}: secret flag and SECRET_KEYS must agree",
                spec.key
            );
        }
    }

    #[test]
    fn mask_is_char_safe_for_non_ascii_secrets() {
        assert_eq!(mask(Some("密钥")), "••••密钥");
        // A byte slice would panic on the second character here.
        assert_eq!(mask(Some("😀a")), "••••😀a");
        assert_eq!(mask(Some("abcde")), "••••bcde");
        assert_eq!(mask(None), "(未设置)");
    }

    #[test]
    fn backup_env_file_copies_next_to_the_original() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nuwa.env");
        std::fs::write(&path, "NUWA_INTERVAL=30\n").expect("write");
        backup_env_file(&path).expect("backup");
        assert_eq!(
            std::fs::read_to_string(path.with_extension("env.bak")).expect("read backup"),
            "NUWA_INTERVAL=30\n"
        );

        // A missing file has nothing to back up.
        let absent = dir.path().join("absent.env");
        backup_env_file(&absent).expect("missing file is a no-op");
        assert!(!absent.with_extension("env.bak").exists());
    }

    #[test]
    fn text_fields_render_current_values_not_placeholders() {
        let map = map(&[
            ("NUWA_MODEL", "custom-model"),
            ("NUWA_BASE_URL", "https://example.test/v1"),
        ]);
        let html = render_settings(&map, None);
        assert!(html.contains("name=\"NUWA_MODEL\" value=\"custom-model\""));
        assert!(html.contains("name=\"NUWA_BASE_URL\" value=\"https://example.test/v1\""));
        assert!(html.contains("placeholder=\"glm-5.3-flash\""));
    }

    #[test]
    fn legacy_page_renders_migrated_values_and_masks_secrets() {
        let text = "# operator note\n\
                    nuwa_interval=45\n\
                    NUWA_GITHUB_TOKEN=abcdefgh1234\n\
                    NUWA_MODEL=custom-model\n";
        let (map, changed) = normalize_known_keys(&parse_env(text));
        assert_eq!(changed, vec!["NUWA_INTERVAL".to_string()]);
        let html = render_settings(&map, None);
        assert!(html.contains("name=\"NUWA_INTERVAL\" value=\"45\""));
        assert!(html.contains("name=\"NUWA_MODEL\" value=\"custom-model\""));
        assert!(html.contains("••••1234"));
        assert!(!html.contains("abcdefgh1234"));
    }

    #[test]
    fn migrate_env_file_missing_path_is_noop() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("absent.env");
        let changed = migrate_env_file_at(&path).expect("noop");
        assert!(changed.is_empty());
        assert!(!path.exists());
        assert!(!path.with_extension("env.bak").exists());
    }

    #[test]
    fn migrate_env_file_is_idempotent_and_keeps_uppercase_value() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nuwa.env");
        std::fs::write(&path, "NUWA_INTERVAL=60\nnuwa_interval=30\nnuwa_provider=zai\n").expect("write");

        let changed = migrate_env_file_at(&path).expect("migrate");
        assert!(changed.contains(&"NUWA_INTERVAL".to_string()));
        assert!(changed.contains(&"NUWA_PROVIDER".to_string()));

        let text = std::fs::read_to_string(&path).expect("read");
        assert!(text.contains("NUWA_INTERVAL=60"));
        assert!(text.contains("NUWA_PROVIDER=zai"));
        assert!(!text.contains("nuwa_interval"));
        assert!(!text.contains("nuwa_provider"));

        // 迁移前原文件已备份（含小写历史键）。
        let backup = std::fs::read_to_string(path.with_extension("env.bak")).expect("backup");
        assert!(backup.contains("nuwa_interval=30"));

        assert!(migrate_env_file_at(&path).expect("second").is_empty());
    }
}
