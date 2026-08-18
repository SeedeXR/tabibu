//! Tauri command surface — the Rust core called directly (no FFI). Streaming
//! results use a Tauri `Channel`; everything else is request/response with
//! serde types the core crates already derive.

use std::sync::{LazyLock, Mutex};

use serde::Serialize;
use tauri::ipc::Channel;

use tabibu_dupes::{DupeConfig, DuplicateGroup};
use tabibu_engine::{
    reclaim as engine_reclaim, smart_scan, CancelToken, CleanupItem, ReclaimReport,
};
use tabibu_monitor::{Sampler, SystemSample, TopBy};
use tabibu_walk::DirNode;

use crate::system;

/// Cancel token for the STREAMING `scan` (its Stop button → `cancel_scan`).
static CURRENT_SCAN: LazyLock<Mutex<Option<CancelToken>>> = LazyLock::new(|| Mutex::new(None));
/// Cancel tokens for the long SYNCHRONOUS commands (whole-home duplicates,
/// leftovers, security) — a registry, not a single slot, because commands run
/// async on worker threads and can overlap (e.g. Duplicates still walking while
/// Security starts). A single slot would orphan the earlier op's token and make
/// it uncancellable; the registry keeps every in-flight op cancellable.
static CURRENT_SYNC: LazyLock<Mutex<Vec<CancelToken>>> = LazyLock::new(|| Mutex::new(Vec::new()));
/// Persistent monitor samplers (CPU deltas need a long-lived `System`, and
/// each polling surface needs its own — see `sample_with`). The tray tooltip
/// keeps a third sampler of its own in `tray.rs`.
static SAMPLER: LazyLock<Mutex<Option<Sampler>>> = LazyLock::new(|| Mutex::new(None));
static POPOVER_SAMPLER: LazyLock<Mutex<Option<Sampler>>> = LazyLock::new(|| Mutex::new(None));

/// Register a fresh synchronous-op cancel token. The returned guard drives the
/// operation and deregisters the token when the op ends (any path — success,
/// error, panic) so the registry stays bounded without relying on Stop.
fn begin_sync_op() -> SyncOpGuard {
    let token = CancelToken::new();
    let mut reg = CURRENT_SYNC
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    reg.retain(|t| !t.is_cancelled());
    reg.push(token.clone());
    SyncOpGuard(token)
}

/// RAII handle for one synchronous op's cancel token.
struct SyncOpGuard(CancelToken);

impl std::ops::Deref for SyncOpGuard {
    type Target = CancelToken;
    fn deref(&self) -> &CancelToken {
        &self.0
    }
}

impl Drop for SyncOpGuard {
    fn drop(&mut self) {
        let mut reg = CURRENT_SYNC
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Tokens have no identity beyond their shared flag; cancelling ours
        // and pruning cancelled entries removes exactly the finished ops.
        // (A token cancelled here has already served its purpose.)
        self.0.cancel();
        reg.retain(|t| !t.is_cancelled());
    }
}

/// Cancel every in-flight synchronous op (duplicates / leftovers / security).
/// There is one Stop affordance, so stopping cancels all running scans.
#[tauri::command(async)]
pub fn cancel_sync() {
    let mut reg = CURRENT_SYNC
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for token in reg.drain(..) {
        token.cancel();
    }
}

// ---------------------------------------------------------------------------
// Streaming scan
// ---------------------------------------------------------------------------

#[derive(Serialize, Clone)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ScanEvent {
    Item {
        item: CleanupItem,
    },
    Done {
        cancelled: bool,
        scanners: Vec<ScannerOutcomeDto>,
    },
}

#[derive(Serialize, Clone)]
pub struct ScannerOutcomeDto {
    id: String,
    items: u64,
    guard_rejected: u64,
    error: Option<String>,
}

/// Start a scan over the given scanner ids (empty = all junk scanners).
/// Items stream to `on_event` as found; a final `Done` carries the per-scanner
/// summary. Returns immediately — work runs on a background thread.
#[tauri::command(async)]
pub fn scan(scanners: Vec<String>, on_event: Channel<ScanEvent>) {
    let cancel = CancelToken::new();
    // One streaming scan at a time: starting a new one cancels any scan still
    // running, otherwise the old token is orphaned and its thread keeps
    // hammering the disk while streaming into a stale channel.
    if let Some(prev) = CURRENT_SCAN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .replace(cancel.clone())
    {
        prev.cancel();
    }

    std::thread::spawn(move || {
        let ctx = system::default_scan_ctx(&[]);
        let wanted = scanners;
        let all: Vec<Box<dyn tabibu_engine::Scanner>> = tabibu_junk::scanners()
            .into_iter()
            .chain(tabibu_malware::scanners())
            .filter(|s| {
                if wanted.is_empty() {
                    !matches!(s.id(), "adware_heuristics" | "rogue_profiles")
                } else {
                    wanted.iter().any(|w| w == s.id())
                }
            })
            .collect();

        let report = smart_scan(&all, &ctx, &cancel, &|item: CleanupItem| {
            let _ = on_event.send(ScanEvent::Item { item });
        });

        let _ = on_event.send(ScanEvent::Done {
            cancelled: report.cancelled,
            scanners: report
                .outcomes
                .iter()
                .map(|o| ScannerOutcomeDto {
                    id: o.scanner_id.to_string(),
                    items: o.items_emitted,
                    guard_rejected: o.guard_rejected,
                    error: o.error.clone(),
                })
                .collect(),
        });
    });
}

#[tauri::command(async)]
pub fn cancel_scan() {
    if let Some(token) = CURRENT_SCAN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
    {
        token.cancel();
    }
}

// ---------------------------------------------------------------------------
// Reclaim (the only mutating path)
// ---------------------------------------------------------------------------

