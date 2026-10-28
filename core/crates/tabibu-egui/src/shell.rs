//! Menu-bar shell for the native egui app — the co-equal surface alongside the
//! dashboard window (Tabibu is both a normal desktop app and a menu-bar app).
//!
//! Ports the Tauri shell (`app/src-tauri/src/tray.rs`) to the standalone eframe
//! build: an `NSStatusItem` with a live CPU%/memory% tooltip, a right-click menu
//! (Open Dashboard / Settings / Pause Monitoring / Quit), and a `Ctrl+Cmd+N`
//! global shortcut. The tray, menu and hotkey events arrive on `tray-icon` /
//! `global-hotkey`'s global channels; the egui app drains them each frame via
//! [`Shell::poll`] and acts on the returned [`ShellEvent`]s.
//!
//! All AppKit-touching handles (`TrayIcon`, the menu items) are `!Send`, so they
//! live here on the main thread. A background thread owns the sampler and ships
//! tooltip text + notification requests over a channel; [`Shell::poll`] (main
//! thread) applies them — keeping every AppKit call on the main thread.
//!
//! DEFERRED to the bundling phase (notifications only deliver from a signed
//! bundle anyway, and these need extra native surface): the floating health
//! popover (a second viewport), Regular↔Accessory activation-policy switching,
//! the Trash-size and thermal-pressure alerts, autostart, and the persisted
//! alert enable/snooze prefs. The two alerts wired here (memory pressure, a
//! single app hogging RAM) come free from the tooltip's own sample.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager};
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};

/// A single app using ≥ this much RAM fires the "an app is hogging memory"
/// notification. Matches `alerts::PROCESS_RAM_ALERT_BYTES` in the Tauri build.
const PROCESS_RAM_ALERT_BYTES: u64 = 2_000_000_000;

/// What a tray/menu/hotkey interaction asks the app to do. The egui app maps
/// these onto its own view state + viewport commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellEvent {
    /// Show + focus the dashboard window (tray left-click, menu "Open", hotkey).
    Open,
    /// Show the dashboard and navigate it to Settings.
    Settings,
    /// Quit the whole app.
    Quit,
}

/// A message from the background sampler thread to the main thread.
enum ShellMsg {
    Tooltip(String),
    Notify { title: String, body: String },
}

/// Hysteresis edge detector shared by the threshold alerts: fire once when
/// `value` first reaches `high` (then disarm), and only re-arm after it falls
/// back below `rearm`. Keeps a value hovering at the line from spamming.
fn alert_edge(value: f64, high: f64, rearm: f64, armed: &mut bool) -> bool {
    if *armed && value >= high {
        *armed = false;
        true
    } else {
        if value < rearm {
            *armed = true;
        }
        false
    }
}

pub struct Shell {
    // Kept alive for the process lifetime; dropping these removes the status
    // item / unregisters the hotkey. The leading `_` ones are never read.
    _tray: TrayIcon,
    _hotkey_mgr: GlobalHotKeyManager,
    pause_item: MenuItem,
    open_id: MenuId,
    settings_id: MenuId,
    pause_id: MenuId,
    quit_id: MenuId,
    hotkey_id: u32,
    paused: Arc<AtomicBool>,
    rx: Receiver<ShellMsg>,
}

