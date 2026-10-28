//! The plumdeck backend server, assembled for headless use (`plumdeck-backend`
//! binary) and, from S13, for running inside the Tauri process.
//!
//! S0 serves only the health check `GET /`; everything else gets the Python
//! backend's plain-text 404 (see [`unmatched`]). S1 opens the library before
//! serving and closes it after ([`startup`]). Startup progress lines
//! (`PLUMDECK_STARTUP:{...}`) and the managed-sidecar stdin shutdown line keep
//! the desktop's and scripts/smoke-backend.py's contracts with the Python
//! sidecar. Logs go where Python's go ([`logging`]).

pub mod logging;
pub mod sandbox;
pub mod startup;

use std::future::Future;
use std::io;

use axum::extract::Request;
use axum::http::{header, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use plumdeck_core::compat::pyjson::{dumps, DumpOptions, PyValue};
use plumdeck_core::error::ApiError;
use serde_json::{json, Value};
use tokio::net::TcpListener;

/// Body of `GET /` (main.py: root).
pub const ROOT_MESSAGE: &str = "plumdeck Backend API is running";

/// Seconds the server waits for open requests after a shutdown request
/// (uvicorn's `timeout_graceful_shutdown=5`).
pub const GRACEFUL_SHUTDOWN_SECS: u64 = 5;

/// [`ApiError`] as an axum response: the status plus `{"detail": ...}`.
#[derive(Debug)]
pub struct HttpError(pub ApiError);

impl From<ApiError> for HttpError {
    fn from(error: ApiError) -> Self {
        Self(error)
    }
}

impl IntoResponse for HttpError {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.0.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        (status, Json(self.0.body())).into_response()
    }
}

async fn root() -> Json<Value> {
    Json(json!({ "message": ROOT_MESSAGE }))
}

/// What the Python backend answers when no route takes a request.
///
/// main.py mounts the MCP app at `/`, so every request FastAPI's routes do
/// not fully match (an unknown path, a known path with another method such as
/// `POST /`, `HEAD` on a GET route, a plain `OPTIONS`) falls into it and gets
/// Starlette's plain-text 404, never FastAPI's JSON 404 or a 405. Kept
/// bug-compatible until a deviation is approved. CORS preflights are answered
/// by the CORS middleware before this point (S2).
pub fn unmatched() -> Response {
    (StatusCode::NOT_FOUND, [(header::CONTENT_TYPE, "text/plain; charset=utf-8")], "Not Found").into_response()
}

async fn unmatched_handler() -> Response {
    unmatched()
}

/// FastAPI's GET routes do not answer HEAD (axum's do), so HEAD is unmatched.
async fn head_is_unmatched(request: Request, next: Next) -> Response {
    if request.method() == Method::HEAD {
        return unmatched();
    }
    next.run(request).await
}

/// The HTTP routes.
pub fn router() -> Router {
    Router::new()
        .route("/", get(root))
        .fallback(unmatched_handler)
        .method_not_allowed_fallback(unmatched_handler)
        .layer(middleware::from_fn(head_is_unmatched))
}

/// Serve `router()` on `listener` until `shutdown` resolves, then finish
/// in-flight requests.
pub async fn serve(
    listener: TcpListener,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> io::Result<()> {
    axum::serve(listener, router()).with_graceful_shutdown(shutdown).await
}

/// Startup milestones, as startup_progress.report prints them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartupStage {
    Settings,
    Services,
    Database,
    Queue,
    Ready,
}

impl StartupStage {
    fn parts(self) -> (&'static str, &'static str, i128) {
        match self {
            Self::Settings => ("settings", "設定・保存先を読み込んでいます", 1),
            Self::Services => ("services", "ライブラリ・解析・再生の機能を読み込んでいます", 2),
            Self::Database => ("database", "楽曲データベースを準備しています", 3),
            Self::Queue => ("queue", "前回の解析・取り込み状態を復元しています", 4),
            Self::Ready => ("ready", "起動処理が完了しました。接続を確認しています", 5),
        }
    }

