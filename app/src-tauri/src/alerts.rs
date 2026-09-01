//! Proactive alert preferences + snooze state for the background sampler.
//!
//! Two alerts fire from the tray sampler thread (`tray.rs`): "Trash is large"
//! (> 2 GB) and "Memory pressure is high". Each can be disabled or snoozed
//! (daily / weekly / forever) from Settings. State persists to a small JSON in
//! the app config dir so a snooze survives relaunch.
//!
//! The pure decision — is this alert allowed to fire right now? — is
//! [`is_active`], unit-tested. Everything else is a thin global + JSON I/O.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Sentinel `snooze_until` meaning "never show again" (until re-enabled).
pub const FOREVER: u64 = u64::MAX;
/// The Trash-size threshold the alert fires above.
pub const TRASH_ALERT_BYTES: u64 = 2 * 1000 * 1000 * 1000; // 2 GB (decimal, matches Finder)
/// A single process using at least this much RAM fires the "app using a lot of
/// memory" alert.
pub const PROCESS_RAM_ALERT_BYTES: u64 = 2 * 1000 * 1000 * 1000; // 2 GB

/// One alert's user preference.
#[derive(Clone, Serialize, Deserialize)]
pub struct AlertSetting {
    pub enabled: bool,
    /// Unix seconds until which the alert is snoozed; `None` = not snoozed,
    /// [`FOREVER`] = snoozed indefinitely.
    pub snooze_until: Option<u64>,
}

impl Default for AlertSetting {
    fn default() -> Self {
        Self {
            enabled: true,
            snooze_until: None,
        }
    }
}

/// All alert preferences (one per alert kind).
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct AlertPrefs {
    #[serde(default)]
    pub trash: AlertSetting,
    #[serde(default)]
    pub memory: AlertSetting,
    /// "A single app is using a lot of RAM" (≥ [`PROCESS_RAM_ALERT_BYTES`]).
    #[serde(default)]
    pub process_ram: AlertSetting,
}

/// Whether an alert may fire now: enabled AND not currently snoozed.
#[must_use]
pub fn is_active(s: &AlertSetting, now_secs: u64) -> bool {
    s.enabled && s.snooze_until.is_none_or(|until| now_secs >= until)
}

/// Translate a UI snooze choice into a `snooze_until` value. `None` return =
/// unrecognized choice (caller ignores).
#[must_use]
pub fn snooze_until_for(choice: &str, now_secs: u64) -> Option<Option<u64>> {
    match choice {
        "daily" => Some(Some(now_secs + 24 * 60 * 60)),
        "weekly" => Some(Some(now_secs + 7 * 24 * 60 * 60)),
        "forever" => Some(Some(FOREVER)),
        "clear" => Some(None), // un-snooze
        _ => None,
    }
}

#[must_use]
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn cell() -> &'static Mutex<AlertPrefs> {
    static C: OnceLock<Mutex<AlertPrefs>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(AlertPrefs::default()))
}

fn lock() -> std::sync::MutexGuard<'static, AlertPrefs> {
    cell().lock().unwrap_or_else(PoisonError::into_inner)
}

/// A clone of the current prefs (for the Settings UI).
#[must_use]
pub fn snapshot() -> AlertPrefs {
    lock().clone()
}

/// Is the Trash alert allowed to fire now?
#[must_use]
pub fn trash_active() -> bool {
    is_active(&lock().trash, now_secs())
}

/// Is the memory alert allowed to fire now?
#[must_use]
pub fn memory_active() -> bool {
    is_active(&lock().memory, now_secs())
}
#[must_use]
pub fn process_ram_active() -> bool {
    is_active(&lock().process_ram, now_secs())
}

/// The JSON file backing the prefs.
fn prefs_path(config_dir: &Path) -> PathBuf {
    config_dir.join("alerts.json")
}