impl Shell {
    /// Build the status item, menu and global shortcut, and start the tooltip /
    /// alert sampler thread. Returns `None` if the tray can't be created (the
    /// app still runs as a plain window). Call on the main thread, after the
    /// event loop is up (first `update`) — an `NSStatusItem` built too early
    /// silently fails to attach to the menu bar.
    pub fn new() -> Option<Self> {
        let open = MenuItem::with_id("open", "Open Dashboard", true, None);
        let settings = MenuItem::with_id("settings", "Settings…", true, None);
        let pause = MenuItem::with_id("pause", "Pause Monitoring", true, None);
        let quit = MenuItem::with_id("quit", "Quit Tabibu", true, None);
        let menu = Menu::new();
        menu.append_items(&[
            &open,
            &settings,
            &PredefinedMenuItem::separator(),
            &pause,
            &PredefinedMenuItem::separator(),
            &quit,
        ])
        .ok()?;

        let mut builder = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("Tabibu")
            // Left click toggles the dashboard; the menu stays on right click.
            .with_menu_on_left_click(false);
        if let Some(icon) = load_tray_icon() {
            builder = builder.with_icon(icon).with_icon_as_template(true);
        }
        let tray = builder.build().ok()?;

        // Global shortcut Ctrl+Cmd+N — a reliable way in when the tray icon is
        // hidden behind the notch on a crowded menu bar. If another app owns the
        // combo, registration fails: we log and carry on (the menu still works).
        let hotkey_mgr = GlobalHotKeyManager::new().ok()?;
        let hotkey = HotKey::new(Some(Modifiers::CONTROL | Modifiers::SUPER), Code::KeyN);
        let hotkey_id = hotkey.id();
        if let Err(e) = hotkey_mgr.register(hotkey) {
            eprintln!("global shortcut Ctrl+Cmd+N unavailable (already in use?): {e}");
        }

        let paused = Arc::new(AtomicBool::new(false));
        let (tx, rx) = std::sync::mpsc::channel();
        spawn_sampler(Arc::clone(&paused), tx);

        Some(Self {
            _tray: tray,
            _hotkey_mgr: hotkey_mgr,
            open_id: open.id().clone(),
            settings_id: settings.id().clone(),
            pause_id: pause.id().clone(),
            quit_id: quit.id().clone(),
            pause_item: pause,
            hotkey_id,
            paused,
            rx,
        })
    }

    /// Drain the tray/menu/hotkey channels and the sampler channel. Applies
    /// tooltip + notification messages (both AppKit calls, done here on the main
    /// thread) and returns the user-driven events for the app to act on. Call
    /// once per frame.
    pub fn poll(&self) -> Vec<ShellEvent> {
        // Apply sampler messages: tooltip text and fire-and-forget notifications.
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                ShellMsg::Tooltip(tip) => {
                    let _ = self._tray.set_tooltip(Some(&tip));
                }
                ShellMsg::Notify { title, body } => {
                    let _ = notify_rust::Notification::new()
                        .summary(&title)
                        .body(&body)
                        .show();
                }
            }
        }

        let mut events = Vec::new();
        while let Ok(ev) = MenuEvent::receiver().try_recv() {
            let id = ev.id();
            if *id == self.open_id {
                events.push(ShellEvent::Open);
            } else if *id == self.settings_id {
                events.push(ShellEvent::Settings);
            } else if *id == self.quit_id {
                events.push(ShellEvent::Quit);
            } else if *id == self.pause_id {
                let paused = !self.paused.load(Ordering::Relaxed);
                self.paused.store(paused, Ordering::Relaxed);
                self.pause_item.set_text(if paused {
                    "Resume Monitoring"
                } else {
                    "Pause Monitoring"
                });
            }
        }
        // Left-click on the tray icon opens the dashboard (the floating popover
        // is deferred; see the module note).
        while let Ok(ev) = TrayIconEvent::receiver().try_recv() {
            if let TrayIconEvent::Click {
                button: tray_icon::MouseButton::Left,
                button_state: tray_icon::MouseButtonState::Up,
                ..
            } = ev
            {
                events.push(ShellEvent::Open);
            }
        }
        while let Ok(ev) = GlobalHotKeyEvent::receiver().try_recv() {
            if ev.id == self.hotkey_id && ev.state == global_hotkey::HotKeyState::Pressed {
                events.push(ShellEvent::Open);
            }
        }
        events
    }
}

