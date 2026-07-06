// Tabibu — Tauri desktop shell. The Rust core is called directly through the
// commands in `commands.rs` (no FFI bridge). See ADR-0003.
//
// Tabibu runs as a macOS MENU BAR app (agent): the tray icon is the primary
// surface, the dashboard window is summoned from it, and closing the dashboard
// hides it instead of quitting. Only the tray's Quit (or an explicit exit)
// ends the process.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod system;
mod tray;

use tauri::Manager;

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .setup(|app| {
            // No Dock icon / app switcher entry. LSUIElement in Info.plist
            // covers the bundle from launch; this covers `tauri dev`.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            tray::setup(app)?;
            Ok(())
        })
        .on_window_event(|window, event| match (window.label(), event) {
            // Closing the dashboard hides it; the app lives in the menu bar.
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
            commands::pick_folder,
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
        .run(|_app, event| {
            // Keep running with zero visible windows (menu bar app). `code` is
            // Some(_) only for explicit exit()/restart — let those through.
            if let tauri::RunEvent::ExitRequested { api, code, .. } = event {
                if code.is_none() {
                    api.prevent_exit();
                }
            }
        });
}
