//! Menu-bar tray — a co-equal surface alongside the dashboard window (the app
//! runs as both a normal desktop app and a menu-bar app). A status item with a
//! live tooltip (CPU% + memory%), a right-click menu (Open Dashboard /
//! Settings / Pause Monitoring / Quit), and a left-click health popover (the
//! `menubar` window). Sampling runs on a 5s cadence to stay light (within the
//! monitor resource budget).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, PhysicalPosition};
use tauri_plugin_notification::NotificationExt;

const TRAY_ID: &str = "tabibu-tray";
/// The menu-bar status-item icon: a monochrome gourd silhouette (tilted 45°),
/// used as a macOS template image. Kept as a const so the regression test
/// exercises the exact bytes production ships.
const TRAY_ICON_BYTES: &[u8] = include_bytes!("../icons/tray-template.png");
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
        // Become a Regular, activatable app (Dock icon + a window that reliably
        // takes focus and stays frontmost). This is the promotion path back
        // from Accessory — after a close (which hides the window and drops to
        // Accessory) or a quiet autostart launch. As a pure Accessory app the
        // window shows but never truly activates: it drops behind the
        // still-active app the moment focus shifts, which reads as the window
        // "closing".
        #[cfg(target_os = "macos")]
        let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
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
    let Some(pop) = app.get_webview_window("menubar") else {
        return;
    };
    let scale = pop.scale_factor().unwrap_or(2.0);
    let Ok(size) = pop.inner_size() else { return };
    let cur_w = f64::from(size.width) / scale;
    let expanded = cur_w > (POPOVER_W + POPOVER_DETAIL_W) / 2.0;
    if expanded == open {
        return;
    }
    let delta = POPOVER_DETAIL_W - POPOVER_W;
    let Ok(pos) = pop.outer_position() else {
        return;
    };
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
        x = clamp_x(x, w, left, right);
    }
    let _ = pop.set_size(LogicalSize::new(w, h));
    let _ = pop.set_position(LogicalPosition::new(x, y));
}

/// Keep a `w`-wide window's left edge `x` inside `[left, right]` (logical px)
/// with an 8px margin — so the popover never spills off the clicked display.
fn clamp_x(x: f64, w: f64, left: f64, right: f64) -> f64 {
    x.min(right - w - 8.0).max(left + 8.0)
}

/// Set the popover's HEIGHT to fit its content (measured in the webview and
/// sent from JS), keeping the current width. This is the anti-clipping
/// mechanism: the real WKWebView's font metrics differ from any headless
/// measurement, so the window sizes itself to whatever the content actually
/// renders as. Clamped so a bogus value can't make an off-screen window.
pub fn set_popover_height(app: &AppHandle, height: f64) {
    let Some(pop) = app.get_webview_window("menubar") else {
        return;
    };
    let scale = pop.scale_factor().unwrap_or(2.0);
    let Ok(size) = pop.inner_size() else { return };
    let w = f64::from(size.width) / scale; // keep current width (overview or detail)
                                           // Never taller than the display it sits on (leave room for the menu bar).
    let mut max_h = 1200.0;
    if let Ok(Some(mon)) = pop.current_monitor() {
        max_h = (f64::from(mon.size().height) / mon.scale_factor() - 48.0).max(200.0);
    }
    let h = clamp_height(height, max_h);
    if (h - f64::from(size.height) / scale).abs() > 0.5 {
        let _ = pop.set_size(LogicalSize::new(w, h));
    }
}

/// Clamp a requested popover height to a sane range (never absurdly small, never
/// past the display). Pure, so it's unit-tested.
fn clamp_height(height: f64, max_h: f64) -> f64 {
    height.clamp(200.0, max_h)
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
    let Some(pop) = app.get_webview_window("menubar") else {
        return;
    };
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
        x = clamp_x(x, POPOVER_W, left, right);
    }
    let _ = pop.set_position(LogicalPosition::new(x, y));
    let _ = pop.show();
    let _ = pop.set_focus();
}

