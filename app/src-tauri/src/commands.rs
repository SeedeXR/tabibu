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
/// Persistent monitor sampler (CPU deltas need a long-lived `System`).
static SAMPLER: LazyLock<Mutex<Option<Sampler>>> = LazyLock::new(|| Mutex::new(None));

/// Register a fresh synchronous-op cancel token and return a clone to drive the
/// operation. Already-cancelled tokens are pruned so the registry stays bounded.
fn begin_sync_op() -> CancelToken {
    let token = CancelToken::new();
    let mut reg = CURRENT_SYNC
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    reg.retain(|t| !t.is_cancelled());
    reg.push(token.clone());
    token
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
    *CURRENT_SCAN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(cancel.clone());

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
    for item in &mut items {
        item.selected = true;
    }
    let ctx = system::default_scan_ctx(&extra_roots);
    engine_reclaim(&ctx, &items, std::path::Path::new(&system::undo_dir()))
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Space map
// ---------------------------------------------------------------------------

#[tauri::command(async)]
pub fn size_tree(root: String, max_depth: Option<usize>) -> Result<DirNode, String> {
    let cancel = CancelToken::new();
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

#[tauri::command(async)]
pub fn monitor_sample(top_n: usize, by_cpu: bool) -> SystemSample {
    // Poison-tolerant (matches system.rs): a panic in one sampler call must not
    // permanently break every later monitor_sample (and the tray thread).
    let mut guard = SAMPLER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let sampler = guard.get_or_insert_with(Sampler::new);
    let by = if by_cpu { TopBy::Cpu } else { TopBy::Memory };
    sampler.sample(top_n, by)
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
    let _ = std::process::Command::new("/usr/bin/open")
        .arg("-R")
        .arg(path)
        .spawn();
}

#[tauri::command(async)]
pub fn open_url(url: String) {
    let _ = std::process::Command::new("/usr/bin/open").arg(url).spawn();
}

/// Move a path to the Trash via the OS (used by the uninstaller's "also trash
/// the app", which lives outside the engine's user roots).
#[tauri::command(async)]
pub fn trash_path(path: String) -> Result<(), String> {
    trash::delete(&path).map_err(|e| e.to_string())
}

/// Native folder picker, done entirely in Rust (no reliance on the JS dialog
/// global). Returns the chosen path, or null if cancelled.
#[tauri::command(async)]
pub fn pick_folder(app: tauri::AppHandle) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    app.dialog()
        .file()
        .blocking_pick_folder()
        .map(|p| p.to_string())
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
        None => "Nominal",
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
    // Throttle: if the newest sample is < 1h old, update it in place rather
    // than appending (keeps ≤ 1 point/hour and avoids per-tick disk churn).
    match history.last_mut() {
        Some(last) if ts_unix.saturating_sub(last.0) < FREE_SPACE_MIN_SPACING => {
            *last = (ts_unix, free_bytes);
        }
        _ => history.push((ts_unix, free_bytes)),
    }
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
