//! Menu-bar tray — Tabibu's primary surface (macOS agent app). A status item
//! with a live tooltip (CPU% + memory%), a right-click menu (Open Dashboard /
//! Settings / Pause Monitoring / Quit), and a left-click health popover (the
//! `menubar` window). Sampling runs on a 5s cadence to stay light (within the
//! monitor resource budget).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{App, AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, PhysicalPosition};
use tauri_plugin_notification::NotificationExt;

const TRAY_ID: &str = "tabibu-tray";
/// Popover width in logical px — must match the `menubar` window in
/// tauri.conf.json.
const POPOVER_W: f64 = 360.0;
/// Width with a component detail panel beside the overview (menubar.html's
/// expanded layout).
const POPOVER_DETAIL_W: f64 = 740.0;

/// The background monitoring "service" (tray tooltip sampler), pausable from
/// the tray menu. Relaxed ordering is fine: one writer (menu handler), one
/// reader (sampler thread), no data guarded by it.
static PAUSED: AtomicBool = AtomicBool::new(false);

/// Show + focus the dashboard, optionally navigating it to a view. Shared by
/// the tray menu and the popover's buttons (via `commands::show_main_window`).
pub fn show_main(app: &AppHandle, view: Option<&str>) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
        if let Some(v) = view {
            let _ = app.emit_to("main", "navigate", v);
        }
    }
}

/// When the popover last auto-hid on losing focus (see `main.rs`). Clicking
/// the tray icon while the popover is open blurs it FIRST (auto-hide) and
/// only then delivers the Click — without remembering the hide instant, the
/// click meant to dismiss would instantly reopen it.
static POPOVER_HIDDEN_AT: Mutex<Option<Instant>> = Mutex::new(None);

/// Expand/collapse the popover for the component detail panel. The window
/// grows LEFTWARD (like the references), keeping the overview anchored under
/// the tray icon. Reads the current width instead of tracking state, so a
/// double invoke is a no-op.
pub fn set_popover_detail(app: &AppHandle, open: bool) {
    let Some(pop) = app.get_webview_window("menubar") else { return };
    let scale = pop.scale_factor().unwrap_or(2.0);
    let Ok(size) = pop.inner_size() else { return };
    let cur_w = f64::from(size.width) / scale;
    let expanded = cur_w > (POPOVER_W + POPOVER_DETAIL_W) / 2.0;
    if expanded == open {
        return;
    }
    let delta = POPOVER_DETAIL_W - POPOVER_W;
    let Ok(pos) = pop.outer_position() else { return };
    let mut x = f64::from(pos.x) / scale + if open { -delta } else { delta };
    let y = f64::from(pos.y) / scale;
    let h = f64::from(size.height) / scale;
    let w = if open { POPOVER_DETAIL_W } else { POPOVER_W };
    // Clamp within the display the popover is on — not the primary. A display
    // arranged left of primary has negative global X, so a bare `.max(8.0)`
    // would teleport the popover to the primary's edge.
    if let Ok(Some(mon)) = pop.current_monitor() {
        let ms = mon.scale_factor();
        let left = f64::from(mon.position().x) / ms;
        let right = left + mon.size().width as f64 / ms;
        x = x.min(right - w - 8.0).max(left + 8.0);
    }
    let _ = pop.set_size(LogicalSize::new(w, h));
    let _ = pop.set_position(LogicalPosition::new(x, y));
}

/// Called from the `Focused(false)` auto-hide in `main.rs`.
pub fn note_popover_hidden() {
    *POPOVER_HIDDEN_AT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Instant::now());
}

/// Toggle the health popover under the clicked tray icon. The click position
/// is PHYSICAL (tray-icon multiplies the logical cursor by the clicked
/// display's scale), while positioning happens in LOGICAL points — macOS's
/// native global space — so the window's own (possibly different) scale
/// factor never distorts the result on mixed-DPI setups.
fn toggle_popover(app: &AppHandle, click: PhysicalPosition<f64>) {
    let Some(pop) = app.get_webview_window("menubar") else { return };
    if pop.is_visible().unwrap_or(false) {
        let _ = pop.hide();
        return;
    }
    if POPOVER_HIDDEN_AT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .is_some_and(|t| t.elapsed() < Duration::from_millis(300))
    {
        // This click is the tail of a dismiss (blur already hid us): done.
        return;
    }
    // Find the clicked display by matching PHYSICAL bounds. (monitor_from_point
    // expects logical coords — which need the very scale we don't know yet.)
    let mon = app
        .available_monitors()
        .ok()
        .into_iter()
        .flatten()
        .find(|m| {
            let (p, s) = (m.position(), m.size());
            click.x >= f64::from(p.x)
                && click.x < f64::from(p.x) + s.width as f64
                && click.y >= f64::from(p.y)
                && click.y < f64::from(p.y) + s.height as f64
        });
    let scale = mon.as_ref().map_or(2.0, tauri::Monitor::scale_factor);
    // Back to the global logical space the click originated in.
    let (cx, cy) = (click.x / scale, click.y / scale);
    let mut x = cx - POPOVER_W / 2.0;
    let y = cy + 14.0; // just under the menu bar
    if let Some(m) = &mon {
        let left = f64::from(m.position().x) / scale;
        let right = left + m.size().width as f64 / scale;
        x = x.min(right - POPOVER_W - 8.0).max(left + 8.0);
    }
    let _ = pop.set_position(LogicalPosition::new(x, y));
    let _ = pop.show();
    let _ = pop.set_focus();
}