/// Toggle the popover at the top-right of the primary display. Used by the
/// global shortcut, which has no tray-click position to anchor to — and whose
/// whole reason for existing is that the tray icon can be hidden behind the
/// notch on a crowded menu bar. Always opens collapsed to the overview width.
pub fn show_popover_default(app: &AppHandle) {
    let Some(pop) = app.get_webview_window("menubar") else {
        return;
    };
    if pop.is_visible().unwrap_or(false) {
        let _ = pop.hide();
        return;
    }
    let scale = pop.scale_factor().unwrap_or(2.0);
    let h = pop
        .inner_size()
        .map_or(536.0, |s| f64::from(s.height) / scale); // matches menubar window height in tauri.conf.json
    let (x, y) = match pop.primary_monitor() {
        Ok(Some(mon)) => {
            let ms = mon.scale_factor();
            let left = f64::from(mon.position().x) / ms;
            let top = f64::from(mon.position().y) / ms;
            let right = left + f64::from(mon.size().width) / ms;
            (right - POPOVER_W - 8.0, top + 32.0) // just under the menu bar
        }
        _ => (8.0, 32.0),
    };
    let _ = pop.set_size(LogicalSize::new(POPOVER_W, h));
    let _ = pop.set_position(LogicalPosition::new(x, y));
    let _ = pop.show();
    let _ = pop.set_focus();
}

