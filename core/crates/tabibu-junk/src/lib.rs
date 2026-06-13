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
        Box::new(TrashScanner),
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

/// Recursive size of a directory tree. Symlinks are counted by their own
/// metadata and never followed; unreadable entries count as zero. Checks
/// `cancel` on entering every directory.
fn dir_size(path: &Path, cancel: &CancelToken) -> Result<u64, ScanError> {
    if cancel.is_cancelled() {
        return Err(ScanError::Cancelled);
    }
    let Ok(entries) = fs::read_dir(path) else {
        return Ok(0);
    };
    let mut total = 0u64;
    for entry in entries.flatten() {
        // `DirEntry::metadata` does not traverse symlinks.
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            total += dir_size(&entry.path(), cancel)?;
        } else {
            total += meta.len();
        }
    }
    Ok(total)
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

/// Lists every top-level entry of `~/.Trash` as an individually reviewable
/// item, sized recursively.
pub struct TrashScanner;

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
        let root = ctx.home.join(".Trash");
        let Ok(entries) = fs::read_dir(&root) else {
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
                format!("\"{name}\" is already in the Trash — emptying frees its space"),
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// UserCacheScanner
// ---------------------------------------------------------------------------

/// Lists per-app cache folders under `~/Library/Caches`. Folders named after
/// a bundle ID whose app is currently running are skipped entirely
/// (running-process guard).
pub struct UserCacheScanner;

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
            let item = if looks_like_bundle_id(&name) {
                if ctx.running_bundle_ids.contains(&name) {
                    continue; // running-process guard
                }
                let size = dir_size(&entry.path(), cancel)?;
                let reason = if BROWSER_BUNDLE_IDS.contains(&name.as_str()) {
                    format!("Browser cache for {name} (not running)")
                } else {
                    format!("Cache for {name} (not running)")
                };
                CleanupItem::new(
                    entry.path(),
                    Category::UserCache,
                    size,
                    SafetyTier::Safe,
                    reason,
                )
            } else {
                let size = dir_size(&entry.path(), cancel)?;
                CleanupItem::new(
                    entry.path(),
                    Category::UserCache,
                    size,
                    SafetyTier::Review,
                    format!(
                        "Cache folder \"{name}\" — owning app not identified, review before removing"
                    ),
                )
            };
            sink(item);
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
