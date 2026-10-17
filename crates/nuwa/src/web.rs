//! SPA 静态资源与 fallback。M2a Task 3 先返回占位页；Task 6 换成
//! `rust-embed`（debug 读磁盘、release 编入二进制）与 SPA fallback。

use std::borrow::Cow;

use axum::body::Body;
use axum::http::{header, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

/// Frontend build output (M3). The folder is kept in git via `.gitkeep` so a
/// fresh clone compiles; debug builds read it from disk, release embeds it.
#[derive(RustEmbed)]
#[folder = "../../web/dist/"]
struct Assets;

const PLACEHOLDER: &str = "<!doctype html><html lang=\"zh\"><meta charset=\"utf-8\"><title>Nuwa</title><body><h1>Nuwa console</h1><p>frontend not built yet</p></body></html>";

/// Vite hashes `assets/*`, so those names are content-addressed.
const IMMUTABLE_CACHE: &str = "public, max-age=31536000, immutable";
/// `index.html` and unhashed files must revalidate so a deploy is visible.
const NO_CACHE: &str = "no-cache";

/// Static fallback. `/api/v1` is mounted explicitly; this answers the SPA
/// shell, embedded assets, and 404s for everything else.
pub async fn serve(method: Method, uri: Uri) -> Response {
    let raw = uri.path().trim_start_matches('/');
    // Only `/api/v1` is a real API mount. Any other `/api` path is an API
    // 404 and must never be swallowed by the SPA fallback.
    if raw == "api" || raw.starts_with("api/") {
        return crate::api::api_not_found().into_response();
    }
    // Legacy backend namespaces are not SPA routes: a browser GET (or a proxy
    // typo) must 404 instead of receiving the shell with a 200.
    if raw == "ingest" || raw == "health" || raw.starts_with("health/") {
        return not_found(&method);
    }
    // Dotfiles (e.g. `web/dist/.gitkeep`) are build artifacts, never assets.
    if raw.starts_with('.') {
        return not_found(&method);
    }
    // Static serving is read-only. This gate sits before the asset lookup so
    // a write to an existing file (or to `/` once `index.html` exists) can
    // never answer 200.
    if method != Method::GET && method != Method::HEAD {
        return not_found(&method);
    }
    let path = if raw.is_empty() { "index.html" } else { raw };
    if let Some(asset) = Assets::get(path) {
        return static_response(path, asset.data, &method);
    }
    // Missing files (anything with a dot or under `assets/`) are real 404s.
    // Only extensionless paths are client-side routes.
    if looks_like_file(raw) {
        return not_found(&method);
    }
    match Assets::get("index.html") {
        Some(index) => static_response("index.html", index.data, &method),
        None => placeholder(&method),
    }
}

fn looks_like_file(path: &str) -> bool {
    path.contains('.') || path.starts_with("assets/")
}

fn static_response(path: &str, data: Cow<'static, [u8]>, method: &Method) -> Response {
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let cache = if path.starts_with("assets/") {
        IMMUTABLE_CACHE
    } else {
        NO_CACHE
    };
    let mut response = (
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_str(mime.as_ref())
                    .unwrap_or(HeaderValue::from_static("application/octet-stream")),
            ),
            (header::CACHE_CONTROL, HeaderValue::from_static(cache)),
        ],
        data,
    )
        .into_response();
    strip_head_body(method, &mut response);
    response
}

fn placeholder(method: &Method) -> Response {
    let mut response = (
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            ),
            (header::CACHE_CONTROL, HeaderValue::from_static(NO_CACHE)),
        ],
        PLACEHOLDER,
    )
        .into_response();
    strip_head_body(method, &mut response);
    response
}

fn not_found(method: &Method) -> Response {
    let mut response = (StatusCode::NOT_FOUND, "not found").into_response();
    strip_head_body(method, &mut response);
    response
}

