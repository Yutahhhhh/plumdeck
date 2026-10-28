//! Headless backend: `plumdeck-backend [--port N] [--parity]`.
//!
//! Reads the same environment as the Python sidecar (PLUMDECK_PORT, DB_PATH,
//! USER_DATA_DIR, PLUMDECK_LOG_DIR, PLUMDECK_MANAGED_SIDECAR, ...), binds
//! 127.0.0.1 and stops gracefully on SIGINT/SIGTERM or, as a managed sidecar,
//! on the desktop's `plumdeck:shutdown` stdin line. Startup opens the library
//! at DB_PATH before binding (stages database, queue, ready); shutdown
//! CHECKPOINTs and closes it. A failed startup exits with status 3.
//!
//! `--parity` (side B of plumdeck-parity) refuses to start unless DB_PATH,
//! USER_DATA_DIR and PLUMDECK_REKORDBOX_DB are set and nothing resolves inside
//! the live library or rekordbox's folders.

use std::io::IsTerminal;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use plumdeck_backend::sandbox::check_parity_sandbox;
use plumdeck_backend::startup::{close_library, open_library, CompactionNotPorted, Library};
use plumdeck_backend::{serve, shutdown_signal, stdin_shutdown_line, StartupStage, GRACEFUL_SHUTDOWN_SECS};
use plumdeck_core::config::Config;
use plumdeck_core::paths::{live_data_dirs, live_rekordbox_dirs};
use plumdeck_core::time::SystemClock;
use tokio::net::TcpListener;

#[derive(Parser, Debug)]
#[command(name = "plumdeck-backend", about = "Headless plumdeck backend server")]
struct Args {
    /// Port on 127.0.0.1 (default: PLUMDECK_PORT, then 8001).
    #[arg(long)]
    port: Option<u16>,
    /// Parity side B: only run against sandbox copies (see module docs).
    #[arg(long)]
    parity: bool,
}

fn main() -> ExitCode {
    let args = Args::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .init();

    println!("{}", StartupStage::Settings.line());
    let config = match Config::from_env() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("plumdeck-backend: {error}");
            return ExitCode::from(2);
        }
    };
    if args.parity {
        let vars: Vec<(String, String)> = std::env::vars().collect();
        let env = |name: &str| std::env::var(name).ok();
        let home = std::env::home_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        let checked =
            check_parity_sandbox(&vars, &config, &live_data_dirs(&env, &home), &live_rekordbox_dirs(&env, &home));
        if let Err(error) = checked {
            eprintln!("plumdeck-backend: refusing to start: {error}");
            return ExitCode::from(2);
        }
    }
    if let Err(error) = std::fs::create_dir_all(&config.user_data_dir) {
        eprintln!("plumdeck-backend: cannot create {}: {error}", config.user_data_dir.display());
        return ExitCode::from(1);
    }
    let port = args.port.unwrap_or(config.port);

    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio runtime");
    runtime.block_on(async move {
        println!("{}", StartupStage::Services.line());
        // connection.py creates the database folder when it is imported.
        if let Some(parent) = config.db_path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            if let Err(error) = std::fs::create_dir_all(parent) {
                eprintln!("plumdeck-backend: cannot create {}: {error}", parent.display());
                return ExitCode::from(1);
            }
        }
        println!("Starting plumdeck Backend Server on port {port}...");
        println!("User Data Directory: {}", config.user_data_dir.display());

        // Listen for a stop request from here on; startup itself runs to the
        // end (uvicorn finishes the lifespan startup, then exits).
        let managed = config.managed_sidecar;
        let signal = shutdown_signal();
        let (request_stop, mut stop_requested) = tokio::sync::watch::channel(false);
        tokio::spawn(async move {
            if managed {
                tokio::select! {
                    _ = signal => {}
                    _ = stdin_shutdown_line() => {}
                }
            } else {
                signal.await;
            }
            let _ = request_stop.send(true);
        });

        // main.py lifespan, before the socket is bound (uvicorn's order).
        let db_path = config.db_path.clone();
        let opened = tokio::task::spawn_blocking(move || {
            open_library(&db_path, &CompactionNotPorted, Arc::new(SystemClock), &mut |stage| {
                println!("{}", stage.line());
            })
        })
        .await
        .expect("startup task");
        let library = match opened {
            Ok(library) => library,
            Err(error) => {
                println!("{error}");
                eprintln!("plumdeck-backend: application startup failed");
                return ExitCode::from(STARTUP_FAILURE);
            }
        };
        println!("{}", StartupStage::Ready.line());
        if *stop_requested.borrow() {
            close(library).await;
            return ExitCode::SUCCESS;
        }

        let listener = match TcpListener::bind(("127.0.0.1", port)).await {
            Ok(listener) => listener,
            Err(error) => {
                // Never take a port from another process; report and exit.
                eprintln!("plumdeck-backend: cannot bind 127.0.0.1:{port}: {error}");
                close(library).await;
                return ExitCode::from(1);
            }
        };
        let (stop, mut stopped) = tokio::sync::watch::channel(false);
        let mut server = tokio::spawn(serve(listener, async move {
            let _ = stopped.wait_for(|stop| *stop).await;
        }));
        tracing::info!(port, "listening on 127.0.0.1");

        let requested = async move {
            let _ = stop_requested.wait_for(|stop| *stop).await;
        };
        let code = tokio::select! {
            result = &mut server => {
                eprintln!("plumdeck-backend: server stopped: {result:?}");
                ExitCode::from(1)
            }
            _ = requested => {
                tracing::info!("shutting down");
                let _ = stop.send(true);
                match tokio::time::timeout(Duration::from_secs(GRACEFUL_SHUTDOWN_SECS), server).await {
                    Ok(Ok(Ok(()))) => ExitCode::SUCCESS,
                    Ok(result) => {
                        eprintln!("plumdeck-backend: server error: {result:?}");
                        ExitCode::from(1)
                    }
                    Err(_) => {
                        tracing::warn!("graceful shutdown timed out");
                        ExitCode::SUCCESS
                    }
                }
            }
        };
        // main.py lifespan shutdown: CHECKPOINT and close.
        close(library).await;
        code
    })
}

/// uvicorn's exit status when the application fails to start.
const STARTUP_FAILURE: u8 = 3;

async fn close(library: Library) {
    let message = tokio::task::spawn_blocking(move || close_library(&library)).await.expect("shutdown task");
    if let Some(message) = message {
        println!("{message}");
    }
}
