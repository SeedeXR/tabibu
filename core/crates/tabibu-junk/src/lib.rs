//! tabibu-junk — read-only scanners for reclaimable junk on macOS.
//!
//! Five scanners cover the classic cleanup buckets: Trash contents, per-app
//! user caches (with a running-process guard), developer tool caches,
//! stale temporary files, and old log files. All scanners derive their roots
//! from [`ScanCtx::home`] (never from environment variables), never mutate
//! the filesystem, skip entries they cannot read instead of failing, and
//! honour cooperative cancellation at every directory boundary.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use rayon::prelude::*;
use tabibu_engine::{CancelToken, Category, CleanupItem, SafetyTier, ScanCtx, ScanError, Scanner};

const SEVEN_DAYS: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const THIRTY_DAYS: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Cache folder names whose owning app is a well-known browser; used only to
/// produce a more specific user-facing reason (tier is `Safe` either way).
const BROWSER_BUNDLE_IDS: &[&str] = &[
    "com.apple.Safari",
    "com.google.Chrome",
    "org.mozilla.firefox",
    "com.microsoft.edgemac",
    "com.brave.Browser",
    "com.operasoftware.Opera",
    "com.vivaldi.Vivaldi",
    "company.thebrowser.Browser",
];

/// Developer cache locations relative to the user home: (path, tier, reason).
const DEV_CACHE_TARGETS: &[(&str, SafetyTier, &str)] = &[
    (
        "Library/Developer/Xcode/DerivedData",
        SafetyTier::Review,
        "Xcode build products — regenerated on next build",
    ),
    (
        "Library/Developer/CoreSimulator/Caches",
        SafetyTier::Review,
        "iOS Simulator caches — regenerated as needed",
    ),
    (
        ".npm/_cacache",
        SafetyTier::Safe,
        "npm package cache — packages are re-downloaded on demand",
    ),
    (
        "Library/Caches/Yarn",
        SafetyTier::Safe,
        "Yarn package cache — packages are re-downloaded on demand",
    ),
    (
        "Library/Caches/pip",
        SafetyTier::Safe,
        "pip package cache — packages are re-downloaded on demand",
    ),
    (
        ".cargo/registry/cache",
        SafetyTier::Review,
        "Cargo registry cache — crates are re-downloaded on demand",
    ),
    (
        "Library/Caches/Homebrew",
        SafetyTier::Safe,
        "Homebrew download cache — formulae are re-downloaded on demand",
    ),
];

pub mod large_old;
pub use large_old::LargeOldScanner;

/// All junk scanners, in review-UI order.
#[must_use]
pub fn scanners() -> Vec<Box<dyn Scanner>> {
    vec![
        Box::new(TrashScanner::new()),
        Box::new(UserCacheScanner),
        Box::new(DevCacheScanner),
        Box::new(TempScanner::new()),
        Box::new(LogScanner),
        Box::new(LargeOldScanner::new()),
    ]
}

// ---------------------------------------------------------------------------
// Sizing and staleness helpers
// ---------------------------------------------------------------------------

/// Recursive directory size, parallelized with `rayon`: a directory's
/// children are sized concurrently (nested recursion uses the same work-
/// stealing pool). Never follows symlinks. Per-entry I/O errors count as 0,
/// matching the read-only "skip what we can't read" rule; cancellation is
/// checked at every directory boundary.
fn dir_size(path: &Path, cancel: &CancelToken) -> Result<u64, ScanError> {
    if cancel.is_cancelled() {
        return Err(ScanError::Cancelled);
    }
    let Ok(entries) = fs::read_dir(path) else {
        return Ok(0);
    };
    let children: Vec<fs::DirEntry> = entries.flatten().collect();
    // Propagate cancellation OUT of the parallel walk rather than swallowing it
    // to 0: a cancel deep in a subtree now aborts the whole sizing via
    // `try_reduce`'s short-circuit, instead of contributing 0 while the rest of
    // the level finishes and is summed.
    children
        .par_iter()
        .map(|entry| -> Result<u64, ScanError> {
            if cancel.is_cancelled() {
                return Err(ScanError::Cancelled);
            }
            // `DirEntry::metadata` does not traverse symlinks.
            let Ok(meta) = entry.metadata() else {
                return Ok(0);
            };
            if meta.is_dir() {
                dir_size(&entry.path(), cancel)
            } else {
                Ok(meta.len())
            }
        })
        .try_reduce(|| 0, |a, b| Ok(a + b))
}