pub fn setup(app: &AppHandle) -> tauri::Result<()> {
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

    // A monochrome gourd SILHOUETTE marked as a template image — macOS then
    // tints it to match the menu bar (white on dark, black on light), the
    // standard for status items. The full-colour app icon (used before) is a
    // dark navy squircle that vanishes into a dark menu bar; a template icon
    // stays visible in both appearances.
    let icon =
        tauri::image::Image::from_bytes(TRAY_ICON_BYTES).expect("bundled tray template icon");
    let _tray = TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        .icon_as_template(true)
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
                let _ = pause.set_text(if paused {
                    "Resume Monitoring"
                } else {
                    "Pause Monitoring"
                });
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
    let handle = app.clone();
    let mut sampler = tabibu_monitor::Sampler::new();
    std::thread::spawn(move || {
        let mut was_paused = false;
        // Alerts fire once on ENTERING a bad state and re-arm only after real
        // recovery (hysteresis, so a value hovering at the threshold can't
        // spam). Pausing monitoring pauses the alerts too.
        let mut mem_alert_armed = true;
        let mut therm_alert_armed = true;
        let mut trash_alert_armed = true;
        let mut process_ram_alert_armed = true;
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
                // Order by memory: the tooltip only needs cpu_percent/mem_pct
                // (system-wide, order-independent), and this hands the per-app-RAM
                // check below the top-memory process WITHOUT a second sweep.
                let snap = sampler.sample(1, tabibu_monitor::TopBy::Memory);
                let mem_pct = if snap.total_memory_bytes > 0 {
                    (snap.used_memory_bytes as f64 / snap.total_memory_bytes as f64 * 100.0).round()
                        as u32
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

                // RAM nearly depleted → notify once (unless the user disabled or
                // snoozed the alert); re-arm below 85%.
                if mem_pct >= 90 && mem_alert_armed && crate::alerts::memory_active() {
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

                // Trash grown past 2 GB → notify once (walking the Trash is the
                // heavy bit, so only every 60th tick ≈ 5 min); re-arm well below
                // the line. Gated by the user's snooze/disable choice.
                if tick % 60 == 0 {
                    let size = tabibu_junk::trash_total_size(
                        &crate::commands::trash_dirs(),
                        &tabibu_engine::CancelToken::new(),
                    );
                    if size >= crate::alerts::TRASH_ALERT_BYTES
                        && trash_alert_armed
                        && crate::alerts::trash_active()
                    {
                        trash_alert_armed = false;
                        notify(
                            &handle,
                            "Your Trash is getting large",
                            &format!(
                                "The Trash holds {:.1} GB. Open Tabibu → Junk to \
                                 empty it and reclaim the space.",
                                size as f64 / 1_000_000_000.0
                            ),
                        );
                    } else if size < crate::alerts::TRASH_ALERT_BYTES * 8 / 10 {
                        trash_alert_armed = true;
                    }
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

                // A single app hogging RAM (≥ 2 GB) → notify once; re-arm when it
                // drops below 80% of the line. Checked every 60th tick (≈5 min),
                // reading the top-memory process from THIS tick's sample.
                if tick % 60 == 0 {
                    if let Some(top) = snap.top_processes.first() {
                        if top.memory_bytes >= crate::alerts::PROCESS_RAM_ALERT_BYTES
                            && process_ram_alert_armed
                            && crate::alerts::process_ram_active()
                        {
                            process_ram_alert_armed = false;
                            notify(
                                &handle,
                                "An app is using a lot of memory",
                                &format!(
                                    "{} is using {:.1} GB of RAM. Open Tabibu \
                                     from the menu bar → Memory to review or quit it.",
                                    top.name,
                                    top.memory_bytes as f64 / 1_000_000_000.0
                                ),
                            );
                        } else if top.memory_bytes < crate::alerts::PROCESS_RAM_ALERT_BYTES * 8 / 10
                        {
                            process_ram_alert_armed = true;
                        }
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

#[cfg(test)]
mod tests {
    use super::{clamp_height, clamp_x, TRAY_ICON_BYTES};

    // Popover auto-fit height: a content measurement is clamped so a bogus or
    // huge value can't produce an off-screen / degenerate window.
    #[test]
    fn clamp_height_stays_sane() {
        assert_eq!(
            clamp_height(528.0, 900.0),
            528.0,
            "normal fit passes through"
        );
        assert_eq!(
            clamp_height(5.0, 900.0),
            200.0,
            "too-small clamps up to a floor"
        );
        assert_eq!(
            clamp_height(9999.0, 900.0),
            900.0,
            "taller than display clamps to display"
        );
    }

    // Popover positioning: clamp_x keeps a w-wide window inside [left, right]
    // with an 8px margin, so the popover is never clipped off the clicked
    // display (the "items clipped because the window ran off the edge" class).
    #[test]
    fn clamp_x_keeps_popover_on_screen() {
        // Primary display 0..1440, popover 360 wide.
        let (left, right, w) = (0.0, 1440.0, 360.0);
        // Comfortably inside → unchanged.
        assert_eq!(clamp_x(500.0, w, left, right), 500.0);
        // Off the right edge → pulled in to right - w - 8.
        assert_eq!(clamp_x(1400.0, w, left, right), 1440.0 - 360.0 - 8.0);
        // Off the left edge → pushed to left + 8.
        assert_eq!(clamp_x(-50.0, w, left, right), 8.0);
    }

    #[test]
    fn clamp_x_respects_a_display_left_of_primary() {
        // A monitor arranged to the LEFT of primary has negative global X.
        // The popover must stay on THAT display, not teleport to x=8 (primary).
        let (left, right, w) = (-1920.0, 0.0, 360.0);
        let x = clamp_x(-100.0, w, left, right);
        assert!(x <= right - w - 8.0, "must sit within the left display");
        assert!(x >= left + 8.0, "must not spill off the left display");
        assert_eq!(x, 0.0 - 360.0 - 8.0);
    }

    /// Regression guard for the menu-bar icon. The tray went invisible twice:
    /// once because the status item was built before the app finished launching
    /// (fixed by building on RunEvent::Ready), and the class of failure this
    /// test locks down is the OTHER way the icon disappears — a broken asset.
    ///
    /// The icon is a macOS *template* image, so only the alpha channel matters
    /// (macOS discards the RGB and tints the shape). This decodes the exact
    /// bytes production ships, through the exact decoder production uses, and
    /// asserts the alpha forms a real silhouette: right size, and neither blank
    /// (invisible) nor a solid block (a filled square in the menu bar).
    #[test]
    fn tray_icon_is_a_valid_silhouette() {
        let img = tauri::image::Image::from_bytes(TRAY_ICON_BYTES)
            .expect("tray-template.png must decode");
        assert_eq!(img.width(), 44, "tray icon width");
        assert_eq!(img.height(), 44, "tray icon height");

        let rgba = img.rgba();
        assert_eq!(rgba.len(), 44 * 44 * 4, "RGBA buffer size");

        let opaque = rgba.chunks_exact(4).filter(|px| px[3] > 0).count();
        let total = 44 * 44;
        let coverage = opaque as f64 / total as f64;
        // A blank icon (coverage ~0) is an invisible tray; a fully-opaque icon
        // (coverage ~1) renders as a solid rectangle. A real gourd silhouette
        // sits comfortably between.
        assert!(
            (0.05..=0.90).contains(&coverage),
            "tray icon alpha coverage {coverage:.3} outside sane silhouette band \
             (blank or solid?)"
        );
    }
}
