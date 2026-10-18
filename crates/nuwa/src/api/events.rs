//! `GET /api/v1/events` — the console activity feed (docs/09 §9.2): filtered
//! lifecycle events, with optional hour aggregation of historical `reported`
//! rows and 1-based pagination applied after grouping.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::api::common::{
    non_empty, parse_flag, parse_ms, parse_page_number, PageMeta, DEFAULT_PAGE_SIZE,
    MAX_PAGE_SIZE,
};
use crate::api::ApiError;
use crate::serve::AppState;
use shanhaijing::{EventQuery, EventRow, ShanResult, Store};

/// Hard `list_events` cap: grouping and pagination never scan more rows.
const EVENT_SCAN_LIMIT: u32 = 5_000;
const HOUR_MS: i64 = 3_600_000;

/// Raw feed query. Values stay strings so invalid input maps to the JSON
/// `validation_failed` contract instead of axum's plain-text rejection.
#[derive(Debug, Default, Deserialize)]
pub struct EventListQuery {
    kind: Option<String>,
    repo: Option<String>,
    fingerprint: Option<String>,
    from_ms: Option<String>,
    to_ms: Option<String>,
    aggregate: Option<String>,
    page: Option<String>,
    page_size: Option<String>,
}

struct ListParams {
    kinds: Vec<String>,
    repo: Option<String>,
    fingerprint: Option<String>,
    from_ms: Option<i64>,
    to_ms: Option<i64>,
    aggregate: bool,
    page: u64,
    page_size: u64,
}

impl ListParams {
    fn parse(query: &EventListQuery) -> Result<Self, ApiError> {
        let mut fields = BTreeMap::new();
        let kinds = parse_kinds(query.kind.as_deref());
        let repo = non_empty(query.repo.as_deref());
        let fingerprint = non_empty(query.fingerprint.as_deref());
        let from_ms = parse_ms("from_ms", query.from_ms.as_deref(), &mut fields);
        let to_ms = parse_ms("to_ms", query.to_ms.as_deref(), &mut fields);
        let aggregate = parse_flag("aggregate", query.aggregate.as_deref(), &mut fields);
        let page = parse_page_number("page", query.page.as_deref(), 1, None, &mut fields);
        let page_size = parse_page_number(
            "page_size",
            query.page_size.as_deref(),
            DEFAULT_PAGE_SIZE,
            Some(MAX_PAGE_SIZE),
            &mut fields,
        );
        if !fields.is_empty() {
            return Err(ApiError::validation(fields));
        }
        Ok(Self {
            kinds,
            repo,
            fingerprint,
            from_ms,
            to_ms,
            aggregate,
            page,
            page_size,
        })
    }
}

/// `kind=a,b` — trimmed, empty segments ignored; unknown kinds are allowed
/// (the journal grows; a typo simply returns no rows).
fn parse_kinds(raw: Option<&str>) -> Vec<String> {
    let Some(raw) = raw else {
        return Vec::new();
    };
    let mut kinds = BTreeSet::new();
    for part in raw.split(',') {
        let part = part.trim();
        if !part.is_empty() {
            kinds.insert(part.to_string());
        }
    }
    kinds.into_iter().collect()
}

#[derive(Debug, Serialize)]
pub struct EventListResponse {
    pub data: Vec<EventItem>,
    pub meta: PageMeta,
}

/// One feed row. Raw rows carry `count=1` and `first_ts_ms = last_ts_ms =
/// ts_ms`; aggregated `reported` rows carry the group size and hour bounds.
#[derive(Debug, Serialize)]
pub struct EventItem {
    pub fingerprint: String,
    pub kind: String,
    pub detail: String,
    #[serde(rename = "ts_ms")]
    pub ts: i64,
    pub repo: String,
    pub actor: Option<String>,
    pub count: u64,
    #[serde(rename = "first_ts_ms")]
    pub first_ts: i64,
    #[serde(rename = "last_ts_ms")]
    pub last_ts: i64,
}

impl EventItem {
    fn raw(event: EventRow) -> Self {
        Self {
            fingerprint: event.fingerprint,
            kind: event.kind,
            detail: event.detail,
            ts: event.ts,
            repo: event.repo,
            actor: event.actor,
            count: 1,
            first_ts: event.ts,
            last_ts: event.ts,
        }
    }
}

