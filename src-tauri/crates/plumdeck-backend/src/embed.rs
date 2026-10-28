//! The backend inside another process (S13): the desktop app starts the
//! same server the `plumdeck-backend` binary runs, on its own thread and
//! tokio runtime, and stops it on exit. Startup stages are reported through
//! a callback (the desktop turns them into `backend://startup` events).

use std::sync::Arc;
use std::time::Duration;

use plumdeck_core::config::Config;
use plumdeck_core::time::SystemClock;
use tokio::net::TcpListener;

use crate::middleware::CorsPolicy;
use crate::startup::{close_library, open_library, HealthCheckWithoutRebuild};
use crate::{serve, AppState, StartupStage, GRACEFUL_SHUTDOWN_SECS};

/// A running embedded backend.
pub struct Embedded {
    stop: tokio::sync::watch::Sender<bool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Embedded {
    /// Stop serving, CHECKPOINT and close the library, and wait for it.
    pub fn shutdown(mut self) {
        let _ = self.stop.send(true);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Start the backend on 127.0.0.1:`port` with the environment's config.
/// `on_stage` receives each startup stage; `on_failure` the reason when
/// startup fails or the server stops on its own.
pub fn start(
    port: u16,
    on_stage: impl Fn(StartupStage) + Send + Sync + 'static,
    on_failure: impl Fn(String) + Send + Sync + 'static,
) -> Result<Embedded, String> {
    let config = Config::from_env().map_err(|error| error.to_string())?;
    crate::logging::init(&config.log_dir);
    std::fs::create_dir_all(&config.user_data_dir).map_err(|error| error.to_string())?;
    if let Some(parent) = config.db_path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let (stop, mut stopped) = tokio::sync::watch::channel(false);
    let cors = CorsPolicy::for_ports(port, config.frontend_port);
    let on_stage = Arc::new(on_stage);
    let thread = std::thread::Builder::new()
        .name("plumdeck-backend".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().thread_name("plumdeck-backend-worker").build() {
                Ok(runtime) => runtime,
                Err(error) => return on_failure(format!("バックエンドを起動できません: {error}")),
            };
            runtime.block_on(async move {
                on_stage(StartupStage::Settings);
                on_stage(StartupStage::Services);
                let db_path = config.db_path.clone();
                let stages = on_stage.clone();
                let opened = tokio::task::spawn_blocking(move || open_library(&db_path, &HealthCheckWithoutRebuild, Arc::new(SystemClock), &mut |stage| stages(stage))).await;
                let library = match opened {
                    Ok(Ok(library)) => library,
                    Ok(Err(error)) => return on_failure(error.to_string()),
                    Err(error) => return on_failure(error.to_string()),
                };
                on_stage(StartupStage::Ready);
                let listener = match TcpListener::bind(("127.0.0.1", port)).await {
                    Ok(listener) => listener,
                    Err(error) => {
                        on_failure(format!("127.0.0.1:{port} を使用できません: {error}"));
                        let _ = tokio::task::spawn_blocking(move || close_library(&library)).await;
                        return;
                    }
                };
                let state = AppState::new(library.db.clone(), cors);
                let jobs = state.analysis.jobs.clone();
                tokio::task::spawn_blocking(move || {
                    jobs.launch_next();
                    jobs.enqueue_missing_embeddings();
                });
                let mut shutdown = stopped.clone();
                let mut server = tokio::spawn(serve(listener, state, async move {
                    let _ = shutdown.wait_for(|stop| *stop).await;
                }));
                tokio::select! {
                    result = &mut server => on_failure(format!("バックエンドが停止しました: {result:?}")),
                    _ = stopped.wait_for(|stop| *stop) => {
                        let _ = tokio::time::timeout(Duration::from_secs(GRACEFUL_SHUTDOWN_SECS), server).await;
                    }
                }
                let _ = tokio::task::spawn_blocking(move || close_library(&library)).await;
            });
        })
        .map_err(|error| error.to_string())?;
    Ok(Embedded { stop, thread: Some(thread) })
}

/// `--analysis-worker` for a host binary: run one request from stdin and
/// return the exit code.
pub fn run_analysis_worker() -> i32 {
    let stdin = std::io::stdin();
    plumdeck_analysis::run_worker(&mut stdin.lock(), &mut std::io::stdout())
}