/// Background sampler: refreshes the tooltip (CPU%/memory%) every 5s and fires
/// the memory-pressure and single-app-RAM notifications (with hysteresis).
/// Mirrors the Tauri tray sampler, minus the Trash/thermal checks (deferred).
fn spawn_sampler(paused: Arc<AtomicBool>, tx: Sender<ShellMsg>) {
    std::thread::spawn(move || {
        let mut sampler = tabibu_monitor::Sampler::new();
        let mut was_paused = false;
        let mut mem_armed = true;
        let mut proc_armed = true;
        loop {
            if paused.load(Ordering::Relaxed) {
                if !was_paused {
                    was_paused = true;
                    if tx
                        .send(ShellMsg::Tooltip("Tabibu — monitoring paused".into()))
                        .is_err()
                    {
                        return; // app gone
                    }
                }
            } else {
                was_paused = false;
                // Order by memory: the tooltip needs only system-wide CPU%/mem%,
                // and this hands the per-app-RAM check the top process without a
                // second sweep.
                let snap = sampler.sample(1, tabibu_monitor::TopBy::Memory);
                let mem_pct = if snap.total_memory_bytes > 0 {
                    (snap.used_memory_bytes as f64 / snap.total_memory_bytes as f64 * 100.0).round()
                } else {
                    0.0
                };
                let tip = format!(
                    "Tabibu — CPU {}% · Memory {}%",
                    snap.cpu_percent.round() as i64,
                    mem_pct as i64
                );
                if tx.send(ShellMsg::Tooltip(tip)).is_err() {
                    return;
                }

                // RAM nearly depleted → notify once; re-arm below 85%.
                if alert_edge(mem_pct, 90.0, 85.0, &mut mem_armed) {
                    let _ = tx.send(ShellMsg::Notify {
                        title: "Memory pressure is high".into(),
                        body: format!(
                            "RAM is {mem_pct}% full. Open Tabibu from the menu bar \
                             and check Memory & CPU for the top consumers."
                        ),
                    });
                }
                // A single app hogging RAM (≥ 2 GB) → notify once; re-arm at 80%.
                if let Some(top) = snap.top_processes.first() {
                    let bytes = top.memory_bytes as f64;
                    let fired = alert_edge(
                        bytes,
                        PROCESS_RAM_ALERT_BYTES as f64,
                        PROCESS_RAM_ALERT_BYTES as f64 * 0.8,
                        &mut proc_armed,
                    );
                    if fired {
                        let _ = tx.send(ShellMsg::Notify {
                            title: "An app is using a lot of memory".into(),
                            body: format!(
                                "{} is using {:.1} GB of RAM. Open Tabibu → Memory & \
                                 CPU to review or quit it.",
                                top.name,
                                bytes / 1_000_000_000.0
                            ),
                        });
                    }
                }
            }
            std::thread::sleep(Duration::from_secs(5));
        }
    });
}

/// Decode the bundled 44×44 RGBA template PNG into a tray icon. Returns `None`
/// on any decode surprise (the tray then shows without an icon rather than
/// crashing).
fn load_tray_icon() -> Option<Icon> {
    const BYTES: &[u8] = include_bytes!("../assets/tray-template.png");
    let decoder = png::Decoder::new(std::io::Cursor::new(BYTES));
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    // The asset is RGBA8 (verified); bail on anything else.
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return None;
    }
    buf.truncate(info.buffer_size());
    Icon::from_rgba(buf, info.width, info.height).ok()
}

#[cfg(test)]
mod tests {
    use super::alert_edge;

    #[test]
    fn alert_edge_fires_once_then_rearms_below_threshold() {
        let mut armed = true;
        // Below the line: no fire, stays armed.
        assert!(!alert_edge(70.0, 90.0, 85.0, &mut armed));
        assert!(armed);
        // Crosses the line: fires once, disarms.
        assert!(alert_edge(92.0, 90.0, 85.0, &mut armed));
        assert!(!armed);
        // Still high but disarmed: no repeat fire (no spam at the threshold).
        assert!(!alert_edge(95.0, 90.0, 85.0, &mut armed));
        // Dips into the hysteresis band (85–90): still disarmed, no re-arm.
        assert!(!alert_edge(87.0, 90.0, 85.0, &mut armed));
        assert!(!armed);
        // Recovers below re-arm line: re-arms, no fire.
        assert!(!alert_edge(80.0, 90.0, 85.0, &mut armed));
        assert!(armed);
        // Crosses again → fires again.
        assert!(alert_edge(91.0, 90.0, 85.0, &mut armed));
    }
}
