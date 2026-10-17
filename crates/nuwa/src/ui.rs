//! Web 控制台 — 山海经运维观测台，挂现有 serve（同端口同鉴权）。
//!
//! 回答四个问题（按优先级）：什么需要我？机器在干什么？系统健康吗？全部问题单长什么样？
//! 设计语言继承神话宇宙：状态机漏斗是骨架，五色石映射问题等级（docs/05）。
//! 服务端渲染、零 JS 框架、只读为主 + 两个鉴权写操作（reopen / sync）。
//! 鉴权复用上报令牌（Basic，密码 = NUWA_INGEST_TOKEN）：一个令牌两个用途，泄露后果同级。

use axum::extract::{Query, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Redirect, Response};
use std::collections::{BTreeMap, HashMap};

use crate::Shared;

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

/// 状态→色（GitHub dark 调）。
fn state_color(state: &str) -> &'static str {
    match state {
        "new" => "#3fb950",
        "triggered" | "fixing" => "#d29922",
        "pr_open" => "#58a6ff",
        "merged" => "#a371f7",
        _ => "#f85149", // rejected / escalated
    }
}

/// 五色石：问题等级→石色（docs/05：五色对应五类问题等级）。
fn sev_color(sev: &str) -> &'static str {
    match sev {
        "critical" => "#f85149",
        "high" => "#db6d28",
        "medium" => "#d29922",
        "low" => "#3fb950",
        _ => "#8b949e",
    }
}

/// 相对时间（分钟粒度）。
pub fn rel_time(ts_ms: i64, now_ms: i64) -> String {
    let secs = (now_ms - ts_ms).max(0) / 1000;
    match secs {
        0..=59 => "刚刚".into(),
        60..=3599 => format!("{} 分钟前", secs / 60),
        3600..=86_399 => format!("{} 小时前", secs / 3600),
        _ => format!("{} 天前", secs / 86_400),
    }
}

const STATES: &[&str] = &[
    "new",
    "triggered",
    "fixing",
    "pr_open",
    "merged",
    "rejected",
    "escalated",
];

fn state_label(state: &str) -> &'static str {
    match state {
        "new" => "new 新发现",
        "triggered" => "triggered 已触发",
        "fixing" => "fixing 修复中",
        "pr_open" => "pr_open PR 待合",
        "merged" => "merged 已合并",
        "rejected" => "rejected 被拒",
        "escalated" => "escalated 已升级",
        _ => "unknown",
    }
}

pub struct DashData {
    pub rows: Vec<shanhaijing::IssueRow>,
    pub events: Vec<shanhaijing::EventRow>,
    pub serve: String,
    pub work: String,
    pub work_hb: Option<i64>,
    pub now: i64,
    pub filter_state: Option<String>,
    pub filter_repo: Option<String>,
    pub flash: Option<String>,
}

