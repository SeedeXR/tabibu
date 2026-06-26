// Tabibu — Tauri desktop shell. The Rust core is called directly through the
// commands in `commands.rs` (no FFI bridge). See ADR-0003.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod system;
mod tray;

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            tray::setup(app)?;
            Ok(())
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running Tabibu");
}