/// Read prefs from disk (pure — no global). Missing/corrupt → defaults.
fn read_prefs(config_dir: &Path) -> AlertPrefs {
    std::fs::read_to_string(prefs_path(config_dir))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Write prefs to disk (pure — no global). Best-effort.
fn write_prefs(config_dir: &Path, prefs: &AlertPrefs) {
    let _ = std::fs::create_dir_all(config_dir);
    if let Ok(text) = serde_json::to_string_pretty(prefs) {
        let _ = std::fs::write(prefs_path(config_dir), text);
    }
}

/// Load prefs from disk into the global (call once at startup).
pub fn load(config_dir: &Path) {
    *lock() = read_prefs(config_dir);
}

/// Apply a mutation to the global prefs and persist. `f` receives the mutable
/// prefs; returns the updated snapshot.
pub fn update(config_dir: &Path, f: impl FnOnce(&mut AlertPrefs)) -> AlertPrefs {
    // Persist while STILL holding the lock, so two concurrent writers can't land
    // their file writes out of order (the on-disk JSON always matches the last
    // in-memory state). The write is brief; contention here is negligible.
    let mut g = lock();
    f(&mut g);
    write_prefs(config_dir, &g);
    g.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_active_respects_enabled_and_snooze() {
        let now = 1_000_000;
        // Enabled, not snoozed → fires.
        assert!(is_active(
            &AlertSetting {
                enabled: true,
                snooze_until: None
            },
            now
        ));
        // Disabled → never fires.
        assert!(!is_active(
            &AlertSetting {
                enabled: false,
                snooze_until: None
            },
            now
        ));
        // Snoozed into the future → suppressed.
        assert!(!is_active(
            &AlertSetting {
                enabled: true,
                snooze_until: Some(now + 10)
            },
            now
        ));
        // Snooze already elapsed → fires again.
        assert!(is_active(
            &AlertSetting {
                enabled: true,
                snooze_until: Some(now - 1)
            },
            now
        ));
        // Forever → never fires while enabled.
        assert!(!is_active(
            &AlertSetting {
                enabled: true,
                snooze_until: Some(FOREVER)
            },
            now
        ));
    }

    #[test]
    fn snooze_choices_map_to_durations() {
        let now = 1_000;
        assert_eq!(snooze_until_for("daily", now), Some(Some(now + 86_400)));
        assert_eq!(snooze_until_for("weekly", now), Some(Some(now + 604_800)));
        assert_eq!(snooze_until_for("forever", now), Some(Some(FOREVER)));
        assert_eq!(snooze_until_for("clear", now), Some(None));
        assert_eq!(snooze_until_for("nonsense", now), None);
    }

    // Persistence tested on the pure read/write helpers (no shared global), so
    // these can't race the other tests running in-process in parallel.
    #[test]
    fn read_missing_file_is_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let p = read_prefs(dir.path());
        assert!(p.trash.enabled && p.memory.enabled);
        assert!(p.trash.snooze_until.is_none() && p.memory.snooze_until.is_none());
    }

    #[test]
    fn all_alert_kinds_default_on_and_active() {
        // Regression: every alert — including the new per-app-RAM one — defaults
        // enabled and is allowed to fire, and the 2 GB thresholds are set.
        let p = AlertPrefs::default();
        for s in [&p.trash, &p.memory, &p.process_ram] {
            assert!(s.enabled && is_active(s, now_secs()));
        }
        assert_eq!(TRASH_ALERT_BYTES, 2_000_000_000);
        assert_eq!(PROCESS_RAM_ALERT_BYTES, 2_000_000_000);
    }

    #[test]
    fn write_then_read_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let mut prefs = AlertPrefs::default();
        prefs.trash.snooze_until = Some(FOREVER);
        prefs.memory.enabled = false;
        write_prefs(dir.path(), &prefs);

        let back = read_prefs(dir.path());
        assert_eq!(back.trash.snooze_until, Some(FOREVER));
        assert!(!back.memory.enabled);
    }
}
