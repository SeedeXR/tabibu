//! Menu-bar tray — the Swift-free replacement for the old `TabibuMonitor`
//! agent (inspiration: the CleanMyMac menu widget). A status item with a live
//! tooltip (CPU% + memory%) and a small menu (Open Tabibu / Quit). Sampling
//! runs on a 5s cadence to stay light (within the monitor resource budget).

use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::TrayIconBuilder;
use tauri::{App, Manager};

const TRAY_ID: &str = "tabibu-tray";

pub fn setup(app: &App) -> tauri::Result<()> {
    let open = MenuItemBuilder::with_id("open", "Open Tabibu").build(app)?;
    let quit = MenuItemBuilder::with_id("quit", "Quit Tabibu").build(app)?;
    let menu = MenuBuilder::new(app).items(&[&open, &quit]).build()?;

    let _tray = TrayIconBuilder::with_id(TRAY_ID)
        .icon(app.default_window_icon().expect("bundled icon").clone())
        .tooltip("Tabibu")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => {
                if let Some(win) = app.get_webview_window("main") {
                    let _ = win.show();
                    let _ = win.unminimize();
                    let _ = win.set_focus();
                }
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .build(app)?;

    // Live tooltip: refresh CPU%/memory% every 5s from a background thread.
    // Uses its OWN sampler rather than the UI's process-wide one: sysinfo
    // derives CPU% from the elapsed time since the prior refresh on that
    // `System`, so a shared sampler refreshed on two cadences (UI ~2s + tray 5s)
    // computes deltas over the wrong interval and reports garbage CPU% to both.
    // A dedicated, lightly-refreshed sampler keeps each consumer's deltas sane.
    let handle = app.handle().clone();
    let mut sampler = tabibu_monitor::Sampler::new();
    std::thread::spawn(move || loop {
        let snap = sampler.sample(1, tabibu_monitor::TopBy::Cpu);
        let mem_pct = if snap.total_memory_bytes > 0 {
            (snap.used_memory_bytes as f64 / snap.total_memory_bytes as f64 * 100.0).round() as u32
        } else {
            0
        };
        let tip = format!(
            "Tabibu — CPU {}% · Memory {}%",
            snap.cpu_percent.round() as i64,
            mem_pct
        );
        if let Some(tray) = handle.tray_by_id(TRAY_ID) {
            let _ = tray.set_tooltip(Some(&tip));
        }
        std::thread::sleep(std::time::Duration::from_secs(5));
    });

    Ok(())
}
