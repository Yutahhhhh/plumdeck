//! The plumdeck backend server, assembled for headless use (`plumdeck-backend`
//! binary) and, from S13, for running inside the Tauri process.
//!
//! Routes so far: `GET /` (S0), `GET/POST /api/settings` and
//! `GET /api/dashboard` (S2), the library core (S3: tracks, lyrics, genres,
//! `/api/stream`, `/api/metadata`, see [`routes`]); everything else gets the
//! Python backend's plain-text 404 ([`middleware::unmatched`]). Every route sits behind the
//! layers in [`middleware`] (panic → 500, loopback Host/Origin, CORS, HEAD).
//! S1 opens the library before serving and closes it after ([`startup`]).
//! Startup progress lines (`PLUMDECK_STARTUP:{...}`) and the managed-sidecar
//! stdin shutdown line keep the desktop's and scripts/smoke-backend.py's
//! contracts with the Python sidecar. Logs go where Python's go
//! ([`logging`]).

pub mod body;
pub mod lax;
pub mod logging;
pub mod middleware;
pub mod models;
pub mod query;
pub mod response;
pub mod routes;
pub mod sandbox;
pub mod startup;
pub mod tasks;

use std::future::Future;
use std::io;
use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use axum::http::StatusCode;
use axum::middleware::{from_fn, from_fn_with_state};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use plumdeck_core::compat::pyjson::{dumps, DumpOptions, PyValue};
use plumdeck_core::error::ApiError;
use plumdeck_core::time::{Clock, SystemClock};
use plumdeck_db::handle::DbHandle;
use plumdeck_db::DbError;
use tokio::net::TcpListener;

use middleware::{catch_panic_layer, cors, head_is_unmatched, internal_server_error, loopback_only, CorsPolicy};
pub use middleware::unmatched;
use response::PyJson;

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

/// An unexpected failure: logged, answered like an unhandled Python
/// exception (500 "Internal Server Error").
pub fn server_error(error: &dyn std::fmt::Display) -> Response {
    tracing::error!(target: "plumdeck_backend::http", "request failed: {error}");
    internal_server_error()
}

/// What every handler shares: the library, the CORS policy and the clock
/// for stored timestamps (local naive time, plan R3).
#[derive(Clone)]
pub struct AppState {
    pub db: DbHandle,
    pub cors: Arc<CorsPolicy>,
    pub clock: Arc<dyn Clock>,
    /// The release-date / lyrics job (S8).
    pub metadata: routes::metadata_jobs::MetadataJobs,
    /// The Assist window and agent bridge (S6).
    pub assist: routes::assist::AssistBridge,
}

impl AppState {
    pub fn new(db: DbHandle, cors: CorsPolicy) -> Self {
        let library_dir = db.path().parent().map(std::path::Path::to_path_buf).unwrap_or_default();
        Self { db, cors: Arc::new(cors), clock: Arc::new(SystemClock), metadata: routes::metadata_jobs::MetadataJobs::new(library_dir), assist: Default::default() }
    }

    /// Run blocking work (DuckDB) off the async workers; a panic in it is a
    /// 500, as it would be in the handler itself.
    pub async fn blocking_result<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, Response> {
        tokio::task::spawn_blocking(work).await.map_err(|error| server_error(&error))
    }

    /// [`Self::blocking_result`] for database work: any [`DbError`] is a 500
    /// (Python let SQL and pool errors escape as unhandled exceptions).
    pub async fn blocking<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> Result<T, DbError> + Send + 'static,
    ) -> Result<T, Response> {
        self.blocking_result(work).await?.map_err(|error| server_error(&error))
    }
}

async fn root() -> PyJson {
    PyJson(response::dict([("message", response::text(ROOT_MESSAGE))]))
}

async fn unmatched_handler() -> Response {
    unmatched()
}

/// The HTTP routes with their middleware.
pub fn router(state: AppState) -> Router {
    let cors_policy = state.cors.clone();
    let routes = Router::new()
        .route("/", get(root))
        .merge(routes::api())
        .fallback(unmatched_handler)
        .method_not_allowed_fallback(unmatched_handler)
        .with_state(state);
    with_layers(routes, cors_policy)
}