pub fn render_dashboard(d: &DashData) -> String {
    // ---- counts per state ----
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for s in STATES {
        counts.insert(s, 0);
    }
    for r in &d.rows {
        *counts.entry(r.state.as_str()).or_insert(0) += 1;
    }
    let repos: Vec<String> = {
        let mut set: Vec<String> = d.rows.iter().map(|r| r.repo.clone()).collect();
        set.sort();
        set.dedup();
        set
    };

    // ---- pipeline funnel ----
    let mut pipeline = String::new();
    for (i, s) in STATES.iter().enumerate() {
        if i > 0 {
            pipeline.push_str("<span class=\"arrow\">→</span>");
        }
        let active = d
            .filter_state
            .as_deref()
            .map(|f| f == *s)
            .unwrap_or(false);
        pipeline.push_str(&format!(
            "<a class=\"stage{}\" href=\"/ui?state={s}\"><b>{}</b><span>{}</span></a>",
            if active { " on" } else { "" },
            counts.get(*s).copied().unwrap_or(0),
            state_label(s)
        ));
    }

    // ---- filtered rows ----
    let rows: Vec<&shanhaijing::IssueRow> = d
        .rows
        .iter()
        .filter(|r| {
            d.filter_state
                .as_deref()
                .map(|f| r.state == f)
                .unwrap_or(true)
                && d
                    .filter_repo
                    .as_deref()
                    .map(|f| r.repo == f)
                    .unwrap_or(true)
        })
        .collect();

    // ---- sections: needs attention ----
    let needs: Vec<&&shanhaijing::IssueRow> = rows
        .iter()
        .filter(|r| matches!(r.state.as_str(), "escalated" | "rejected"))
        .collect();

    let mut attention = String::new();
    for r in &needs {
        let why = if r.state == "escalated" {
            "修复 runner 失败或被 CI 拒绝，需要你复核"
        } else {
            "PR 被关闭且未合并，等待回归信号或人工处理"
        };
        attention.push_str(&format!(
            "<div class=\"card\"><div class=\"card-head\"><span class=\"stone\" style=\"background:{}\"></span>\
             <b>{}</b><span class=\"fp\">{}</span></div>\
             <div class=\"why\">{why}</div>\
             <div class=\"card-act\"><span>{}</span>·<span>seen {}x</span>·\
             <form method=\"post\" action=\"/ui/reopen\" style=\"display:inline\">\
             <input type=\"hidden\" name=\"fp\" value=\"{}\"><button>reopen 重新入队</button></form></div></div>",
            sev_color(&r.severity),
            esc(&r.title),
            esc(&r.fingerprint),
            esc(&r.repo),
            r.occurrences,
            esc(&r.fingerprint),
        ));
    }

    let mut table = String::new();
    for r in &rows {
        let pr = r
            .pr_url
            .as_deref()
            .map(|u| format!("<a href=\"{}\" target=\"_blank\" rel=\"noopener\">PR ↗</a>", esc(u)))
            .unwrap_or_else(|| "<span class=\"dim\">-</span>".into());
        let action = if r.state == "escalated" {
            format!(
                "<form method=\"post\" action=\"/ui/reopen\" style=\"margin:0\">\
                 <input type=\"hidden\" name=\"fp\" value=\"{}\"><button>reopen</button></form>",
                esc(&r.fingerprint)
            )
        } else {
            "<span class=\"dim\">-</span>".into()
        };
        table.push_str(&format!(
            "<tr><td class=\"mono\">{}</td>\
             <td><span class=\"badge\" style=\"background:{}\">{}</span></td>\
             <td><span class=\"stone\" style=\"background:{}\"></span>{}</td>\
             <td>{}</td><td>{}</td><td>{}</td><td class=\"mono dim\">{}</td><td>{pr}</td><td>{action}</td></tr>",
            esc(&r.fingerprint[..16.min(r.fingerprint.len())]),
            state_color(&r.state),
            esc(&r.state),
            sev_color(&r.severity),
            esc(&r.severity),
            r.occurrences,
            rel_time(r.last_seen, d.now),
            esc(&r.title),
            esc(&r.repo),
            action = action,
        ));
    }

    let mut feed = String::new();
    for e in d.events.iter().take(12) {
        let detail = if e.detail.is_empty() {
            String::new()
        } else {
            format!("<span class=\"dim\">{}</span>", esc(&e.detail))
        };
        feed.push_str(&format!(
            "<li><span class=\"mono dim\">{}</span> <b>{}</b> {} <span class=\"dim\">· {} · {}</span></li>",
            esc(&e.fingerprint[..12.min(e.fingerprint.len())]),
            esc(&e.kind),
            detail,
            esc(&e.repo),
            rel_time(e.ts, d.now),
        ));
    }

    let repo_chips: String = repos
        .iter()
        .map(|repo| {
            let active = d.filter_repo.as_deref() == Some(repo.as_str());
            format!(
                "<a class=\"chip{}\" href=\"/ui?repo={}\">{}</a>",
                if active { " on" } else { "" },
                esc(repo),
                esc(repo)
            )
        })
        .collect();

    let flash_html = d
        .flash
        .as_deref()
        .map(|m| format!("<div class=\"flash\">{}</div>", esc(m)))
        .unwrap_or_default();

    let hb_txt = match d.work_hb {
        Some(ts) => rel_time(ts, d.now),
        None => "无记录".into(),
    };
    let state_note = match (
        d.filter_state.as_deref(),
        d.filter_repo.as_deref(),
    ) {
        (Some(s), Some(r)) => format!("筛选：state={s} · repo={r}（<a href=\"/ui\">清除</a>）"),
        (Some(s), None) => format!("筛选：state={s}（<a href=\"/ui\">清除</a>）"),
        (None, Some(r)) => format!("筛选：repo={r}（<a href=\"/ui\">清除</a>）"),
        _ => String::new(),
    };

    format!(
        r#"<!doctype html><html lang="zh"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<meta http-equiv="refresh" content="30">
<title>Nuwa · 山海经 — 运维台</title><style>
*{{box-sizing:border-box}}
body{{font-family:system-ui,-apple-system,sans-serif;background:#0d1117;color:#e6edf3;margin:0;padding:24px;max-width:1180px;margin-inline:auto}}
a{{color:#58a6ff;text-decoration:none}} a:hover{{text-decoration:underline}}
header{{display:flex;justify-content:space-between;align-items:baseline;border-bottom:1px solid #21262d;padding-bottom:14px;margin-bottom:18px;flex-wrap:wrap;gap:8px}}
h1{{font-size:22px;margin:0}} h1 .sub{{font-size:13px;color:#8b949e;font-weight:400;margin-left:10px}}
.svc{{font-size:13px;color:#8b949e}} .svc b.ok{{color:#3fb950}} .svc b.bad{{color:#f85149}}
.pipeline{{display:flex;align-items:center;gap:6px;flex-wrap:wrap;background:#161b22;border:1px solid #30363d;border-radius:8px;padding:12px 16px;margin-bottom:18px}}
.stage{{display:flex;flex-direction:column;align-items:center;min-width:64px;padding:6px 10px;border-radius:8px;color:#8b949e}}
.stage b{{font-size:20px;color:#e6edf3}} .stage span{{font-size:11px}}
.stage:hover{{background:#21262d;text-decoration:none}} .stage.on{{background:#1f6feb33;outline:1px solid #1f6feb}}
.arrow{{color:#30363d;font-size:16px}}
.flash{{background:#1f6feb33;border:1px solid #1f6feb;border-radius:8px;padding:10px 14px;margin-bottom:14px}}
.chips{{margin:10px 0 16px;display:flex;gap:8px;flex-wrap:wrap}}
.chip{{background:#161b22;border:1px solid #30363d;border-radius:14px;padding:3px 12px;font-size:12px;color:#8b949e}}
.chip.on{{background:#1f6feb33;border-color:#1f6feb;color:#e6edf3}}
h2{{font-size:15px;color:#8b949e;text-transform:uppercase;letter-spacing:.08em;margin:26px 0 10px}}
.card{{background:#161b22;border:1px solid #30363d;border-left:3px solid #f85149;border-radius:8px;padding:12px 16px;margin-bottom:10px}}
.card-head{{display:flex;align-items:center;gap:10px}} .fp{{color:#8b949e;font-size:12px}}
.why{{color:#e6edf3;font-size:13px;margin:6px 0}} .card-act{{font-size:13px;color:#8b949e;display:flex;gap:8px;align-items:center}}
table{{border-collapse:collapse;width:100%;font-size:13.5px}}
th,td{{text-align:left;padding:7px 10px;border-bottom:1px solid #21262d;vertical-align:middle}}
th{{color:#8b949e;font-weight:600;font-size:12px;text-transform:uppercase;letter-spacing:.06em}}
.badge{{color:#0d1117;padding:2px 9px;border-radius:10px;font-weight:600;font-size:12px}}
.stone{{display:inline-block;width:9px;height:9px;transform:rotate(45deg);margin-right:7px;border-radius:2px}}
.mono{{font-family:ui-monospace,monospace;font-size:12px}} .dim{{color:#8b949e}}
button{{background:#238636;color:#fff;border:0;border-radius:6px;padding:4px 12px;cursor:pointer;font-size:12px}}
button:hover{{background:#2ea043}}
ul.feed{{list-style:none;padding:0;margin:0;font-size:13px}} ul.feed li{{padding:5px 0;border-bottom:1px solid #21262d}}
footer{{margin-top:28px;color:#8b949e;font-size:12px;border-top:1px solid #21262d;padding-top:14px;display:flex;justify-content:space-between;flex-wrap:wrap;gap:8px}}
footer form{{display:inline}}
</style></head><body>
<header>
  <div><h1>女娲 · 山海经<span class="sub">自愈式修复 Agent · 运维台</span></h1></div>
  <div class="svc">serve: <b class="{serve_cls}">{serve}</b> &nbsp; work: <b class="{work_cls}">{work}</b> &nbsp; 心跳: {hb}</div>
</header>
<div class="pipeline">{pipeline}</div>
{flash_html}
{state_note}
{attention_html}
<h2>全部问题单</h2>
<div class="chips">{repo_chips}</div>
<table><tr><th>fingerprint</th><th>state</th><th>severity</th><th>seen</th><th>last seen</th><th>title</th><th>repo</th><th>PR</th><th>action</th></tr>
{table}
</table>
<h2>活动流</h2>
<ul class="feed">{feed}</ul>
<footer>
  <span>合并闸门 = CI + 人工 · escalated 需人工复核后 reopen</span>
  <form method="post" action="/ui/sync"><button>sync 对账 GitHub</button></form>
</footer>
</body></html>"#,
        serve = esc(&d.serve),
        serve_cls = if d.serve == "active" { "ok" } else { "bad" },
        work = esc(&d.work),
        work_cls = if d.work == "active" { "ok" } else { "bad" },
        hb = esc(&hb_txt),
        pipeline = pipeline,
        flash_html = flash_html,
        state_note = state_note,
        attention_html = if needs.is_empty() {
            String::new()
        } else {
            format!("<h2>需要你处理（{n}）</h2>{attention}", n = needs.len())
        },
        repo_chips = repo_chips,
        table = if table.is_empty() {
            "<tr><td colspan=\"9\" class=\"dim\">当前筛选下没有问题单</td></tr>".into()
        } else {
            table
        },
        feed = if feed.is_empty() {
            "<li class=\"dim\">暂无事件</li>".into()
        } else {
            feed
        },
    )
}

pub async fn dashboard(
    State((store, _)): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let rows = store.list_issues().unwrap_or_default();
    let events = store.recent_events(12).unwrap_or_default();
    let serve = svc_active("nuwa-serve");
    let work = svc_active("nuwa-work");
    let data = DashData {
        work_hb: store.last_heartbeat().unwrap_or(None),
        filter_state: q.get("state").cloned(),
        filter_repo: q.get("repo").cloned(),
        flash: q.get("msg").cloned(),
        rows,
        events,
        serve,
        work,
        now: crate::now_ms(),
    };
    Html(render_dashboard(&data)).into_response()
}

pub async fn reopen(
    State((store, _)): State<Shared>,
    axum::extract::Form(form): axum::extract::Form<HashMap<String, String>>,
) -> Response {
    let Some(fp) = form.get("fp") else {
        return (StatusCode::BAD_REQUEST, "missing fp").into_response();
    };
    let rows = store.list_issues().unwrap_or_default();
    let Some(row) = rows.iter().find(|r| r.fingerprint == *fp) else {
        return (StatusCode::NOT_FOUND, "unknown fingerprint").into_response();
    };
    if row.state != "escalated" {
        return (
            StatusCode::CONFLICT,
            format!("state is {}, not escalated", row.state),
        )
            .into_response();
    }
    match store.set_state(fp, shanhaijing::State::New, crate::now_ms(), Some("reopened via web console"))
    {
        Ok(()) => Redirect::to("/ui?msg=reopened").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response(),
    }
}

pub async fn sync_now(State((store, _)): State<Shared>) -> Response {
    let Ok(token) = std::env::var("NUWA_GITHUB_TOKEN").or_else(|_| std::env::var("GITHUB_TOKEN")) else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "NUWA_GITHUB_TOKEN not set").into_response();
    };
    match crate::ci::reconcile_all(&store, &token, crate::now_ms()).await {
        Ok(lines) => {
            let body = if lines.is_empty() {
                "没有 pr_open 问题单需要对账".to_string()
            } else {
                lines.join("\n")
            };
            Html(format!(
                "<!doctype html><meta charset=\"utf-8\"><body style=\"font-family:system-ui;background:#0d1117;color:#e6edf3;padding:24px\">\
                 <h2>sync 对账结果</h2><pre style=\"white-space:pre-wrap\">{}</pre>\
                 <p><a href=\"/ui\">← 返回控制台</a></p>",
                crate::ui::esc(&body)
            ))
            .into_response()
        }
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
            [(header::WWW_AUTHENTICATE, r#"Basic realm="nuwa""#)],
            "auth required",
        )
            .into_response();
    }
    next.run(req).await
}

pub fn render_issue_detail(
    row: &shanhaijing::IssueRow,
    stack: Option<&str>,
    events: &[shanhaijing::EventRow],
    now: i64,
) -> String {
    let sc = state_color(&row.state);
    let mut timeline = String::new();
    for e in events {
        timeline.push_str(&format!(
            "<li><span class=\"mono dim\">{}</span> <b>{}</b> {} <span class=\"dim\">{}</span></li>",
            rel_time(e.ts, now),
            esc(&e.kind),
            if e.detail.is_empty() {
                String::new()
            } else {
                format!("<span class=\"dim\">— {}</span>", esc(&e.detail))
            },
            esc(&e.repo),
        ));
    }
    let timeline = if timeline.is_empty() {
        "<li class=\"dim\">暂无事件</li>".into()
    } else {
        format!("<ul class=\"feed\">{timeline}</ul>")
    };
    let stack_html = match stack {
        Some(s) if !s.is_empty() => format!("<pre class=\"stack\">{}</pre>", esc(s)),
        _ => "<p class=\"dim\">无堆栈样本</p>".to_string(),
    };
    let pr_html = row
        .pr_url
        .as_deref()
        .map(|u| format!("<a href=\"{}\" target=\"_blank\" rel=\"noopener\">{}</a>", esc(u), esc(u)))
        .unwrap_or_else(|| "<span class=\"dim\">-</span>".into());
    let action = if matches!(row.state.as_str(), "escalated" | "rejected") {
        format!(
            "<form method=\"post\" action=\"/ui/reopen\" style=\"display:inline\">\
             <input type=\"hidden\" name=\"fp\" value=\"{}\"><button>reopen 重新入队</button></form>",
            esc(&row.fingerprint)
        )
    } else {
        String::new()
    };
    format!(
        r#"<!doctype html><html lang="zh"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>{title} · Nuwa</title><style>
body{{font-family:system-ui,sans-serif;background:#0d1117;color:#e6edf3;margin:24px;max-width:980px;margin-inline:auto}}
a{{color:#58a6ff;text-decoration:none}}
h1{{font-size:20px}} h2{{font-size:14px;color:#8b949e;text-transform:uppercase;letter-spacing:.08em;margin:24px 0 10px}}
.badge{{color:#0d1117;padding:2px 9px;border-radius:10px;font-weight:600;font-size:12px}}
.mono{{font-family:ui-monospace,monospace;font-size:12px}} .dim{{color:#8b949e}}
.meta{{display:flex;gap:18px;flex-wrap:wrap;color:#8b949e;font-size:13px;margin:10px 0}}
pre.stack{{background:#161b22;border:1px solid #30363d;border-radius:8px;padding:14px;overflow-x:auto;font-size:12px;line-height:1.5}}
ul.feed{{list-style:none;padding:0;margin:0;font-size:13px}} ul.feed li{{padding:6px 0;border-bottom:1px solid #21262d}}
button{{background:#238636;color:#fff;border:0;border-radius:6px;padding:6px 14px;cursor:pointer}}
.card{{background:#161b22;border:1px solid #30363d;border-radius:8px;padding:12px 16px;margin-bottom:10px}}
</style></head><body>
<p><a href="/ui">← 控制台</a></p>
<h1>{title} <span class="badge" style="background:{sc}">{state}</span></h1>
<div class="meta">
  <span class="mono">{fp}</span><span>严重度：{sev}</span><span>出现 {occ} 次</span>
  <span>首现 {first}</span><span>最近 {last}</span><span>仓库：{repo}</span>
</div>
<h2>PR</h2><div class="card">{pr}</div>
<h2>堆栈样本</h2>
{stack_html}
<h2>生命周期</h2>
{timeline}
<p>{action}</p>
</body></html>"#,
        title = esc(&row.title),
        state = esc(&row.state),
        sc = sc,
        fp = esc(&row.fingerprint),
        sev = esc(&row.severity),
        occ = row.occurrences,
        first = rel_time(row.first_seen, now),
        last = rel_time(row.last_seen, now),
        repo = esc(&row.repo),
        pr = pr_html,
        stack_html = stack_html,
        timeline = timeline,
        action = action,
    )
}

/// 详情页 handler。
pub async fn issue_detail(
    State((store, _)): State<Shared>,
    axum::extract::Path(fp): axum::extract::Path<String>,
) -> Response {
    let rows = store.list_issues().unwrap_or_default();
    let Some(row) = rows.iter().find(|r| r.fingerprint == fp) else {
        return (StatusCode::NOT_FOUND, "unknown fingerprint").into_response();
    };
    let stack = store
        .get(&fp)
        .ok()
        .flatten()
        .map(|s| s.stack_sample)
        .unwrap_or_default();
    let events = store.events_for(&fp, 100).unwrap_or_default();
    Html(render_issue_detail(row, Some(&stack), &events, crate::now_ms())).into_response()
}

#[cfg(test)]
mod tests {
    use super::{basic_password_ok, esc, rel_time};

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
        assert_eq!(
            esc("<a href=\"x\">&</a>"),
            "&lt;a href=&quot;x&quot;&gt;&amp;&lt;/a&gt;"
        );
    }

    #[test]
    fn rel_time_buckets() {
        assert_eq!(rel_time(0, 30_000), "刚刚");
        assert_eq!(rel_time(0, 60_000), "1 分钟前");
        assert_eq!(rel_time(0, 150_000), "2 分钟前");
        assert_eq!(rel_time(0, 7_200_000), "2 小时前");
        assert_eq!(rel_time(0, 172_800_000), "2 天前");
    }
}
