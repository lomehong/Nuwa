//! `GET /api/v1/issues/{fp}/transcript` — parsed repair session transcript
//! (docs/09 §9.2). The session path comes from the run row, never from the
//! request; only `kind=entry,type=message` JSONL records are mapped.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Component, Path as FsPath, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::api::common::parse_page_number;
use crate::api::ApiError;
use crate::serve::AppState;
use shanhaijing::{ShanResult, Store};

/// Transcript pagination (docs/09 §9.2): 20 messages per page, 100 at most.
const DEFAULT_PAGE_SIZE: u64 = 20;
const MAX_PAGE_SIZE: u64 = 100;
/// Only the tail of a session file is served; a larger file sets `truncated`.
const TAIL_BYTES: u64 = 2 * 1024 * 1024;
/// Per-block byte budget (16KB of UTF-8 content before the ellipsis). Applied
/// to text, thinking, tool_result text and serialized tool_call arguments.
const MAX_BLOCK_BYTES: usize = 16 * 1024;
/// Session lookup bounds: the layout is `<run dir>/<cwd slug>/<ts>_<id>.jsonl`.
const MAX_SESSION_DEPTH: usize = 4;
const MAX_SESSION_ENTRIES: usize = 1_000;

/// Raw transcript query. Values stay strings so invalid input maps to the
/// `validation_failed` contract instead of axum's plain-text rejection.
#[derive(Debug, Default, Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct TranscriptQuery {
    run_id: Option<String>,
    page: Option<String>,
    page_size: Option<String>,
}

struct Params {
    run_id: Option<String>,
    page: u64,
    page_size: u64,
}

impl Params {
    fn parse(query: &TranscriptQuery) -> Result<Self, ApiError> {
        let mut fields = BTreeMap::new();
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
            run_id: query
                .run_id
                .as_deref()
                .map(str::trim)
                .filter(|run_id| !run_id.is_empty())
                .map(str::to_string),
            page,
            page_size,
        })
    }
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct TranscriptResponse {
    pub data: Vec<TranscriptMessage>,
    pub meta: TranscriptMeta,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct TranscriptMessage {
    pub seq: u64,
    #[cfg_attr(feature = "ts-export", ts(type = "string"))]
    pub role: &'static str,
    #[serde(rename = "ts_ms")]
    pub ts: i64,
    pub blocks: Vec<TranscriptBlock>,
}

/// Displayable content block. Wire tags: `thinking`, `text`, `tool_call`,
/// `tool_result` (docs/09 §9.2). `truncated` appears only on a block whose
/// content (or serialized arguments) hit the 16KB budget.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub enum TranscriptBlock {
    Thinking {
        text: String,
        #[serde(skip_serializing_if = "is_false")]
        truncated: bool,
    },
    Text {
        text: String,
        #[serde(skip_serializing_if = "is_false")]
        truncated: bool,
    },
    ToolCall {
        name: String,
        arguments: Value,
        #[serde(skip_serializing_if = "is_false")]
        truncated: bool,
    },
    ToolResult {
        name: String,
        text: String,
        is_error: bool,
        #[serde(skip_serializing_if = "is_false")]
        truncated: bool,
    },
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct TranscriptMeta {
    pub page: u64,
    pub page_size: u64,
    pub total: usize,
    /// The session file was larger than the 2MB tail window (older lines,
    /// including a partial first line, were dropped).
    pub truncated: bool,
    /// No run with a session dir, no `.jsonl`, or no displayable messages.
    pub empty: bool,
}

enum Outcome {
    Ready(TranscriptResponse),
    IssueNotFound,
    RunNotFound,
}

/// `GET /api/v1/issues/{fp}/transcript` — latest (or `run_id`-selected) run's
/// session transcript, parsed and paginated. File IO and parsing run on the
/// blocking pool.
pub async fn transcript(
    State(state): State<AppState>,
    Path(fingerprint): Path<String>,
    Query(query): Query<TranscriptQuery>,
) -> Response {
    let params = match Params::parse(&query) {
        Ok(params) => params,
        Err(error) => return error.into_response(),
    };
    let store = Arc::clone(&state.store);
    match tokio::task::spawn_blocking(move || build_transcript(&store, &fingerprint, &params)).await
    {
        Ok(Ok(Outcome::Ready(body))) => Json(body).into_response(),
        Ok(Ok(Outcome::IssueNotFound)) => ApiError::not_found("issue not found").into_response(),
        Ok(Ok(Outcome::RunNotFound)) => ApiError::not_found("run not found").into_response(),
        Ok(Err(error)) => {
            eprintln!("transcript: store read failed: {error}");
            internal_error()
        }
        Err(error) => {
            eprintln!("transcript: blocking task failed: {error}");
            internal_error()
        }
    }
}

fn build_transcript(store: &Store, fingerprint: &str, params: &Params) -> ShanResult<Outcome> {
    if store.get(fingerprint)?.is_none() {
        return Ok(Outcome::IssueNotFound);
    }
    let run = match &params.run_id {
        // Direct id lookup: not limited to the newest runs, scoped by
        // fingerprint. A run of another fingerprint is "not found" here.
        Some(run_id) => match store.run_for(fingerprint, run_id)? {
            Some(run) => run,
            None => return Ok(Outcome::RunNotFound),
        },
        None => match store.latest_run_with_session(fingerprint)? {
            Some(run) => run,
            None => return Ok(Outcome::Ready(empty_response(params))),
        },
    };
    let Some(file) = run
        .session_dir
        .as_deref()
        .and_then(validated_session_dir)
        .and_then(|dir| newest_jsonl(&dir))
    else {
        return Ok(Outcome::Ready(empty_response(params)));
    };
    let (tail, truncated) = match read_tail(&file) {
        Ok(tail) => tail,
        Err(error) => {
            // The file can vanish or stay locked while the runner appends;
            // the transcript is auxiliary, so log and serve an empty page.
            eprintln!(
                "transcript: unreadable session file {}: {error}",
                file.display()
            );
            return Ok(Outcome::Ready(empty_response(params)));
        }
    };
    let messages = parse_messages(&tail);
    let total = messages.len();
    // A huge page number must be an empty page, never an overflow/panic.
    let start = (params.page - 1)
        .checked_mul(params.page_size)
        .and_then(|offset| usize::try_from(offset).ok())
        .unwrap_or(usize::MAX);
    let data = if start >= total {
        Vec::new()
    } else {
        messages
            .into_iter()
            .skip(start)
            .take(params.page_size as usize)
            .collect()
    };
    Ok(Outcome::Ready(TranscriptResponse {
        data,
        meta: TranscriptMeta {
            page: params.page,
            page_size: params.page_size,
            total,
            truncated,
            empty: total == 0,
        },
    }))
}

fn empty_response(params: &Params) -> TranscriptResponse {
    TranscriptResponse {
        data: Vec::new(),
        meta: TranscriptMeta {
            page: params.page,
            page_size: params.page_size,
            total: 0,
            truncated: false,
            empty: true,
        },
    }
}

/// The session dir is DB-derived (never request input), but a corrupt or
/// hostile row must not turn into an arbitrary filesystem read.
fn validated_session_dir(raw: &str) -> Option<PathBuf> {
    let path = FsPath::new(raw);
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return None;
    }
    Some(path.to_path_buf())
}