/// The fallback is a plain handler, so axum's automatic HEAD body stripping
/// (a `MethodRouter` feature) does not apply here.
fn strip_head_body(method: &Method, response: &mut Response) {
    if *method == Method::HEAD {
        *response.body_mut() = Body::empty();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{Method, Uri};
    use std::borrow::Cow;

    async fn serve_request(method: Method, path: &str) -> Response {
        serve(method, path.parse::<Uri>().expect("valid uri")).await
    }

    async fn body_text(response: Response) -> String {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        String::from_utf8_lossy(&bytes).into_owned()
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

    /// Status, content type and cache policy hold whether the shell is the
    /// placeholder or the built `web/dist/index.html` (M3+). The body assertion
    /// only applies while the dist folder has no index.html.
    async fn assert_index_shell(response: Response) {
        assert_eq!(response.status(), StatusCode::OK);
        let content_type = header_str(&response, header::CONTENT_TYPE);
        assert!(
            content_type.starts_with("text/html"),
            "got content type {content_type}"
        );
        assert_eq!(header_str(&response, header::CACHE_CONTROL), "no-cache");
        if Assets::get("index.html").is_none() {
            assert!(
                body_text(response).await.contains("frontend not built"),
                "expected the placeholder page while web/dist has no index.html"
            );
        }
    }

    #[tokio::test]
    async fn root_serves_the_spa_shell() {
        assert_index_shell(serve_request(Method::GET, "/").await).await;
    }

    #[tokio::test]
    async fn extensionless_spa_routes_fall_back_to_index_or_placeholder() {
        for path in ["/issues/abc", "/settings/github", "/issues/"] {
            assert_index_shell(serve_request(Method::GET, path).await).await;
        }
    }

    // Cover the placeholder generator directly so the route tests above stay
    // build-independent once `web/dist/index.html` exists.
    #[tokio::test]
    async fn placeholder_generator_serves_the_unbuilt_page() {
        let response = placeholder(&Method::GET);
        assert_eq!(response.status(), StatusCode::OK);
        assert!(header_str(&response, header::CONTENT_TYPE).starts_with("text/html"));
        assert_eq!(header_str(&response, header::CACHE_CONTROL), "no-cache");
        assert!(body_text(response).await.contains("frontend not built"));
    }

    #[tokio::test]
    async fn missing_hashed_asset_is_404() {
        let response = serve_request(Method::GET, "/assets/missing-abc123.js").await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn missing_file_like_paths_are_404() {
        for path in ["/missing-asset-xyz.js", "/missing-file-xyz.xyz", "/assets/"] {
            let response = serve_request(Method::GET, path).await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        }
    }

    #[tokio::test]
    async fn legacy_backend_paths_never_fall_back_to_the_spa() {
        for path in ["/ingest", "/health/unknown"] {
            let response = serve_request(Method::GET, path).await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        }
    }

    #[tokio::test]
    async fn dotfiles_are_never_served() {
        for path in ["/.gitkeep", "/.hidden"] {
            let response = serve_request(Method::GET, path).await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        }
    }

    #[tokio::test]
    async fn non_get_unmatched_paths_are_404() {
        for method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
            let response = serve_request(method.clone(), "/issues/abc").await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{method}");
            let response = serve_request(method.clone(), "/").await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{method}");
        }
    }

    #[tokio::test]
    async fn api_namespace_is_never_an_spa_route() {
        for path in ["/api", "/api/v2/x", "/api/unknown"] {
            let response = serve_request(Method::GET, path).await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
            let content_type = header_str(&response, header::CONTENT_TYPE);
            assert!(
                content_type.starts_with("application/json"),
                "{path} got {content_type}"
            );
            let value: serde_json::Value =
                serde_json::from_str(&body_text(response).await).expect("json body");
            assert_eq!(value["error"]["code"], "not_found", "{path}");
        }
    }

    #[tokio::test]
    async fn head_spa_route_is_200_without_body() {
        let response = serve_request(Method::HEAD, "/issues/abc").await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(header_str(&response, header::CONTENT_TYPE).starts_with("text/html"));
        assert!(body_text(response).await.is_empty());
    }

    #[test]
    fn hashed_assets_are_immutable_and_typed() {
        let response = static_response(
            "assets/index-abc123.js",
            Cow::Borrowed(b"console.log(1);"),
            &Method::GET,
        );
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            header_str(&response, header::CACHE_CONTROL),
            "public, max-age=31536000, immutable"
        );
        let content_type = header_str(&response, header::CONTENT_TYPE);
        assert!(content_type.contains("javascript"), "got {content_type}");
    }

    #[test]
    fn non_hashed_files_are_no_cache() {
        let response = static_response("index.html", Cow::Borrowed(b"<html></html>"), &Method::GET);
        assert_eq!(header_str(&response, header::CACHE_CONTROL), "no-cache");
        assert!(header_str(&response, header::CONTENT_TYPE).starts_with("text/html"));
    }

    #[test]
    fn placeholder_respects_the_strict_csp() {
        assert!(!PLACEHOLDER.contains("style="), "no inline style allowed");
        assert!(!PLACEHOLDER.contains("<script"), "no inline script allowed");
    }
}