/// `GET /api/v1/events` — newest first, optional hour aggregation.
pub async fn list(
    State(state): State<AppState>,
    Query(query): Query<EventListQuery>,
) -> Response {
    let params = match ListParams::parse(&query) {
        Ok(params) => params,
        Err(error) => return error.into_response(),
    };
    let store = Arc::clone(&state.store);
    match tokio::task::spawn_blocking(move || build_events(&store, &params)).await {
        Ok(Ok(body)) => Json(body).into_response(),
        Ok(Err(error)) => {
            eprintln!("events: store read failed: {error}");
            internal_error()
        }
        Err(error) => {
            eprintln!("events: blocking task failed: {error}");
            internal_error()
        }
    }
}

fn build_events(store: &Store, params: &ListParams) -> ShanResult<EventListResponse> {
    let query = EventQuery {
        kinds: params.kinds.clone(),
        repo: params.repo.clone(),
        fingerprint: params.fingerprint.clone(),
        from_ms: params.from_ms,
        to_ms: params.to_ms,
    };
    let rows = store.list_events(&query, EVENT_SCAN_LIMIT)?;
    let mut items: Vec<EventItem> = if params.aggregate {
        aggregate_reported(rows)
    } else {
        rows.into_iter().map(EventItem::raw).collect()
    };
    // Newest first by the row's latest timestamp; the remaining keys keep
    // pagination stable when timestamps tie.
    items.sort_by(|left, right| {
        right
            .last_ts
            .cmp(&left.last_ts)
            .then_with(|| left.fingerprint.cmp(&right.fingerprint))
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.detail.cmp(&right.detail))
    });

    let total = items.len();
    // A huge page number must be an empty page, never an overflow/panic.
    let start = (params.page - 1)
        .checked_mul(params.page_size)
        .and_then(|offset| usize::try_from(offset).ok())
        .unwrap_or(usize::MAX);
    let data = if start >= total {
        Vec::new()
    } else {
        items
            .into_iter()
            .skip(start)
            .take(params.page_size as usize)
            .collect()
    };
    Ok(EventListResponse {
        data,
        meta: PageMeta {
            page: params.page,
            page_size: params.page_size,
            total,
        },
    })
}

/// Group `reported` rows by fingerprint + UTC hour; every other kind passes
/// through raw. `rows` arrive newest first, so the first row of a group also
/// supplies the representative detail/actor/repo.
fn aggregate_reported(rows: Vec<EventRow>) -> Vec<EventItem> {
    const AGGREGATED_KIND: &str = "reported";
    let mut raw = Vec::new();
    let mut groups: BTreeMap<(String, i64), EventItem> = BTreeMap::new();
    for row in rows {
        if row.kind != AGGREGATED_KIND {
            raw.push(EventItem::raw(row));
            continue;
        }
        let hour = row.ts.div_euclid(HOUR_MS) * HOUR_MS;
        let key = (row.fingerprint.clone(), hour);
        match groups.get_mut(&key) {
            Some(group) => {
                group.count += 1;
                group.first_ts = group.first_ts.min(row.ts);
                group.last_ts = group.last_ts.max(row.ts);
            }
            None => {
                groups.insert(
                    key,
                    EventItem {
                        fingerprint: row.fingerprint,
                        kind: row.kind,
                        detail: row.detail,
                        ts: row.ts,
                        repo: row.repo,
                        actor: row.actor,
                        count: 1,
                        first_ts: row.ts,
                        last_ts: row.ts,
                    },
                );
            }
        }
    }
    let mut items: Vec<EventItem> = groups.into_values().collect();
    items.extend(raw);
    items
}

