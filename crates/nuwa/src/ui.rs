//! Web 控制台 — 山海经运维面板，挂现有 serve（同端口同鉴权）。
//!
//! 只读为主 + 两个鉴权写操作（reopen / sync）。鉴权复用上报令牌
//! （Basic，密码 = NUWA_INGEST_TOKEN）：一个令牌两个用途，泄露后果同
//! 级（垃圾信号 + 误 reopen），轮换一处即可。

use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Response};
use axum::Json;
use std::sync::Arc;
use std::collections::BTreeMap;

pub type Shared = (Arc<shanhaijing::Store>, Arc<Option<String>>);

/// Basic auth: password must equal the ingest token. Local mode (no token
/// configured) allows all — same trust domain as /ingest without a token.
pub fn basic_password_ok(authorization: Option<&str>, expected: &str) -> bool {
    if expected.is_empty() {
        return true;
    }
    let Some(encoded) = authorization
        .and_then(|v| v.strip_prefix("Basic "))
        .map(str::trim)
    else {
        return false;
    };
    use base64::Engine;
    let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(encoded) else {
        return false;
    };
    let Ok(text) = String::from_utf8(decoded) else {
        return false;
    };
    // password = everything after the first ':' (username is ignored).
    text.split_once(':').map(|(_, pass)| pass) == Some(expected)
}

pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub async fn ui_auth(
    State((_, token)): State<Shared>,
    headers: HeaderMap,
    req: Request,
    next: Next,
) -> Response {
    let expected = token.as_deref().unwrap_or("");
    let ok = basic_password_ok(
        headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()),
        expected,
    );
    if !ok {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, r#"Basic realm="nuwa""#, )],
            "auth required",
        )
            .into_response();
    }
    next.run(req).await
}

fn badge(state: &str) -> &'static str {
    match state {
        "new" => "#3fb950",
        "triggered" | "fixing" => "#d29922",
        "pr_open" => "#58a6ff",
        "merged" => "#8957e5",
        "rejected" | "escalated" => "#f85149",
        _ => "#8b949e",
    }
}

