// Tabibu — Tauri desktop shell. The Rust core is called directly through the
// commands in `commands.rs` (no FFI bridge). See ADR-0003.
//
// Tabibu is BOTH a normal desktop app and a menu-bar app: a manual launch
// shows the dashboard (Regular — Dock icon + window) alongside a persistent
// menu-bar tray. Closing the dashboard hides the window AND drops to Accessory,
// so the Dock icon goes away and only the tray remains; the process keeps
// running and you reopen from the tray (→ Regular + Dock icon again). A login
// (autostart) launch starts quietly in the menu bar (Accessory, no window)
// until first opened. Only Quit (tray / Cmd-Q) exits.
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
            // tray. CloseRequested then hides the window and drops to Accessory
            // (Dock icon gone, tray only); tray::show_main promotes back to
            // Regular on reopen.
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
            // Closing the dashboard (red traffic-light) hides the window AND
            // drops the app to Accessory, so the Dock icon disappears and only
            // the menu-bar tray remains. The process keeps running; reopen from
            // the tray (Open Dashboard / the popover), which calls
            // tray::show_main → back to Regular + Dock icon. Only Quit
            // (tray / Cmd-Q) actually exits.
            ("main", tauri::WindowEvent::CloseRequested { api, .. }) => {
                api.prevent_close();
                let _ = window.hide();
                #[cfg(target_os = "macos")]
                let _ = window
                    .app_handle()
                    .set_activation_policy(tauri::ActivationPolicy::Accessory);
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
            commands::scan_universal,
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
            // NOTE: we do NOT prevent ExitRequested. Staying alive when the
            // dashboard is closed is already handled by prevent_close in the
            // CloseRequested handler (the window is hidden, never destroyed, so
            // no last-window exit fires). The only remaining ExitRequested
            // sources are genuine quits — the tray's Quit (app.exit), the app
            // menu / Cmd-Q, and logout/shutdown — all of which SHOULD exit.
            // Clicking the Dock icon (applicationShouldHandleReopen) with no
            // visible window brings the dashboard back; without this the Dock
            // icon would be a dead affordance.
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { has_visible_windows: false, .. } = event {
                tray::show_main(_app, None);
            }
        });
}
