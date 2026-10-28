//! The layers around every route, outermost first:
//!
//! 1. [`catch_panic_layer`]: a handler panic becomes Starlette's
//!    `ServerErrorMiddleware` answer, 500 `text/plain` "Internal Server
//!    Error" (no CORS headers: that middleware sits outside CORS in Python).
//! 2. [`loopback_only`]: deviation D-06, new in Rust. A request whose Host is
//!    not a loopback name gets 421 and one whose Origin is neither loopback
//!    nor the Tauri WebView gets 403, so a web page cannot reach the backend
//!    through DNS rebinding or a cross-site request. Python accepted any.
//! 3. [`cors`]: Starlette's `CORSMiddleware` exactly as backend/main.py
//!    configures it (explicit origins, credentials, all methods and headers).
//!    Not tower-http's `CorsLayer`: its answers differ in the details the
//!    parity suite compares (credentials on every response that carries an
//!    Origin, the 400 "Disallowed CORS ..." preflight failures, mirrored
//!    request headers, `Vary` merging).
//! 4. [`head_is_unmatched`]: FastAPI's GET routes do not answer HEAD.

use std::any::Any;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::header::{self, HeaderMap, HeaderName, HeaderValue};
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use tower_http::catch_panic::CatchPanicLayer;

/// Marks a 500 that Python's `ServerErrorMiddleware` would have produced
/// (an unhandled exception): CORS leaves it alone.
#[derive(Clone, Copy, Debug)]
pub struct ServerErrorMarker;

/// Starlette's answer to an unhandled exception.
pub fn internal_server_error() -> Response {
    let mut response = (
        StatusCode::INTERNAL_SERVER_ERROR,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        "Internal Server Error",
    )
        .into_response();
    response.extensions_mut().insert(ServerErrorMarker);
    response
}

fn panic_response(payload: Box<dyn Any + Send + 'static>) -> Response {
    let message = payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| payload.downcast_ref::<&str>().copied())
        .unwrap_or("non-string panic payload");
    tracing::error!(target: "plumdeck_backend::http", "handler panicked: {message}");
    internal_server_error()
}

