// Tabibu — Tauri desktop shell. The Rust core is called directly through the
// commands in `commands.rs` (no FFI bridge). See ADR-0003.
//
// Tabibu is BOTH a normal desktop app and a menu-bar app: a manual launch
// shows the dashboard (Regular — Dock icon + window) alongside a persistent
// menu-bar tray. Closing the dashboard only HIDES it; the app stays Regular so
// the Dock icon remains a live reopen affordance (Dock click → RunEvent::Reopen)
// and the tray's Open Dashboard reopens it too. The process keeps running
// either way. A login (autostart) launch starts quietly in the menu bar
// (Accessory, no window) until first opened. Only Quit (tray / Cmd-Q) exits.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod system;
mod tray;

use tauri::Manager;

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            // Marker arg so a login (LaunchAgent) start can be told apart from a
            // manual open: at login we start quietly in the menu bar; a manual
            // open shows the dashboard too. See setup().
            Some(vec!["--from-autostart"]),
        ))
        .setup(|app| {
            // A login (LaunchAgent) start passes `--from-autostart`; start
            // quietly in the menu bar then (Accessory, dashboard hidden) so we
            // don't pop a 1120x740 window on every login. A manual open has no
            // such arg: show the dashboard (Regular, Dock icon) alongside the
            // tray. After launch the app stays Regular; CloseRequested only
            // hides the window (Dock icon persists), and RunEvent::Reopen /
            // tray::show_main bring it back.
            #[cfg(target_os = "macos")]
            {
                let from_autostart = std::env::args().any(|a| a == "--from-autostart");
                if from_autostart {
                    app.set_activation_policy(tauri::ActivationPolicy::Accessory);
                    if let Some(w) = app.get_webview_window("main") {
                        let _ = w.hide();
                    }
                } else {
                    app.set_activation_policy(tauri::ActivationPolicy::Regular);
                }
            }
            tray::setup(app)?;
            Ok(())
        })
        .on_window_event(|window, event| match (window.label(), event) {
            // Closing the dashboard only HIDES it — the process keeps running
            // for the tray. The app stays Regular so its Dock icon remains a
            // working reopen affordance (clicking it fires RunEvent::Reopen,
            // handled below); the tray's Open Dashboard reopens it too. Only
            // Quit (tray / Cmd-Q) actually exits.
            ("main", tauri::WindowEvent::CloseRequested { api, .. }) => {
                api.prevent_close();
                let _ = window.hide();
            }
            // The tray popover dismisses itself when it loses focus. The
            // timestamp lets the tray Click handler tell "dismiss click"
            // from "open click" (the blur fires before the click arrives).
            // Collapsing here (not in JS) guarantees the next open is always
            // overview-sized, whatever state the webview was left in.
            ("menubar", tauri::WindowEvent::Focused(false)) => {
                let _ = window.hide();
                tray::note_popover_hidden();
                tray::set_popover_detail(window.app_handle(), false);
            }
            _ => {}
        })
        .invoke_handler(tauri::generate_handler![
            commands::scan,
            commands::cancel_scan,
            commands::cancel_sync,
            commands::reclaim,
            commands::size_tree,
            commands::find_duplicates,
            commands::find_remnants,
            commands::installed_apps,
            commands::monitor_sample,
            commands::menubar_sample,
            commands::disk_space,
            commands::system_info,
            commands::battery_info,
            commands::startup_items,
            commands::reveal_in_finder,
            commands::open_url,
            commands::trash_path,
            commands::telemetry_enabled,
            commands::set_telemetry_enabled,
            commands::record_deselection,
            commands::quit_process,
            commands::thermal_info,
            commands::smart_status,
            commands::scan_orphans,
            commands::scan_malware,
            commands::quarantine,
            commands::record_free_space,
            commands::brew_analyze,
            commands::brew_cleanup,
            commands::brew_autoremove,
            commands::brew_uninstall,
            commands::launch_at_login,
            commands::set_launch_at_login,
            commands::show_main_window,
            commands::quit_app,
            commands::popover_detail,
            commands::uptime_secs,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Tabibu")
        .run(|_app, event| match event {
            // Keep running with zero visible windows (menu bar app). `code:None`
            // is a user-initiated close/last-window; Some(_) is an explicit
            // exit()/restart, which we let through.
            tauri::RunEvent::ExitRequested { api, code: None, .. } => {
                api.prevent_exit();
            }
            // Clicking the Dock icon (applicationShouldHandleReopen) with no
            // visible window: bring the dashboard back. Without this the Dock
            // icon is a dead affordance — the window never reappears.
            #[cfg(target_os = "macos")]
            tauri::RunEvent::Reopen { has_visible_windows: false, .. } => {
                tray::show_main(_app, None);
            }
            _ => {}
        });
}
