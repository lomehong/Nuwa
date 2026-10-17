//! SPA 静态资源与 fallback。M2a Task 3 先返回占位页；Task 6 换成
//! `rust-embed`（debug 读磁盘、release 编入二进制）与 SPA fallback。

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};

pub async fn serve() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"))],
        "<!doctype html><html lang=\"zh\"><meta charset=\"utf-8\"><title>Nuwa</title><body><h1>Nuwa console</h1><p>frontend not built yet</p></body></html>",
    )
        .into_response()
}
