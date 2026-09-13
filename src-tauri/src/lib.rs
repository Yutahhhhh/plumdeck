mod waveform;
mod startup;
use std::env;
use std::sync::Arc;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{Manager, Emitter};
use std::sync::Mutex;
#[derive(Default)]
struct JunctionInvite(Mutex<Option<String>>);
fn valid_junction_invite(value: &str) -> bool {
    value.len() <= 8192 && value.starts_with("plumdeck-junction://join?") && !value.chars().any(char::is_control)
}
fn receive_junction_invite(app: &tauri::AppHandle, value: &str) {
    if !valid_junction_invite(value) { return; }
    if let Some(state) = app.try_state::<JunctionInvite>() {
        if let Ok(mut pending) = state.0.lock() { *pending = Some(value.to_string()); }
    }
    let _ = app.emit("junction://invite", value);
    if let Some(window) = app.get_webview_window("main") { let _ = window.set_focus(); }
}
#[tauri::command]
fn junction_pending_invite(state: tauri::State<JunctionInvite>) -> Option<String> {
    state.0.lock().ok()?.take()
}
use tauri_plugin_shell::process::{CommandEvent, CommandChild};
#[derive(Default)]
struct BackendChild(Mutex<Option<ManagedBackend>>);
struct ManagedBackend {
    child: CommandChild,
    ended: std::sync::mpsc::Receiver<()>,
}
fn stop_backend(mut backend: ManagedBackend) {
    let _ = backend.child.write(b"plumdeck:shutdown\n");
    // The analysis worker gets 30 seconds to checkpoint; Uvicorn first drains
    // requests for up to 5 seconds. Do not kill it before that work can finish.
    if backend.ended.recv_timeout(std::time::Duration::from_secs(40)).is_ok() { return; }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        // PyInstaller's one-file bootloader owns a Python child. A forced
        // fallback must end this owned tree, never another listener on a port.
        if let Some(system) = std::env::var_os("SystemRoot") {
            if let Ok(mut command) = std::process::Command::new(std::path::PathBuf::from(system).join("System32/taskkill.exe"))
                .args(["/PID", &backend.child.pid().to_string(), "/T", "/F"])
                .creation_flags(0x08000000).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn() {
                let deadline=std::time::Instant::now()+std::time::Duration::from_secs(3);
                while matches!(command.try_wait(), Ok(None)) && std::time::Instant::now()<deadline {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                let _=command.kill(); let _=command.wait();
            }
        }
    }
    let _ = backend.child.kill();
}
use tauri_plugin_shell::ShellExt;
use tauri_plugin_deep_link::DeepLinkExt;