pub fn render_dashboard(
    rows: &[shanhaijing::IssueRow],
    serve_active: &str,
    work_active: &str,
) -> String {
    let mut body = String::new();
    for r in rows {
        let pr = r
            .pr_url
            .as_deref()
            .map(|u| format!(r#"<a href="{}" target="_blank" rel="noopener">{}</a>"#, esc(u), esc(u)))
            .unwrap_or_else(|| "-".into());
        let action = if r.state == "escalated" {
            format!(
                r#"<form method="post" action="/ui/reopen" style="margin:0"><input type="hidden" name="fp" value="{}"><button>reopen</button></form>"#,
                esc(&r.fingerprint)
            )
        } else {
            "-".into()
        };
        body.push_str(&format!(
            "<tr><td>{}</td><td><span class=\"badge\" style=\"background:{}\">{}</span></td>\
             <td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            esc(&r.fingerprint[..16.min(r.fingerprint.len())]),
            badge(&r.state),
            esc(&r.state),
            esc(&r.severity),
            r.occurrences,
            esc(&r.title),
            pr,
            action,
        ));
    }
    let escalated = rows.iter().filter(|r| r.state == "escalated").count();
    format!(
        r#"<!doctype html><html lang="zh"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<meta http-equiv="refresh" content="60">
<title>Nuwa · 山海经</title><style>
body{{font-family:system-ui,sans-serif;background:#0d1117;color:#e6edf3;margin:24px}}
h1{{font-size:20px}} table{{border-collapse:collapse;width:100%;font-size:14px}}
th,td{{text-align:left;padding:6px 10px;border-bottom:1px solid #21262d}}
th{{color:#8b949e;font-weight:600}} .badge{{color:#0d1117;padding:2px 8px;border-radius:10px;font-weight:600}}
button{{background:#238636;color:#fff;border:0;border-radius:6px;padding:4px 10px;cursor:pointer}}
.svc{{color:#8b949e;font-size:13px;margin:8px 0 16px}} .svc b{{color:{serve_color}}}
a{{color:#58a6ff;text-decoration:none}} .cnt{{color:#8b949e;font-size:13px}}
</style></head><body>
<h1>Nuwa · 山海经 <span class="cnt">({total} 单，escalated {esc_n})</span></h1>
<div class="svc">serve: <b>{serve}</b> · work: <b>{work}</b> · 刷新间隔 60s ·
<form method="post" action="/ui/sync" style="display:inline"><button>sync 对账</button></form></div>
<table><tr><th>fingerprint</th><th>state</th><th>severity</th><th>seen</th><th>title</th><th>PR</th><th>action</th></tr>
{body}
</table></body></html>"#,
        total = rows.len(),
        esc_n = escalated,
        serve = esc(serve_active),
        serve_color = if serve_active == "active" { "#3fb950" } else { "#f85149" },
        work = esc(work_active),
        body = body,
    )
}

pub async fn dashboard(State((store, _)): State<Shared>) -> Response {
    let rows = store.list_issues().unwrap_or_default();
    let serve = svc_active("nuwa-serve");
    let work = svc_active("nuwa-work");
    Html(render_dashboard(&rows, &serve, &work)).into_response()
}

pub async fn reopen(
    State((store, _)): State<Shared>,
    axum::extract::Form(form): axum::extract::Form<BTreeMap<String, String>>,
) -> Response {
    let Some(fp) = form.get("fp") else {
        return (StatusCode::BAD_REQUEST, "missing fp").into_response();
    };
    let rows = store.list_issues().unwrap_or_default();
    let Some(row) = rows.iter().find(|r| r.fingerprint == *fp) else {
        return (StatusCode::NOT_FOUND, "unknown fingerprint").into_response();
    };
    if row.state != "escalated" {
        return (StatusCode::CONFLICT, format!("state is {}, not escalated", row.state))
            .into_response();
    }
    match store.set_state(fp, shanhaijing::State::New, crate::now_ms(), Some("reopened via web console"))
    {
        Ok(()) => axum::response::Redirect::to("/ui").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response(),
    }
}

pub async fn sync_now(
    State((store, _)): State<Shared>,
) -> Response {
    let Ok(token) = std::env::var("NUWA_GITHUB_TOKEN").or_else(|_| std::env::var("GITHUB_TOKEN")) else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "NUWA_GITHUB_TOKEN not set").into_response();
    };
    match crate::ci::reconcile_all(&store, &token, crate::now_ms()).await {
        Ok(lines) => Json(serde_json::json!({ "outcomes": lines })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response(),
    }
}

fn svc_active(name: &str) -> String {
    std::process::Command::new("systemctl")
        .args(["is-active", name])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|_| "n/a".into())
}

#[cfg(test)]
mod tests {
    use super::{basic_password_ok, esc};

    #[test]
    fn basic_auth_accepts_matching_password() {
        // nuwa:t123
        assert!(basic_password_ok(Some("Basic bnV3YTp0MTIz"), "t123"));
    }

    #[test]
    fn basic_auth_rejects_wrong_or_missing() {
        assert!(!basic_password_ok(Some("Basic bnV3YTp0MTIz"), "other"));
        assert!(!basic_password_ok(Some("Basic !!!not-base64"), "x"));
        assert!(!basic_password_ok(Some("Bearer xyz"), "x"));
        assert!(!basic_password_ok(None, "x"));
    }

    #[test]
    fn local_mode_allows_all() {
        assert!(basic_password_ok(None, ""));
        assert!(basic_password_ok(Some("Basic whatever"), ""));
    }

    #[test]
    fn esc_neutralizes_html() {
        assert_eq!(esc("<a href=\"x\">&</a>"), "&lt;a href=&quot;x&quot;&gt;&amp;&lt;/a&gt;");
    }
}