/// The outermost layer: panics answer 500 instead of dropping the connection.
pub fn catch_panic_layer() -> CatchPanicLayer<fn(Box<dyn Any + Send + 'static>) -> Response> {
    CatchPanicLayer::custom(panic_response as fn(Box<dyn Any + Send + 'static>) -> Response)
}

/// The Python backend's answer when no route takes a request.
///
/// main.py mounts the MCP app at `/`, so every request FastAPI's routes do
/// not fully match (an unknown path, a known path with another method such as
/// `POST /`, `HEAD` on a GET route, a plain `OPTIONS`) falls into it and gets
/// Starlette's plain-text 404, never FastAPI's JSON 404 or a 405. Kept
/// bug-compatible until deviation D-12 is approved. CORS preflights are
/// answered by [`cors`] before this point.
pub fn unmatched() -> Response {
    (StatusCode::NOT_FOUND, [(header::CONTENT_TYPE, "text/plain; charset=utf-8")], "Not Found").into_response()
}

/// FastAPI's GET routes do not answer HEAD (axum's do), so HEAD is unmatched.
pub async fn head_is_unmatched(request: Request, next: Next) -> Response {
    if request.method() == Method::HEAD {
        return unmatched();
    }
    next.run(request).await
}

// --- D-06: loopback Host and Origin ---------------------------------------

/// Origins outside loopback that the desktop's WebView uses: macOS
/// (`tauri://localhost`) and Windows (`http(s)://tauri.localhost`, both in
/// main.py's CORS list).
pub const WEBVIEW_ORIGINS: [&str; 3] = ["tauri://localhost", "http://tauri.localhost", "https://tauri.localhost"];

/// `127.0.0.1`, `localhost` (any case) or `[::1]`, with an optional numeric
/// port.
pub fn is_loopback_authority(authority: &str) -> bool {
    let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
        match rest.split_once(']') {
            Some((inner, port)) => (&authority[..inner.len() + 2], port),
            None => return false,
        }
    } else {
        match authority.split_once(':') {
            Some((host, _)) => (host, &authority[host.len()..]),
            None => (authority, ""),
        }
    };
    let port_ok = match port.strip_prefix(':') {
        None => port.is_empty(),
        Some(digits) => !digits.is_empty() && digits.len() <= 5 && digits.bytes().all(|b| b.is_ascii_digit()),
    };
    port_ok && (host == "127.0.0.1" || host == "[::1]" || host.eq_ignore_ascii_case("localhost"))
}

/// A loopback `http(s)://host[:port]` origin, or the WebView's.
pub fn is_allowed_origin_d06(origin: &str) -> bool {
    if WEBVIEW_ORIGINS.contains(&origin) {
        return true;
    }
    let authority = origin.strip_prefix("http://").or_else(|| origin.strip_prefix("https://"));
    authority.is_some_and(is_loopback_authority)
}

fn detail(status: StatusCode, text: &str) -> Response {
    (status, Json(json!({ "detail": text }))).into_response()
}

/// Deviation D-06: refuse requests that did not come from this machine's
/// own pages. A missing Host (HTTP/1.0) or Origin (non-browser clients such
/// as MCP hosts and scripts) is allowed.
pub async fn loopback_only(request: Request, next: Next) -> Response {
    let host = request.headers().get(header::HOST).map(HeaderValue::as_bytes);
    let authority = request.uri().authority().map(|a| a.as_str().as_bytes());
    for value in host.into_iter().chain(authority) {
        if !std::str::from_utf8(value).is_ok_and(is_loopback_authority) {
            return detail(StatusCode::MISDIRECTED_REQUEST, "Invalid Host header");
        }
    }
    for origin in request.headers().get_all(header::ORIGIN) {
        if !std::str::from_utf8(origin.as_bytes()).is_ok_and(is_allowed_origin_d06) {
            return detail(StatusCode::FORBIDDEN, "Invalid Origin header");
        }
    }
    next.run(request).await
}

// --- CORS (Starlette CORSMiddleware) ---------------------------------------

/// `allow_methods=["*"]` expands to this tuple; a preflight's requested
/// method must be one of them, case-sensitively.
const ALL_METHODS: [&str; 7] = ["DELETE", "GET", "HEAD", "OPTIONS", "PATCH", "POST", "PUT"];

/// main.py's `allow_origins`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CorsPolicy {
    origins: Vec<String>,
}

impl CorsPolicy {
    /// The list main.py builds from `FRONTEND_PORT` and `PLUMDECK_PORT`.
    pub fn for_ports(plumdeck_port: u16, frontend_port: u16) -> Self {
        Self {
            origins: vec![
                format!("http://localhost:{frontend_port}"),
                format!("http://127.0.0.1:{frontend_port}"),
                format!("http://localhost:{plumdeck_port}"),
                format!("http://127.0.0.1:{plumdeck_port}"),
                "tauri://localhost".into(),
                "http://tauri.localhost".into(),
                "https://tauri.localhost".into(),
            ],
        }
    }

    pub fn origins(&self) -> &[String] {
        &self.origins
    }

    fn allows(&self, origin: &str) -> bool {
        self.origins.iter().any(|allowed| allowed == origin)
    }
}

/// Starlette decodes header values as Latin-1.
fn latin1(value: &HeaderValue) -> String {
    value.as_bytes().iter().map(|&b| char::from(b)).collect()
}

fn first_header(headers: &HeaderMap, name: &str) -> Option<String> {
    headers.get(name).map(latin1)
}

fn set(headers: &mut HeaderMap, name: &'static str, value: HeaderValue) {
    headers.insert(HeaderName::from_static(name), value);
}

