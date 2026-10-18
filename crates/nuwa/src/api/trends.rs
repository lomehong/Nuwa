//! `GET /api/v1/metrics/trends` — UTC daily console buckets and range totals
//! (docs/09 §9.2). Buckets come from `Store::trends`; totals sum the buckets
//! and add the median run start → PR-opened time for the same range.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::api::common::{non_empty, DAY_MS};
use crate::api::ApiError;
use crate::serve::AppState;
use shanhaijing::{ShanResult, Store, TrendBucket};

const DEFAULT_RANGE: &str = "14d";
/// The four documented ranges; anything else is a 422 (docs/09 §9.2).
const RANGES: [(&str, u32); 4] = [("7d", 7), ("14d", 14), ("30d", 30), ("90d", 90)];

#[derive(Debug, Default, Deserialize)]
pub struct TrendsQuery {
    range: Option<String>,
    repo: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TrendsResponse {
    pub range: String,
    pub buckets: Vec<TrendBucket>,
    pub totals: TrendTotals,
}

#[derive(Debug, Default, PartialEq, Eq, Serialize)]
pub struct TrendTotals {
    pub reports: u64,
    pub new_issues: u64,
    pub prs_opened: u64,
    pub merged: u64,
    pub rejected: u64,
    pub tokens: u64,
    pub median_time_to_pr_secs: Option<u64>,
}

/// `GET /api/v1/metrics/trends?range=7d|14d|30d|90d&repo=`.
pub async fn trends(
    State(state): State<AppState>,
    Query(query): Query<TrendsQuery>,
) -> Response {
    let mut fields = BTreeMap::new();
    let (range, days) = parse_range(query.range.as_deref(), &mut fields);
    if !fields.is_empty() {
        return ApiError::validation(fields).into_response();
    }
    let repo = non_empty(query.repo.as_deref());
    let now = crate::now_ms();
    let store = Arc::clone(&state.store);
    match tokio::task::spawn_blocking(move || {
        build_trends(&store, range, days, repo.as_deref(), now)
    })
    .await
    {
        Ok(Ok(body)) => Json(body).into_response(),
        Ok(Err(error)) => {
            eprintln!("trends: store read failed: {error}");
            internal_error()
        }
        Err(error) => {
            eprintln!("trends: blocking task failed: {error}");
            internal_error()
        }
    }
}

/// Map the requested range to its day count; an unknown value leaves an
/// error in `fields` and falls back to the default so the type stays simple.
fn parse_range(raw: Option<&str>, fields: &mut BTreeMap<String, String>) -> (&'static str, u32) {
    let requested = non_empty(raw).unwrap_or_else(|| DEFAULT_RANGE.to_string());
    match RANGES.iter().find(|(label, _)| *label == requested) {
        Some((label, days)) => (label, *days),
        None => {
            fields.insert(
                "range".to_string(),
                format!("must be one of 7d, 14d, 30d or 90d, got {requested}"),
            );
            (DEFAULT_RANGE, 14)
        }
    }
}

fn build_trends(
    store: &Store,
    range: &'static str,
    days: u32,
    repo: Option<&str>,
    now: i64,
) -> ShanResult<TrendsResponse> {
    let buckets = store.trends(days, repo, now)?;
    let totals = total_buckets(&buckets);
    let today_start = now - now.rem_euclid(DAY_MS);
    let window_start = today_start - (i64::from(days) - 1) * DAY_MS;
    let window_end = today_start + DAY_MS;
    let median = median_time_to_pr_secs(store, repo, window_start, window_end)?;
    Ok(TrendsResponse {
        range: range.to_string(),
        buckets,
        totals: TrendTotals {
            median_time_to_pr_secs: median,
            ..totals
        },
    })
}

fn total_buckets(buckets: &[TrendBucket]) -> TrendTotals {
    let mut totals = TrendTotals::default();
    for bucket in buckets {
        totals.reports = totals.reports.saturating_add(bucket.reports);
        totals.new_issues = totals.new_issues.saturating_add(bucket.new_issues);
        totals.prs_opened = totals.prs_opened.saturating_add(bucket.prs_opened);
        totals.merged = totals.merged.saturating_add(bucket.merged);
        totals.rejected = totals.rejected.saturating_add(bucket.rejected);
        totals.tokens = totals.tokens.saturating_add(bucket.tokens);
    }
    totals
}

/// Median `runs.started_at → runs.pr_opened_at`, in whole seconds, for PRs
/// opened inside the trend window; runs without a PR timestamp are ignored.
/// Runs are read per issue through the existing Store helpers (single-owner
/// scale, R25) so this task adds no new Store query.
fn median_time_to_pr_secs(
    store: &Store,
    repo: Option<&str>,
    window_start: i64,
    window_end: i64,
) -> ShanResult<Option<u64>> {
    let mut durations: Vec<i64> = Vec::new();
    for issue in store.list_issues()? {
        if repo.is_some_and(|repo| issue.repo != repo) {
            continue;
        }
        for run in store.runs_for(&issue.fingerprint, u32::MAX)? {
            if repo.is_some_and(|repo| run.repo != repo) {
                continue;
            }
            let Some(pr_opened_at) = run.pr_opened_at else {
                continue;
            };
            if pr_opened_at < window_start || pr_opened_at >= window_end {
                continue;
            }
            durations.push(pr_opened_at.saturating_sub(run.started_at).max(0) / 1_000);
        }
    }
    if durations.is_empty() {
        return Ok(None);
    }
    durations.sort_unstable();
    let mid = durations.len() / 2;
    let median = if durations.len() % 2 == 1 {
        durations[mid]
    } else {
        (durations[mid - 1] + durations[mid]) / 2
    };
    Ok(Some(u64::try_from(median).unwrap_or(0)))
}

fn internal_error() -> Response {
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal",
        "failed to read trends",
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
    const DAY_MS: i64 = 86_400_000;
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

    #[allow(clippy::too_many_arguments)]
    fn run(
        store: &shanhaijing::Store,
        id: &str,
        fingerprint: &str,
        repo: &str,
        started_at: i64,
        tokens: (i64, i64),
        pr: Option<(&str, i64)>,
    ) {
        store
            .start_run(&shanhaijing::RunStart {
                id: id.to_string(),
                fingerprint: fingerprint.to_string(),
                runner_id: "host:runner".to_string(),
                repo: repo.to_string(),
                started_at,
                provider: None,
                model: None,
            })
            .expect("start run");
        store
            .update_run(
                id,
                &shanhaijing::RunUpdate {
                    tokens_uncached: Some(tokens.0),
                    tokens_cached: Some(tokens.1),
                    pr_url: pr.map(|(url, _)| url.to_string()),
                    pr_opened_at: pr.map(|(_, at)| at),
                    ..shanhaijing::RunUpdate::default()
                },
            )
            .expect("update run");
    }

    /// Seeds today, 10, 20 and 40 days ago, plus a second repo:
    /// - reports: 2 today, 1 on each older day, 1 in the other repo today;
    /// - PRs: runs A/B today (60s and 120s to PR), legacy issue PRs on d10
    ///   and d20 with no run, a token-only run for the other repo;
    /// - terminal events: merged+rejected today, rejected d10, merged d20.
    ///
    /// Returns `today`'s UTC midnight.
    fn seed(store: &shanhaijing::Store) -> i64 {
        let now = crate::now_ms();
        let today = now - now.rem_euclid(DAY_MS);
        let d10 = today - 10 * DAY_MS;
        let d20 = today - 20 * DAY_MS;
        let d40 = today - 40 * DAY_MS;

        let fp_a = report(store, "alpha", APP, today + 30 * 60_000);
        let fp_b = report(store, "beta", APP, today + 40 * 60_000);
        let fp_c = report(store, "gamma", APP, d10 + 30 * 60_000);
        let fp_d = report(store, "delta", APP, d20 + 30 * 60_000);
        report(store, "epsilon", APP, d40 + 30 * 60_000);
        let fp_other = report(store, "other", OTHER, today + 50 * 60_000);

        let url_a = "https://github.com/acme/app/pull/1";
        let url_b = "https://github.com/acme/app/pull/2";
        run(
            store,
            "run-a",
            &fp_a,
            APP,
            today + HOUR_MS,
            (100, 50),
            Some((url_a, today + HOUR_MS + 60_000)),
        );
        run(
            store,
            "run-b",
            &fp_b,
            APP,
            today + 2 * HOUR_MS,
            (20, 0),
            Some((url_b, today + 2 * HOUR_MS + 120_000)),
        );
        run(store, "run-c", &fp_a, APP, today + 3 * HOUR_MS, (5, 0), None);
        run(store, "run-d", &fp_c, APP, d10 + 4 * HOUR_MS, (100, 0), None);
        run(store, "run-e", &fp_d, APP, d20 + 4 * HOUR_MS, (30, 0), None);
        run(
            store,
            "run-other",
            &fp_other,
            OTHER,
            today + 6 * HOUR_MS,
            (999, 0),
            None,
        );

        // Pre-run legacy PRs: only the issue row carries them.
        store
            .set_pr(&fp_c, "https://github.com/acme/app/pull/3", d10 + 5 * HOUR_MS)
            .expect("legacy pr d10");
        store
            .set_pr(&fp_d, "https://github.com/acme/app/pull/4", d20 + 5 * HOUR_MS)
            .expect("legacy pr d20");

        let event = |fp: &str, ts: i64, actor: &str, kind: &str, detail: &str| {
            store
                .record_event(fp, ts, Some(APP), actor, kind, detail)
                .expect("record event");
        };
        event(&fp_a, today + 7 * HOUR_MS, "system", "merged", url_a);
        event(&fp_b, today + 8 * HOUR_MS, "runner", "rejected", "ci_red: PR #2 closed");
        event(&fp_c, d10 + 6 * HOUR_MS, "system", "rejected", "PR closed without merge");
        event(&fp_d, d20 + 6 * HOUR_MS, "system", "merged", "https://github.com/acme/app/pull/4");
        event(&fp_a, today + 9 * HOUR_MS, "runner", "ci_green", "head");
        today
    }

    fn bucket<'a>(value: &'a serde_json::Value, day: &str) -> &'a serde_json::Value {
        value["buckets"]
            .as_array()
            .expect("buckets")
            .iter()
            .find(|bucket| bucket["day"].as_str() == Some(day))
            .unwrap_or_else(|| panic!("missing bucket {day}: {value}"))
    }

    fn totals(value: &serde_json::Value) -> (u64, u64, u64, u64, u64, u64) {
        let totals = &value["totals"];
        let number = |key: &str| {
            totals[key]
                .as_u64()
                .unwrap_or_else(|| panic!("totals.{key} missing: {value}"))
        };
        (
            number("reports"),
            number("new_issues"),
            number("prs_opened"),
            number("merged"),
            number("rejected"),
            number("tokens"),
        )
    }

    #[tokio::test]
    async fn trends_return_buckets_totals_median_and_the_default_range() {
        let (_dir, state) = test_state();
        let today = seed(&state.store);
        let app = build_app(state);

        let response = get(&app, "/api/v1/metrics/trends?repo=example.com%2Fapp").await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_api_headers(&response);
        let value = json_body(response).await;
        assert_eq!(value["range"].as_str(), Some("14d"), "default range");

        let buckets = value["buckets"].as_array().expect("buckets");
        assert_eq!(buckets.len(), 14);
        assert_eq!(
            buckets[0]["day"].as_str(),
            Some(shanhaijing::utc_day(today - 13 * DAY_MS).as_str())
        );
        assert_eq!(
            buckets[13]["day"].as_str(),
            Some(shanhaijing::utc_day(today).as_str())
        );

        // A day without data is zero-filled.
        let empty = bucket(&value, &shanhaijing::utc_day(today - DAY_MS));
        assert_eq!(
            empty,
            &serde_json::json!({
                "day": shanhaijing::utc_day(today - DAY_MS),
                "reports": 0, "new_issues": 0, "prs_opened": 0, "merged": 0,
                "rejected": 0, "tokens": 0, "ci_green": 0, "ci_red": 0,
            })
        );

        let today_bucket = bucket(&value, &shanhaijing::utc_day(today));
        assert_eq!(today_bucket["reports"].as_u64(), Some(2));
        assert_eq!(today_bucket["new_issues"].as_u64(), Some(2));
        assert_eq!(today_bucket["prs_opened"].as_u64(), Some(2));
        assert_eq!(today_bucket["merged"].as_u64(), Some(1));
        assert_eq!(today_bucket["rejected"].as_u64(), Some(1));
        assert_eq!(today_bucket["tokens"].as_u64(), Some(175));
        assert_eq!(today_bucket["ci_green"].as_u64(), Some(1));

        let d10_bucket = bucket(&value, &shanhaijing::utc_day(today - 10 * DAY_MS));
        assert_eq!(d10_bucket["reports"].as_u64(), Some(1));
        assert_eq!(d10_bucket["new_issues"].as_u64(), Some(1));
        assert_eq!(
            d10_bucket["prs_opened"].as_u64(),
            Some(1),
            "the legacy issue PR counts without a run"
        );
        assert_eq!(d10_bucket["rejected"].as_u64(), Some(1));
        assert_eq!(d10_bucket["tokens"].as_u64(), Some(100));

        assert_eq!(totals(&value), (3, 3, 3, 1, 2, 275));
        assert_eq!(
            value["totals"]["median_time_to_pr_secs"].as_u64(),
            Some(90),
            "(60s + 120s) / 2"
        );
    }

    #[tokio::test]
    async fn trends_ranges_clamp_and_scope_totals() {
        let (_dir, state) = test_state();
        let today = seed(&state.store);
        let app = build_app(state);

        for (range, bucket_count, expected) in [
            ("7d", 7, (2, 2, 2, 1, 1, 175)),
            ("14d", 14, (3, 3, 3, 1, 2, 275)),
            ("30d", 30, (4, 4, 4, 2, 2, 305)),
            ("90d", 90, (5, 5, 4, 2, 2, 305)),
        ] {
            let uri = format!("/api/v1/metrics/trends?range={range}&repo=example.com%2Fapp");
            let value = json_body(get(&app, &uri).await).await;
            assert_eq!(value["range"].as_str(), Some(range));
            let buckets = value["buckets"].as_array().expect("buckets");
            assert_eq!(buckets.len(), bucket_count, "{range}");
            assert_eq!(
                buckets[0]["day"].as_str(),
                Some(shanhaijing::utc_day(today - (bucket_count as i64 - 1) * DAY_MS).as_str()),
                "{range} oldest day"
            );
            assert_eq!(totals(&value), expected, "{range}");
            assert_eq!(
                value["totals"]["median_time_to_pr_secs"].as_u64(),
                Some(90),
                "{range}"
            );
        }

        // The d20 bucket only exists from 30d on.
        let value = json_body(
            get(
                &app,
                "/api/v1/metrics/trends?range=30d&repo=example.com%2Fapp",
            )
            .await,
        )
        .await;
        let d20 = bucket(&value, &shanhaijing::utc_day(today - 20 * DAY_MS));
        assert_eq!(d20["prs_opened"].as_u64(), Some(1));
        assert_eq!(d20["merged"].as_u64(), Some(1));
        assert_eq!(d20["tokens"].as_u64(), Some(30));

        // Without a repo filter the other repo's report and tokens join in.
        let value = json_body(get(&app, "/api/v1/metrics/trends?range=7d").await).await;
        assert_eq!(totals(&value), (3, 3, 2, 1, 1, 1174));
        let other = bucket(&value, &shanhaijing::utc_day(today));
        assert_eq!(other["reports"].as_u64(), Some(3));
        assert_eq!(other["tokens"].as_u64(), Some(1174));
    }

    #[tokio::test]
    async fn trends_reject_unknown_ranges_and_empty_databases() {
        let (_dir, state) = test_state();
        let app = build_app(state);

        for range in ["1d", "weekly", "14D", "0d"] {
            let uri = format!("/api/v1/metrics/trends?range={range}");
            let response = get(&app, &uri).await;
            assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY, "{range}");
            assert_api_headers(&response);
            let value = json_body(response).await;
            assert_eq!(value["error"]["code"], "validation_failed");
            assert!(
                value["error"]["fields"]["range"].is_string(),
                "{range}: {value}"
            );
        }

        // An empty store still returns the full zero-filled range.
        let value = json_body(get(&app, "/api/v1/metrics/trends?range=7d").await).await;
        assert_eq!(value["range"].as_str(), Some("7d"));
        let buckets = value["buckets"].as_array().expect("buckets");
        assert_eq!(buckets.len(), 7);
        assert!(buckets.iter().all(|bucket| bucket["reports"] == 0));
        assert_eq!(totals(&value), (0, 0, 0, 0, 0, 0));
        assert_eq!(
            value["totals"]["median_time_to_pr_secs"],
            serde_json::Value::Null
        );
    }
}
