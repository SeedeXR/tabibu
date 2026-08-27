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

mod alerts;
mod commands;
mod system;
mod tray;
mod vpn;

use tauri::Manager;

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            // Marker arg so a login (LaunchAgent) start can be told apart from a
            // manual open: at login we start quietly in the menu bar; a manual
            // open shows the dashboard too. See setup().
            Some(vec!["--from-autostart"]),
        ))
        .setup(|app| {
            // Load persisted alert prefs (Trash / memory snooze) before the tray
            // sampler starts reading them.
            if let Ok(dir) = app.path().app_config_dir() {
                alerts::load(&dir);
                vpn::load(&dir);
            }
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
            // Global shortcut (Ctrl+Cmd+N) to toggle the health popover — a
            // reliable way in when the tray icon is hidden behind the notch on a
            // crowded menu bar. Registered at runtime (NOT via the builder's
            // with_shortcut, whose registration error would propagate out of
            // plugin setup and abort launch): if another app already owns the
            // combo we just log and carry on, dashboard → Network still works.
            {
                use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
                if let Err(e) = app.global_shortcut().on_shortcut(
                    "Control+Command+N",
                    |app, _shortcut, event| {
                        if event.state == ShortcutState::Pressed {
                            tray::show_popover_default(app);
                        }
                    },
                ) {
                    eprintln!("global shortcut Ctrl+Cmd+N unavailable (already in use?): {e}");
                }
            }
            // The tray is created on RunEvent::Ready (see run() below), NOT here:
            // an NSStatusItem added during setup() — before NSApplication has
            // finished launching — silently fails to attach to the menu bar.
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
            commands::trash_size,
            commands::empty_trash,
            commands::flush_dns,
            commands::free_memory,
            commands::relaunch,
            commands::get_alert_prefs,
            commands::set_alert_enabled,
            commands::snooze_alert,
            commands::send_test_notification,
            commands::vpn_config,
            commands::vpn_upsert_server,
            commands::vpn_remove_server,
            commands::vpn_set_active,
            commands::vpn_state,
            commands::vpn_provision,
            commands::vpn_connect,
            commands::vpn_disconnect,
            commands::telemetry_enabled,
            commands::set_telemetry_enabled,
            commands::record_deselection,
            commands::quit_process,
            commands::thermal_info,
            commands::smart_status,
            commands::scan_orphans,
            commands::scan_malware,
            commands::scan_universal,
            commands::strip_universal,
            commands::scan_dev_artifacts,
            commands::quarantine,
            commands::record_free_space,
            commands::brew_analyze,
            commands::brew_cleanup,
            commands::brew_autoremove,
            commands::brew_uninstall,
            commands::docker_analyze,
            commands::docker_prune_build_cache,
            commands::docker_prune_images,
            commands::docker_prune_containers,
            commands::docker_prune_volumes,
            commands::network_sample,
            commands::connection_test,
            commands::salama_status,
            commands::salama_engine_status,
            commands::salama_engine_on,
            commands::salama_engine_off,
            commands::launch_at_login,
            commands::set_launch_at_login,
            commands::show_main_window,
            commands::quit_app,
            commands::popover_detail,
            commands::popover_resize,
            commands::uptime_secs,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Tabibu")
        .run(|app, event| match event {
            // Create the menu-bar tray only once NSApplication has finished
            // launching. Building the NSStatusItem earlier (in setup()) returns
            // Ok but the item never appears in the menu bar.
            tauri::RunEvent::Ready => {
                if let Err(e) = tray::setup(app) {
                    eprintln!("tray setup failed: {e}");
                }
            }
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
            tauri::RunEvent::Reopen {
                has_visible_windows: false,
                ..
            } => {
                tray::show_main(app, None);
            }
            _ => {}
        });
}
