//! Platform facts the UI needs, gathered without Objective-C bindings:
//! home directory, Full Disk Access probe, and the bundle IDs of running apps
//! (the running-process guard input). Mirrors what the Swift `AppModel` did,
//! in pure Rust + the `plist` crate.

use serde::Serialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};
use tabibu_engine::ScanCtx;

#[derive(Debug, Clone, Serialize)]
pub struct SystemInfo {
    pub home: String,
    pub full_disk_access: bool,
}

#[must_use]
pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// TCC offers no query API; the honest probe is reading a path that is gated
/// behind Full Disk Access and that we never otherwise touch. Readable ⇒ FDA.
#[must_use]
pub fn has_full_disk_access(home: &Path) -> bool {
    std::fs::read_dir(home.join("Library/Safari")).is_ok()
}

#[must_use]
pub fn system_info() -> SystemInfo {
    let home = home_dir();
    SystemInfo {
        full_disk_access: has_full_disk_access(&home),
        home: home.to_string_lossy().into_owned(),
    }
}

/// Cached `running_bundle_ids` — the full process+plist sweep is expensive and
/// the set changes slowly, so we reuse it across commands fired close together
/// (a Smart Scan → Security → Leftovers burst would otherwise sweep 3×).
type BundleIdCache = Mutex<Option<(Instant, HashSet<String>)>>;
static BUNDLE_ID_CACHE: LazyLock<BundleIdCache> = LazyLock::new(|| Mutex::new(None));
const BUNDLE_ID_TTL: Duration = Duration::from_secs(15);

/// Bundle IDs of currently running apps (cached for [`BUNDLE_ID_TTL`]).
#[must_use]
pub fn running_bundle_ids() -> HashSet<String> {
    let mut guard = BUNDLE_ID_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((at, ids)) = guard.as_ref() {
        if at.elapsed() < BUNDLE_ID_TTL {
            return ids.clone();
        }
    }
    let ids = compute_running_bundle_ids();
    *guard = Some((Instant::now(), ids.clone()));
    ids
}

/// Derived from each process's executable path: walk up to the enclosing
/// `*.app` bundle and read `Contents/Info.plist`'s `CFBundleIdentifier`.
/// Best-effort — processes not inside an `.app` contribute no ID.
fn compute_running_bundle_ids() -> HashSet<String> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_exe(UpdateKind::Always),
    );

    let mut ids = HashSet::new();
    let mut seen_bundles = HashSet::new();
    for proc in sys.processes().values() {
        let Some(exe) = proc.exe() else { continue };
        let Some(bundle) = enclosing_app_bundle(exe) else {
            continue;
        };
        if !seen_bundles.insert(bundle.clone()) {
            continue; // already read this bundle's plist
        }
        if let Some(id) = bundle_identifier(&bundle) {
            ids.insert(id);
        }
    }
    ids
}

/// Given `/Applications/Foo.app/Contents/MacOS/Foo`, return `…/Foo.app`.
fn enclosing_app_bundle(exe: &Path) -> Option<PathBuf> {
    let mut cur = exe;
    while let Some(parent) = cur.parent() {
        if parent.extension().is_some_and(|e| e == "app") {
            return Some(parent.to_path_buf());
        }
        cur = parent;
    }
    None
}

fn bundle_identifier(app: &Path) -> Option<String> {
    let plist_path = app.join("Contents/Info.plist");
    let value = plist::Value::from_file(plist_path).ok()?;
    value
        .as_dictionary()
        .and_then(|d| d.get("CFBundleIdentifier"))
        .and_then(plist::Value::as_string)
        .map(str::to_owned)
}

/// Build the standard junk-scan context (home roots + running-process guard).
#[must_use]
pub fn default_scan_ctx(extra_roots: &[String]) -> ScanCtx {
    let home = home_dir();
    let mut allowed_roots: Vec<PathBuf> = [
        ".Trash",
        "Library/Caches",
        "Library/Logs",
        "Library/Developer/Xcode/DerivedData",
        "Library/Developer/CoreSimulator/Caches",
        ".npm",
        ".cargo/registry/cache",
        "Downloads",
    ]
    .iter()
    .map(|r| home.join(r))
    .collect();
    allowed_roots.push(std::env::temp_dir());
    // Per-volume trashes only — the SPECIFIC `/Volumes/*/.Trashes/<uid>` dirs,
    // NOT all of `/Volumes`. Whitelisting the whole mount root would let
    // reclaim trash arbitrary external-drive files (the denylist has no
    // /Volumes coverage); narrowing to the trash dirs keeps the boundary tight.
    allowed_roots.extend(per_volume_trash_roots());
    allowed_roots.extend(extra_roots.iter().map(PathBuf::from));

    ScanCtx {
        home: home.clone(),
        allowed_roots,
        running_bundle_ids: running_bundle_ids(),
        full_disk_access: has_full_disk_access(&home),
    }
}

/// Per-volume Trash directories to add to the reclaim allowed-roots. Delegates
/// to `tabibu_junk::per_volume_trash_dirs` — the SAME derivation the Trash
/// scanner uses — so the scanned paths and the reclaim guard never drift.
#[must_use]
pub fn per_volume_trash_roots() -> Vec<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let home = home_dir();
    let Ok(uid) = std::fs::metadata(&home).map(|m| m.uid()) else {
        return Vec::new();
    };
    tabibu_junk::per_volume_trash_dirs(Path::new("/Volumes"), uid)
}

#[must_use]
pub fn undo_dir() -> String {
    home_dir()
        .join("Library/Application Support/Tabibu/undo")
        .to_string_lossy()
        .into_owned()
}