fn preflight(policy: &CorsPolicy, headers: &HeaderMap) -> Response {
    let origin = headers.get(header::ORIGIN).cloned().unwrap_or_else(|| HeaderValue::from_static(""));
    let requested_method = first_header(headers, "access-control-request-method").unwrap_or_default();
    let requested_headers = headers.get("access-control-request-headers").cloned();

    let mut response = Response::new(Body::empty());
    let out = response.headers_mut();
    set(out, "vary", HeaderValue::from_static("Origin"));
    set(out, "access-control-allow-methods", HeaderValue::from_static("DELETE, GET, HEAD, OPTIONS, PATCH, POST, PUT"));
    set(out, "access-control-max-age", HeaderValue::from_static("600"));
    set(out, "access-control-allow-credentials", HeaderValue::from_static("true"));
    let mut failures = Vec::new();
    if policy.allows(&latin1(&origin)) {
        set(out, "access-control-allow-origin", origin);
    } else {
        failures.push("origin");
    }
    if !ALL_METHODS.contains(&requested_method.as_str()) {
        failures.push("method");
    }
    // allow_headers=["*"]: mirror whatever was requested.
    if let Some(requested) = requested_headers {
        set(out, "access-control-allow-headers", requested);
    }
    if headers.contains_key("access-control-request-private-network") {
        failures.push("private-network");
    }
    let (status, text) = if failures.is_empty() {
        (StatusCode::OK, "OK".to_owned())
    } else {
        (StatusCode::BAD_REQUEST, format!("Disallowed CORS {}", failures.join(", ")))
    };
    set(out, "content-type", HeaderValue::from_static("text/plain; charset=utf-8"));
    *response.status_mut() = status;
    *response.body_mut() = Body::from(text);
    response
}

/// `CORSMiddleware.__call__`: requests without Origin pass untouched;
/// preflights (OPTIONS with Access-Control-Request-Method) are answered
/// here for any path; every other response gets the simple headers.
pub async fn cors(State(policy): State<Arc<CorsPolicy>>, request: Request, next: Next) -> Response {
    let Some(origin) = request.headers().get(header::ORIGIN).cloned() else {
        return next.run(request).await;
    };
    if request.method() == Method::OPTIONS && request.headers().contains_key("access-control-request-method") {
        return preflight(&policy, request.headers());
    }
    let mut response = next.run(request).await;
    if response.extensions().get::<ServerErrorMarker>().is_some() {
        return response;
    }
    let headers = response.headers_mut();
    set(headers, "access-control-allow-credentials", HeaderValue::from_static("true"));
    if policy.allows(&latin1(&origin)) {
        set(headers, "access-control-allow-origin", origin);
        // MutableHeaders.add_vary_header: append to the first Vary value.
        let vary = match headers.get(header::VARY) {
            Some(existing) => {
                let mut merged = existing.as_bytes().to_vec();
                merged.extend_from_slice(b", Origin");
                HeaderValue::from_bytes(&merged).unwrap_or_else(|_| HeaderValue::from_static("Origin"))
            }
            None => HeaderValue::from_static("Origin"),
        };
        set(headers, "vary", vary);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_authorities() {
        for ok in ["127.0.0.1", "127.0.0.1:8001", "localhost", "LocalHost:1420", "[::1]", "[::1]:48123"] {
            assert!(is_loopback_authority(ok), "{ok}");
        }
        for bad in [
            "",
            "evil.example",
            "127.0.0.1.nip.io",
            "localhost.evil:80",
            "127.0.0.2",
            "localhost:",
            "localhost:80x",
            "localhost:123456",
            "[::1",
            "[::2]:80",
            "user@localhost",
            "0.0.0.0:8001",
        ] {
            assert!(!is_loopback_authority(bad), "{bad}");
        }
    }

    #[test]
    fn d06_origins() {
        for ok in [
            "http://localhost:1420",
            "http://127.0.0.1:8001",
            "https://[::1]:5173",
            "tauri://localhost",
            "http://tauri.localhost",
            "https://tauri.localhost",
            "http://localhost",
        ] {
            assert!(is_allowed_origin_d06(ok), "{ok}");
        }
        for bad in ["null", "http://evil.example", "tauri://evil", "file://", "http://localhost:1420/path", "ws://localhost:1"] {
            assert!(!is_allowed_origin_d06(bad), "{bad}");
        }
    }

    #[test]
    fn cors_origins_follow_main_py() {
        let policy = CorsPolicy::for_ports(48123, 1420);
        assert!(policy.allows("http://127.0.0.1:48123"));
        assert!(policy.allows("https://tauri.localhost"));
        assert!(!policy.allows("http://127.0.0.1:8001"));
        assert!(!policy.allows("http://localhost:1420/"));
        assert_eq!(policy.origins().len(), 7);
    }
}
