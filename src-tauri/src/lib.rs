//! The Tauri v2 desktop shell.
//!
//! The shell owns windows, the tray, notifications and the engine process; it owns no session
//! logic. Every client frame crosses [`bridge`] into `orion_core::engine::Engine`, the same seam
//! the browser gateway uses, so the desktop and the PWA cannot drift apart.

mod bridge;

use std::sync::atomic::{AtomicBool, Ordering};

use orion_core::engine::Engine;
use orion_core::process;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, WindowEvent};
use tracing::{error, info, warn};

/// State every window shares. `quitting` distinguishes the tray's Quit from a window close,
/// which only hides to the tray.
#[derive(Debug, Default)]
pub struct AppState {
    pub quitting: AtomicBool,
}

/// Shows and focuses the main window, wherever it was hidden.
fn show_main_window(app: &AppHandle) {
    match app.get_webview_window("main") {
        Some(window) => {
            let _ = window.show();
            let _ = window.unminimize();
            let _ = window.set_focus();
        }
        None => warn!("the main window is gone; nothing to show"),
    }
}

/// The tray: Show, and Quit (the only path that really exits). A left click shows the window.
fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Show Orion", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Orion", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;

    let mut builder = TrayIconBuilder::with_id("orion")
        .tooltip("Orion")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_main_window(app),
            "quit" => {
                app.state::<AppState>()
                    .quitting
                    .store(true, Ordering::Relaxed);
                app.exit(0);
            }
            other => warn!("unknown tray action: {other}"),
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        });

    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon);
    }

    builder.build(app)?;
    Ok(())
}

/// Starts the desktop shell. Installer, window and tray exist here; sessions do not.
pub fn run() {
    tracing_subscriber::fmt::init();

    if let Err(error) = process::contain_current_process() {
        // Nesting inside another job is refused on some managed machines: the app still starts,
        // and M1 assigns each agent to its own job instead.
        error!("could not place Orion in a job object: {error}");
    }
    info!("Starting Orion desktop application");

    tauri::Builder::default()
        // Must be first: a second instance hands its argv to this one and exits.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            show_main_window(app)
        }))
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(Engine::new())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            bridge::orion_request,
            bridge::orion_connect,
            bridge::orion_send,
            bridge::orion_close,
        ])
        .setup(|app| {
            // The window exists before the saved session is rebuilt, so the first `orion_connect`
            // already sees the workspaces and panes the last run left.
            let engine = app.state::<Engine>().inner().clone();
            engine.restore();
            build_tray(app.handle())?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let app = window.app_handle();
                if app.state::<AppState>().quitting.load(Ordering::Relaxed) {
                    return;
                }
                // Closing the window hides it: the agents keep running in the tray.
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running Orion");
}