/// Size of one filesystem entry: recursive for directories, `len()` for
/// everything else. Never follows symlinks.
fn entry_size(path: &Path, cancel: &CancelToken) -> Result<u64, ScanError> {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return Ok(0);
    };
    if meta.is_dir() {
        dir_size(path, cancel)
    } else {
        Ok(meta.len())
    }
}

/// True when the entry's mtime is more than `max_age` before `now`.
/// Unreadable or future mtimes are treated as fresh (never stale).
fn is_older_than(meta: &fs::Metadata, max_age: Duration, now: SystemTime) -> bool {
    meta.modified()
        .is_ok_and(|mtime| now.duration_since(mtime).is_ok_and(|age| age > max_age))
}

// ---------------------------------------------------------------------------
// TrashScanner
// ---------------------------------------------------------------------------

/// Lists every top-level entry of `~/.Trash` **and** per-volume trashes
/// (`/Volumes/*/.Trashes/<uid>`) as individually reviewable items, sized
/// recursively. Hidden entries are included — the Trash holds whatever was
/// deleted, hidden or not.
pub struct TrashScanner {
    /// Root that holds mounted volumes (default `/Volumes`; injectable for tests).
    volumes_root: PathBuf,
}

impl Default for TrashScanner {
    fn default() -> Self {
        Self {
            volumes_root: PathBuf::from("/Volumes"),
        }
    }
}

impl TrashScanner {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_volumes_root(volumes_root: PathBuf) -> Self {
        Self { volumes_root }
    }

    /// Emit each top-level entry of one trash directory.
    fn scan_trash_dir(
        dir: &Path,
        where_label: &str,
        cancel: &CancelToken,
        sink: &mut dyn FnMut(CleanupItem),
    ) -> Result<(), ScanError> {
        let Ok(entries) = fs::read_dir(dir) else {
            return Ok(());
        };
        for entry in entries.flatten() {
            if cancel.is_cancelled() {
                return Err(ScanError::Cancelled);
            }
            let path = entry.path();
            let size = entry_size(&path, cancel)?;
            let name = entry.file_name().to_string_lossy().into_owned();
            sink(CleanupItem::new(
                path,
                Category::Trash,
                size,
                SafetyTier::Safe,
                format!("\"{name}\" is in {where_label} — emptying frees its space"),
            ));
        }
        Ok(())
    }
}

impl Scanner for TrashScanner {
    fn id(&self) -> &'static str {
        "trash"
    }

    fn scan(
        &self,
        ctx: &ScanCtx,
        cancel: &CancelToken,
        sink: &mut dyn FnMut(CleanupItem),
    ) -> Result<(), ScanError> {
        if cancel.is_cancelled() {
            return Err(ScanError::Cancelled);
        }
        // 1. The main Trash.
        Self::scan_trash_dir(&ctx.home.join(".Trash"), "the Trash", cancel, sink)?;

        // 2. Per-volume trashes — the dir list comes from the SINGLE shared
        // derivation `per_volume_trash_dirs` (also used by the app's
        // allowed-roots builder, so the scanned paths and the reclaim guard
        // can never drift).
        use std::os::unix::fs::MetadataExt;
        let Ok(uid) = fs::metadata(&ctx.home).map(|m| m.uid()) else {
            return Ok(());
        };
        for trash in per_volume_trash_dirs(&self.volumes_root, uid) {
            if cancel.is_cancelled() {
                return Err(ScanError::Cancelled);
            }
            Self::scan_trash_dir(&trash, "an external volume's Trash", cancel, sink)?;
        }
        Ok(())
    }
}

