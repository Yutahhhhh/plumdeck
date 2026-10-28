// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // The Rust backend runs analysis in a child of this same executable.
    #[cfg(feature = "rust-backend")]
    if std::env::args().any(|arg| arg == "--analysis-worker") {
        std::process::exit(plumdeck_backend::embed::run_analysis_worker());
    }
    plumdeck_lib::run()
}