mod assist;
mod dj_engine;
mod junction_mcp_bridge;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default();
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.unminimize(); let _ = window.show(); let _ = window.set_focus();
        }
    }));
    builder
        .plugin(tauri_plugin_deep_link::init())
        .manage(BackendChild::default())
        .manage(startup::StartupState::default())
        .plugin(tauri_plugin_shell::init())
        // 開発者ツールを有効化 (リリースビルドでもF12/右クリックで開けるようにする)
        .plugin(tauri_plugin_devtools::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_http::init())
        .plugin(tauri_plugin_drag::init())
        .menu(|handle| {
            let menu = Menu::new(handle)?;

            #[cfg(target_os = "macos")]
            {
                let app_menu = Submenu::new(handle, "plumdeck", true)?;
                app_menu.append(&PredefinedMenuItem::hide(handle, None)?)?;
                app_menu.append(&PredefinedMenuItem::hide_others(handle, None)?)?;
                app_menu.append(&PredefinedMenuItem::quit(handle, None)?)?;
                menu.append(&app_menu)?;
            }

            let edit_menu = Submenu::new(handle, "Edit", true)?;
            edit_menu.append(&PredefinedMenuItem::undo(handle, None)?)?;
            edit_menu.append(&PredefinedMenuItem::redo(handle, None)?)?;
            edit_menu.append(&PredefinedMenuItem::separator(handle)?)?;
            edit_menu.append(&PredefinedMenuItem::cut(handle, None)?)?;
            edit_menu.append(&PredefinedMenuItem::copy(handle, None)?)?;
            edit_menu.append(&PredefinedMenuItem::paste(handle, None)?)?;
            edit_menu.append(&PredefinedMenuItem::select_all(handle, None)?)?;
            menu.append(&edit_menu)?;

            let view_menu = Submenu::new(handle, "View", true)?;
            view_menu.append(&PredefinedMenuItem::fullscreen(handle, None)?)?;
            view_menu.append(&MenuItem::with_id(
                handle,
                "toggle_devtools",
                "Toggle Developer Tools",
                true,
                None::<&str>,
            )?)?;
            menu.append(&view_menu)?;

            let help_menu = Submenu::new(handle, "Help", true)?;
            help_menu.append(&MenuItem::with_id(handle, "check_updates", "更新を確認...", true, None::<&str>)?)?;
            menu.append(&help_menu)?;

            Ok(menu)
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.state::<Arc<dj_engine::EngineSupervisor>>().junction_active().unwrap_or(true) {
                    api.prevent_close();
                    let _ = window.emit("junction://close-blocked", ());
                }
            }
        })
        .on_menu_event(|app, event| {
            if event.id() == "check_updates" {
                let _ = app.emit("app://check-updates", ());
            }
            if event.id() == "toggle_devtools" {
                if let Some(window) = app.get_webview_window("main") {
                    if window.is_devtools_open() {
                        window.close_devtools();
                    } else {
                        window.open_devtools();
                    }
                }
            }
        })
        .manage(Arc::new(dj_engine::EngineSupervisor::new()))
        // Assist mode only reads; both of these are passive state holders.
        .manage(JunctionInvite::default())
        .manage(assist::AssistState::default())
        .manage(assist::commands::WindowBounds::default())
        .invoke_handler(tauri::generate_handler![
            junction_pending_invite,
            startup::backend_startup_status,
            assist::commands::assist_snapshot,
            assist::commands::assist_request_accessibility,
            assist::commands::assist_open_accessibility_settings,
            assist::commands::assist_enter_compact_window,
            assist::commands::assist_exit_compact_window,
            assist::commands::assist_set_always_on_top,
            dj_engine::midi::dj_midi_status,
            dj_engine::midi::dj_midi_send,
            dj_engine::midi::dj_midi_read,
            dj_engine::midi::dj_midi_devices,
            dj_engine::midi::dj_midi_select_device,
            dj_engine::midi::dj_midi_performance_config,
            dj_engine::midi::dj_jog_display_update,
            dj_engine::commands::dj_engine_status,
            dj_engine::commands::dj_engine_start,
            dj_engine::commands::dj_engine_stop,
            dj_engine::commands::dj_engine_connect,
            dj_engine::commands::dj_engine_send,
            waveform::dj_waveform_tile,
            waveform::dj_waveform_pcm,
            waveform::dj_waveform_manifest,
            dj_engine::commands::junction_command,
            junction_mcp_bridge::junction_live_monitor_deck,
            junction_mcp_bridge::junction_live_monitor_set,
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            app.deep_link().on_open_url(move |event| {
                for url in event.urls() { receive_junction_invite(&handle, url.as_str()); }
            });
            for arg in env::args().skip(1) { if valid_junction_invite(&arg) { if let Ok(mut pending) = app.state::<JunctionInvite>().0.lock() { *pending = Some(arg); } } }
            app.manage(dj_engine::midi::controller(app.handle().clone()));
            app.manage(dj_engine::jog_display::JogDisplay::new());

            // The Python MCP sidecar never owns an audio engine. It reaches the
            // Tauri-owned engine only through this per-launch authenticated,
            // loopback-only Junction bridge.
            let supervisor = app
                .state::<Arc<dj_engine::EngineSupervisor>>()
                .inner()
                .clone();
            let junction_bridge = junction_mcp_bridge::JunctionMcpBridge::start(
                app.handle().clone(),
                supervisor,
            )
            .map_err(std::io::Error::other)?;
            let junction_bridge_url = junction_bridge.url().to_string();
            let junction_bridge_token = junction_bridge.token().to_string();
            app.manage(junction_bridge);
            // ネイティブ DJ エンジン（Phase 0 シミュレータ）はオプトイン起動。
            // 既定では起動せず、フロントは「未起動」を受け取って素直に劣化する。
            // Python サイドカーとは独立なので、CI 判定より前に置く。
            if env_flag("PLUMDECK_DJ_ENGINE_AUTOSTART") {
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    let supervisor = handle
                        .state::<Arc<dj_engine::EngineSupervisor>>()
                        .inner()
                        .clone();
                    match supervisor.start(&handle, None, None) {
                        Ok(status) => println!(
                            "[dj-engine] 自動起動しました running={} simulated={}",
                            status.running, status.simulated
                        ),
                        Err(error) => eprintln!("[dj-engine] 自動起動に失敗しました: {}", error),
                    }
                });
            }

            // CI環境やビルド時はサイドカーを起動しない
            if cfg!(debug_assertions) && (env_flag("CI") || env_flag("TAURI_SKIP_SIDECAR")) {
                println!("Skipping sidecar startup (CI/build environment)");
                return Ok(());
            }

            // サイドカーの起動
            // 本番環境（リリースビルド）では競合しにくいポートを使用する
            // 開発環境ではデフォルトの8001を使用
            #[cfg(debug_assertions)]
            let port = "8001";
            #[cfg(not(debug_assertions))]
            let port = "48123"; // 競合しにくいポート番号

            let sidecar = match app.shell().sidecar("plumdeck-server") {
                Ok(command) => command,
                Err(error) => {
                    startup::fail(app.handle(), format!("解析サービスの起動を準備できません: {}", error));
                    return Ok(());
                }
            };
            let sidecar_command = sidecar
                .env("PLUMDECK_PORT", port)
                .env("PLUMDECK_MANAGED_SIDECAR", "1")
                .env("PLUMDECK_JUNCTION_BRIDGE_URL", junction_bridge_url)
                .env("PLUMDECK_JUNCTION_BRIDGE_TOKEN", junction_bridge_token);

            // コマンドの実行結果を詳細にログ出力
            println!("Attempting to spawn sidecar with port: {}", port);

            let (mut _rx, _child) = match sidecar_command.spawn() {
                Ok(result) => result,
                Err(error) => {
                    startup::fail(app.handle(), format!("解析サービスを起動できません: {}", error));
                    return Ok(());
                }
            };

            let (ended, completion) = std::sync::mpsc::channel();
            *app.state::<BackendChild>().0.lock().unwrap() = Some(ManagedBackend {child: _child, ended: completion});

            // 非同期でログを出力するスレッドを作成（デバッグ用）
            let startup_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                while let Some(event) = _rx.recv().await {
                    if let CommandEvent::Stdout(line) = event {
                        if let Some(progress) = startup::parse(&line) {
                            startup::publish(&startup_handle, progress);
                        }
                        println!("[PY]: {}", String::from_utf8_lossy(&line));
                    } else if let CommandEvent::Stderr(line) = event {
                        eprintln!("[PY ERR]: {}", String::from_utf8_lossy(&line));
                    } else if let CommandEvent::Terminated(status) = event {
                        startup::fail(&startup_handle, format!("解析サービスが終了しました（終了コード: {:?}）", status.code));
                        let _ = ended.send(());
                    } else if let CommandEvent::Error(error) = event {
                        startup::fail(&startup_handle, format!("解析サービスとの通信に失敗しました: {}", error));
                    }
                }
            });

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                let _ = app.state::<Arc<dj_engine::EngineSupervisor>>().stop();
                if let Ok(mut child) = app.state::<BackendChild>().0.lock() {
                    if let Some(child) = child.take() { stop_backend(child); }
                }
            }
            if let tauri::RunEvent::ExitRequested { ref api, .. } = event {
                if app.state::<Arc<dj_engine::EngineSupervisor>>().junction_active().unwrap_or(true) {
                    api.prevent_exit(); let _ = app.emit("junction://close-blocked", ());
                }
            }

        });
}

fn env_flag(name: &str) -> bool {
    env::var(name)
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}