struct SessionFile {
    path: PathBuf,
    modified: Option<SystemTime>,
}

/// Newest `.jsonl` under the run dir, newest by mtime (a resumed session is
/// appended to) with the timestamped filename as the deterministic tiebreak.
fn newest_jsonl(root: &FsPath) -> Option<PathBuf> {
    let mut files = Vec::new();
    let mut budget = MAX_SESSION_ENTRIES;
    collect_jsonl(root, 0, &mut budget, &mut files);
    files
        .into_iter()
        .max_by(|left, right| {
            left.modified
                .cmp(&right.modified)
                .then_with(|| left.path.cmp(&right.path))
        })
        .map(|file| file.path)
}

fn collect_jsonl(dir: &FsPath, depth: usize, budget: &mut usize, out: &mut Vec<SessionFile>) {
    if depth > MAX_SESSION_DEPTH || *budget == 0 {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            eprintln!(
                "transcript: cannot list session dir {}: {error}",
                dir.display()
            );
            return;
        }
    };
    for entry in entries.flatten() {
        if *budget == 0 {
            return;
        }
        *budget -= 1;
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if file_type.is_dir() {
            collect_jsonl(&path, depth + 1, budget, out);
        } else if file_type.is_file()
            && path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("jsonl"))
        {
            out.push(SessionFile {
                modified: entry
                    .metadata()
                    .ok()
                    .and_then(|metadata| metadata.modified().ok()),
                path,
            });
        }
    }
}

/// Read at most the last 2MB of a session file. When the window starts
/// mid-line the partial first line is dropped; `truncated` reports that
/// earlier content exists. IO errors surface so the caller can log them.
fn read_tail(path: &FsPath) -> std::io::Result<(String, bool)> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let truncated = len > TAIL_BYTES;
    let start = len.saturating_sub(TAIL_BYTES);
    // One byte before the window tells us whether the first line is complete.
    let read_from = start.saturating_sub(1);
    file.seek(SeekFrom::Start(read_from))?;
    let mut bytes = Vec::new();
    file.take(len.saturating_sub(read_from))
        .read_to_end(&mut bytes)?;
    let text_bytes: &[u8] = if start == 0 {
        &bytes
    } else if bytes.first() == Some(&b'\n') {
        &bytes[1..]
    } else {
        match bytes.iter().position(|byte| *byte == b'\n') {
            Some(index) => &bytes[index + 1..],
            None => &[],
        }
    };
    // A window split inside a multi-byte character must not lose the tail.
    Ok((String::from_utf8_lossy(text_bytes).into_owned(), truncated))
}