/// The per-volume Trash directories (`<volumes_root>/<vol>/.Trashes/<uid>`) for
/// mounted non-boot volumes. The SINGLE source of truth for these paths: both
/// [`TrashScanner`] (what it emits) and the app's allowed-roots builder (what
/// reclaim permits) call this, so the two can't drift.
///
/// The boot volume's `/Volumes` entry is a symlink to `/`; we skip it with a
/// non-blocking `symlink_metadata` + `read_link` (NOT `canonicalize`, which
/// resolves the target and can hang on a stale/slow network mount).
#[must_use]
pub fn per_volume_trash_dirs(volumes_root: &Path, uid: u32) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(volumes_root) else {
        return Vec::new();
    };
    let mut dirs = Vec::new();
    for entry in entries.flatten() {
        let p = entry.path();
        // Skip the boot-volume firmlink symlink (-> /) without traversing it.
        let is_root_link = fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_symlink())
            && fs::read_link(&p).is_ok_and(|t| t == Path::new("/"));
        if is_root_link {
            continue;
        }
        dirs.push(p.join(".Trashes").join(uid.to_string()));
    }
    dirs
}

// ---------------------------------------------------------------------------
// UserCacheScanner
// ---------------------------------------------------------------------------

/// Lists per-app cache folders under `~/Library/Caches`. Folders named after
/// a bundle ID whose app is currently running are skipped entirely
/// (running-process guard).
pub struct UserCacheScanner;

/// A cache folder selected for sizing — tier/reason decided up front so the
/// expensive sizing can run in parallel without touching the sink.
struct CacheCandidate {
    path: PathBuf,
    tier: SafetyTier,
    reason: String,
}

/// Heuristic reverse-DNS check: at least two non-empty, dot-separated
/// segments of ASCII alphanumerics, hyphens, or underscores.
fn looks_like_bundle_id(name: &str) -> bool {
    let mut segments = 0usize;
    for segment in name.split('.') {
        if segment.is_empty()
            || !segment
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return false;
        }
        segments += 1;
    }
    segments >= 2
}

/// True when a cache folder named `name` belongs to a currently-running app.
/// Beyond an exact bundle-id match this also treats a folder as in-use when it
/// is a dotted *sub-identifier* of a running id (e.g. `com.foo.Bar.Helper`
/// while `com.foo.Bar` runs) or vice-versa, so a running app's helper/XPC cache
/// is not offered as Safe-to-delete while the app is live.
fn belongs_to_running(name: &str, running: &std::collections::HashSet<String>) -> bool {
    running.iter().any(|r| {
        name == r
            || name
                .strip_prefix(r.as_str())
                .is_some_and(|s| s.starts_with('.'))
            || r.strip_prefix(name).is_some_and(|s| s.starts_with('.'))
    })
}