/// The middleware stack of [`router`], outermost last (see [`middleware`]).
pub fn with_layers(routes: Router, cors_policy: Arc<CorsPolicy>) -> Router {
    routes
        // Starlette reads bodies of any size.
        .layer(DefaultBodyLimit::disable())
        .layer(from_fn(head_is_unmatched))
        .layer(from_fn_with_state(cors_policy, cors))
        .layer(from_fn(loopback_only))
        .layer(catch_panic_layer())
}

/// Serve [`router`] on `listener` until `shutdown` resolves, then finish
/// in-flight requests.
pub async fn serve(
    listener: TcpListener,
    state: AppState,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> io::Result<()> {
    axum::serve(listener, router(state)).with_graceful_shutdown(shutdown).await
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
pub(crate) mod test_support {
    use std::path::PathBuf;

    use super::*;
    use axum::body::{to_bytes, Body};
    use axum::http::{HeaderMap, Method, Request};
    use tower::ServiceExt;

    /// A fresh v5 library in a temp dir, removed on drop.
    pub struct TestLibrary {
        pub db: DbHandle,
        dir: PathBuf,
    }

    impl TestLibrary {
        pub fn new(name: &str) -> Self {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let unique = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("plumdeck-backend-{name}-{}-{unique}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let db = DbHandle::open(&dir.join("plumdeck.duckdb")).unwrap();
            db.write(|conn| plumdeck_db::schema::init_schema(conn)).unwrap();
            Self { db, dir }
        }

        pub fn state(&self) -> AppState {
            AppState::new(self.db.clone(), CorsPolicy::for_ports(8001, 1420))
        }

        pub fn execute(&self, sql: &str) {
            self.db.write(|conn| Ok(conn.execute_batch(sql)?)).unwrap();
        }
    }

    impl Drop for TestLibrary {
        fn drop(&mut self) {
            let _ = self.db.shutdown(std::time::Duration::from_secs(5));
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    pub struct Answer {
        pub status: StatusCode,
        pub headers: HeaderMap,
        pub body: String,
    }

    impl Answer {
        pub fn header(&self, name: &str) -> Option<&str> {
            self.headers.get(name).map(|v| v.to_str().unwrap())
        }

        pub fn json(&self) -> serde_json::Value {
            serde_json::from_str(&self.body).unwrap()
        }
    }

    pub async fn send(app: Router, method: Method, path: &str, headers: &[(&str, &str)], body: &str) -> Answer {
        let mut request = Request::builder().method(method).uri(path);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = app.oneshot(request.body(Body::from(body.to_owned())).unwrap()).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = to_bytes(response.into_body(), 1 << 20).await.unwrap();
        Answer { status, headers, body: String::from_utf8(body.to_vec()).unwrap() }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{send, TestLibrary};
    use super::*;
    use axum::http::{header, Method};

    async fn call(method: Method, path: &str) -> (StatusCode, String, String) {
        let library = TestLibrary::new("call");
        let answer = send(router(library.state()), method, path, &[], "").await;
        let content_type = answer.header(header::CONTENT_TYPE.as_str()).unwrap_or_default().to_owned();
        (answer.status, content_type, answer.body)
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
        let library = TestLibrary::new("unmatched");
        for (method, path) in [
            (Method::GET, "/api/does-not-exist"),
            (Method::GET, "/api/"),
            (Method::GET, "/api/settings/"),
            (Method::POST, "/"),
            (Method::DELETE, "/"),
            (Method::PUT, "/api/settings"),
            (Method::DELETE, "/api/dashboard"),
            (Method::OPTIONS, "/"),
        ] {
            let answer = send(router(library.state()), method.clone(), path, &[], "").await;
            assert_eq!(answer.status, StatusCode::NOT_FOUND, "{method} {path}");
            assert_eq!(answer.header("content-type"), Some("text/plain; charset=utf-8"), "{method} {path}");
            assert_eq!(answer.body, "Not Found", "{method} {path}");
        }
        for path in ["/", "/api/settings", "/api/dashboard"] {
            let answer = send(router(library.state()), Method::HEAD, path, &[], "").await;
            assert_eq!(answer.status, StatusCode::NOT_FOUND);
            assert_eq!(answer.header("content-type"), Some("text/plain; charset=utf-8"));
            // axum drops HEAD bodies, as uvicorn does.
            assert_eq!(answer.body, "");
        }
    }

    #[tokio::test]
    async fn cors_follows_starlette() {
        let library = TestLibrary::new("cors");
        let app = || router(library.state());
        let allowed = [("origin", "http://localhost:1420")];
        let answer = send(app(), Method::GET, "/api/settings", &allowed, "").await;
        assert_eq!(answer.status, StatusCode::OK);
        assert_eq!(answer.header("access-control-allow-origin"), Some("http://localhost:1420"));
        assert_eq!(answer.header("access-control-allow-credentials"), Some("true"));
        assert_eq!(answer.header("vary"), Some("Origin"));
        // A loopback origin outside the list: credentials only, as Starlette.
        let other = send(app(), Method::GET, "/nope", &[("origin", "http://localhost:5555")], "").await;
        assert_eq!(other.status, StatusCode::NOT_FOUND);
        assert_eq!(other.header("access-control-allow-credentials"), Some("true"));
        assert_eq!(other.header("access-control-allow-origin"), None);
        assert_eq!(other.header("vary"), None);
        // No Origin: no CORS headers at all.
        let plain = send(app(), Method::GET, "/", &[], "").await;
        assert_eq!(plain.header("access-control-allow-credentials"), None);

        let preflight = [
            ("origin", "tauri://localhost"),
            ("access-control-request-method", "PATCH"),
            ("access-control-request-headers", "X-Foo, content-type"),
        ];
        let answer = send(app(), Method::OPTIONS, "/api/anything", &preflight, "").await;
        assert_eq!(answer.status, StatusCode::OK);
        assert_eq!(answer.body, "OK");
        assert_eq!(answer.header("content-type"), Some("text/plain; charset=utf-8"));
        assert_eq!(answer.header("access-control-allow-methods"), Some("DELETE, GET, HEAD, OPTIONS, PATCH, POST, PUT"));
        assert_eq!(answer.header("access-control-allow-headers"), Some("X-Foo, content-type"));
        assert_eq!(answer.header("access-control-max-age"), Some("600"));
        assert_eq!(answer.header("access-control-allow-origin"), Some("tauri://localhost"));
        let refused = [
            ("origin", "http://localhost:5555"),
            ("access-control-request-method", "patch"),
            ("access-control-request-private-network", "true"),
        ];
        let answer = send(app(), Method::OPTIONS, "/", &refused, "").await;
        assert_eq!(answer.status, StatusCode::BAD_REQUEST);
        assert_eq!(answer.body, "Disallowed CORS origin, method, private-network");
        assert_eq!(answer.header("access-control-allow-origin"), None);
        assert_eq!(answer.header("vary"), Some("Origin"));
    }

    #[tokio::test]
    async fn d06_refuses_foreign_hosts_and_origins() {
        let library = TestLibrary::new("d06");
        let app = || router(library.state());
        let answer = send(app(), Method::GET, "/", &[("host", "evil.example:8001")], "").await;
        assert_eq!(answer.status, StatusCode::MISDIRECTED_REQUEST);
        assert_eq!(answer.body, r#"{"detail":"Invalid Host header"}"#);
        let answer = send(app(), Method::GET, "/", &[("host", "127.0.0.1:8001"), ("origin", "http://evil.example")], "").await;
        assert_eq!(answer.status, StatusCode::FORBIDDEN);
        assert_eq!(answer.body, r#"{"detail":"Invalid Origin header"}"#);
        assert_eq!(answer.header("access-control-allow-credentials"), None);
        let answer = send(app(), Method::GET, "/", &[("host", "localhost:8001"), ("origin", "null")], "").await;
        assert_eq!(answer.status, StatusCode::FORBIDDEN);
        let answer = send(app(), Method::GET, "/", &[("host", "[::1]:8001"), ("origin", "http://localhost:5555")], "").await;
        assert_eq!(answer.status, StatusCode::OK);
    }

    #[tokio::test]
    async fn panics_answer_500_without_cors() {
        async fn boom() -> &'static str {
            panic!("boom")
        }
        let app = with_layers(Router::new().route("/boom", get(boom)), Arc::new(CorsPolicy::for_ports(8001, 1420)));
        let answer = send(app, Method::GET, "/boom", &[("origin", "http://localhost:1420")], "").await;
        assert_eq!(answer.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(answer.header("content-type"), Some("text/plain; charset=utf-8"));
        assert_eq!(answer.body, "Internal Server Error");
        assert_eq!(answer.header("access-control-allow-origin"), None);
        assert_eq!(answer.header("access-control-allow-credentials"), None);
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
        let library = TestLibrary::new("serve");
        let server = tokio::spawn(serve(listener, library.state(), async {
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