fn parse_messages(tail: &str) -> Vec<TranscriptMessage> {
    tail.lines()
        .filter_map(|line| {
            let record: Value = serde_json::from_str(line.trim()).ok()?;
            if record.get("kind").and_then(Value::as_str) != Some("entry")
                || record.get("type").and_then(Value::as_str) != Some("message")
            {
                return None;
            }
            let message = record.get("message")?;
            let seq = record.get("seq").and_then(Value::as_u64).unwrap_or(0);
            let ts = record.get("timestamp").and_then(Value::as_i64).unwrap_or(0);
            map_message(message, seq, ts)
        })
        .collect()
}

fn map_message(message: &Value, seq: u64, ts: i64) -> Option<TranscriptMessage> {
    let (role, blocks) = match message.get("kind").and_then(Value::as_str)? {
        "user" => ("user", user_blocks(message.get("content"))),
        "assistant" => ("assistant", assistant_blocks(message.get("content"))),
        // Real JSONL serializes `AgentMessage::ToolResult` as snake_case;
        // accept the camelCase spelling too so older exports keep working.
        "tool_result" | "toolResult" => ("tool", tool_result_blocks(message)),
        _ => return None,
    };
    if blocks.is_empty() {
        return None;
    }
    Some(TranscriptMessage {
        seq,
        role,
        ts,
        blocks,
    })
}

