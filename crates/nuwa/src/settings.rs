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
use std::collections::{BTreeMap, HashMap};

pub const ENV_KEYS: &[&str] = &[
    "nuwa_github_token",
    "zai_api_key",
    "nuwa_ingest_token",
    "nuwa_ui_token",
    "nuwa_provider",
    "nuwa_model",
    "nuwa_base_url",
    "nuwa_interval",
    "nuwa_ci_timeout",
    "nuwa_daily_pr_limit",
    "nuwa_max_turns",
    "nuwa_max_tokens",
];

const SECRET_KEYS: &[&str] = &[
    "nuwa_github_token",
    "zai_api_key",
    "nuwa_ingest_token",
    "nuwa_ui_token",
];

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

pub fn render_env(map: &BTreeMap<String, String>) -> String {
    let mut out = String::from("# Nuwa env — edited via web console (docs/09 §5)\n");
    for (k, v) in map {
        out.push_str(&format!("{k}={v}\n"));
    }
    out
}

pub fn mask(v: Option<&str>) -> String {
    match v {
        Some(v) if !v.is_empty() => format!("••••{}", &v[v.len().saturating_sub(4)..]),
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
    let map = parse_env(&text);
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
    let old = parse_env(&text);

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
        let _ = store.record_event("__config__", now, "config_changed", key);
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
            "nuwa_provider" => {
                if !matches!(trimmed, "zai" | "anthropic" | "faux") {
                    return Err(format!("nuwa_provider 非法：{trimmed}（zai|anthropic|faux）"));
                }
                trimmed.to_string()
            }
            "nuwa_interval" | "nuwa_ci_timeout" | "nuwa_daily_pr_limit" | "nuwa_max_turns"
            | "nuwa_max_tokens" => {
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
             <td><input type=\"text\" name=\"{key}\" value=\"{}\"></td></tr>",
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
            let sel = if get("nuwa_provider") == *p { " selected" } else { "" };
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
<tr><td>provider</td><td class="mono dim">nuwa_provider</td><td><select name="nuwa_provider">{provider_options}</select></td></tr>
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
        secret_github = secret_row("nuwa_github_token", "GitHub PAT（Contents: RW + Actions: R）"),
        secret_zai = secret_row("zai_api_key", "智谱 API Key"),
        secret_ingest = secret_row("nuwa_ingest_token", "上报令牌（青鸟 X-NUWA-Token）"),
        secret_ui = secret_row("nuwa_ui_token", "控制台密码（留空 = 同上报令牌）"),
        provider_options = provider_options,
        text_row_model = text_row("nuwa_model", "模型", "glm-5.3-flash"),
        text_row_base = text_row("nuwa_base_url", "Base URL", "https://open.bigmodel.cn/api/coding/paas/v4"),
        num_interval = num_row("nuwa_interval", "轮询间隔（秒）"),
        num_ci = num_row("nuwa_ci_timeout", "CI 等待上限（秒）"),
        num_limit = num_row("nuwa_daily_pr_limit", "每日 PR 上限"),
        num_turns = num_row("nuwa_max_turns", "单问题单 turn 上限"),
        num_tokens = num_row("nuwa_max_tokens", "单问题单 token 上限"),
    )
}
