//! Headless backend: `plumdeck-backend [--port N]`.
//!
//! Reads the same environment as the Python sidecar (PLUMDECK_PORT, DB_PATH,
//! USER_DATA_DIR, PLUMDECK_LOG_DIR, PLUMDECK_MANAGED_SIDECAR, ...), binds
//! 127.0.0.1 and stops gracefully on SIGINT/SIGTERM or, as a managed sidecar,
//! on the desktop's `plumdeck:shutdown` stdin line.

use std::io::IsTerminal;
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;
use plumdeck_backend::{serve, shutdown_signal, stdin_shutdown_line, StartupStage, GRACEFUL_SHUTDOWN_SECS};
use plumdeck_core::config::Config;
use tokio::net::TcpListener;

#[derive(Parser, Debug)]
#[command(name = "plumdeck-backend", about = "Headless plumdeck backend server")]
struct Args {
    /// Port on 127.0.0.1 (default: PLUMDECK_PORT, then 8001).
    #[arg(long)]
    port: Option<u16>,
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
    if let Err(error) = std::fs::create_dir_all(&config.user_data_dir) {
        eprintln!("plumdeck-backend: cannot create {}: {error}", config.user_data_dir.display());
        return ExitCode::from(1);
    }
    let port = args.port.unwrap_or(config.port);

    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio runtime");
    runtime.block_on(async move {
        println!("{}", StartupStage::Services.line());
        println!("Starting plumdeck Backend Server on port {port}...");
        println!("User Data Directory: {}", config.user_data_dir.display());
        let listener = match TcpListener::bind(("127.0.0.1", port)).await {
            Ok(listener) => listener,
            Err(error) => {
                // Never take a port from another process; report and exit.
                eprintln!("plumdeck-backend: cannot bind 127.0.0.1:{port}: {error}");
                return ExitCode::from(1);
            }
        };
        let (stop, mut stopped) = tokio::sync::watch::channel(false);
        let mut server = tokio::spawn(serve(listener, async move {
            let _ = stopped.wait_for(|stop| *stop).await;
        }));
        println!("{}", StartupStage::Ready.line());
        tracing::info!(port, "listening on 127.0.0.1");

        let managed = config.managed_sidecar;
        let requested = async move {
            if managed {
                tokio::select! {
                    _ = shutdown_signal() => {}
                    _ = stdin_shutdown_line() => {}
                }
            } else {
                shutdown_signal().await;
            }
        };
        tokio::select! {
            result = &mut server => {
                eprintln!("plumdeck-backend: server stopped: {result:?}");
                return ExitCode::from(1);
            }
            _ = requested => {}
        }
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
    })
}