fn user_blocks(content: Option<&Value>) -> Vec<TranscriptBlock> {
    match content {
        Some(Value::String(text)) => vec![text_block(text)],
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| {
                if item.get("type").and_then(Value::as_str) != Some("text") {
                    return None;
                }
                let text = item.get("text").and_then(Value::as_str)?;
                Some(text_block(text))
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn text_block(text: &str) -> TranscriptBlock {
    let (text, truncated) = truncate_block(text);
    TranscriptBlock::Text { text, truncated }
}

fn assistant_blocks(content: Option<&Value>) -> Vec<TranscriptBlock> {
    let Some(items) = content.and_then(Value::as_array) else {
        return Vec::new();
    };
    items.iter().filter_map(assistant_block).collect()
}

fn assistant_block(item: &Value) -> Option<TranscriptBlock> {
    match item.get("type").and_then(Value::as_str)? {
        "thinking" => {
            let text = item.get("thinking").and_then(Value::as_str)?;
            let (text, truncated) = truncate_block(text);
            Some(TranscriptBlock::Thinking { text, truncated })
        }
        "text" => {
            let text = item.get("text").and_then(Value::as_str)?;
            Some(text_block(text))
        }
        "toolCall" => {
            let name = item.get("name").and_then(Value::as_str)?;
            let (arguments, truncated) =
                bounded_arguments(item.get("arguments").cloned().unwrap_or(Value::Null));
            Some(TranscriptBlock::ToolCall {
                name: name.to_string(),
                arguments,
                truncated,
            })
        }
        // Images and future block kinds have no console rendering yet.
        _ => None,
    }
}

fn tool_result_blocks(message: &Value) -> Vec<TranscriptBlock> {
    let name = message
        .get("toolName")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let is_error = message
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let text = match message.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| {
                if item.get("type").and_then(Value::as_str) != Some("text") {
                    return None;
                }
                item.get("text").and_then(Value::as_str)
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    let (text, truncated) = truncate_block(&text);
    vec![TranscriptBlock::ToolResult {
        name,
        text,
        is_error,
        truncated,
    }]
}

/// Cap one block at 16KB of UTF-8, cutting on a char boundary and appending
/// `…`. Returns whether the content was capped.
fn truncate_block(text: &str) -> (String, bool) {
    if text.len() <= MAX_BLOCK_BYTES {
        return (text.to_string(), false);
    }
    let mut end = MAX_BLOCK_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut truncated = String::with_capacity(end + '…'.len_utf8());
    truncated.push_str(&text[..end]);
    truncated.push('…');
    (truncated, true)
}

/// Tool-call arguments are served as JSON. A pathological argument blob is
/// replaced by a short preview so one call cannot dominate the page.
fn bounded_arguments(arguments: Value) -> (Value, bool) {
    let bytes = serde_json::to_string(&arguments).map_or(0, |json| json.len());
    if bytes <= MAX_BLOCK_BYTES {
        return (arguments, false);
    }
    (Value::String(format!("<truncated: {bytes} bytes>")), true)
}

fn internal_error() -> Response {
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal",
        "failed to read transcript",
    )
    .into_response()
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{header, Request, StatusCode};
    use axum::response::Response;
    use axum::Router;
    use serde_json::json;
    use tower::ServiceExt;

    use crate::serve::{build_app, AppState};
    use shanhaijing::Store;

    use super::{newest_jsonl, truncate_block, validated_session_dir, MAX_BLOCK_BYTES};

    const APP: &str = "example.com/transcript";
    const STRICT_CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; font-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

    // ---- fixture ----------------------------------------------------------

    struct Fixture {
        _dir: tempfile::TempDir,
        app: Router,
        fp: String,
        other: String,
        empty: String,
    }

    /// One issue with an older transcript run, a newer run without a session
    /// dir, and a tail-truncation run; a second issue with its own run (the
    /// cross-fingerprint 404 subject); a third issue without any session.
    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("s.db");
        let store = Arc::new(Store::open(&db_path).expect("open"));
        let session_root = dir.path().join("sessions");

        let fp = report(&store, "transcript crash");
        let other = report(&store, "other crash");
        let empty = report(&store, "no session crash");

        // Newest run of `fp` has no session dir: default selection must fall
        // through to the newest run that has one.
        start_run(&store, &fp, "run-main", 2_000_000);
        start_run(&store, &fp, "run-new-nosession", 3_000_000);
        let main_dir = session_root.join("run-main");
        write_jsonl(
            &main_dir,
            "2025-01-01T00-00-00-000Z_stale.jsonl",
            &[json!({
                "kind": "entry", "type": "message", "seq": 1, "timestamp": 1,
                "message": { "kind": "user", "role": "user", "content": "stale transcript" }
            })
            .to_string()],
        );
        write_jsonl(
            &main_dir,
            "2026-10-15T14-30-30-024Z_main.jsonl",
            &main_lines(),
        );
        set_session_dir(&store, "run-main", &main_dir);

        start_run(&store, &fp, "run-heavy", 1_000_000);
        let heavy_dir = session_root.join("run-heavy");
        write_jsonl(
            &heavy_dir,
            "2026-10-15T15-00-00-000Z_heavy.jsonl",
            &heavy_lines(),
        );
        set_session_dir(&store, "run-heavy", &heavy_dir);

        // Oversized text/thinking/tool_result/tool-call arguments.
        start_run(&store, &fp, "run-bounded", 1_500_000);
        let bounded_dir = session_root.join("run-bounded");
        write_jsonl(
            &bounded_dir,
            "2026-10-15T15-30-00-000Z_bounded.jsonl",
            &bounded_lines(),
        );
        set_session_dir(&store, "run-bounded", &bounded_dir);

        start_run(&store, &other, "run-other", 4_000_000);
        let other_dir = session_root.join("run-other");
        write_jsonl(
            &other_dir,
            "2026-10-15T16-00-00-000Z_other.jsonl",
            &[json!({
                "kind": "entry", "type": "message", "seq": 1, "timestamp": 1,
                "message": { "kind": "user", "role": "user", "content": "other transcript" }
            })
            .to_string()],
        );
        set_session_dir(&store, "run-other", &other_dir);

        start_run(&store, &empty, "run-empty", 5_000_000);
        // A session dir that exists but holds no `.jsonl` is still an empty
        // transcript, not an error.
        let empty_dir = session_root.join("run-empty");
        std::fs::create_dir_all(&empty_dir).expect("empty session dir");
        std::fs::write(empty_dir.join("notes.txt"), "not a session").expect("notes");
        set_session_dir(&store, "run-empty", &empty_dir);

        let state = AppState {
            store,
            ui_token: Arc::new(None),
            ingest_token: Arc::new(None),
            session: Arc::new(crate::session::SessionManager::new("", 86_400_000)),
            limiter: Arc::new(crate::session::LoginLimiter::new()),
            started_at_ms: crate::now_ms(),
            env_path: dir.path().join("nuwa.env"),
            db_path,
            cookie_secure: false,
            cookie_secure_never: false,
            serve_unit: "nuwa-serve".to_string(),
            runner_units: vec!["nuwa-work".to_string()],
        };
        let app = build_app(state);
        Fixture {
            _dir: dir,
            app,
            fp,
            other,
            empty,
        }
    }

    /// Four displayable messages among header/record/custom entries: a user
    /// string, an assistant thinking+text+toolCall (plus an ignored image),
    /// a tool result, and a user content-blocks message.
    fn main_lines() -> Vec<String> {
        vec![
            json!({ "kind": "header", "version": 4, "id": "s1", "createdAt": 0, "cwd": "/repo" })
                .to_string(),
            json!({
                "kind": "record", "type": "assistant_frame", "seq": 1,
                "op": "append", "frame": { "type": "start" }
            })
            .to_string(),
            json!({
                "kind": "entry", "type": "message", "seq": 1, "timestamp": 1_000,
                "message": {
                    "kind": "user", "role": "user", "content": "Please fix the bug"
                }
            })
            .to_string(),
            json!({
                "kind": "entry", "type": "message", "seq": 2, "timestamp": 2_000,
                "message": {
                    "kind": "assistant", "role": "assistant",
                    "content": [
                        { "type": "thinking", "thinking": "think step by step" },
                        { "type": "text", "text": "I will read calc.go" },
                        { "type": "toolCall", "name": "read", "arguments": { "path": "calc.go" } },
                        { "type": "image", "data": "aGk=", "mimeType": "image/png" }
                    ]
                }
            })
            .to_string(),
            json!({
                "kind": "entry", "type": "message", "seq": 3, "timestamp": 3_000,
                "message": {
                    "kind": "tool_result", "role": "toolResult", "toolName": "read",
                    "content": [{ "type": "text", "text": "package main\nfunc Average(xs []int) int {" }],
                    "isError": false
                }
            })
            .to_string(),
            json!({
                "kind": "entry", "type": "message", "seq": 4, "timestamp": 3_500,
                "message": { "kind": "custom", "role": "notice", "content": "hidden" }
            })
            .to_string(),
            json!({
                "kind": "entry", "type": "model_change", "seq": 5,
                "provider": "faux", "modelId": "faux-1"
            })
            .to_string(),
            json!({ "kind": "record", "type": "usage", "seq": 6 }).to_string(),
            json!({
                "kind": "entry", "type": "message", "seq": 6, "timestamp": 4_000,
                "message": {
                    "kind": "user", "role": "user",
                    "content": [{ "type": "text", "text": "Thanks" }]
                }
            })
            .to_string(),
        ]
    }

    /// A >2MB file whose pad record straddles the tail window (so the oldest
    /// message is dropped) plus a 20K-character text block.
    fn heavy_lines() -> Vec<String> {
        let padding = "x".repeat(2_100_000);
        vec![
            json!({
                "kind": "entry", "type": "message", "seq": 1, "timestamp": 1_000,
                "message": {
                    "kind": "user", "role": "user", "content": "ancient message the tail drops"
                }
            })
            .to_string(),
            format!(r#"{{"kind":"record","type":"usage","padding":"{padding}"}}"#),
            json!({
                "kind": "entry", "type": "message", "seq": 2, "timestamp": 5_000,
                "message": {
                    "kind": "assistant", "role": "assistant",
                    "content": [{ "type": "text", "text": "汉".repeat(20_000) }]
                }
            })
            .to_string(),
            json!({
                "kind": "entry", "type": "message", "seq": 3, "timestamp": 6_000,
                "message": {
                    "kind": "user", "role": "user",
                    "content": [{ "type": "text", "text": "tail-visible" }]
                }
            })
            .to_string(),
        ]
    }

    /// Messages whose blocks all exceed the 16KB byte budget: ASCII text,
    /// CJK thinking, a huge tool-call argument blob, and a tool result.
    fn bounded_lines() -> Vec<String> {
        vec![
            json!({
                "kind": "entry", "type": "message", "seq": 1, "timestamp": 1_000,
                "message": {
                    "kind": "assistant", "role": "assistant",
                    "content": [
                        { "type": "text", "text": "a".repeat(20_000) },
                        { "type": "thinking", "thinking": "汉".repeat(20_000) },
                        {
                            "type": "toolCall", "name": "write",
                            "arguments": { "path": "calc.go", "content": "x".repeat(20_000) }
                        },
                        { "type": "toolCall", "name": "read", "arguments": { "path": "calc.go" } }
                    ]
                }
            })
            .to_string(),
            json!({
                "kind": "entry", "type": "message", "seq": 2, "timestamp": 2_000,
                "message": {
                    "kind": "tool_result", "role": "toolResult", "toolName": "write",
                    "content": [{ "type": "text", "text": "y".repeat(20_000) }],
                    "isError": false
                }
            })
            .to_string(),
        ]
    }

    // ---- seed helpers -----------------------------------------------------

    fn signal(title: &str) -> shanhaijing::Signal {
        shanhaijing::Signal {
            repo: APP.to_string(),
            language: "go".to_string(),
            title: title.to_string(),
            severity: "high".to_string(),
            message: Some(format!("boom {title}")),
            log_window: Vec::new(),
            stack_trace: format!(
                "panic: {title}\ngoroutine 1 [running]:\nexample.com/transcript.F()\n\t/app/transcript.go:1 +0x1"
            ),
        }
    }

    fn report(store: &Store, title: &str) -> String {
        match store.report_signal(&signal(title), 1_000).expect("report") {
            shanhaijing::ReportOutcome::NewIssue { fingerprint } => fingerprint,
            other => panic!("{title} must be a new issue, got {other:?}"),
        }
    }

    fn start_run(store: &Store, fingerprint: &str, run_id: &str, started_at: i64) {
        store
            .start_run(&shanhaijing::RunStart {
                id: run_id.to_string(),
                fingerprint: fingerprint.to_string(),
                runner_id: "runner-test".to_string(),
                repo: APP.to_string(),
                started_at,
                provider: None,
                model: None,
            })
            .expect("start run");
    }

    fn set_session_dir(store: &Store, run_id: &str, dir: &Path) {
        store
            .update_run(
                run_id,
                &shanhaijing::RunUpdate {
                    session_dir: Some(dir.display().to_string()),
                    ..Default::default()
                },
            )
            .expect("update run");
    }

    fn write_jsonl(root: &Path, file_name: &str, lines: &[String]) -> PathBuf {
        let dir = root.join("--repo--");
        std::fs::create_dir_all(&dir).expect("session dir");
        let path = dir.join(file_name);
        let mut body = String::new();
        for line in lines {
            body.push_str(line);
            body.push('\n');
        }
        std::fs::write(&path, body).expect("write jsonl");
        path
    }

    // ---- request helpers --------------------------------------------------

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

    fn seqs(value: &serde_json::Value) -> Vec<u64> {
        value["data"]
            .as_array()
            .unwrap_or_else(|| panic!("data array missing: {value}"))
            .iter()
            .map(|item| {
                item["seq"]
                    .as_u64()
                    .unwrap_or_else(|| panic!("seq: {item}"))
            })
            .collect()
    }

    // ---- tests ------------------------------------------------------------

    #[tokio::test]
    async fn transcript_defaults_to_latest_run_with_session_and_maps_blocks() {
        let fixture = fixture();

        // No run_id: the newest run has no session dir, so the parser falls
        // back to run-main. A GET needs no CSRF token even in local mode.
        let response = get(
            &fixture.app,
            &format!("/api/v1/issues/{}/transcript", fixture.fp),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_api_headers(&response);
        let value = json_body(response).await;

        assert_eq!(
            value["meta"],
            json!({ "page": 1, "page_size": 20, "total": 4, "truncated": false, "empty": false })
        );
        assert_eq!(seqs(&value), vec![1, 2, 3, 6]);

        let user = &value["data"][0];
        assert_eq!(user["role"].as_str(), Some("user"));
        assert_eq!(user["ts_ms"].as_i64(), Some(1_000));
        assert_eq!(
            user["blocks"],
            json!([{ "type": "text", "text": "Please fix the bug" }])
        );

        let assistant = &value["data"][1];
        assert_eq!(assistant["role"].as_str(), Some("assistant"));
        assert_eq!(assistant["ts_ms"].as_i64(), Some(2_000));
        assert_eq!(
            assistant["blocks"],
            json!([
                { "type": "thinking", "text": "think step by step" },
                { "type": "text", "text": "I will read calc.go" },
                { "type": "tool_call", "name": "read", "arguments": { "path": "calc.go" } }
            ]),
            "image blocks are ignored, unknown records never surface"
        );

        let tool = &value["data"][2];
        assert_eq!(tool["role"].as_str(), Some("tool"));
        assert_eq!(tool["ts_ms"].as_i64(), Some(3_000));
        assert_eq!(
            tool["blocks"],
            json!([{
                "type": "tool_result",
                "name": "read",
                "text": "package main\nfunc Average(xs []int) int {",
                "is_error": false
            }])
        );

        let thanks = &value["data"][3];
        assert_eq!(thanks["role"].as_str(), Some("user"));
        assert_eq!(thanks["seq"].as_u64(), Some(6));
        assert_eq!(
            thanks["blocks"],
            json!([{ "type": "text", "text": "Thanks" }])
        );

        // The stale session file must not leak into the newest transcript.
        assert!(
            !value.to_string().contains("stale transcript"),
            "only the newest .jsonl is parsed: {value}"
        );

        // An explicit run_id selects the same run.
        let response = get(
            &fixture.app,
            &format!("/api/v1/issues/{}/transcript?run_id=run-main", fixture.fp),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let explicit = json_body(response).await;
        assert_eq!(explicit["data"], value["data"]);
        assert_eq!(explicit["meta"], value["meta"]);
    }

    #[tokio::test]
    async fn transcript_paginates_messages() {
        let fixture = fixture();
        let base = format!(
            "/api/v1/issues/{}/transcript?run_id=run-main&page_size=2",
            fixture.fp
        );

        let response = get(&fixture.app, &format!("{base}&page=1")).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_api_headers(&response);
        let value = json_body(response).await;
        assert_eq!(seqs(&value), vec![1, 2]);
        assert_eq!(
            value["meta"],
            json!({ "page": 1, "page_size": 2, "total": 4, "truncated": false, "empty": false })
        );

        let response = get(&fixture.app, &format!("{base}&page=2")).await;
        assert_eq!(response.status(), StatusCode::OK);
        let value = json_body(response).await;
        assert_eq!(seqs(&value), vec![3, 6]);
        assert_eq!(value["meta"]["total"].as_u64(), Some(4));

        // Past the end is an empty page, not an error.
        let response = get(&fixture.app, &format!("{base}&page=3")).await;
        assert_eq!(response.status(), StatusCode::OK);
        let value = json_body(response).await;
        assert!(value["data"].as_array().expect("data").is_empty());
        assert_eq!(value["meta"]["total"].as_u64(), Some(4));
        assert_eq!(value["meta"]["empty"].as_bool(), Some(false));
    }

    #[tokio::test]
    async fn transcript_truncates_long_blocks_and_flags_the_tail() {
        let fixture = fixture();
        let response = get(
            &fixture.app,
            &format!("/api/v1/issues/{}/transcript?run_id=run-heavy", fixture.fp),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_api_headers(&response);
        let value = json_body(response).await;

        assert_eq!(
            value["meta"],
            json!({ "page": 1, "page_size": 20, "total": 2, "truncated": true, "empty": false })
        );
        // The >2MB file is served from its tail: the ancient message and the
        // straddling pad record are gone.
        assert_eq!(seqs(&value), vec![2, 3]);
        assert!(!value.to_string().contains("ancient message"));

        // 16KB of UTF-8 kept on a char boundary, plus the ellipsis marker.
        let block = &value["data"][0]["blocks"][0];
        let text = block["text"].as_str().expect("truncated text");
        assert!(
            text.len() <= MAX_BLOCK_BYTES + '…'.len_utf8(),
            "capped block is {} bytes",
            text.len()
        );
        assert!(
            text.len() > MAX_BLOCK_BYTES - 4,
            "the 16KB budget is filled"
        );
        assert_eq!(block["truncated"].as_bool(), Some(true));
        assert!(text.ends_with('…'), "truncation marker missing");
        assert!(text.starts_with('汉'), "multi-byte text must stay valid");
        assert!(text.chars().all(|c| c == '汉' || c == '…'));

        assert_eq!(
            value["data"][1]["blocks"],
            json!([{ "type": "text", "text": "tail-visible" }])
        );
    }

    #[tokio::test]
    async fn transcript_caps_oversized_blocks_and_flags_them() {
        let fixture = fixture();
        let response = get(
            &fixture.app,
            &format!(
                "/api/v1/issues/{}/transcript?run_id=run-bounded",
                fixture.fp
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_api_headers(&response);
        let value = json_body(response).await;

        assert_eq!(value["meta"]["total"].as_u64(), Some(2));
        let blocks = value["data"][0]["blocks"].as_array().expect("blocks");
        assert_eq!(blocks.len(), 4);

        // A >16KB ASCII text block: exactly the byte budget + `…`.
        assert_eq!(blocks[0]["type"], "text");
        let ascii = blocks[0]["text"].as_str().expect("ascii text");
        assert_eq!(ascii.len(), MAX_BLOCK_BYTES + '…'.len_utf8());
        assert!(ascii.chars().all(|c| c == 'a' || c == '…'));
        assert_eq!(blocks[0]["truncated"].as_bool(), Some(true));

        // A >16KB CJK thinking block: cut on a char boundary.
        assert_eq!(blocks[1]["type"], "thinking");
        let cjk = blocks[1]["text"].as_str().expect("cjk text");
        assert!(
            cjk.len() <= MAX_BLOCK_BYTES + '…'.len_utf8(),
            "capped block is {} bytes",
            cjk.len()
        );
        assert!(cjk.starts_with('汉') && cjk.ends_with('…'));
        assert!(cjk.chars().all(|c| c == '汉' || c == '…'));
        assert_eq!(blocks[1]["truncated"].as_bool(), Some(true));

        // Multi-KB tool-call arguments become a short preview string.
        let big_call = &blocks[2];
        assert_eq!(big_call["type"], "tool_call");
        assert_eq!(big_call["name"], "write");
        assert_eq!(big_call["truncated"].as_bool(), Some(true));
        let preview = big_call["arguments"].as_str().expect("arguments preview");
        assert!(preview.starts_with("<truncated: "), "{preview}");
        assert!(preview.ends_with(" bytes>"), "{preview}");
        assert!(preview.len() < 64, "{preview}");

        // Small arguments keep the plain object shape and no flag.
        let small_call = &blocks[3];
        assert_eq!(small_call["arguments"], json!({ "path": "calc.go" }));
        assert!(
            small_call.get("truncated").is_none(),
            "an uncapped block carries no flag: {small_call}"
        );

        // tool_result text is capped too.
        let tool = &value["data"][1]["blocks"][0];
        assert_eq!(tool["type"], "tool_result");
        assert_eq!(tool["truncated"].as_bool(), Some(true));
        let tool_text = tool["text"].as_str().expect("tool text");
        assert!(tool_text.len() <= MAX_BLOCK_BYTES + '…'.len_utf8());
        assert!(tool_text.ends_with('…'));
    }

    #[test]
    fn truncate_block_uses_a_utf8_byte_budget() {
        let ascii = "a".repeat(MAX_BLOCK_BYTES + 1);
        let (capped, truncated) = truncate_block(&ascii);
        assert!(truncated);
        assert_eq!(capped.len(), MAX_BLOCK_BYTES + '…'.len_utf8());
        assert_eq!(
            capped.chars().filter(|c| *c == 'a').count(),
            MAX_BLOCK_BYTES
        );
        assert!(capped.ends_with('…'));

        let cjk = "汉".repeat(20_000);
        let (capped, truncated) = truncate_block(&cjk);
        assert!(truncated);
        // A 3-byte character can never straddle the budget.
        assert!(
            capped.len() <= MAX_BLOCK_BYTES + '…'.len_utf8(),
            "capped block is {} bytes",
            capped.len()
        );
        assert!(capped.len() > MAX_BLOCK_BYTES - 4, "{} bytes", capped.len());
        assert!(capped.ends_with('…'));
        assert!(capped.trim_end_matches('…').chars().all(|c| c == '汉'));

        // Content at or under the budget is untouched.
        let exact = "a".repeat(MAX_BLOCK_BYTES);
        let (untouched, truncated) = truncate_block(&exact);
        assert!(!truncated);
        assert_eq!(untouched, exact);
    }

    #[test]
    fn validated_session_dir_rejects_relative_and_parent_paths() {
        // Relative DB values never reach the filesystem.
        assert!(validated_session_dir("sessions/run").is_none());
        assert!(validated_session_dir("../sessions").is_none());
        assert!(validated_session_dir("").is_none());

        // An absolute path with a parent component is rejected too.
        let sneaky = std::env::temp_dir().join("a").join("..").join("b");
        assert!(
            validated_session_dir(&sneaky.display().to_string()).is_none(),
            "{}",
            sneaky.display()
        );

        // A plain absolute directory is accepted.
        let ok = std::env::temp_dir();
        let validated = validated_session_dir(&ok.display().to_string()).expect("absolute dir");
        assert_eq!(validated, ok);
    }

    #[test]
    fn newest_jsonl_none_without_a_jsonl_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("repo")).expect("nested");
        std::fs::write(dir.path().join("repo").join("notes.txt"), "x").expect("write");
        assert!(newest_jsonl(dir.path()).is_none());
        assert!(newest_jsonl(&dir.path().join("missing")).is_none());
    }

    #[tokio::test]
    async fn transcript_unknown_run_and_cross_fingerprint_are_404() {
        let fixture = fixture();

        let response = get(
            &fixture.app,
            &format!(
                "/api/v1/issues/{}/transcript?run_id=does-not-exist",
                fixture.fp
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_api_headers(&response);
        let value = json_body(response).await;
        assert_eq!(value["error"]["code"], "not_found");
        assert_eq!(value["error"]["message"], "run not found");

        let response = get(
            &fixture.app,
            &format!(
                "/api/v1/issues/{}/transcript?run_id=..%2F..%2Fsessions",
                fixture.fp
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let value = json_body(response).await;
        assert_eq!(value["error"]["message"], "run not found");

        // run-other exists, but belongs to the other fingerprint.
        let response = get(
            &fixture.app,
            &format!("/api/v1/issues/{}/transcript?run_id=run-other", fixture.fp),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let value = json_body(response).await;
        assert_eq!(value["error"]["message"], "run not found");
        assert_ne!(fixture.fp, fixture.other);

        // The other issue can read its own run.
        let response = get(
            &fixture.app,
            &format!(
                "/api/v1/issues/{}/transcript?run_id=run-other",
                fixture.other
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);

        let response = get(&fixture.app, "/api/v1/issues/unknown/transcript").await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let value = json_body(response).await;
        assert_eq!(value["error"]["message"], "issue not found");
    }

    #[tokio::test]
    async fn transcript_without_session_is_empty() {
        let fixture = fixture();

        for uri in [
            format!("/api/v1/issues/{}/transcript", fixture.empty),
            format!(
                "/api/v1/issues/{}/transcript?run_id=run-empty",
                fixture.empty
            ),
        ] {
            let response = get(&fixture.app, &uri).await;
            assert_eq!(response.status(), StatusCode::OK, "{uri}");
            assert_api_headers(&response);
            let value = json_body(response).await;
            assert!(value["data"].as_array().expect("data").is_empty(), "{uri}");
            assert_eq!(
                value["meta"],
                json!({ "page": 1, "page_size": 20, "total": 0, "truncated": false, "empty": true }),
                "{uri}"
            );
        }
    }

    #[tokio::test]
    async fn transcript_rejects_invalid_paging() {
        let fixture = fixture();
        // A page_size above the documented 100 cap is a field error.
        let cases = [
            ("page=0", "page"),
            ("page=abc", "page"),
            ("page_size=0", "page_size"),
            ("page_size=101", "page_size"),
        ];
        for (query, field) in cases {
            let response = get(
                &fixture.app,
                &format!("/api/v1/issues/{}/transcript?{query}", fixture.fp),
            )
            .await;
            assert_eq!(
                response.status(),
                StatusCode::UNPROCESSABLE_ENTITY,
                "{query}"
            );
            assert_api_headers(&response);
            let value = json_body(response).await;
            assert_eq!(value["error"]["code"], "validation_failed", "{query}");
            assert!(
                value["error"]["fields"][field].is_string(),
                "{query} must report the {field} field: {value}"
            );
        }

        // 100 is the documented maximum and is accepted.
        let response = get(
            &fixture.app,
            &format!(
                "/api/v1/issues/{}/transcript?run_id=run-main&page_size=100",
                fixture.fp
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
    }
}