pub fn setup(app: &App) -> tauri::Result<()> {
    let open = MenuItemBuilder::with_id("open", "Open Dashboard").build(app)?;
    let settings = MenuItemBuilder::with_id("settings", "Settings…").build(app)?;
    let pause = MenuItemBuilder::with_id("pause", "Pause Monitoring").build(app)?;
    let quit = MenuItemBuilder::with_id("quit", "Quit Tabibu").build(app)?;
    let menu = MenuBuilder::new(app)
        .items(&[&open, &settings])
        .separator()
        .items(&[&pause])
        .separator()
        .items(&[&quit])
        .build()?;

    let _tray = TrayIconBuilder::with_id(TRAY_ID)
        .icon(app.default_window_icon().expect("bundled icon").clone())
        .tooltip("Tabibu")
        .menu(&menu)
        // Left click toggles the popover; the menu stays on right click.
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| match event.id().as_ref() {
            "open" => show_main(app, None),
            "settings" => show_main(app, Some("settings")),
            "pause" => {
                let paused = !PAUSED.load(Ordering::Relaxed);
                PAUSED.store(paused, Ordering::Relaxed);
                let _ = pause.set_text(if paused { "Resume Monitoring" } else { "Pause Monitoring" });
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                position,
                ..
            } = event
            {
                toggle_popover(tray.app_handle(), position);
            }
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
    std::thread::spawn(move || {
        let mut was_paused = false;
        // Alerts fire once on ENTERING a bad state and re-arm only after real
        // recovery (hysteresis, so a value hovering at the threshold can't
        // spam). Pausing monitoring pauses the alerts too.
        let mut mem_alert_armed = true;
        let mut therm_alert_armed = true;
        let mut tick: u32 = 0;
        loop {
            if PAUSED.load(Ordering::Relaxed) {
                if !was_paused {
                    was_paused = true;
                    if let Some(tray) = handle.tray_by_id(TRAY_ID) {
                        let _ = tray.set_tooltip(Some("Tabibu — monitoring paused"));
                    }
                }
            } else {
                was_paused = false;
                let snap = sampler.sample(1, tabibu_monitor::TopBy::Cpu);
                let mem_pct = if snap.total_memory_bytes > 0 {
                    (snap.used_memory_bytes as f64 / snap.total_memory_bytes as f64 * 100.0)
                        .round() as u32
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

                // RAM nearly depleted → notify once; re-arm below 85%.
                if mem_pct >= 90 && mem_alert_armed {
                    mem_alert_armed = false;
                    notify(
                        &handle,
                        "Memory pressure is high",
                        &format!(
                            "RAM is {mem_pct}% full. Click the Tabibu menu bar \
                             icon and open Memory to see the top consumers and \
                             quit one."
                        ),
                    );
                } else if mem_pct < 85 {
                    mem_alert_armed = true;
                }

                // Thermal costs a pmset spawn — every 6th tick (30s) is plenty.
                if tick % 6 == 0 {
                    let t = crate::commands::thermal_info();
                    // Only Serious/Critical (speed limited below ~75%) alerts,
                    // matching the popover's "bad" state; re-arm at Nominal
                    // (100%). A brief dip into the "Fair" band (75–99%) — common
                    // while charging under load — no longer fires, so a value
                    // oscillating around the line can't spam notifications.
                    let throttling = matches!(t.pressure.as_str(), "Serious" | "Critical");
                    if throttling && therm_alert_armed {
                        therm_alert_armed = false;
                        let limit = t
                            .speed_limit
                            .map(|s| format!(" CPU speed is limited to {s}%."))
                            .unwrap_or_default();
                        notify(
                            &handle,
                            "Your Mac is heating up",
                            &format!(
                                "macOS reports thermal pressure: {}.{limit} Open \
                                 Tabibu from the menu bar and check CPU for the \
                                 top consumers.",
                                t.pressure
                            ),
                        );
                    } else if t.pressure == "Nominal" {
                        therm_alert_armed = true;
                    }
                }
                tick = tick.wrapping_add(1);
            }
            std::thread::sleep(std::time::Duration::from_secs(5));
        }
    });

    Ok(())
}

fn notify(app: &AppHandle, title: &str, body: &str) {
    let _ = app.notification().builder().title(title).body(body).show();
}