impl Scanner for UserCacheScanner {
    fn id(&self) -> &'static str {
        "user_cache"
    }

    fn scan(
        &self,
        ctx: &ScanCtx,
        cancel: &CancelToken,
        sink: &mut dyn FnMut(CleanupItem),
    ) -> Result<(), ScanError> {
        if cancel.is_cancelled() {
            return Err(ScanError::Cancelled);
        }
        let root = ctx.home.join("Library/Caches");
        let Ok(entries) = fs::read_dir(&root) else {
            return Ok(());
        };

        // Phase 1 (cheap, sequential): decide which folders are candidates and
        // their tier/reason, applying the running-process guard. No sizing yet.
        let mut candidates: Vec<CacheCandidate> = Vec::new();
        for entry in entries.flatten() {
            if cancel.is_cancelled() {
                return Err(ScanError::Cancelled);
            }
            // `file_type` does not follow symlinks, so symlinked cache
            // folders are skipped rather than traversed.
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let (tier, reason) = if looks_like_bundle_id(&name) {
                if belongs_to_running(&name, &ctx.running_bundle_ids) {
                    continue; // running-process guard (incl. sub-identifiers)
                }
                let reason = if BROWSER_BUNDLE_IDS.contains(&name.as_str()) {
                    format!("Browser cache for {name} (not running)")
                } else {
                    format!("Cache for {name} (not running)")
                };
                (SafetyTier::Safe, reason)
            } else {
                (
                    SafetyTier::Review,
                    format!(
                        "Cache folder \"{name}\" — owning app not identified, review before removing"
                    ),
                )
            };
            candidates.push(CacheCandidate {
                path: entry.path(),
                tier,
                reason,
            });
        }

        // Phase 2 (the expensive part): size every candidate concurrently.
        // `dir_size` is itself parallel, so this saturates the rayon pool
        // across folders instead of sizing them one at a time.
        // Short-circuit on cancellation instead of swallowing each folder's
        // sizing error to 0: `collect::<Result<_,_>>` aborts on the first
        // `Cancelled` from `dir_size`.
        let sized: Vec<(CacheCandidate, u64)> = candidates
            .into_par_iter()
            .map(|c| -> Result<(CacheCandidate, u64), ScanError> {
                let size = dir_size(&c.path, cancel)?;
                Ok((c, size))
            })
            .collect::<Result<Vec<_>, _>>()?;

        // Phase 3 (cheap, sequential): emit through the single-threaded sink.
        for (c, size) in sized {
            sink(CleanupItem::new(
                c.path,
                Category::UserCache,
                size,
                c.tier,
                c.reason,
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// DevCacheScanner
// ---------------------------------------------------------------------------

/// Checks a fixed list of well-known developer cache directories under the
/// user home; each existing directory becomes one item with its measured
/// recursive size. Non-existent locations are skipped silently.
pub struct DevCacheScanner;

impl Scanner for DevCacheScanner {
    fn id(&self) -> &'static str {
        "dev_cache"
    }

    fn scan(
        &self,
        ctx: &ScanCtx,
        cancel: &CancelToken,
        sink: &mut dyn FnMut(CleanupItem),
    ) -> Result<(), ScanError> {
        for (relative, tier, reason) in DEV_CACHE_TARGETS {
            if cancel.is_cancelled() {
                return Err(ScanError::Cancelled);
            }
            let path = ctx.home.join(relative);
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            if !meta.is_dir() {
                continue;
            }
            let size = dir_size(&path, cancel)?;
            sink(CleanupItem::new(
                path,
                Category::DevCache,
                size,
                *tier,
                *reason,
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// TempScanner
// ---------------------------------------------------------------------------

/// Finds stale (mtime older than 7 days) files directly under
/// `~/Library/Caches/TemporaryItems`, plus stale top-level entries of the
/// system temp directory — but only when that directory resolves into
/// `/var/folders` (the macOS per-user confined temp area).
#[derive(Debug, Default)]
pub struct TempScanner {
    system_temp: Option<PathBuf>,
}

impl TempScanner {
    /// Scanner using [`std::env::temp_dir`] as the system temp directory.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Scanner with an explicit system temp directory (used by tests to stay
    /// inside fixture roots).
    #[must_use]
    pub fn with_system_temp(system_temp: PathBuf) -> Self {
        Self {
            system_temp: Some(system_temp),
        }
    }
}

/// True when `path` (already canonicalized) lives in the macOS per-user
/// confined temp area.
fn is_var_folders(path: &Path) -> bool {
    path.starts_with("/var/folders") || path.starts_with("/private/var/folders")
}

impl Scanner for TempScanner {
    fn id(&self) -> &'static str {
        "temp"
    }

    fn scan(
        &self,
        ctx: &ScanCtx,
        cancel: &CancelToken,
        sink: &mut dyn FnMut(CleanupItem),
    ) -> Result<(), ScanError> {
        if cancel.is_cancelled() {
            return Err(ScanError::Cancelled);
        }
        let now = SystemTime::now();

        // 1. Stale files directly under ~/Library/Caches/TemporaryItems.
        let temporary_items = ctx.home.join("Library/Caches/TemporaryItems");
        if let Ok(entries) = fs::read_dir(&temporary_items) {
            for entry in entries.flatten() {
                if cancel.is_cancelled() {
                    return Err(ScanError::Cancelled);
                }
                let Ok(meta) = entry.metadata() else { continue };
                if !meta.is_file() || !is_older_than(&meta, SEVEN_DAYS, now) {
                    continue;
                }
                sink(CleanupItem::new(
                    entry.path(),
                    Category::Temp,
                    meta.len(),
                    SafetyTier::Review,
                    "Temporary file not modified in over 7 days",
                ));
            }
        }

        // 2. Stale top-level entries of the system temp directory, only when
        //    it canonicalizes into /var/folders.
        let system_temp = self.system_temp.clone().unwrap_or_else(std::env::temp_dir);
        let Ok(canonical) = system_temp.canonicalize() else {
            return Ok(());
        };
        if !is_var_folders(&canonical) {
            return Ok(());
        }
        if let Ok(entries) = fs::read_dir(&canonical) {
            for entry in entries.flatten() {
                if cancel.is_cancelled() {
                    return Err(ScanError::Cancelled);
                }
                let Ok(meta) = entry.metadata() else { continue };
                if !is_older_than(&meta, SEVEN_DAYS, now) {
                    continue;
                }
                let path = entry.path();
                let size = if meta.is_dir() {
                    dir_size(&path, cancel)?
                } else {
                    meta.len()
                };
                sink(CleanupItem::new(
                    path,
                    Category::Temp,
                    size,
                    SafetyTier::Review,
                    "System temp item not modified in over 7 days",
                ));
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// LogScanner
// ---------------------------------------------------------------------------

/// Finds log files under `~/Library/Logs` older than 30 days, grouped per
/// immediate subdirectory (one item per app's log folder, sized by its total
/// stale bytes). Stale files lying directly in `Logs` are reported
/// individually.
pub struct LogScanner;

/// Count and total size of files older than `max_age` anywhere under `dir`.
fn stale_file_stats(
    dir: &Path,
    max_age: Duration,
    now: SystemTime,
    cancel: &CancelToken,
) -> Result<(u64, u64), ScanError> {
    if cancel.is_cancelled() {
        return Err(ScanError::Cancelled);
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return Ok((0, 0));
    };
    let (mut count, mut bytes) = (0u64, 0u64);
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            let (sub_count, sub_bytes) = stale_file_stats(&entry.path(), max_age, now, cancel)?;
            count += sub_count;
            bytes += sub_bytes;
        } else if meta.is_file() && is_older_than(&meta, max_age, now) {
            count += 1;
            bytes += meta.len();
        }
    }
    Ok((count, bytes))
}

impl Scanner for LogScanner {
    fn id(&self) -> &'static str {
        "log"
    }

    fn scan(
        &self,
        ctx: &ScanCtx,
        cancel: &CancelToken,
        sink: &mut dyn FnMut(CleanupItem),
    ) -> Result<(), ScanError> {
        if cancel.is_cancelled() {
            return Err(ScanError::Cancelled);
        }
        let now = SystemTime::now();
        let root = ctx.home.join("Library/Logs");
        let Ok(entries) = fs::read_dir(&root) else {
            return Ok(());
        };
        for entry in entries.flatten() {
            if cancel.is_cancelled() {
                return Err(ScanError::Cancelled);
            }
            let Ok(meta) = entry.metadata() else { continue };
            let name = entry.file_name().to_string_lossy().into_owned();
            if meta.is_dir() {
                let (count, bytes) = stale_file_stats(&entry.path(), THIRTY_DAYS, now, cancel)?;
                if count == 0 {
                    continue;
                }
                sink(CleanupItem::new(
                    entry.path(),
                    Category::Log,
                    bytes,
                    SafetyTier::Safe,
                    format!("Logs from {name} older than 30 days"),
                ));
            } else if meta.is_file() && is_older_than(&meta, THIRTY_DAYS, now) {
                sink(CleanupItem::new(
                    entry.path(),
                    Category::Log,
                    meta.len(),
                    SafetyTier::Safe,
                    format!("Log file \"{name}\" older than 30 days"),
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod unit_tests {
    use super::looks_like_bundle_id;

    #[test]
    fn bundle_id_heuristic() {
        assert!(looks_like_bundle_id("com.example.app"));
        assert!(looks_like_bundle_id("org.mozilla.firefox"));
        assert!(looks_like_bundle_id("io.rust-lang.cargo_ui"));
        assert!(looks_like_bundle_id("a.b"));
        assert!(!looks_like_bundle_id("Chrome"));
        assert!(!looks_like_bundle_id("com..example"));
        assert!(!looks_like_bundle_id(".hidden"));
        assert!(!looks_like_bundle_id("My App.cache"));
        assert!(!looks_like_bundle_id(""));
    }
}