    /// The stdout line, byte-identical to the Python sidecar's.
    pub fn line(self) -> String {
        let (stage, label, completed) = self.parts();
        let payload = PyValue::Dict(vec![
            (PyValue::Str("stage".into()), PyValue::Str(stage.into())),
            (PyValue::Str("label".into()), PyValue::Str(label.into())),
            (PyValue::Str("completed".into()), PyValue::Int(completed)),
            (PyValue::Str("total".into()), PyValue::Int(5)),
        ]);
        let body = dumps(&payload, &DumpOptions::unicode()).expect("startup payload is serializable");
        format!("PLUMDECK_STARTUP:{body}")
    }
}

/// Resolves on SIGINT, or SIGTERM on Unix. On Unix the handlers are
/// installed when this is called, not when the future is first polled, so a
/// signal that arrives during startup is not lost (it then stops the server
/// right after startup, as uvicorn does). Call inside the runtime.
pub fn shutdown_signal() -> impl Future<Output = ()> + Send + 'static {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let interrupt = signal(SignalKind::interrupt());
        let terminate = signal(SignalKind::terminate());
        async move {
            match (interrupt, terminate) {
                (Ok(mut interrupt), Ok(mut terminate)) => {
                    tokio::select! {
                        _ = interrupt.recv() => {}
                        _ = terminate.recv() => {}
                    }
                }
                _ => {
                    let _ = tokio::signal::ctrl_c().await;
                }
            }
        }
    }
    #[cfg(not(unix))]
    async {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// For a managed sidecar: resolves when the desktop writes
/// `plumdeck:shutdown` to stdin or closes it (server.py watch_parent).
pub async fn stdin_shutdown_line() {
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("desktop-lifecycle".into())
        .spawn(move || {
            use std::io::{BufRead, Read};
            let stdin = io::stdin();
            let mut lines = stdin.lock();
            let mut line = Vec::new();
            loop {
                line.clear();
                // Python reads at most 1024 bytes per line.
                match (&mut lines).take(1024).read_until(b'\n', &mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) if line.trim_ascii() == b"plumdeck:shutdown" => break,
                    Ok(_) => {}
                }
            }
            let _ = tx.send(());
        })
        .expect("spawn stdin watcher");
    let _ = rx.await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{to_bytes, Body};
    use axum::http::{header, Method, Request};
    use tower::ServiceExt;

    async fn call(method: Method, path: &str) -> (StatusCode, String, String) {
        let request = Request::builder().method(method).uri(path).body(Body::empty()).unwrap();
        let response = router().oneshot(request).await.unwrap();
        let status = response.status();
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default();
        let body = to_bytes(response.into_body(), 1 << 20).await.unwrap();
        (status, content_type, String::from_utf8(body.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn root_matches_fastapi_bytes() {
        let (status, content_type, body) = call(Method::GET, "/").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(content_type, "application/json");
        assert_eq!(body, r#"{"message":"plumdeck Backend API is running"}"#);
    }

    #[tokio::test]
    async fn unmatched_requests_get_the_mounts_plain_404() {
        for (method, path) in [
            (Method::GET, "/api/does-not-exist"),
            (Method::GET, "/api/"),
            (Method::POST, "/"),
            (Method::DELETE, "/"),
            (Method::OPTIONS, "/"),
        ] {
            let (status, content_type, body) = call(method.clone(), path).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{method} {path}");
            assert_eq!(content_type, "text/plain; charset=utf-8", "{method} {path}");
            assert_eq!(body, "Not Found", "{method} {path}");
        }
        let (status, content_type, body) = call(Method::HEAD, "/").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(content_type, "text/plain; charset=utf-8");
        // axum drops HEAD bodies, as uvicorn does.
        assert_eq!(body, "");
    }

    #[test]
    fn startup_lines_match_python() {
        assert_eq!(
            StartupStage::Settings.line(),
            r#"PLUMDECK_STARTUP:{"stage": "settings", "label": "設定・保存先を読み込んでいます", "completed": 1, "total": 5}"#
        );
    }

    #[tokio::test]
    async fn serve_answers_then_shuts_down_gracefully() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(serve(listener, async {
            let _ = stopped.await;
        }));
        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
        assert!(response.ends_with(r#"{"message":"plumdeck Backend API is running"}"#));
        stop.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), server)
            .await
            .expect("server stops")
            .unwrap()
            .unwrap();
    }
}
