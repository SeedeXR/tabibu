//! Shared macOS platform facts the UI needs, gathered without Objective-C
//! bindings: home directory, Full Disk Access probe, the bundle IDs of running
//! apps (the running-process guard input), and the standard junk-scan context
//! (reclaim allowed-roots + guard inputs).
//!
//! This is the permanent home for what `app/src-tauri/src/system.rs` grew per
//! app. The egui app depends on it directly; the Tauri app still carries its
//! own copy for now and will be collapsed onto this crate (or deleted with the
//! Tauri shell) once the migration reaches parity.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};
use tabibu_engine::ScanCtx;

#[must_use]
pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// Undo manifest directory for `tabibu_engine::reclaim` (so a Move-to-Trash can
/// be reversed).
#[must_use]
pub fn undo_dir() -> PathBuf {
    home_dir().join("Library/Application Support/Tabibu/undo")
}

/// TCC offers no query API; the honest probe is READING (not just stat-ing) a
/// path gated behind Full Disk Access. A successful read ⇒ FDA.
///
/// We try several gated paths and accept the FIRST that reads — crucially we do
/// NOT treat a *missing* path as "denied" (reading only `~/Library/Safari`
/// would make a user who never ran Safari look permission-less even with FDA
/// granted). The per-user TCC store is listed first because it effectively
/// always exists.
#[must_use]
pub fn has_full_disk_access(home: &Path) -> bool {
    const GATED: &[&str] = &[
        "Library/Application Support/com.apple.TCC/TCC.db", // always present, FDA-gated
        "Library/Safari",
        "Library/Mail",
        "Library/Messages",
    ];
    GATED.iter().any(|rel| readable(&home.join(rel)))
}

/// Whether we can actually read `p`'s contents. `stat` succeeds without FDA, so
/// we must open/list, not just check existence: a directory is listed, a file
/// is opened and one byte read. A missing path or a denial both return `false`
/// (a denial is only conclusive across the whole probe set, not per path).
fn readable(p: &Path) -> bool {
    match std::fs::metadata(p) {
        Ok(m) if m.is_dir() => std::fs::read_dir(p).is_ok(),
        Ok(_) => {
            use std::io::Read;
            std::fs::File::open(p)
                .and_then(|mut f| f.read(&mut [0u8; 1]).map(|_| ()))
                .is_ok()
        }
        Err(_) => false,
    }
}

/// Cached `running_bundle_ids` — the full process+plist sweep is expensive and
/// the set changes slowly, so we reuse it across calls fired close together.
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

/// Build the standard junk-scan context (home roots + running-process guard).
///
/// `allowed_roots` is the exact set `tabibu_engine::reclaim`'s guard permits —
/// every scanner's emitted root must appear here or the guard drops its items.
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
        // Hidden dot-directory caches surfaced by DevCacheScanner (must be
        // allowed roots or the GuardedSink drops their items). Scoped to the
        // exact cache subdir — NOT all of ~/.gradle, which also holds
        // gradle.properties (signing keys) and init scripts.
        ".cache",
        ".gradle/caches",
        "Downloads",
    ]
    .iter()
    .map(|r| home.join(r))
    .collect();
    allowed_roots.push(std::env::temp_dir());
    // Per-volume trashes only — the SPECIFIC `/Volumes/*/.Trashes/<uid>` dirs,
    // NOT all of `/Volumes`.
    allowed_roots.extend(per_volume_trash_roots());
    // Sandboxed-app cache dirs — the EXACT Caches subdirs `ContainerCacheScanner`
    // emits, so reclaim permits those and nothing else under the containers.
    allowed_roots.extend(tabibu_junk::container_cache_roots(&home));
    // Shared world-writable temp dirs (/private/tmp, /private/var/tmp).
    allowed_roots.extend(tabibu_junk::shared_temp_roots());
    // Chromium/Electron cache leaves under ~/Library/Application Support (the
    // EXACT dirs AppSupportCacheScanner emits) + the Go module cache.
    allowed_roots.extend(tabibu_junk::app_support_cache_roots(&home));
    allowed_roots.push(home.join("go/pkg/mod"));
    // System-wide caches + logs that SystemCacheScanner / LogScanner surface
    // (user-owned entries only); reclaim permits exactly these roots.
    allowed_roots.push(PathBuf::from("/Library/Caches"));
    allowed_roots.push(PathBuf::from("/Library/Logs"));
    allowed_roots.push(PathBuf::from("/private/var/log"));
    allowed_roots.extend(extra_roots.iter().map(PathBuf::from));

    ScanCtx {
        home: home.clone(),
        allowed_roots,
        running_bundle_ids: running_bundle_ids(),
        full_disk_access: has_full_disk_access(&home),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn readable_reads_file_and_dir_but_not_missing() {
        let base = std::env::temp_dir().join(format!("tabibu-sys-fda-{}", std::process::id()));
        let dir = base.join("d");
        std::fs::create_dir_all(&dir).unwrap();
        let file = base.join("f.txt");
        std::fs::File::create(&file)
            .unwrap()
            .write_all(b"hi")
            .unwrap();

        assert!(readable(&file), "an existing readable file reads");
        assert!(readable(&dir), "an existing readable directory lists");
        assert!(
            !readable(&base.join("nope")),
            "a missing path is a non-signal"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn default_ctx_includes_core_roots() {
        let ctx = default_scan_ctx(&["/tmp/extra-root".to_string()]);
        let home = home_dir();
        assert!(ctx.allowed_roots.contains(&home.join("Library/Caches")));
        assert!(ctx
            .allowed_roots
            .contains(&PathBuf::from("/Library/Caches")));
        assert!(ctx
            .allowed_roots
            .contains(&PathBuf::from("/tmp/extra-root")));
    }
}