#[must_use]
pub fn telemetry_dir() -> PathBuf {
    home_dir().join("Library/Application Support/Tabibu/telemetry")
}

// ---------------------------------------------------------------------------
// Battery — parsed from `pmset` (charge/state) and `ioreg` (health/cycles),
// no IOKit bindings. Every field is optional: desktops have no battery and
// some keys vary by model. Only what actually reads is reported.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Default)]
pub struct BatteryInfo {
    pub has_battery: bool,
    pub charge_percent: Option<u32>,
    pub state: Option<String>,
    pub time_remaining: Option<String>,
    pub cycle_count: Option<i64>,
    pub health_percent: Option<u32>,
    pub condition: Option<String>,
}

#[must_use]
pub fn battery_info() -> BatteryInfo {
    let mut info = BatteryInfo::default();
    parse_pmset(&mut info);
    parse_ioreg(&mut info);
    info
}

fn run(cmd: &str, args: &[&str]) -> Option<Vec<u8>> {
    let out = std::process::Command::new(cmd).args(args).output().ok()?;
    out.status.success().then_some(out.stdout)
}

fn parse_pmset(info: &mut BatteryInfo) {
    let Some(bytes) = run("/usr/bin/pmset", &["-g", "batt"]) else {
        return;
    };
    let text = String::from_utf8_lossy(&bytes);
    if !text.contains("InternalBattery") {
        return;
    }
    info.has_battery = true;
    // e.g. "-InternalBattery-0 (id=…)\t72%; discharging; 4:04 remaining present: true".
    // Whitespace is mixed tabs/spaces and "remaining" sits mid-string, so we
    // tokenize on all whitespace and `;` and classify each token.
    for token in text.split([';', ' ', '\t', '\n']) {
        let t = token.trim();
        if t.is_empty() {
            continue;
        }
        if let Some(pct) = t.strip_suffix('%') {
            if let Ok(v) = pct.parse::<u32>() {
                info.charge_percent = Some(v);
            }
        } else if matches!(t, "charging" | "discharging" | "charged") {
            info.state = Some(t.to_string());
        } else if t.contains(':')
            && t.chars().all(|c| c.is_ascii_digit() || c == ':')
            && t != "0:00"
        {
            info.time_remaining = Some(format!("{t} remaining"));
        }
    }
}

fn parse_ioreg(info: &mut BatteryInfo) {
    let Some(bytes) = run("/usr/sbin/ioreg", &["-r", "-c", "AppleSmartBattery", "-a"]) else {
        return;
    };
    let Ok(value) = plist::Value::from_reader(std::io::Cursor::new(bytes)) else {
        return;
    };
    // `ioreg -a` yields a plist array of matching services; take the first.
    let dict = value
        .as_array()
        .and_then(|a| a.first())
        .and_then(plist::Value::as_dictionary);
    let Some(dict) = dict else { return };
    info.has_battery = true;

    let int = |k: &str| dict.get(k).and_then(plist::Value::as_signed_integer);
    if let Some(c) = int("CycleCount") {
        info.cycle_count = Some(c);
    }
    if let (Some(design), Some(full)) = (
        int("DesignCapacity"),
        int("AppleRawMaxCapacity").or_else(|| int("MaxCapacity")),
    ) {
        if design > 0 {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let pct = ((full as f64 / design as f64) * 100.0).round() as u32;
            info.health_percent = Some(pct);
        }
    }
    if let Some(fail) = int("PermanentFailureStatus") {
        info.condition = Some(
            if fail == 0 {
                "Normal"
            } else {
                "Service recommended"
            }
            .into(),
        );
    }
}

// ---------------------------------------------------------------------------
// Startup items — launch agents/daemons that run at login. Read-only: we
// surface them with their program path; disabling is the user's call in
// System Settings (we never fake a toggle that doesn't work).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct StartupItem {
    pub path: String,
    pub label: String,
    pub program: String,
    pub scope: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct StartupReport {
    pub items: Vec<StartupItem>,
    /// True if a system directory was unreadable (needs Full Disk Access).
    pub partial: bool,
}

#[must_use]
pub fn startup_items() -> StartupReport {
    let home = home_dir();
    let dirs: [(PathBuf, &str); 3] = [
        (home.join("Library/LaunchAgents"), "User"),
        (
            PathBuf::from("/Library/LaunchAgents"),
            "System (LaunchAgents)",
        ),
        (
            PathBuf::from("/Library/LaunchDaemons"),
            "System (LaunchDaemons)",
        ),
    ];
    let mut items = Vec::new();
    let mut partial = false;
    for (dir, scope) in &dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            if *scope != "User" {
                partial = true;
            }
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "plist") {
                continue;
            }
            let (label, program) = parse_launchd(&path);
            items.push(StartupItem {
                path: path.to_string_lossy().into_owned(),
                label,
                program,
                scope: (*scope).to_string(),
            });
        }
    }
    items.sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()));
    StartupReport { items, partial }
}

fn parse_launchd(path: &Path) -> (String, String) {
    let fallback = || {
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let Ok(value) = plist::Value::from_file(path) else {
        return (fallback(), "—".into());
    };
    let Some(dict) = value.as_dictionary() else {
        return (fallback(), "—".into());
    };
    let label = dict
        .get("Label")
        .and_then(plist::Value::as_string)
        .map_or_else(fallback, str::to_owned);
    let program = dict
        .get("Program")
        .and_then(plist::Value::as_string)
        .map(str::to_owned)
        .or_else(|| {
            dict.get("ProgramArguments")
                .and_then(plist::Value::as_array)
                .and_then(|a| a.first())
                .and_then(plist::Value::as_string)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "—".into());
    (label, program)
}