/// Reclaim the supplied items. `extra_roots` widens the allowed-roots set for
/// targets outside the standard junk locations (duplicates / remnants in a
/// chosen folder); the engine still re-checks every path against the denylist.
#[tauri::command(async)]
pub fn reclaim(
    mut items: Vec<CleanupItem>,
    extra_roots: Vec<String>,
) -> Result<ReclaimReport, String> {
    let home = system::home_dir();
    // Defense in depth against a compromised/injected webview (the CSP is the
    // first barrier; this is the second):
    //   1. Force every action to Trash. The scanners only ever emit Trash, so
    //      a Delete (permanent `remove_dir_all`) or Truncate could only come
    //      from abuse — coercing to Trash keeps reclaim always reversible.
    //   2. Only honor extra_roots inside the user's home. Every real caller
    //      passes [], [home] or [home/Library]; dropping anything outside home
    //      stops the allowed-root set being widened to other volumes or "/".
    // (The engine denylist still rejects protected/`..`/system paths on top.)
    for item in &mut items {
        item.selected = true;
        item.action = tabibu_engine::ReclaimAction::Trash;
    }
    let safe_roots: Vec<String> = extra_roots
        .into_iter()
        .filter(|r| std::path::Path::new(r).starts_with(&home))
        .collect();
    let ctx = system::default_scan_ctx(&safe_roots);
    engine_reclaim(&ctx, &items, std::path::Path::new(&system::undo_dir()))
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Universal ("fat") binaries — READ-ONLY report (never modifies files).
// ---------------------------------------------------------------------------

/// Scan `/Applications`, `~/Applications` and `/Applications/Utilities` for
/// universal binaries and report the reclaimable (non-native) slice bytes per
/// app. Read-only: nothing is stripped or trashed — the user decides whether
/// to `lipo`-thin manually (it can break signed apps). Registered in
/// CURRENT_SYNC so Stop / navigating away cancels the walk.
#[tauri::command(async)]
pub fn scan_universal() -> tabibu_universal::UniversalReport {
    let cancel = begin_sync_op();
    let home = system::home_dir();
    let roots = vec![
        std::path::PathBuf::from("/Applications"),
        home.join("Applications"),
    ];
    tabibu_universal::scan(&roots, &cancel)
}

/// Thin one app bundle to this Mac's native architecture and ad-hoc re-sign it,
/// freeing the other-arch slices. Destructive + irreversible (the UI confirms,
/// and warns hard for signed apps whose signature this voids). `path` is the
/// `.app` bundle path from a prior [`scan_universal`] result.
///
/// Validated like the other destructive commands (`trash_path`): a frontend bug
/// or unexpected value must not let this walk-and-thin an arbitrary subtree.
#[tauri::command(async)]
pub fn strip_universal(path: String) -> Result<tabibu_universal::StripResult, String> {
    let p = std::path::Path::new(&path);
    if !p.is_absolute() {
        return Err("path must be absolute".into());
    }
    if p.extension().and_then(|e| e.to_str()) != Some("app") {
        return Err("only .app bundles can be thinned".into());
    }
    if let Some(reason) = tabibu_engine::denylist::denied(p, &system::home_dir()) {
        return Err(format!("protected path ({reason:?}); refusing to strip"));
    }
    Ok(tabibu_universal::strip_app(p))
}

// ---------------------------------------------------------------------------
// Space map
// ---------------------------------------------------------------------------

#[tauri::command(async)]
pub fn size_tree(root: String, max_depth: Option<usize>) -> Result<DirNode, String> {
    // Registered like the other long sync ops so Stop / navigating away
    // (cancel_sync) can abort a huge walk instead of letting it run to the end.
    let cancel = begin_sync_op();
    tabibu_walk::size_tree(std::path::Path::new(&root), &cancel, max_depth)
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Duplicates
// ---------------------------------------------------------------------------

/// Find duplicates. `root` is optional — when omitted, the entire home folder
/// is scanned (the user doesn't have to pick a folder). The walk skips
/// unreadable/system dirs, so this is the practical "whole-disk" scope for a
/// user's duplicate files.
#[tauri::command(async)]
pub fn find_duplicates(root: Option<String>, min_size: u64) -> Result<Vec<DuplicateGroup>, String> {
    // Registers in CURRENT_SYNC (not CURRENT_SCAN) so the Duplicates view's
    // Stop button (cancel_sync) can abort this long whole-home scan without
    // touching a streaming junk scan.
    let cancel = begin_sync_op();
    let root = root.unwrap_or_else(|| system::home_dir().to_string_lossy().into_owned());
    let files = tabibu_dupes::collect_candidates(std::path::Path::new(&root), min_size, &cancel)
        .map_err(|e| e.to_string())?;
    let cfg = DupeConfig { min_size };
    tabibu_dupes::find_duplicates(&files, &cfg, &cancel, &|_g| {}).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Uninstaller
// ---------------------------------------------------------------------------

#[tauri::command(async)]
pub fn find_remnants(bundle_id: String, app_name: String) -> Vec<CleanupItem> {
    let home = system::home_dir();
    let extra = vec![home.join("Library").to_string_lossy().into_owned()];
    let ctx = system::default_scan_ctx(&extra);
    tabibu_uninstall::find_remnants(&bundle_id, &app_name, &ctx)
}

#[derive(Serialize)]
pub struct InstalledApp {
    pub path: String,
    pub name: String,
    pub bundle_id: String,
}

/// Apps in /Applications and ~/Applications with their bundle IDs.
#[tauri::command(async)]
pub fn installed_apps() -> Vec<InstalledApp> {
    let home = system::home_dir();
    let roots = vec![
        std::path::PathBuf::from("/Applications"),
        home.join("Applications"),
    ];
    tabibu_uninstall::installed_apps(&roots)
        .into_iter()
        .map(|(path, bundle_id)| {
            let name = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            InstalledApp {
                path: path.to_string_lossy().into_owned(),
                name,
                bundle_id,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Monitor
// ---------------------------------------------------------------------------

/// Sample through the given long-lived sampler. sysinfo derives per-process
/// (and global) CPU% from the elapsed time since that `System`'s previous
/// refresh, so two consumers refreshing ONE sampler on different cadences
/// compute deltas over the wrong interval and report garbage CPU%. Each
/// surface therefore owns a sampler (dashboard, tray popover, tray tooltip).
fn sample_with(
    cell: &LazyLock<Mutex<Option<Sampler>>>,
    top_n: usize,
    by_cpu: bool,
) -> SystemSample {
    // Poison-tolerant (matches system.rs): a panic in one sampler call must not
    // permanently break every later sample (and the tray thread).
    let mut guard = cell
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let sampler = guard.get_or_insert_with(Sampler::new);
    let by = if by_cpu { TopBy::Cpu } else { TopBy::Memory };
    sampler.sample(top_n, by)
}

/// Dashboard sampler (the main window polls this).
#[tauri::command(async)]
pub fn monitor_sample(top_n: usize, by_cpu: bool) -> SystemSample {
    sample_with(&SAMPLER, top_n, by_cpu)
}

/// Tray-popover sampler — separate `System` from the dashboard's, so the two
/// windows polling on different cadences don't corrupt each other's CPU%.
#[tauri::command(async)]
pub fn menubar_sample(top_n: usize, by_cpu: bool) -> SystemSample {
    sample_with(&POPOVER_SAMPLER, top_n, by_cpu)
}

#[derive(Serialize)]
pub struct DiskSpace {
    pub total_bytes: u64,
    pub available_bytes: u64,
}

/// Free/total bytes on the boot volume ("/"). Measured, for the dashboard.
#[tauri::command(async)]
pub fn disk_space() -> DiskSpace {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    // Prefer the disk mounted at "/"; fall back to the largest.
    let root = disks
        .list()
        .iter()
        .find(|d| d.mount_point() == std::path::Path::new("/"))
        .or_else(|| disks.list().iter().max_by_key(|d| d.total_space()));
    DiskSpace {
        total_bytes: root.map_or(0, sysinfo::Disk::total_space),
        available_bytes: root.map_or(0, sysinfo::Disk::available_space),
    }
}

// ---------------------------------------------------------------------------
// System info + shell actions
// ---------------------------------------------------------------------------

#[tauri::command(async)]
pub fn system_info() -> system::SystemInfo {
    system::system_info()
}

#[tauri::command(async)]
pub fn battery_info() -> system::BatteryInfo {
    system::battery_info()
}

#[tauri::command(async)]
pub fn startup_items() -> system::StartupReport {
    system::startup_items()
}

#[tauri::command(async)]
pub fn reveal_in_finder(path: String) {
    // Absolute paths only: `open` uses getopt, so an argument beginning with
    // '-' would be parsed as a flag (arg injection — e.g. launch an app),
    // exactly what open_url/trash_path guard against. An absolute path starts
    // with '/', so it can never be mistaken for an option.
    if !std::path::Path::new(&path).is_absolute() {
        return;
    }
    let _ = std::process::Command::new("/usr/bin/open")
        .arg("-R")
        .arg(path)
        .spawn();
}

/// Open a link. Scheme-allowlisted: an unvalidated string handed to `open`
/// would upgrade "open a URL" into "launch any app or file" (`open` treats
/// bare paths as documents and parses `-a`/flag-shaped strings as options).
#[tauri::command(async)]
pub fn open_url(url: String) -> Result<(), String> {
    const ALLOWED: &[&str] = &["https://", "http://", "x-apple.systempreferences:"];
    if !ALLOWED.iter().any(|scheme| url.starts_with(scheme)) {
        return Err("URL scheme not allowed".into());
    }
    std::process::Command::new("/usr/bin/open")
        .arg(url)
        .spawn()
        .map(drop)
        .map_err(|e| e.to_string())
}

/// Move a path to the Trash via the OS (used by the uninstaller's "also trash
/// the app", which lives outside the engine's user roots). The engine denylist
/// still applies: nothing under a protected root can be trashed through here.
#[tauri::command(async)]
pub fn trash_path(path: String) -> Result<(), String> {
    let p = std::path::Path::new(&path);
    if !p.is_absolute() {
        return Err("path must be absolute".into());
    }
    if let Some(reason) = tabibu_engine::denylist::denied(p, &system::home_dir()) {
        return Err(format!("protected path ({reason:?}); refusing to trash"));
    }
    // Same silent (no Finder sound), spawn-free trash path as reclaim uses.
    tabibu_engine::move_to_trash(p).map_err(|e| e.to_string())
}

/// `~/.Trash` plus every mounted volume's per-user trash — the same set the
/// Trash scanner and the "Trash is large" alert use. `pub(crate)` so the tray
/// sampler's alert shares this exact derivation (can't drift from the command).
pub(crate) fn trash_dirs() -> Vec<std::path::PathBuf> {
    let mut dirs = vec![system::home_dir().join(".Trash")];
    dirs.extend(system::per_volume_trash_roots());
    dirs
}

/// Current total size of the Trash (for the Empty-Trash button's label).
#[tauri::command(async)]
pub fn trash_size() -> u64 {
    tabibu_junk::trash_total_size(&trash_dirs(), &tabibu_engine::CancelToken::new())
}

/// Result of emptying the Trash.
#[derive(Serialize)]
pub struct EmptyTrashResult {
    pub freed_bytes: u64,
    pub deleted_items: u32,
    pub errors: Vec<String>,
}

/// PERMANENTLY empty the Trash (all of it, incl. per-volume trashes). Destructive
/// and irreversible — the UI confirms first. Returns bytes/items freed.
#[tauri::command(async)]
pub fn empty_trash() -> EmptyTrashResult {
    let o = tabibu_junk::empty_trash_dirs(&trash_dirs());
    EmptyTrashResult {
        freed_bytes: o.freed_bytes,
        deleted_items: o.deleted_items,
        errors: o.errors,
    }
}

// ---------------------------------------------------------------------------
// Proactive alerts (Trash-large / memory-pressure) — enable + snooze prefs.
// ---------------------------------------------------------------------------

fn alerts_config_dir(app: &tauri::AppHandle) -> std::path::PathBuf {
    use tauri::Manager;
    app.path().app_config_dir().unwrap_or_default()
}

/// Current alert preferences (for the Settings UI).
#[tauri::command(async)]
pub fn get_alert_prefs() -> crate::alerts::AlertPrefs {
    crate::alerts::snapshot()
}

/// Enable/disable one alert (`kind` = "trash" | "memory"). Enabling also clears
/// any active snooze. Returns the updated prefs.
#[tauri::command(async)]
pub fn set_alert_enabled(
    app: tauri::AppHandle,
    kind: String,
    enabled: bool,
) -> crate::alerts::AlertPrefs {
    crate::alerts::update(&alerts_config_dir(&app), |p| {
        let s = match kind.as_str() {
            "trash" => &mut p.trash,
            "memory" => &mut p.memory,
            _ => return,
        };
        s.enabled = enabled;
        if enabled {
            s.snooze_until = None; // re-enabling un-snoozes
        }
    })
}

/// Snooze one alert. `choice` = "daily" | "weekly" | "forever" | "clear".
#[tauri::command(async)]
pub fn snooze_alert(
    app: tauri::AppHandle,
    kind: String,
    choice: String,
) -> crate::alerts::AlertPrefs {
    let Some(until) = crate::alerts::snooze_until_for(&choice, crate::alerts::now_secs()) else {
        return crate::alerts::snapshot(); // unrecognized choice: no change
    };
    crate::alerts::update(&alerts_config_dir(&app), |p| match kind.as_str() {
        "trash" => p.trash.snooze_until = until,
        "memory" => p.memory.snooze_until = until,
        _ => {}
    })
}

/// Fire a test notification so the user can confirm macOS is delivering them
/// (and grant permission on first show).
#[tauri::command(async)]
pub fn send_test_notification(app: tauri::AppHandle) -> Result<(), String> {
    use tauri_plugin_notification::NotificationExt;
    app.notification()
        .builder()
        .title("Tabibu notifications are on")
        .body("This is a test alert. Trash and memory alerts will appear like this.")
        .show()
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Deselection telemetry — opt-in, privacy-respecting (no paths/contents).
// ---------------------------------------------------------------------------

#[tauri::command(async)]
pub fn telemetry_enabled() -> bool {
    tabibu_telemetry::Telemetry::load(&system::telemetry_dir()).is_enabled()
}

#[tauri::command(async)]
pub fn set_telemetry_enabled(on: bool) -> Result<(), String> {
    let mut t = tabibu_telemetry::Telemetry::load(&system::telemetry_dir());
    t.set_enabled(on).map_err(|e| e.to_string())
}

/// Record that the user unchecked a suggested item (a false-positive signal).
/// Records only the category, tier, and a coarse size bucket — never the path.
/// No-op (returns false) when telemetry is disabled. `ts_unix` is supplied by
/// the caller so the core stays clock-free.
#[tauri::command(async)]
pub fn record_deselection(
    category: String,
    tier: String,
    size_bytes: u64,
    ts_unix: u64,
) -> Result<bool, String> {
    let t = tabibu_telemetry::Telemetry::load(&system::telemetry_dir());
    let event = tabibu_telemetry::DeselectionEvent {
        category,
        tier,
        size_bucket: tabibu_telemetry::SizeBucket::from_bytes(size_bytes),
        ts_unix,
    };
    t.record(&event).map_err(|e| e.to_string())
}

// ===========================================================================
// v0.1.3 additions
// ===========================================================================

use std::collections::HashSet;
use tabibu_engine::scanner::{run_scanner, ScanCtx, Scanner};

/// Run a single scanner through the engine guard and collect its items, driven
/// by an already-registered cancel token. Multi-scanner ops register ONE token
/// (via [`begin_sync_op`]) and pass it here for every scanner, so a single
/// `cancel_sync` stops the whole sweep — registering per scanner would let each
/// overwrite the previous token, making earlier scanners uncancellable.
fn collect_with(scanner: &dyn Scanner, ctx: &ScanCtx, cancel: &CancelToken) -> Vec<CleanupItem> {
    let mut items = Vec::new();
    let mut sink = |it: CleanupItem| items.push(it);
    let _ = run_scanner(scanner, ctx, cancel, &mut sink);
    items
}

// ---- Force quit ----------------------------------------------------------

/// Ask a process to quit (SIGTERM) or force it (SIGKILL). The UI confirms
/// first and warns about unsaved work. Returns Ok only if the signal was sent.
#[tauri::command(async)]
pub fn quit_process(pid: u32, force: bool) -> Result<(), String> {
    // Reject pid 0 and out-of-range values: `kill(0, sig)` signals the
    // CALLER's whole process group (would kill Tabibu itself), and a pid that
    // doesn't fit pid_t would wrap negative (also a process-group target).
    if pid == 0 || pid > i32::MAX as u32 {
        return Err(format!("refusing to signal invalid pid {pid}"));
    }
    let sig = if force { libc::SIGKILL } else { libc::SIGTERM };
    // SAFETY: kill() is a plain syscall; pid is validated > 0 and in range, sig
    // is a constant. A stale pid just returns -1/ESRCH, surfaced as an error.
    let rc = unsafe { libc::kill(pid as libc::pid_t, sig) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().to_string())
    }
}

// ---- Thermal (honest: pressure, not fabricated die temperature) ----------

#[derive(Serialize)]
pub struct ThermalInfo {
    /// "Nominal" | "Fair" | "Serious" | "Critical" | "Unknown".
    pub pressure: String,
    /// CPU speed limit %, 100 = no throttling (from `pmset -g therm`).
    pub speed_limit: Option<u32>,
    /// Honest note about why exact die temperature isn't shown.
    pub note: String,
}

/// Thermal pressure from `pmset -g therm` (no root). Exact CPU die temperature
/// is intentionally NOT reported: on modern Macs it requires root/SMC access
/// (a privileged helper we don't ship). We show the real management signal.
#[tauri::command(async)]
pub fn thermal_info() -> ThermalInfo {
    let note = "Exact CPU temperature needs elevated access on modern Macs; \
                Tabibu shows the system's real thermal-pressure signal instead."
        .to_string();
    let out = std::process::Command::new("/usr/bin/pmset")
        .args(["-g", "therm"])
        .output();
    let Ok(out) = out else {
        return ThermalInfo {
            pressure: "Unknown".into(),
            speed_limit: None,
            note,
        };
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut speed_limit = None;
    for line in text.lines() {
        if let Some(v) = line.split('=').nth(1) {
            if line.contains("CPU_Speed_Limit") {
                speed_limit = v.trim().trim_end_matches('%').parse::<u32>().ok();
            }
        }
    }
    let pressure = match speed_limit {
        // No CPU_Speed_Limit line (format varies across macOS releases): we
        // have no evidence either way, so say Unknown — never claim health
        // that wasn't measured.
        None => "Unknown",
        Some(s) if s >= 100 => "Nominal",
        Some(s) if s >= 75 => "Fair",
        Some(s) if s >= 50 => "Serious",
        Some(_) => "Critical",
    }
    .to_string();
    ThermalInfo {
        pressure,
        speed_limit,
        note,
    }
}

// ---- SMART disk status ---------------------------------------------------

/// Boot-volume SMART status via `diskutil info -plist /` (no root). Returns
/// e.g. "Verified", "Not Supported", or "Unknown".
#[tauri::command(async)]
pub fn smart_status() -> String {
    let out = std::process::Command::new("/usr/sbin/diskutil")
        .args(["info", "-plist", "/"])
        .output();
    let Ok(out) = out else {
        return "Unknown".into();
    };
    let Ok(val) = plist::Value::from_reader(std::io::Cursor::new(out.stdout)) else {
        return "Unknown".into();
    };
    val.as_dictionary()
        .and_then(|d| d.get("SMARTStatus"))
        .and_then(plist::Value::as_string)
        .unwrap_or("Unknown")
        .to_string()
}

// ---- Uninstaller leftovers (disk-wide orphan artifacts) ------------------

/// Scan for support files whose owning app is no longer installed — the
/// "remaining artifacts after uninstalling software" feature.
#[tauri::command(async)]
pub fn scan_orphans() -> Vec<CleanupItem> {
    let home = system::home_dir();
    let installed: HashSet<String> = tabibu_uninstall::installed_apps(&[
        std::path::PathBuf::from("/Applications"),
        home.join("Applications"),
    ])
    .into_iter()
    .map(|(_, id)| id)
    .collect();
    let scanner = tabibu_uninstall::OrphanScanner::new(installed);
    let ctx = ScanCtx {
        home: home.clone(),
        allowed_roots: vec![
            home.join("Library/Application Support"),
            home.join("Library/Caches"),
            home.join("Library/Containers"),
        ],
        running_bundle_ids: system::running_bundle_ids(),
        full_disk_access: system::has_full_disk_access(&home),
    };
    let cancel = begin_sync_op();
    collect_with(&scanner, &ctx, &cancel)
}

// ---- Security (adware / rogue-profile heuristics) ------------------------

#[tauri::command(async)]
pub fn scan_malware() -> Vec<CleanupItem> {
    let home = system::home_dir();
    let ctx = ScanCtx {
        home: home.clone(),
        allowed_roots: vec![
            home.join("Library/LaunchAgents"),
            std::path::PathBuf::from("/Library/Managed Preferences"),
        ],
        running_bundle_ids: system::running_bundle_ids(),
        full_disk_access: system::has_full_disk_access(&home),
    };
    // One cancel token for the whole multi-scanner sweep so a single Stop
    // aborts it; bail out between scanners once cancelled.
    let cancel = begin_sync_op();
    let mut items = Vec::new();
    for scanner in tabibu_malware::scanners() {
        if cancel.is_cancelled() {
            break;
        }
        items.extend(collect_with(scanner.as_ref(), &ctx, &cancel));
    }
    items
}

/// Move a detected item into the locked quarantine vault (never deletes).
#[tauri::command(async)]
pub fn quarantine(path: String) -> Result<(), String> {
    let home = system::home_dir();
    let vault = tabibu_malware::Vault::new(
        home.join("Library/Application Support/Tabibu/quarantine"),
        home.clone(),
    );
    vault
        .quarantine(std::path::Path::new(&path))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

// ---- Free-space trend (persisted across launches) ------------------------

/// Sampling cadence for the free-space trend: at most one point per hour, so
/// the 90-point history spans ~90 hours regardless of how often the dashboard
/// calls this (it polls every 2s). Without this throttle the "trend across
/// launches" would only hold the last few minutes and rewrite the file
/// constantly.
const FREE_SPACE_MIN_SPACING: u64 = 3600;
/// Serializes the free-space-history read-modify-write. Commands run async on
/// worker threads, so two overlapping calls would otherwise race on the file
/// (lost point, or a torn read of a half-written file).
static FREE_SPACE_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

/// Record a free-space reading (throttled to one per hour) and return the
/// recent history (most recent last).
#[tauri::command(async)]
pub fn record_free_space(ts_unix: u64, free_bytes: u64) -> Vec<(u64, u64)> {
    let _guard = FREE_SPACE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = system::home_dir().join("Library/Application Support/Tabibu");
    let _ = std::fs::create_dir_all(&dir);
    let file = dir.join("free-space-history.json");
    let mut history: Vec<(u64, u64)> = std::fs::read(&file)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    // Throttle: append only once the newest sample is ≥ 1h old, and touch the
    // disk only when appending. (Updating the last point's timestamp in place
    // here would keep resetting the 1h anchor — with a 2s poll the gap would
    // never elapse and the history would hold one forever-sliding point.)
    let stale = history
        .last()
        .is_none_or(|last| ts_unix.saturating_sub(last.0) >= FREE_SPACE_MIN_SPACING);
    if stale {
        history.push((ts_unix, free_bytes));
        // Keep the last 90 samples.
        let len = history.len();
        if len > 90 {
            history.drain(0..len - 90);
        }
        // Atomic write: tmp + rename, so a concurrent reader never sees a torn file.
        if let Ok(json) = serde_json::to_vec(&history) {
            let tmp = file.with_extension("json.tmp");
            if std::fs::write(&tmp, json).is_ok() {
                let _ = std::fs::rename(&tmp, &file);
            }
        }
    }
    history
}

// ---------------------------------------------------------------------------
// Homebrew (terminal-installed software): analysis + safe cleanup.
//
// All removal is delegated to `brew` itself (see tabibu-brew's safety doc):
// `brew cleanup` (old versions + stale cache), `brew autoremove` (orphaned
// dependencies), and `brew uninstall` WITHOUT force (refuses if depended on).
// ---------------------------------------------------------------------------

fn brew_not_found_outcome() -> tabibu_brew::ActionOutcome {
    tabibu_brew::ActionOutcome {
        ok: false,
        freed_bytes: 0,
        message: "Homebrew was not found at /opt/homebrew or /usr/local.".to_string(),
    }
}

fn with_brew<F>(f: F) -> tabibu_brew::ActionOutcome
where
    F: FnOnce(&tabibu_brew::Brew) -> tabibu_brew::ActionOutcome,
{
    tabibu_brew::Brew::detect().map_or_else(brew_not_found_outcome, |b| f(&b))
}

/// Full Homebrew analysis (read-only): status, cleanup preview, orphaned
/// dependencies, and every installed formula/cask with size + install date.
/// Returns `status.installed = false` when Homebrew isn't present.
#[tauri::command(async)]
pub fn brew_analyze() -> tabibu_brew::Report {
    tabibu_brew::Brew::detect().map_or_else(
        || tabibu_brew::Report {
            status: tabibu_brew::Status {
                installed: false,
                prefix: None,
                version: None,
            },
            cleanup: tabibu_brew::CleanupPreview::default(),
            autoremovable: Vec::new(),
            packages: Vec::new(),
        },
        |b| b.analyze(),
    )
}

/// Run `brew cleanup` (old versions + stale download cache only).
#[tauri::command(async)]
pub fn brew_cleanup() -> tabibu_brew::ActionOutcome {
    with_brew(tabibu_brew::Brew::run_cleanup)
}

/// Run `brew autoremove` (orphaned dependencies only).
#[tauri::command(async)]
pub fn brew_autoremove() -> tabibu_brew::ActionOutcome {
    with_brew(tabibu_brew::Brew::run_autoremove)
}

/// Uninstall one Homebrew package by name. Never forces — `brew` refuses if
/// another installed package depends on it (surfaced as `ok = false`).
#[tauri::command(async)]
pub fn brew_uninstall(name: String, cask: bool) -> tabibu_brew::ActionOutcome {
    with_brew(|b| b.uninstall(&name, cask))
}

// ---------------------------------------------------------------------
// Docker: read-only analysis + prune (all removal delegated to `docker`).
// ---------------------------------------------------------------------

fn docker_not_found_outcome() -> tabibu_docker::ActionOutcome {
    tabibu_docker::ActionOutcome {
        ok: false,
        freed_bytes: 0,
        message: "The docker CLI was not found.".to_string(),
    }
}

fn with_docker<F>(f: F) -> tabibu_docker::ActionOutcome
where
    F: FnOnce(&tabibu_docker::Docker) -> tabibu_docker::ActionOutcome,
{
    tabibu_docker::Docker::detect().map_or_else(docker_not_found_outcome, |d| f(&d))
}

/// Read-only Docker disk-usage analysis (`docker system df`). Returns
/// `status.installed = false` if the CLI is absent, or `running = false` if the
/// daemon isn't up.
#[tauri::command(async)]
pub fn docker_analyze() -> tabibu_docker::Report {
    tabibu_docker::Docker::detect().map_or_else(
        || tabibu_docker::Report {
            status: tabibu_docker::Status {
                installed: false,
                running: false,
                version: None,
            },
            artifacts: Vec::new(),
            total_reclaimable_bytes: 0,
        },
        |d| d.analyze(),
    )
}

/// `docker builder prune` (unused build cache).
#[tauri::command(async)]
pub fn docker_prune_build_cache() -> tabibu_docker::ActionOutcome {
    with_docker(tabibu_docker::Docker::prune_build_cache)
}

/// `docker image prune -a` (every image not used by a container).
#[tauri::command(async)]
pub fn docker_prune_images() -> tabibu_docker::ActionOutcome {
    with_docker(tabibu_docker::Docker::prune_images)
}

/// `docker container prune` (all stopped containers).
#[tauri::command(async)]
pub fn docker_prune_containers() -> tabibu_docker::ActionOutcome {
    with_docker(tabibu_docker::Docker::prune_containers)
}

/// `docker volume prune` (unused anonymous volumes only). ⚠ Volumes may hold
/// persistent data — the UI confirms this hard before calling.
#[tauri::command(async)]
pub fn docker_prune_volumes() -> tabibu_docker::ActionOutcome {
    with_docker(tabibu_docker::Docker::prune_volumes)
}

// ---------------------------------------------------------------------
// Network (tray popover): live throughput + on-demand connection test
// ---------------------------------------------------------------------

/// Persistent throughput sampler. Rates are a delta between calls, so the state
/// must survive across the popover's polls (same pattern as the CPU samplers).
static NET_SAMPLER: LazyLock<Mutex<tabibu_net::NetSampler>> =
    LazyLock::new(|| Mutex::new(tabibu_net::NetSampler::new()));

/// Live download/upload rate + cumulative totals for the popover's Network card.
/// Cheap and local (sysinfo counters) — safe to poll on the popover cadence.
#[tauri::command(async)]
pub fn network_sample() -> tabibu_net::Throughput {
    NET_SAMPLER
        .lock()
        .map_or_else(|_| poisoned_throughput(), |mut s| s.sample())
}

/// On-demand connection test: Wi-Fi signal strength + packet loss + latency.
/// Runs `system_profiler` and an outward `ping` — only invoked when the user
/// clicks "Test Connection", never on a timer. `1.1.1.1` is a public resolver
/// (IP literal, so no name-lookup dependency skews the result).
#[tauri::command(async)]
pub fn connection_test() -> tabibu_net::ConnectionTest {
    tabibu_net::connection_test("1.1.1.1")
}

/// A poisoned throughput lock is not worth crashing the popover over — report
/// zeros (the card just shows "—" that tick).
fn poisoned_throughput() -> tabibu_net::Throughput {
    tabibu_net::Throughput {
        down_bps: 0,
        up_bps: 0,
        total_down_bytes: 0,
        total_up_bytes: 0,
    }
}

// ---------------------------------------------------------------------
// Salama — LOCAL privacy (live now): exposure readout + encrypted DNS.
// The IP-hiding VPN is server-dependent and deferred (see tabibu-route).
// ---------------------------------------------------------------------

/// Live privacy status: public IP / relay state + current DNS posture.
#[tauri::command(async)]
pub fn salama_status() -> tabibu_salama::PrivacyStatus {
    tabibu_salama::status()
}

// ---------------------------------------------------------------------
// Salama ENGINE — the WARP-style local encrypted-DNS resolver (tabibu-dohd).
// Fully in-app: one admin prompt installs a root LaunchDaemon that forwards
// 127.0.0.1:53 → DoH, then points system DNS at it. No System Settings.
// ---------------------------------------------------------------------

const DOHD_PLIST: &str = "/Library/LaunchDaemons/xr.seede.tabibu.dohd.plist";

#[derive(Serialize)]
pub struct EngineStatus {
    /// The Salama resolver LaunchDaemon is installed.
    pub installed: bool,
}

/// Whether Salama's own resolver is currently installed.
#[tauri::command(async)]
pub fn salama_engine_status() -> EngineStatus {
    EngineStatus {
        installed: std::path::Path::new(DOHD_PLIST).exists(),
    }
}

/// Turn Salama ON: install the resolver daemon + point system DNS at it, in one
/// admin prompt. Safety: the install script verifies the resolver actually
/// answers BEFORE switching DNS, and rolls itself back if it doesn't — so a
/// failure can never leave the machine with broken DNS.
#[tauri::command(async)]
pub fn salama_engine_on(app: tauri::AppHandle, provider: String) -> Result<(), String> {
    use tauri::Manager;
    let bin = app
        .path()
        .resource_dir()
        .map_err(|e| e.to_string())?
        .join("tabibu-dohd");
    if !bin.exists() {
        return Err("Salama resolver is missing from the app bundle.".into());
    }
    let doh = tabibu_salama::provider_doh_url(&provider);
    run_admin_shell(&install_script(&bin.to_string_lossy(), doh))
}

/// Turn Salama OFF / fully remove: restore system DNS FIRST, then unload and
/// delete the daemon. Idempotent and safe to run when nothing is installed.
#[tauri::command(async)]
pub fn salama_engine_off() -> Result<(), String> {
    run_admin_shell(UNINSTALL_SCRIPT)
}

/// sh single-quote a value so an embedded path/URL can't break out of the script.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Run a shell script as root via one `osascript` admin prompt. The script is
/// passed INLINE (no temp file), which avoids a TOCTOU race on a root-run file.
fn run_admin_shell(script: &str) -> Result<(), String> {
    // AppleScript string escaping: backslashes then double-quotes.
    let escaped = script.replace('\\', "\\\\").replace('"', "\\\"");
    let osa = format!("do shell script \"{escaped}\" with administrator privileges");
    let out = std::process::Command::new("/usr/bin/osascript")
        .args(["-e", &osa])
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        // -128 = user cancelled the admin prompt; report it plainly.
        if err.contains("-128") {
            Err("Cancelled.".into())
        } else {
            Err(err.trim().to_string())
        }
    }
}

/// Build the install script: copy the daemon, write the KeepAlive plist,
/// bootstrap it, VERIFY it resolves, then (only then) switch system DNS.
fn install_script(bin: &str, doh: &str) -> String {
    format!(
        r#"set -e
SRC={src}
DOH={doh}
DEST=/usr/local/libexec/tabibu-dohd
PLIST={plist}
/bin/mkdir -p /usr/local/libexec
/bin/cp "$SRC" "$DEST"
/usr/sbin/chown root:wheel "$DEST"
/bin/chmod 755 "$DEST"
/usr/bin/printf '%s\n' \
 '<?xml version="1.0" encoding="UTF-8"?>' \
 '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">' \
 '<plist version="1.0"><dict>' \
 '<key>Label</key><string>xr.seede.tabibu.dohd</string>' \
 '<key>ProgramArguments</key><array>' \
 "<string>$DEST</string><string>53</string><string>$DOH</string>" \
 '</array>' \
 '<key>RunAtLoad</key><true/>' \
 '<key>KeepAlive</key><true/>' \
 '</dict></plist>' > "$PLIST"
/usr/sbin/chown root:wheel "$PLIST"
/bin/chmod 644 "$PLIST"
/bin/launchctl bootout system "$PLIST" 2>/dev/null || true
/bin/launchctl bootstrap system "$PLIST"
ok=0; i=0
while [ $i -lt 12 ]; do
  if /usr/bin/dig +time=1 +tries=1 +short @127.0.0.1 example.com >/dev/null 2>&1; then ok=1; break; fi
  /bin/sleep 0.5; i=$((i+1))
done
if [ "$ok" != 1 ]; then
  /bin/launchctl bootout system "$PLIST" 2>/dev/null || true
  /bin/rm -f "$PLIST" "$DEST"
  echo 'Salama resolver did not start (port 53 may be in use). No changes made.' >&2
  exit 1
fi
/usr/sbin/networksetup -listallnetworkservices | while IFS= read -r svc; do
  case "$svc" in ''|\**|'An asterisk'*) continue;; esac
  /usr/sbin/networksetup -setdnsservers "$svc" 127.0.0.1 || true
done
exit 0
"#,
        src = sh_quote(bin),
        doh = sh_quote(doh),
        plist = DOHD_PLIST,
    )
}

/// Restore DNS first, then remove the daemon. Idempotent.
const UNINSTALL_SCRIPT: &str = r#"PLIST=/Library/LaunchDaemons/xr.seede.tabibu.dohd.plist
DEST=/usr/local/libexec/tabibu-dohd
/usr/sbin/networksetup -listallnetworkservices | while IFS= read -r svc; do
  case "$svc" in ''|\**|'An asterisk'*) continue;; esac
  /usr/sbin/networksetup -setdnsservers "$svc" empty || true
done
/bin/launchctl bootout system "$PLIST" 2>/dev/null || true
/bin/rm -f "$PLIST" "$DEST"
exit 0
"#;

// ---------------------------------------------------------------------------
// Salama VPN — multi-server config, provisioning, and a default-route-SAFE
// connect (establishes the tunnel + verifies the handshake with host routes
// only; never touches the system default route, so a failed/partial connect
// can't strand your internet). Engine: the bundled `tabibu-wg` (boringtun-cli).
// ---------------------------------------------------------------------------

fn vpn_config_dir(app: &tauri::AppHandle) -> std::path::PathBuf {
    use tauri::Manager;
    app.path().app_config_dir().unwrap_or_default()
}

/// Per-server provisioned client config (0600).
fn vpn_conf_path(app: &tauri::AppHandle, id: &str) -> std::path::PathBuf {
    vpn_config_dir(app).join("vpn").join(format!("{id}.conf"))
}

/// A server id names a file (`<id>.conf`), so restrict it to a safe slug — it can
/// never traverse out of the vpn dir. The UI already slugifies; this is the Rust
/// trust boundary (a compromised renderer can't path-traverse a write/delete).
fn valid_server_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

#[tauri::command(async)]
pub fn vpn_config() -> crate::vpn::VpnConfig {
    crate::vpn::snapshot()
}

#[tauri::command(async)]
pub fn vpn_upsert_server(
    app: tauri::AppHandle,
    id: String,
    name: String,
    url: String,
) -> crate::vpn::VpnConfig {
    if !valid_server_id(&id) {
        return crate::vpn::snapshot(); // reject unsafe ids rather than store them
    }
    crate::vpn::update(&vpn_config_dir(&app), |c| {
        crate::vpn::upsert(c, crate::vpn::VpnServer { id, name, url });
    })
}

#[tauri::command(async)]
pub fn vpn_remove_server(app: tauri::AppHandle, id: String) -> crate::vpn::VpnConfig {
    if !valid_server_id(&id) {
        return crate::vpn::snapshot(); // never build a path from an unsafe id
    }
    let _ = std::fs::remove_file(vpn_conf_path(&app, &id)); // drop its provisioned config
    crate::vpn::update(&vpn_config_dir(&app), |c| crate::vpn::remove(c, &id))
}

#[tauri::command(async)]
pub fn vpn_set_active(app: tauri::AppHandle, id: String) -> crate::vpn::VpnConfig {
    crate::vpn::update(&vpn_config_dir(&app), |c| crate::vpn::set_active(c, &id))
}

/// UI state: is the active server provisioned, is the tunnel up. (The active id
/// itself comes from `vpn_config`, so it isn't duplicated here.)
#[derive(Serialize)]
pub struct VpnState {
    pub provisioned: bool,
    pub connected: bool,
}

#[tauri::command(async)]
pub fn vpn_state(app: tauri::AppHandle) -> VpnState {
    let cfg = crate::vpn::snapshot();
    let provisioned = cfg
        .active
        .as_ref()
        .is_some_and(|id| vpn_conf_path(&app, id).exists());
    // Connected = the marker is present AND the engine is actually alive (a
    // crashed engine would otherwise read as connected until reboot clears
    // /var/run). pgrep sees processes across users.
    let engine_alive = std::process::Command::new("/usr/bin/pgrep")
        .args(["-f", "tabibu-wg"])
        .output()
        .is_ok_and(|o| o.status.success());
    VpnState {
        provisioned,
        connected: engine_alive && std::path::Path::new("/var/run/tabibu-vpn.on").exists(),
    }
}

/// Find a wg-easy client's id by name in the `/api/wireguard/client` JSON array.
fn find_client_id(list_json: &str, name: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(list_json).ok()?;
    v.as_array()?.iter().find_map(|c| {
        (c.get("name")?.as_str()? == name)
            .then(|| c.get("id").and_then(|i| i.as_str()).map(str::to_owned))
            .flatten()
    })
}

/// Provision (or refresh) a client config for a server from its salama-web admin
/// API. `password` is used transiently (never stored). Writes a 0600 `<id>.conf`.
#[tauri::command(async)]
pub fn vpn_provision(app: tauri::AppHandle, id: String, password: String) -> Result<(), String> {
    if !valid_server_id(&id) {
        return Err("Invalid server id.".into());
    }
    // A private 0700 dir holds the session cookie jar, so even a world-readable
    // jar file inside is unreadable to other local users. Removed on EVERY exit
    // path (including the `?` early returns inside the inner fn).
    let dir = std::env::temp_dir().join(format!("tabibu-wg-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    let result = vpn_provision_inner(&app, &id, &password, &dir);
    let _ = std::fs::remove_dir_all(&dir);
    result
}

fn vpn_provision_inner(
    app: &tauri::AppHandle,
    id: &str,
    password: &str,
    dir: &std::path::Path,
) -> Result<(), String> {
    let cfg = crate::vpn::snapshot();
    let base = cfg
        .servers
        .iter()
        .find(|s| s.id == id)
        .ok_or("unknown server")?
        .url
        .trim_end_matches('/')
        .to_owned();
    let jar_s = dir.join("jar").to_string_lossy().into_owned();
    let curl = |args: &[&str]| -> Result<std::process::Output, String> {
        std::process::Command::new("/usr/bin/curl")
            .args(args)
            .output()
            .map_err(|e| e.to_string())
    };
    // Login: the password goes on STDIN (`--data @-`), never in argv where
    // same-user `ps -ww` could read it.
    let body = serde_json::json!({ "password": password }).to_string();
    let login = {
        use std::io::Write;
        let mut child = std::process::Command::new("/usr/bin/curl")
            .args([
                "-fsS",
                "-c",
                &jar_s,
                "-X",
                "POST",
                "-H",
                "Content-Type: application/json",
                "--data",
                "@-",
                &format!("{base}/api/session"),
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
        child
            .stdin
            .take()
            .ok_or("curl stdin unavailable")?
            .write_all(body.as_bytes())
            .map_err(|e| e.to_string())?;
        child.wait_with_output().map_err(|e| e.to_string())?
    };
    if !login.status.success() {
        return Err("Login failed — check the server URL and admin password.".into());
    }
    let name = "tabibu";
    let list_url = format!("{base}/api/wireguard/client");
    let list = curl(&["-fsS", "-b", &jar_s, &list_url])?;
    let cid = match find_client_id(&String::from_utf8_lossy(&list.stdout), name) {
        Some(c) => c,
        None => {
            let create_body = serde_json::json!({ "name": name }).to_string();
            curl(&[
                "-fsS",
                "-b",
                &jar_s,
                "-X",
                "POST",
                "-H",
                "Content-Type: application/json",
                "-d",
                &create_body,
                &list_url,
            ])?;
            let list2 = curl(&["-fsS", "-b", &jar_s, &list_url])?;
            find_client_id(&String::from_utf8_lossy(&list2.stdout), name)
                .ok_or("Could not create a client on the server.")?
        }
    };
    let conf = curl(&[
        "-fsS",
        "-b",
        &jar_s,
        &format!("{base}/api/wireguard/client/{cid}/configuration"),
    ])?;
    let text = String::from_utf8_lossy(&conf.stdout);
    if !conf.status.success() || !text.contains("[Interface]") {
        return Err("Server returned an invalid client config.".into());
    }
    let path = vpn_conf_path(app, id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, text.as_bytes()).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Connect (full tunnel): start the bundled engine, configure the peer, VERIFY
/// the handshake, then route all traffic through the tunnel via the split-default
/// pair (never replacing the real default route) + point DNS at the tunnel
/// resolver. Verify-before-flip means a failed handshake changes nothing; the
/// split-default design means an engine crash fails OPEN (kernel drops the
/// interface routes, the real default resumes) rather than stranding the Mac.
#[tauri::command(async)]
pub fn vpn_connect(app: tauri::AppHandle) -> Result<(), String> {
    use tauri::Manager;
    let cfg = crate::vpn::snapshot();
    let id = cfg.active.ok_or("No active server selected.")?;
    let conf = vpn_conf_path(&app, &id);
    if !conf.exists() {
        return Err("This server isn't provisioned yet — provision it first.".into());
    }
    let wg = app
        .path()
        .resource_dir()
        .map_err(|e| e.to_string())?
        .join("tabibu-wg");
    if !wg.exists() {
        return Err("VPN engine (tabibu-wg) is missing from the app bundle.".into());
    }
    run_admin_shell(&vpn_connect_script(
        &wg.to_string_lossy(),
        &conf.to_string_lossy(),
    ))
}

/// Disconnect: kill the engine (its utun + host routes vanish with it) and clear
/// the marker. Trivial + safe because connect never changed the default route.
#[tauri::command(async)]
pub fn vpn_disconnect() -> Result<(), String> {
    run_admin_shell(VPN_DISCONNECT_SCRIPT)
}

/// Root script: bring up boringtun on an auto-assigned utun, configure the peer
/// over its UAPI socket (keys converted base64→hex with base tools), pin the
/// endpoint via the real gateway, then ping the server's tunnel IP to confirm a
/// live handshake — using host routes only, never the default route. A `trap`
/// tears the engine + endpoint route back down on ANY failure before success,
/// so a partial connect can't leave a live-but-untracked tunnel. POSIX sh only
/// (osascript runs /bin/sh) — no bashisms.
fn vpn_connect_script(wg: &str, conf: &str) -> String {
    format!(
        r#"set -e
WG={wg}
CONF={conf}
PRIV=$(/usr/bin/awk -F ' = ' '/^PrivateKey/{{print $2}}' "$CONF")
ADDR=$(/usr/bin/awk -F ' = ' '/^Address/{{print $2}}' "$CONF" | /usr/bin/cut -d, -f1)
PUB=$(/usr/bin/awk -F ' = ' '/^PublicKey/{{print $2}}' "$CONF")
PSK=$(/usr/bin/awk -F ' = ' '/^PresharedKey/{{print $2}}' "$CONF")
EP=$(/usr/bin/awk -F ' = ' '/^Endpoint/{{print $2}}' "$CONF")
EPH=${{EP%:*}}; EPP=${{EP##*:}}
EPIP=$(/usr/bin/dig +short "$EPH" | /usr/bin/tail -1); [ -z "$EPIP" ] && EPIP="$EPH"
IPONLY=${{ADDR%/*}}
GWVPN=$(echo "$IPONLY" | /usr/bin/awk -F. '{{print $1"."$2"."$3".1"}}')
hex() {{ printf '%s' "$1" | /usr/bin/base64 -d | /usr/bin/xxd -p -c 32; }}
/bin/mkdir -p /var/run/wireguard
"$WG" utun
SOCK=""; i=0
while [ $i -lt 25 ]; do
  SOCK=$(/bin/ls -t /var/run/wireguard/*.sock 2>/dev/null | /usr/bin/head -1)
  [ -n "$SOCK" ] && break; i=$((i+1)); /bin/sleep 0.2
done
[ -z "$SOCK" ] && {{ echo "VPN engine failed to start"; exit 1; }}
IF=$(/usr/bin/basename "$SOCK" .sock)
GW=$(/sbin/route -n get default 2>/dev/null | /usr/bin/awk '/gateway/{{print $2}}')
OK=0
cleanup() {{ [ "$OK" = 1 ] && return; /usr/bin/pkill -f {wg} 2>/dev/null || true; [ -n "$EPIP" ] && /sbin/route delete -host "$EPIP" >/dev/null 2>&1 || true; }}
trap cleanup EXIT
PSKLINE=""
[ -n "$PSK" ] && PSKLINE="preshared_key=$(hex "$PSK")
"
printf 'set=1\nprivate_key=%s\nreplace_peers=true\npublic_key=%s\n%sendpoint=%s:%s\npersistent_keepalive_interval=25\nreplace_allowed_ips=true\nallowed_ip=0.0.0.0/0\n\n' \
  "$(hex "$PRIV")" "$(hex "$PUB")" "$PSKLINE" "$EPIP" "$EPP" | /usr/bin/nc -U "$SOCK"
/sbin/ifconfig "$IF" inet "$IPONLY" "$IPONLY" up
[ -n "$GW" ] && /sbin/route add -host "$EPIP" "$GW" >/dev/null 2>&1 || true
/sbin/route add -host "$GWVPN" -interface "$IF" >/dev/null 2>&1 || true
ok=0; n=0
while [ $n -lt 8 ]; do /sbin/ping -c1 -t1 "$GWVPN" >/dev/null 2>&1 && {{ ok=1; break; }}; n=$((n+1)); /bin/sleep 0.5; done
[ "$ok" != 1 ] && {{ echo "Tunnel did not establish (handshake failed) — internet untouched."; exit 1; }}
OK=1
# --- FULL TUNNEL (only after the handshake verified) ---
# Route ALL traffic through the tunnel with the split-default pair, WITHOUT
# replacing the real default route. If the engine dies, the kernel drops these
# interface-scoped routes and the real default resumes on its own (fail-open —
# internet comes back), so no resident kill-switch daemon is needed.
/sbin/route add -net 0.0.0.0/1 -interface "$IF" >/dev/null 2>&1 || true
/sbin/route add -net 128.0.0.0/1 -interface "$IF" >/dev/null 2>&1 || true
# Point DNS at the tunnel's resolver (stops DNS leaking to the ISP), saving each
# service's CURRENT setting so disconnect restores it exactly (incl. Salama's).
DNS=$(/usr/bin/awk -F ' = ' '/^DNS/{{print $2}}' "$CONF" | /usr/bin/cut -d, -f1)
: > /var/run/tabibu-vpn.dns
/usr/sbin/networksetup -listallnetworkservices 2>/dev/null | while IFS= read -r svc; do
  case "$svc" in ''|\**|"An asterisk"*) continue;; esac
  cur=$(/usr/sbin/networksetup -getdnsservers "$svc" 2>/dev/null)
  case "$cur" in "There aren't any"*) cur="empty";; *) cur=$(echo "$cur" | /usr/bin/tr '\n' ' ');; esac
  printf '%s\t%s\n' "$svc" "$cur" >> /var/run/tabibu-vpn.dns
  [ -n "$DNS" ] && /usr/sbin/networksetup -setdnsservers "$svc" "$DNS" >/dev/null 2>&1 || true
done
printf '%s\n%s\n' "$IF" "$EPIP" > /var/run/tabibu-vpn.on
/bin/chmod 644 /var/run/tabibu-vpn.on
echo "Connected — all traffic via $IF"
"#,
        wg = sh_quote(wg),
        conf = sh_quote(conf),
    )
}

/// Tear the full tunnel down and restore everything: DNS FIRST (from the saved
/// manifest, exactly as it was — including Salama's resolver), then the
/// split-default + endpoint routes, then kill the engine and clear the markers.
/// Idempotent and safe when nothing is up. Restore-first mirrors the DNS engine.
const VPN_DISCONNECT_SCRIPT: &str = r#"if [ -f /var/run/tabibu-vpn.dns ]; then
  while IFS="$(printf '\t')" read -r svc dns; do
    [ -z "$svc" ] && continue
    /usr/sbin/networksetup -setdnsservers "$svc" $dns >/dev/null 2>&1 || true
  done < /var/run/tabibu-vpn.dns
  /bin/rm -f /var/run/tabibu-vpn.dns
fi
/sbin/route delete -net 0.0.0.0/1 >/dev/null 2>&1 || true
/sbin/route delete -net 128.0.0.0/1 >/dev/null 2>&1 || true
EPIP=""
[ -f /var/run/tabibu-vpn.on ] && EPIP=$(/usr/bin/sed -n 2p /var/run/tabibu-vpn.on)
[ -n "$EPIP" ] && /sbin/route delete -host "$EPIP" >/dev/null 2>&1 || true
/usr/bin/pkill -f 'tabibu-wg' 2>/dev/null || true
/bin/rm -f /var/run/tabibu-vpn.on
exit 0
"#;

// ---------------------------------------------------------------------
// Menu bar app lifecycle (tray popover + Settings)
// ---------------------------------------------------------------------

/// Whether Tabibu starts at login (macOS Launch Agent, autostart plugin).
#[tauri::command(async)]
pub fn launch_at_login(app: tauri::AppHandle) -> Result<bool, String> {
    use tauri_plugin_autostart::ManagerExt;
    app.autolaunch().is_enabled().map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub fn set_launch_at_login(app: tauri::AppHandle, on: bool) -> Result<(), String> {
    use tauri_plugin_autostart::ManagerExt;
    let launcher = app.autolaunch();
    if on {
        launcher.enable()
    } else {
        launcher.disable()
    }
    .map_err(|e| e.to_string())
}

/// Show + focus the dashboard (used by the tray popover's buttons),
/// optionally navigating it to a view id like "settings" or "memory".
#[tauri::command]
pub fn show_main_window(app: tauri::AppHandle, view: Option<String>) {
    crate::tray::show_main(&app, view.as_deref());
}

/// Full exit — the menu bar app otherwise survives window closes.
#[tauri::command]
pub fn quit_app(app: tauri::AppHandle) {
    app.exit(0);
}

/// Expand/collapse the tray popover for a component detail panel.
#[tauri::command]
pub fn popover_detail(app: tauri::AppHandle, open: bool) {
    crate::tray::set_popover_detail(&app, open);
}

/// Size the tray popover to fit its content (height in logical px, measured by
/// the webview). Keeps nothing clipped regardless of the webview's font metrics.
#[tauri::command]
pub fn popover_resize(app: tauri::AppHandle, height: f64) {
    crate::tray::set_popover_height(&app, height);
}

/// Seconds since boot (the CPU detail panel's uptime card).
#[tauri::command(async)]
pub fn uptime_secs() -> u64 {
    sysinfo::System::uptime()
}

#[cfg(test)]
mod tests {
    use super::{find_client_id, valid_server_id};

    #[test]
    fn valid_server_id_blocks_path_traversal() {
        assert!(valid_server_id("home"));
        assert!(valid_server_id("my-vpn-2"));
        // Anything that could escape the vpn dir or name a weird file is rejected.
        assert!(!valid_server_id("../etc/passwd"));
        assert!(!valid_server_id("a/b"));
        assert!(!valid_server_id("a.b"));
        assert!(!valid_server_id("A")); // upper-case not in the slug set
        assert!(!valid_server_id(""));
        assert!(!valid_server_id(&"x".repeat(65)));
    }

    #[test]
    fn find_client_id_matches_by_name() {
        let json = r#"[
            {"id":"aaa-111","name":"other"},
            {"id":"bbb-222","name":"tabibu"},
            {"id":"ccc-333","name":"laptop"}
        ]"#;
        assert_eq!(find_client_id(json, "tabibu").as_deref(), Some("bbb-222"));
        assert_eq!(find_client_id(json, "ghost"), None);
        // Malformed / empty inputs are None, never a panic.
        assert_eq!(find_client_id("not json", "tabibu"), None);
        assert_eq!(find_client_id("[]", "tabibu"), None);
    }
}