fn internal_error() -> Response {
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal",
        "failed to read events",
    )
    .into_response()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{header, Request, StatusCode};
    use axum::response::Response;
    use axum::Router;
    use tower::ServiceExt;

    use crate::serve::{build_app, AppState};

    const APP: &str = "example.com/app";
    const OTHER: &str = "example.com/other";
    const HOUR_MS: i64 = 3_600_000;
    const STRICT_CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; font-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

    fn test_state() -> (tempfile::TempDir, AppState) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(shanhaijing::Store::open(dir.path().join("s.db")).expect("open"));
        let state = AppState {
            store,
            ui_token: Arc::new(None),
            ingest_token: Arc::new(Some("ingest-secret".into())),
            session: Arc::new(crate::session::SessionManager::new("", 86_400_000)),
            limiter: Arc::new(crate::session::LoginLimiter::new()),
            started_at_ms: crate::now_ms(),
            cookie_secure: false,
            serve_unit: "nuwa-serve".to_string(),
            runner_units: vec!["nuwa-work".to_string()],
        };
        (dir, state)
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

    async fn json_body(response: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        serde_json::from_slice(&bytes).expect("json body")
    }

    fn header_str(response: &Response, name: header::HeaderName) -> String {
        response
            .headers()
            .get(&name)
            .unwrap_or_else(|| panic!("missing header {name}"))
            .to_str()
            .expect("ascii header")
            .to_string()
    }

    fn assert_api_headers(response: &Response) {
        assert_eq!(header_str(response, header::CACHE_CONTROL), "no-store");
        assert_eq!(
            header_str(response, header::CONTENT_SECURITY_POLICY),
            STRICT_CSP
        );
        assert_eq!(
            header_str(response, header::X_CONTENT_TYPE_OPTIONS),
            "nosniff"
        );
    }

    fn report(store: &shanhaijing::Store, title: &str, repo: &str, at: i64) -> String {
        let signal = shanhaijing::Signal {
            repo: repo.to_string(),
            language: "go".to_string(),
            title: title.to_string(),
            severity: "high".to_string(),
            message: Some(format!("boom {title}")),
            stack_trace: format!(
                "panic: {title}\ngoroutine 1 [running]:\nexample.com/app.Fn_{}()\n\t/app/x.go:1 +0x1",
                title.replace(' ', "_")
            ),
        };
        match store.report_signal(&signal, at).expect("report signal") {
            shanhaijing::ReportOutcome::NewIssue { fingerprint } => fingerprint,
            other => panic!("{title} must be a new issue, got {other:?}"),
        }
    }

    struct Seed {
        hour: i64,
        t1: i64,
        t3: i64,
        t_state: i64,
        t_next: i64,
        t_b: i64,
        fp_a: String,
        fp_b: String,
    }

    /// One hour of interleaved events for fingerprint A (three `reported`
    /// rows, a `state_changed` between them, one report in the next hour),
    /// a report for fingerprint B, a legacy NULL-repo event that resolves
    /// through the issue join, an event in another repo and a report outside
    /// the window. Frames come from fixed epoch timestamps, not the clock.
    fn seed(store: &shanhaijing::Store) -> Seed {
        let base = 1_700_000_000_000i64;
        let hour = base - base.rem_euclid(HOUR_MS);
        let t1 = hour + 10 * 60_000;
        let t2 = hour + 20 * 60_000;
        let t3 = hour + 50 * 60_000;
        let t_state = hour + 30 * 60_000;
        let t_next = hour + 61 * 60_000;
        let t_b = hour + 15 * 60_000;
        let t_c = hour + 5 * 60_000;
        let t_old = hour - 24 * HOUR_MS;

        let fp_a = report(store, "alpha", APP, t_old);
        let fp_b = report(store, "beta", APP, t_old);
        let fp_c = report(store, "gamma", OTHER, t_old);

        let record = |fp: &str, ts: i64, repo: Option<&str>, actor: &str, kind: &str, detail: &str| {
            store
                .record_event(fp, ts, repo, actor, kind, detail)
                .expect("record event");
        };
        record(&fp_a, t1, Some(APP), "system", "reported", "r1");
        record(&fp_a, t_state, Some(APP), "runner", "state_changed", "claimed");
        record(&fp_a, t2, Some(APP), "system", "reported", "r2");
        record(&fp_a, t3, Some(APP), "system", "reported", "r3");
        record(&fp_a, t_next, Some(APP), "system", "reported", "r-next");
        record(&fp_b, t_b, Some(APP), "system", "reported", "b1");
        record(&fp_c, t_c, Some(OTHER), "system", "reported", "c1");
        record(&fp_a, t_old, Some(APP), "system", "reported", "old");
        // Legacy row without a repo column: list_events resolves APP via the
        // issue join.
        record(&fp_b, t_b + 1_000, None, "system", "ci_green", "legacy");

        Seed {
            hour,
            t1,
            t3,
            t_state,
            t_next,
            t_b,
            fp_a,
            fp_b,
        }
    }

    fn items(value: &serde_json::Value) -> &Vec<serde_json::Value> {
        value["data"].as_array().expect("event data")
    }

    #[tokio::test]
    async fn events_filter_repo_kind_fingerprint_and_window() {
        let (_dir, state) = test_state();
        let seed = seed(&state.store);
        let app = build_app(state);

        // Repo filter + window start; every raw row carries count=1 and
        // first=last=ts. The legacy NULL-repo row resolves through the issue.
        let uri = format!(
            "/api/v1/events?repo=example.com%2Fapp&from_ms={}",
            seed.hour
        );
        let response = get(&app, &uri).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_api_headers(&response);
        let value = json_body(response).await;
        assert_eq!(
            value["meta"],
            serde_json::json!({ "page": 1, "page_size": 50, "total": 7 }),
            "legacy NULL-repo rows resolve and the out-of-window row is dropped"
        );
        let rows = items(&value);
        assert_eq!(rows.len(), 7);
        assert_eq!(rows[0]["fingerprint"].as_str(), Some(seed.fp_a.as_str()));
        assert_eq!(rows[0]["kind"].as_str(), Some("reported"));
        assert_eq!(rows[0]["detail"].as_str(), Some("r-next"));
        assert_eq!(rows[0]["ts_ms"].as_i64(), Some(seed.t_next));
        assert_eq!(rows[0]["repo"].as_str(), Some(APP));
        assert_eq!(rows[0]["actor"].as_str(), Some("system"));
        assert_eq!(rows[0]["count"].as_u64(), Some(1));
        assert_eq!(rows[0]["first_ts_ms"].as_i64(), Some(seed.t_next));
        assert_eq!(rows[0]["last_ts_ms"].as_i64(), Some(seed.t_next));
        for row in rows {
            assert_eq!(row["count"].as_u64(), Some(1), "raw rows are not grouped");
            assert_eq!(row["first_ts_ms"], row["ts_ms"]);
            assert_eq!(row["last_ts_ms"], row["ts_ms"]);
        }
        let legacy = rows
            .iter()
            .find(|row| row["kind"].as_str() == Some("ci_green"))
            .expect("legacy row");
        assert_eq!(legacy["repo"].as_str(), Some(APP));
        assert_eq!(legacy["fingerprint"].as_str(), Some(seed.fp_b.as_str()));
        assert_eq!(legacy["ts_ms"].as_i64(), Some(seed.t_b + 1_000));
        assert!(
            rows.windows(2)
                .all(|pair| pair[0]["ts_ms"].as_i64() >= pair[1]["ts_ms"].as_i64()),
            "events are newest first"
        );

        // kind is a comma list; state_changed and ci_green drop out.
        let uri = format!(
            "/api/v1/events?repo=example.com%2Fapp&from_ms={}&kind=reported",
            seed.hour
        );
        let value = json_body(get(&app, &uri).await).await;
        assert_eq!(value["meta"]["total"].as_u64(), Some(5));
        assert!(items(&value)
            .iter()
            .all(|row| row["kind"].as_str() == Some("reported")));

        // fingerprint narrows to A's five rows.
        let uri = format!(
            "/api/v1/events?repo=example.com%2Fapp&from_ms={}&fingerprint={}",
            seed.hour, seed.fp_a
        );
        let value = json_body(get(&app, &uri).await).await;
        assert_eq!(value["meta"]["total"].as_u64(), Some(5));
        assert!(items(&value)
            .iter()
            .all(|row| row["fingerprint"].as_str() == Some(seed.fp_a.as_str())));

        // to_ms is inclusive: state_changed and everything older.
        let uri = format!(
            "/api/v1/events?repo=example.com%2Fapp&from_ms={}&to_ms={}",
            seed.hour, seed.t_state
        );
        let value = json_body(get(&app, &uri).await).await;
        assert_eq!(value["meta"]["total"].as_u64(), Some(5));
        assert_eq!(items(&value)[0]["ts_ms"].as_i64(), Some(seed.t_state));

        // Other repo sees only its own event.
        let value = json_body(
            get(
                &app,
                &format!("/api/v1/events?repo=example.com%2Fother&from_ms={}", seed.hour),
            )
            .await,
        )
        .await;
        assert_eq!(value["meta"]["total"].as_u64(), Some(1));
        assert_eq!(items(&value)[0]["repo"].as_str(), Some(OTHER));
    }

    #[tokio::test]
    async fn events_aggregate_reported_rows_by_fingerprint_and_hour() {
        let (_dir, state) = test_state();
        let seed = seed(&state.store);
        let app = build_app(state);

        let uri = format!(
            "/api/v1/events?repo=example.com%2Fapp&from_ms={}&aggregate=1",
            seed.hour
        );
        let response = get(&app, &uri).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_api_headers(&response);
        let value = json_body(response).await;
        assert_eq!(
            value["meta"],
            serde_json::json!({ "page": 1, "page_size": 50, "total": 5 }),
            "three reports collapse into one row"
        );
        let rows = items(&value);

        // Newest first by latest ts: next-hour report, the A group, the
        // interleaved state_changed, then the legacy and B rows.
        assert_eq!(rows[0]["fingerprint"].as_str(), Some(seed.fp_a.as_str()));
        assert_eq!(rows[0]["kind"].as_str(), Some("reported"));
        assert_eq!(rows[0]["detail"].as_str(), Some("r-next"));
        assert_eq!(rows[0]["count"].as_u64(), Some(1));
        assert_eq!(rows[0]["ts_ms"].as_i64(), Some(seed.t_next));

        let group = &rows[1];
        assert_eq!(group["fingerprint"].as_str(), Some(seed.fp_a.as_str()));
        assert_eq!(group["kind"].as_str(), Some("reported"));
        assert_eq!(group["count"].as_u64(), Some(3));
        assert_eq!(group["first_ts_ms"].as_i64(), Some(seed.t1));
        assert_eq!(group["last_ts_ms"].as_i64(), Some(seed.t3));
        assert_eq!(group["ts_ms"].as_i64(), Some(seed.t3));
        assert_eq!(group["detail"].as_str(), Some("r3"), "newest detail wins");
        assert_eq!(group["repo"].as_str(), Some(APP));

        assert_eq!(rows[2]["kind"].as_str(), Some("state_changed"));
        assert_eq!(rows[2]["count"].as_u64(), Some(1));
        assert_eq!(rows[3]["kind"].as_str(), Some("ci_green"));
        assert_eq!(rows[4]["fingerprint"].as_str(), Some(seed.fp_b.as_str()));

        // Aggregation plus the kind filter: only the three reported rows.
        let uri = format!(
            "/api/v1/events?repo=example.com%2Fapp&from_ms={}&aggregate=1&kind=reported",
            seed.hour
        );
        let value = json_body(get(&app, &uri).await).await;
        assert_eq!(value["meta"]["total"].as_u64(), Some(3));
        let rows = items(&value);
        assert_eq!(rows[0]["count"].as_u64(), Some(1), "next-hour report first");
        let group = rows
            .iter()
            .find(|row| row["last_ts_ms"].as_i64() == Some(seed.t3))
            .expect("same-hour group");
        assert_eq!(group["count"].as_u64(), Some(3));

        // Pagination runs after grouping: page 2 holds rows 3 and 4.
        let uri = format!(
            "/api/v1/events?repo=example.com%2Fapp&from_ms={}&aggregate=1&page=2&page_size=2",
            seed.hour
        );
        let value = json_body(get(&app, &uri).await).await;
        assert_eq!(
            value["meta"],
            serde_json::json!({ "page": 2, "page_size": 2, "total": 5 })
        );
        let rows = items(&value);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["kind"].as_str(), Some("state_changed"));
        assert_eq!(rows[1]["kind"].as_str(), Some("ci_green"));

        // A page past the end is empty but keeps the real total.
        let uri = format!(
            "/api/v1/events?repo=example.com%2Fapp&from_ms={}&aggregate=1&page=4&page_size=2",
            seed.hour
        );
        let value = json_body(get(&app, &uri).await).await;
        assert!(items(&value).is_empty());
        assert_eq!(value["meta"]["total"].as_u64(), Some(5));
    }

    #[tokio::test]
    async fn events_reject_invalid_query_fields() {
        let (_dir, state) = test_state();
        let app = build_app(state);

        for (uri, field) in [
            ("/api/v1/events?page=0", "page"),
            ("/api/v1/events?page_size=201", "page_size"),
            ("/api/v1/events?aggregate=yes", "aggregate"),
            ("/api/v1/events?from_ms=abc", "from_ms"),
            ("/api/v1/events?to_ms=abc", "to_ms"),
        ] {
            let response = get(&app, uri).await;
            assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY, "{uri}");
            assert_api_headers(&response);
            let value = json_body(response).await;
            assert_eq!(value["error"]["code"], "validation_failed", "{uri}");
            assert!(
                value["error"]["fields"][field].is_string(),
                "{uri}: {value}"
            );
        }

        // Every bad field is reported at once.
        let response = get(&app, "/api/v1/events?page=0&page_size=999").await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let value = json_body(response).await;
        assert!(value["error"]["fields"]["page"].is_string());
        assert!(value["error"]["fields"]["page_size"].is_string());
    }
}
