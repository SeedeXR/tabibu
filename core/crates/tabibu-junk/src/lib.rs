//! tabibu-junk — read-only scanners for reclaimable junk on macOS.
//!
//! Six scanners cover the classic cleanup buckets: Trash contents, per-app
//! user caches (`~/Library/Caches`, with a running-process guard), sandboxed
//! app caches (`~/Library/Containers/*/Data/Library/Caches` + group containers),
//! developer tool caches, stale temporary files, and old log files. All scanners
//! derive their roots
//! from [`ScanCtx::home`] (never from environment variables), never mutate
//! the filesystem, skip entries they cannot read instead of failing, and
//! honour cooperative cancellation at every directory boundary.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

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
///
/// INVARIANT: no entry may live under `Library/Caches` — `UserCacheScanner`
/// already emits every top-level folder there, so a `Library/Caches/*` entry
/// here would double-emit the same path (the engine does not dedup). Regenerable
/// caches under `Library/Caches` (Homebrew, Yarn, pip, CocoaPods, …) are instead
/// tiered `Safe` by [`KNOWN_SAFE_CACHE_NAMES`] in `UserCacheScanner`. A test
/// (`dev_targets_never_under_library_caches`) enforces this.
const DEV_CACHE_TARGETS: &[(&str, SafetyTier, &str)] = &[
    (
        "Library/Developer/Xcode/DerivedData",
        SafetyTier::Review,
        "Xcode build products — regenerated on next build",
    ),
    (
        "Library/Developer/Xcode/iOS DeviceSupport",
        SafetyTier::Review,
        "Xcode device debug symbols — re-downloaded when a device reconnects",
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
    // Review, not Safe: anything started with `npx` (dev servers, MCP servers)
    // keeps RUNNING out of this directory, so it must not be auto-selected.
    (
        ".npm/_npx",
        SafetyTier::Review,
        "npx package cache — re-installed on demand; quit anything started with npx first",
    ),
    (
        "Library/pnpm/store",
        SafetyTier::Review,
        "pnpm package store — re-downloaded on demand; projects hard-link into it, so space is only freed once their node_modules are gone too",
    ),
    (
        ".cargo/registry/cache",
        SafetyTier::Review,
        "Cargo registry cache — crates are re-downloaded on demand",
    ),
    // Review for the same reason as its sibling above: a running `cargo build`
    // or rust-analyzer reads these sources.
    (
        ".cargo/registry/src",
        SafetyTier::Review,
        "Cargo extracted crate sources — re-extracted from the registry cache on next build",
    ),
    // Hidden dot-directory caches (Finder hides these; users reach them via
    // Cmd+Shift+G). Review tier — broad, so surfaced but never auto-selected.
    (
        ".cache",
        SafetyTier::Review,
        "Hidden XDG cache (~/.cache) — disposable caches for tools like pip, uv, Playwright, Hugging Face",
    ),
    (
        ".gradle/caches",
        SafetyTier::Review,
        "Gradle build & dependency cache — rebuilt/re-downloaded on next build",
    ),
    (
        "go/pkg/mod",
        SafetyTier::Review,
        "Go module cache — modules are re-downloaded on demand (go clean -modcache)",
    ),
];

/// Absolute developer-cache roots for this home, derived from
/// [`DEV_CACHE_TARGETS`]. This is the SINGLE source of truth the reclaim
/// allowed-roots builders (`system::default_scan_ctx`, the CLI `junk_ctx`) use,
/// so adding a dev target automatically permits reclaiming it — no hand-kept
/// parallel list to drift. Mirrors [`container_cache_roots`] /
/// [`app_support_cache_roots`].
#[must_use]
pub fn dev_cache_roots(home: &Path) -> Vec<PathBuf> {
    DEV_CACHE_TARGETS
        .iter()
        .map(|(rel, _, _)| home.join(rel))
        .collect()
}

/// Folder names directly under `~/Library/Caches` that are NOT reverse-DNS
/// bundle IDs but are well-known regenerable tool/package caches — tiered
/// `Safe` (auto-selectable) with a clear reason instead of the generic
/// `Review` fallback. Kept tight: only caches that are unambiguously
/// re-downloaded/rebuilt on demand.
const KNOWN_SAFE_CACHE_NAMES: &[&str] = &[
    "Homebrew",
    "Yarn",
    "pip",
    "pipenv",
    "pnpm",
    "CocoaPods",
    "go-build",
    "ms-playwright",
    "node-gyp",
    "deno",
    "typescript",
    "puppeteer",
];

/// The per-user Darwin *confined cache* dir (`/var/folders/<..>/C`) — sibling of
/// the Darwin temp dir macOS exposes as `$TMPDIR` (`.../T`). macOS stores a large
/// share of daemon/app caches HERE, outside `~/Library/Caches`:
/// `com.apple.mediaanalysisd`, `com.apple.metal`, `com.apple.Safari.SafeBrowsing`,
/// `com.apple.iconservicesagent`, Lightroom, Electron helpers, … — all of which a
/// scan of only `~/Library/Caches` misses. Derived from `std::env::temp_dir()`
/// (no `confstr`/unsafe): the temp dir ends in `/T`; the cache dir is its sibling
/// `/C`. Returns `None` when `$TMPDIR` isn't a `/var/folders` path (nothing
/// macOS-specific to add, e.g. a cron run with `TMPDIR` unset).
#[must_use]
pub fn darwin_user_cache_dir() -> Option<PathBuf> {
    let t = std::env::temp_dir();
    if !t.to_string_lossy().contains("/var/folders/") {
        return None;
    }
    if t.file_name().is_some_and(|n| n == "T") {
        return t.parent().map(|p| p.join("C"));
    }
    None
}

pub mod large_old;
pub use large_old::LargeOldScanner;

/// All junk scanners, in review-UI order.
#[must_use]
pub fn scanners() -> Vec<Box<dyn Scanner>> {
    vec![
        Box::new(TrashScanner::new()),
        Box::new(UserCacheScanner::new()),
        Box::new(ContainerCacheScanner),
        Box::new(AppSupportCacheScanner),
        Box::new(SystemCacheScanner::new()),
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

/// Outcome of permanently emptying trash directories.
pub struct EmptyTrashOutcome {
    pub freed_bytes: u64,
    pub deleted_items: u32,
    /// Per-entry failures (e.g. a locked/permission-denied item); the rest still
    /// get deleted.
    pub errors: Vec<String>,
}

/// Total recursive size of the given trash directories — the figure the
/// "your Trash is large" alert and the Empty-Trash button display. Missing dirs
/// contribute 0; never follows symlinks.
#[must_use]
pub fn trash_total_size(dirs: &[PathBuf], cancel: &CancelToken) -> u64 {
    dirs.iter()
        .flat_map(|d| fs::read_dir(d).into_iter().flatten().flatten())
        .map(|e| entry_size(&e.path(), cancel).unwrap_or(0))
        .sum()
}

/// Whether the trash dirs can actually be READ. macOS gates `~/.Trash` behind
/// Full Disk Access, so without it `read_dir` fails with `PermissionDenied` and
/// the Trash silently *looks* empty (size 0, "nothing to empty"). Returns
/// `false` when a dir that exists cannot be read — the signal to tell the user
/// to grant Full Disk Access. A missing dir is fine (no trash there).
#[must_use]
pub fn trash_accessible(dirs: &[PathBuf]) -> bool {
    !dirs.iter().any(|d| {
        fs::read_dir(d)
            .err()
            .is_some_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied)
    })
}

/// PERMANENTLY delete every top-level entry in each trash directory (the OS
/// "Empty Trash"). Sizes each entry first so the caller can report bytes freed.
/// Best-effort and irreversible: a failed delete is recorded and the rest
/// proceed. Callers MUST confirm with the user before invoking.
#[must_use]
pub fn empty_trash_dirs(dirs: &[PathBuf]) -> EmptyTrashOutcome {
    let cancel = CancelToken::new();
    let mut freed = 0u64;
    let mut deleted_items = 0u32;
    let mut errors = Vec::new();
    for dir in dirs {
        let Ok(entries) = fs::read_dir(dir) else {
            continue; // trash dir doesn't exist / unreadable → nothing to empty
        };
        for entry in entries.flatten() {
            let p = entry.path();
            let size = entry_size(&p, &cancel).unwrap_or(0);
            // symlink → remove the link itself (never the target); dir →
            // recursive; file → unlink.
            let res = match fs::symlink_metadata(&p) {
                Ok(m) if m.is_dir() => fs::remove_dir_all(&p),
                _ => fs::remove_file(&p),
            };
            match res {
                Ok(()) => {
                    freed += size;
                    deleted_items += 1;
                }
                Err(e) => errors.push(format!("{}: {e}", p.display())),
            }
        }
    }
    EmptyTrashOutcome {
        freed_bytes: freed,
        deleted_items,
        errors,
    }
}

// ---------------------------------------------------------------------------
// UserCacheScanner
// ---------------------------------------------------------------------------

/// Lists per-app cache folders under `~/Library/Caches`. Folders named after
/// a bundle ID whose app is currently running are skipped entirely
/// (running-process guard).
pub struct UserCacheScanner {
    /// Cache roots to scan IN ADDITION to `~/Library/Caches`. `new()` fills this
    /// with [`darwin_user_cache_dir`]; tests use [`UserCacheScanner::home_only`]
    /// to stay hermetic (no machine-wide dirs).
    extra_roots: Vec<PathBuf>,
}

impl Default for UserCacheScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl UserCacheScanner {
    /// Scans `~/Library/Caches` plus the Darwin confined cache dir
    /// (`/var/folders/<..>/C`), the two per-user cache roots macOS uses.
    #[must_use]
    pub fn new() -> Self {
        Self {
            extra_roots: darwin_user_cache_dir().into_iter().collect(),
        }
    }

    /// Only `~/Library/Caches` — no machine-wide dirs. For hermetic tests.
    #[must_use]
    pub fn home_only() -> Self {
        Self {
            extra_roots: Vec::new(),
        }
    }
}

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
///
/// `running` also carries bare EXECUTABLE names: daemons live outside any `.app`
/// and so have no bundle id (`/usr/libexec/duetexpertd`). A folder whose last
/// dotted segment is a running executable's name (`com.apple.duetexpertd`) is
/// that daemon's cache — without this every daemon cache read as "not running".
fn belongs_to_running(name: &str, running: &std::collections::HashSet<String>) -> bool {
    let last_segment = name.rsplit('.').next().unwrap_or(name);
    running.iter().any(|r| {
        name == r
            || last_segment == r
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
        // Scan BOTH per-user cache roots: ~/Library/Caches AND the Darwin confined
        // cache dir (/var/folders/<..>/C). macOS keeps many daemon/app caches in
        // the latter (com.apple.mediaanalysisd, com.apple.metal, Safari.SafeBrowsing,
        // iconservicesagent, Lightroom, …) — missed entirely by a scan of only
        // ~/Library/Caches. Same per-entry tiering applies to both.
        let mut roots = vec![ctx.home.join("Library/Caches")];
        roots.extend(self.extra_roots.iter().cloned());

        // Phase 1 (cheap, sequential): decide which folders are candidates and
        // their tier/reason, applying the running-process guard. No sizing yet.
        let mut candidates: Vec<CacheCandidate> = Vec::new();
        for root in &roots {
            let Ok(entries) = fs::read_dir(root) else {
                continue; // a missing/unreadable root is skipped, others still scan
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
                if name == "TemporaryItems" {
                    // TempScanner owns this folder: it emits the STALE files
                    // inside. Emitting the whole folder here as well counted
                    // those bytes twice, and offered the fresh temp files —
                    // which may be in use — for removal. Fresh ones are meant
                    // to go unreported.
                    continue;
                }
                let (tier, reason) = if looks_like_bundle_id(&name) {
                    if belongs_to_running(&name, &ctx.running_bundle_ids) {
                        // Running-process guard: previously these were SKIPPED
                        // entirely, which silently missed the largest user caches —
                        // always-on com.apple.* daemons (mediaanalysisd, metal,
                        // iconservicesagent, photolibraryd, quicklook, replayd,
                        // Safari.SafeBrowsing …) and any app that happens to be open
                        // (Edge, Blender, Lightroom …). Surface them as Review (never
                        // auto-selected): clearing is safe, but the live process may
                        // rebuild them immediately, so quitting it gives a full clean.
                        (
                            SafetyTier::Review,
                            format!(
                                "Cache for {name} — app/process running (quit it for a full clean)"
                            ),
                        )
                    } else {
                        let reason = if BROWSER_BUNDLE_IDS.contains(&name.as_str()) {
                            format!("Browser cache for {name} (not running)")
                        } else {
                            format!("Cache for {name} (not running)")
                        };
                        (SafetyTier::Safe, reason)
                    }
                } else if KNOWN_SAFE_CACHE_NAMES.contains(&name.as_str()) {
                    (
                        SafetyTier::Safe,
                        format!("{name} cache — regenerated/re-downloaded on demand"),
                    )
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
        emit_caches(sized, sink);
        Ok(())
    }
}

/// Emit sized cache candidates, dropping empty ones: a 0-byte cache folder
/// frees nothing and is pure noise in the list (dozens exist on a typical Mac),
/// and a folder that was just trashed sizes to 0 as well.
fn emit_caches(sized: Vec<(CacheCandidate, u64)>, sink: &mut dyn FnMut(CleanupItem)) {
    for (c, size) in sized {
        if size > 0 {
            sink(CleanupItem::new(
                c.path,
                Category::UserCache,
                size,
                c.tier,
                c.reason,
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// ContainerCacheScanner — sandboxed-app caches
// ---------------------------------------------------------------------------

/// The `Caches` directory of every sandboxed-app container and group container
/// that exists — the sandboxed equivalent of `~/Library/Caches/<app>`, which
/// `UserCacheScanner` alone misses (App Store apps and many others put their
/// caches here, not in `~/Library/Caches`).
///
/// Returns ONLY the `Caches` subdir of each container — NEVER the container's
/// `Data` (which holds the app's real user data). Reading inside a container is
/// gated by Full Disk Access, so without it `is_dir` fails and the list is
/// empty (the caches are unreadable anyway). Ctx builders add these to
/// `allowed_roots` so `reclaim` permits exactly these cache dirs and nothing
/// else under the containers.
#[must_use]
pub fn container_cache_roots(home: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    // Per-app: ~/Library/Containers/<bundle-id>/Data/Library/Caches
    if let Ok(entries) = fs::read_dir(home.join("Library/Containers")) {
        for e in entries.flatten() {
            let c = e.path().join("Data/Library/Caches");
            if c.is_dir() {
                out.push(c);
            }
        }
    }
    // Shared: ~/Library/Group Containers/<group-id>/Library/Caches
    if let Ok(entries) = fs::read_dir(home.join("Library/Group Containers")) {
        for e in entries.flatten() {
            let c = e.path().join("Library/Caches");
            if c.is_dir() {
                out.push(c);
            }
        }
    }
    out
}

/// From a container `Caches` path, recover the container id and whether it's a
/// group container — used for the running-process guard and the reason text.
fn container_id_and_group(cache_path: &Path) -> Option<(String, bool)> {
    let comps: Vec<_> = cache_path.components().collect();
    comps.iter().enumerate().find_map(|(i, c)| {
        let seg = c.as_os_str().to_string_lossy();
        let is_group = seg == "Group Containers";
        (is_group || seg == "Containers").then(|| {
            let id = comps.get(i + 1)?.as_os_str().to_string_lossy().into_owned();
            Some((id, is_group))
        })?
    })
}

/// Sweeps the `Caches` dir of each sandboxed / group container. A per-app cache
/// whose app is currently running is skipped (running-process guard, as for
/// `~/Library/Caches`). Group-container caches are `Review` (ownership is shared,
/// not a single app).
pub struct ContainerCacheScanner;

impl Scanner for ContainerCacheScanner {
    fn id(&self) -> &'static str {
        "container_cache"
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
        // Phase 1: candidates + tier/reason (running guard), no sizing yet.
        let mut candidates: Vec<CacheCandidate> = Vec::new();
        for path in container_cache_roots(&ctx.home) {
            if cancel.is_cancelled() {
                return Err(ScanError::Cancelled);
            }
            let Some((id, is_group)) = container_id_and_group(&path) else {
                continue;
            };
            let (tier, reason) = if is_group {
                (
                    SafetyTier::Review,
                    format!("Shared (group) cache for {id} — review before removing"),
                )
            } else if belongs_to_running(&id, &ctx.running_bundle_ids) {
                // Surface (Review), don't skip — running sandboxed apps' caches
                // are reclaimable too; quitting the app gives a full clean.
                (
                    SafetyTier::Review,
                    format!("Cache for {id} (sandboxed app running — quit it for a full clean)"),
                )
            } else if looks_like_bundle_id(&id) {
                (
                    SafetyTier::Safe,
                    format!("Cache for {id} (sandboxed app, not running)"),
                )
            } else {
                (
                    SafetyTier::Review,
                    format!("Sandboxed cache \"{id}\" — owning app not identified"),
                )
            };
            candidates.push(CacheCandidate { path, tier, reason });
        }

        // Phase 2: size concurrently.
        let sized: Vec<(CacheCandidate, u64)> = candidates
            .into_par_iter()
            .map(|c| -> Result<(CacheCandidate, u64), ScanError> {
                let size = dir_size(&c.path, cancel)?;
                Ok((c, size))
            })
            .collect::<Result<Vec<_>, _>>()?;

        // Phase 3: emit.
        emit_caches(sized, sink);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// AppSupportCacheScanner — app caches under Application Support
// ---------------------------------------------------------------------------

/// Subtrees the Application Support sweep never matches AND never enters
/// (lowercase names). A name containing "cache" is not proof of a disposable
/// cache; these are the classes a broad match would wrongly catch:
///   - dependency trees — packages merely NAMED `…cache…` (`node_modules/
///     lru-cache`, `…/typescript-auto-import-cache`); removing one breaks the tool
///   - live stores — Spotify `PersistentCache` (downloaded offline media),
///     Google Drive `content_cache` (file content that may not be uploaded yet)
///   - `__pycache__` — bytecode inside an app's own install, not a user cache
const APP_SUPPORT_SKIP: &[&str] = &[
    "node_modules",
    "site-packages",
    "persistentcache",
    "content_cache",
    "__pycache__",
];

/// Whether the sweep must stay out of this directory entirely: a listed name
/// ([`APP_SUPPORT_SKIP`]) or a hidden dot-directory (app-internal state such as
/// a key-value store named `.cache` or an install-identity `.hwcache`).
fn skip_subtree(name: &str) -> bool {
    name.starts_with('.') || APP_SUPPORT_SKIP.contains(&name.to_ascii_lowercase().as_str())
}

/// Max directory depth searched below each Application Support app dir for a
/// cache dir (`Google/Chrome/Default/Cache` = 3; `Code/WebStorage/N/CacheStorage`
/// and `Figma/DesktopProfile/v43/Code Cache` = 4). Bounded so the search stays
/// shallow and fast — and matched cache dirs are never descended into anyway.
const APP_SUPPORT_MAX_DEPTH: usize = 5;

/// Recursively collect cache dirs under `dir`. A matched cache dir is pushed and
/// NOT descended into (so we never walk the cache's own contents here); other
/// dirs are followed until [`APP_SUPPORT_MAX_DEPTH`]. Symlinks are not followed.
fn collect_cache_leaves(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        if !ft.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if skip_subtree(&name) {
            continue; // neither matched nor descended into
        }
        // A cache dir is any (non-skipped) directory with "cache" in its name.
        // Deliberately BROAD so we catch what a fixed list misses — `Cache`,
        // `Code Cache`, `GPUCache`, `CacheStorage`, `crx_cache`, `download-cache`,
        // `CachedData`, `bincache*`, `DerivedDataCache`, … all regenerated on demand.
        if name.to_ascii_lowercase().contains("cache") {
            out.push(entry.path());
        } else if depth < APP_SUPPORT_MAX_DEPTH {
            collect_cache_leaves(&entry.path(), depth + 1, out);
        }
    }
}

fn compute_app_support_cache_roots(home: &Path) -> Vec<PathBuf> {
    let base = home.join("Library/Application Support");
    let Ok(entries) = fs::read_dir(&base) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|ft| ft.is_dir()) {
            collect_cache_leaves(&entry.path(), 1, &mut out);
        }
    }
    out
}

/// Short-lived memo of the last scan's result, keyed by home. The depth-4 walk
/// (~0.4s on a full Application Support) is called up to three times per cleanup
/// cycle — twice building a scan ctx + inside the scanner, plus every `reclaim`
/// rebuilds the ctx — so without this each one re-walks. A stale entry only
/// means a just-created cache leaf isn't permitted until the TTL lapses, or a
/// just-deleted one lingers as a no-op allowed root; both are harmless.
/// ponytail: process-global memo, 15s TTL — fine for this read-mostly list.
static APP_SUPPORT_CACHE_MEMO: Mutex<Option<(PathBuf, Instant, Vec<PathBuf>)>> = Mutex::new(None);

/// Chromium/Electron cache-leaf directories under `~/Library/Application
/// Support`. Shared by [`AppSupportCacheScanner`] and the reclaim allowed-roots
/// so they never drift: reclaim permits EXACTLY these leaves and nothing else
/// under Application Support — each app's real data stays outside the boundary.
/// Memoized (see [`APP_SUPPORT_CACHE_MEMO`]) to avoid re-walking on each call.
#[must_use]
pub fn app_support_cache_roots(home: &Path) -> Vec<PathBuf> {
    const TTL: Duration = Duration::from_secs(15);
    if let Ok(guard) = APP_SUPPORT_CACHE_MEMO.lock() {
        if let Some((h, at, roots)) = guard.as_ref() {
            if h == home && at.elapsed() < TTL {
                return roots.clone();
            }
        }
    }
    let roots = compute_app_support_cache_roots(home);
    if let Ok(mut guard) = APP_SUPPORT_CACHE_MEMO.lock() {
        *guard = Some((home.to_path_buf(), Instant::now(), roots.clone()));
    }
    roots
}

/// The Application Support app/vendor dir a cache leaf belongs to (the segment
/// right after `Application Support`) — for the reason text.
fn app_support_owner(path: &Path, home: &Path) -> String {
    let base = home.join("Library/Application Support");
    path.strip_prefix(&base)
        .ok()
        .and_then(|rel| rel.components().next())
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .unwrap_or_else(|| "an app".to_string())
}

/// Sweeps app cache directories under `~/Library/Application Support`.
/// ALL `Review` tier: these apps hold the cache open while running, so clearing
/// a live app reclaims nothing and can corrupt its profile — the user should
/// quit the app first. Surfaced for review, never auto-selected.
pub struct AppSupportCacheScanner;

impl Scanner for AppSupportCacheScanner {
    fn id(&self) -> &'static str {
        "app_support_cache"
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
        let candidates = app_support_cache_roots(&ctx.home);
        // Size concurrently (dir_size is itself parallel); short-circuit on cancel.
        let sized: Vec<(PathBuf, u64)> = candidates
            .into_par_iter()
            .map(|p| -> Result<(PathBuf, u64), ScanError> {
                let size = dir_size(&p, cancel)?;
                Ok((p, size))
            })
            .collect::<Result<Vec<_>, _>>()?;
        for (path, size) in sized {
            if size == 0 {
                continue; // nothing to free (or just trashed and still memoized)
            }
            let owner = app_support_owner(&path, &ctx.home);
            let leaf = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            sink(CleanupItem::new(
                path,
                Category::UserCache,
                size,
                SafetyTier::Review,
                format!("{owner} cache ({leaf}) — quit {owner} first; regenerates on next launch"),
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

/// World-writable shared temp dirs on macOS worth sweeping for the *user's own*
/// stale files: `/private/tmp` (the `/tmp` alias — reachable in Finder via
/// ⇧⌘G) and `/private/var/tmp` (`/var/tmp`, which persists across reboots).
/// Fixed list — the scanner and the reclaim allowed-roots both use it so they
/// never drift. These live under `/private`, which the denylist already permits
/// (only `/private/var/db` etc. are denied); `normalize` maps the `/tmp`↔
/// `/private/tmp` symlink aliases so an allowed root matches either spelling.
#[must_use]
pub fn shared_temp_roots() -> Vec<PathBuf> {
    vec![
        PathBuf::from("/private/tmp"),
        PathBuf::from("/private/var/tmp"),
    ]
}

/// Finds stale (mtime older than 7 days) files directly under
/// `~/Library/Caches/TemporaryItems`, stale top-level entries of the system
/// temp directory (only when it resolves into `/var/folders`, the per-user
/// confined area), and stale *user-owned* top-level entries of the shared
/// world-writable temp dirs ([`shared_temp_roots`]).
#[derive(Debug)]
pub struct TempScanner {
    system_temp: Option<PathBuf>,
    shared_roots: Vec<PathBuf>,
}

impl Default for TempScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl TempScanner {
    /// Scanner using [`std::env::temp_dir`] and the real [`shared_temp_roots`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            system_temp: None,
            shared_roots: shared_temp_roots(),
        }
    }

    /// Scanner with an explicit system temp directory and NO shared roots (used
    /// by tests to stay inside fixture roots).
    #[must_use]
    pub fn with_system_temp(system_temp: PathBuf) -> Self {
        Self {
            system_temp: Some(system_temp),
            shared_roots: Vec::new(),
        }
    }

    /// Scanner with an explicit system temp dir AND explicit shared roots (used
    /// by tests to exercise the shared-temp sweep inside fixtures).
    #[must_use]
    pub fn with_roots(system_temp: PathBuf, shared_roots: Vec<PathBuf>) -> Self {
        Self {
            system_temp: Some(system_temp),
            shared_roots,
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
        //    it canonicalizes into /var/folders (the per-user confined area, so
        //    no ownership filter needed).
        let system_temp = self.system_temp.clone().unwrap_or_else(std::env::temp_dir);
        if let Ok(canonical) = system_temp.canonicalize() {
            if is_var_folders(&canonical) {
                scan_temp_dir(
                    &canonical,
                    "System temp item not modified in over 7 days",
                    None,
                    cancel,
                    now,
                    sink,
                )?;
            }
        }

        // 3. Stale entries of the world-writable shared temp dirs (/private/tmp,
        //    /private/var/tmp). These are shared by all users and system
        //    processes, so we emit ONLY entries owned by this user — never
        //    root's or another user's files (which reclaim couldn't trash
        //    anyway). If the user's uid can't be determined, skip them entirely.
        let owner_uid = {
            use std::os::unix::fs::MetadataExt;
            fs::metadata(&ctx.home).ok().map(|m| m.uid())
        };
        if let Some(uid) = owner_uid {
            for root in &self.shared_roots {
                if let Ok(canonical) = root.canonicalize() {
                    scan_temp_dir(
                        &canonical,
                        "Shared temp item not modified in over 7 days",
                        Some(uid),
                        cancel,
                        now,
                        sink,
                    )?;
                }
            }
        }
        Ok(())
    }
}

/// Emit stale (>7 days) top-level entries of `dir` as Review [`Category::Temp`]
/// items. When `owner_uid` is `Some`, only entries owned by that uid are
/// emitted — the safety guard for world-writable shared temp dirs, where other
/// users' and root's files must never be offered for removal.
fn scan_temp_dir(
    dir: &Path,
    reason: &'static str,
    owner_uid: Option<u32>,
    cancel: &CancelToken,
    now: SystemTime,
    sink: &mut dyn FnMut(CleanupItem),
) -> Result<(), ScanError> {
    use std::os::unix::fs::MetadataExt;
    let Ok(entries) = fs::read_dir(dir) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        if cancel.is_cancelled() {
            return Err(ScanError::Cancelled);
        }
        // DirEntry::metadata does not follow symlinks, so a symlink in /tmp is
        // sized/owned as the link itself and never traversed.
        let Ok(meta) = entry.metadata() else { continue };
        if !is_older_than(&meta, SEVEN_DAYS, now) {
            continue;
        }
        if owner_uid.is_some_and(|uid| meta.uid() != uid) {
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
            reason,
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// SystemCacheScanner — system-wide app caches under /Library/Caches
// ---------------------------------------------------------------------------

/// Sweeps `/Library/Caches` (system-wide app caches, distinct from the per-user
/// `~/Library/Caches` that [`UserCacheScanner`] covers). Only entries OWNED BY
/// THE USER are offered — root-owned system caches couldn't be trashed anyway and
/// listing them would be noise. All `Review` tier (shared/system location). macOS
/// seals `/System/Library/Caches`, so that is intentionally not scanned.
pub struct SystemCacheScanner {
    root: PathBuf,
}

impl Default for SystemCacheScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemCacheScanner {
    #[must_use]
    pub fn new() -> Self {
        Self {
            root: PathBuf::from("/Library/Caches"),
        }
    }
    /// Test-only: scan an explicit root instead of `/Library/Caches`.
    #[must_use]
    pub fn with_root(root: PathBuf) -> Self {
        Self { root }
    }
}

impl Scanner for SystemCacheScanner {
    fn id(&self) -> &'static str {
        "system_cache"
    }

    fn scan(
        &self,
        ctx: &ScanCtx,
        cancel: &CancelToken,
        sink: &mut dyn FnMut(CleanupItem),
    ) -> Result<(), ScanError> {
        use std::os::unix::fs::MetadataExt;
        if cancel.is_cancelled() {
            return Err(ScanError::Cancelled);
        }
        // Only offer caches the user owns (removable); skip entirely if the home
        // uid can't be read.
        let Some(uid) = fs::metadata(&ctx.home).ok().map(|m| m.uid()) else {
            return Ok(());
        };
        let Ok(entries) = fs::read_dir(&self.root) else {
            return Ok(());
        };
        // Phase 1: user-owned cache dirs (cheap, no sizing yet).
        let mut candidates: Vec<(PathBuf, String)> = Vec::new();
        for entry in entries.flatten() {
            if cancel.is_cancelled() {
                return Err(ScanError::Cancelled);
            }
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.is_dir() || meta.uid() != uid {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            candidates.push((entry.path(), name));
        }
        // Phase 2: size concurrently.
        let sized: Vec<(PathBuf, String, u64)> = candidates
            .into_par_iter()
            .map(|(p, name)| -> Result<(PathBuf, String, u64), ScanError> {
                let size = dir_size(&p, cancel)?;
                Ok((p, name, size))
            })
            .collect::<Result<Vec<_>, _>>()?;
        // Phase 3: emit.
        for (path, name, size) in sized {
            sink(CleanupItem::new(
                path,
                Category::UserCache,
                size,
                SafetyTier::Review,
                format!("System cache for {name} — review before removing"),
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// LogScanner
// ---------------------------------------------------------------------------

/// Finds log FILES under `~/Library/Logs` older than 30 days — always
/// individual files, never a directory, so nothing current can ride along
/// with the stale bytes.
pub struct LogScanner;

/// Policy for a log sweep. `owner`, when `Some`, restricts to files owned by that
/// uid — used for system log dirs (/var/log, /Library/Logs) so only the user's
/// own removable logs are offered, never root-owned system logs that couldn't be
/// trashed anyway. `tier` lets system logs be `Review` while user logs stay `Safe`.
struct LogPolicy<'a> {
    max_age: Duration,
    app: &'a str,
    owner: Option<u32>,
    tier: SafetyTier,
}

/// Emit one item per stale file anywhere under `dir`, per `policy`.
fn emit_stale_logs(
    dir: &Path,
    now: SystemTime,
    cancel: &CancelToken,
    sink: &mut dyn FnMut(CleanupItem),
    policy: &LogPolicy,
) -> Result<(), ScanError> {
    use std::os::unix::fs::MetadataExt;
    if cancel.is_cancelled() {
        return Err(ScanError::Cancelled);
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            emit_stale_logs(&entry.path(), now, cancel, sink, policy)?;
        } else if meta.is_file()
            && is_older_than(&meta, policy.max_age, now)
            && policy.owner.is_none_or(|uid| meta.uid() == uid)
        {
            // Each stale FILE is its own item. Never the directory: a dir mixes
            // stale and current logs, so trashing it would delete live data the
            // size/reason never mentioned.
            sink(CleanupItem::new(
                entry.path(),
                Category::Log,
                meta.len(),
                policy.tier,
                format!("Log from {} older than 30 days", policy.app),
            ));
        }
    }
    Ok(())
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
        // 1. User logs under ~/Library/Logs — all user-owned, Safe.
        let root = ctx.home.join("Library/Logs");
        if let Ok(entries) = fs::read_dir(&root) {
            for entry in entries.flatten() {
                if cancel.is_cancelled() {
                    return Err(ScanError::Cancelled);
                }
                let Ok(meta) = entry.metadata() else { continue };
                let name = entry.file_name().to_string_lossy().into_owned();
                if meta.is_dir() {
                    emit_stale_logs(
                        &entry.path(),
                        now,
                        cancel,
                        sink,
                        &LogPolicy {
                            max_age: THIRTY_DAYS,
                            app: &name,
                            owner: None,
                            tier: SafetyTier::Safe,
                        },
                    )?;
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
        }
        // 2. System-wide logs (/private/var/log, /Library/Logs) — restricted to
        //    the user's OWN stale files (never root-owned system logs), Review.
        let owner = {
            use std::os::unix::fs::MetadataExt;
            fs::metadata(&ctx.home).ok().map(|m| m.uid())
        };
        if let Some(uid) = owner {
            for sys in ["/private/var/log", "/Library/Logs"] {
                emit_stale_logs(
                    Path::new(sys),
                    now,
                    cancel,
                    sink,
                    &LogPolicy {
                        max_age: THIRTY_DAYS,
                        app: "the system",
                        owner: Some(uid),
                        tier: SafetyTier::Review,
                    },
                )?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod unit_tests {
    use super::{looks_like_bundle_id, trash_accessible};

    #[test]
    #[cfg(unix)]
    fn trash_accessible_flags_permission_denied_not_missing() {
        use std::os::unix::fs::PermissionsExt;
        let base = tempfile::tempdir().unwrap();
        let readable = base.path().join("readable");
        std::fs::create_dir(&readable).unwrap();
        let missing = base.path().join("missing");
        // Readable dir + a missing dir → accessible (missing is not a permission
        // problem, just nothing there).
        assert!(trash_accessible(&[readable.clone(), missing]));

        // A dir with no read/exec permission → NOT accessible (the ~/.Trash-
        // without-Full-Disk-Access case: reads fail with PermissionDenied).
        let locked = base.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        assert!(!trash_accessible(&[readable, locked.clone()]));
        // Restore perms so the tempdir can be cleaned up.
        let _ = std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700));
    }

    #[test]
    fn container_cache_roots_are_only_caches_never_app_data() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        let mk = |p: &str| {
            let f = h.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(&f, b"x").unwrap();
        };
        mk("Library/Containers/com.foo.bar/Data/Library/Caches/c1");
        // Real app DATA next to the cache — MUST NOT be returned as a root.
        mk("Library/Containers/com.foo.bar/Data/Library/Application Support/db");
        mk("Library/Group Containers/GRP.com.foo/Library/Caches/g1");

        let roots = super::container_cache_roots(h);
        assert!(roots
            .iter()
            .any(|r| r.ends_with("Containers/com.foo.bar/Data/Library/Caches")));
        assert!(roots
            .iter()
            .any(|r| r.ends_with("Group Containers/GRP.com.foo/Library/Caches")));
        assert!(
            !roots
                .iter()
                .any(|r| r.to_string_lossy().contains("Application Support")),
            "only Caches subdirs — never the container's app data"
        );
    }

    #[test]
    fn dev_targets_never_under_library_caches() {
        // Guards against re-introducing the double-emit: UserCacheScanner already
        // emits every ~/Library/Caches folder, so a DEV_CACHE_TARGETS entry there
        // would emit the same path twice (the engine does not dedup).
        for (rel, _, _) in super::DEV_CACHE_TARGETS {
            assert!(
                !rel.starts_with("Library/Caches"),
                "{rel} is under Library/Caches — would double-emit with UserCacheScanner"
            );
        }
    }

    #[test]
    fn dev_cache_roots_cover_every_target() {
        let home = std::path::Path::new("/Users/test");
        let roots = super::dev_cache_roots(home);
        assert_eq!(roots.len(), super::DEV_CACHE_TARGETS.len());
        for (rel, _, _) in super::DEV_CACHE_TARGETS {
            assert!(
                roots.contains(&home.join(rel)),
                "missing reclaim root for {rel}"
            );
        }
    }

    #[test]
    fn user_cache_tiers_known_regenerable_folders_safe() {
        use tabibu_engine::{CancelToken, SafetyTier, ScanCtx, Scanner};
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        for (folder, file) in [
            ("Homebrew", "x"),
            ("SomeRandomThing", "y"),
            ("com.apple.duetexpertd", "d"), // cache of a daemon (no bundle id)
            ("TemporaryItems", "t"),        // owned by TempScanner
        ] {
            std::fs::create_dir_all(h.join("Library/Caches").join(folder)).unwrap();
            std::fs::write(h.join("Library/Caches").join(folder).join(file), b"z").unwrap();
        }
        std::fs::create_dir_all(h.join("Library/Caches/com.empty.app")).unwrap(); // 0 bytes
        let ctx = ScanCtx {
            home: h.to_path_buf(),
            allowed_roots: vec![h.join("Library/Caches")],
            // A daemon is only known by its executable name.
            running_bundle_ids: std::collections::HashSet::from(["duetexpertd".to_string()]),
            full_disk_access: true,
        };
        let mut items = Vec::new();
        super::UserCacheScanner::home_only()
            .scan(&ctx, &CancelToken::new(), &mut |it| items.push(it))
            .unwrap();
        let tier_of = |needle: &str| {
            items
                .iter()
                .find(|i| i.path.ends_with(needle))
                .unwrap_or_else(|| panic!("no item for {needle}"))
                .tier
        };
        // Known regenerable tool cache → Safe (auto-selectable); unknown
        // non-bundle-id folder → Review (surfaced, never auto-selected).
        assert_eq!(tier_of("Homebrew"), SafetyTier::Safe);
        assert_eq!(tier_of("SomeRandomThing"), SafetyTier::Review);
        // A running daemon's cache is Review, not "Safe (not running)".
        assert_eq!(tier_of("com.apple.duetexpertd"), SafetyTier::Review);
        // Not emitted at all: TempScanner's folder (would double-count) and an
        // empty cache folder (frees nothing).
        for absent in ["TemporaryItems", "com.empty.app"] {
            assert!(
                !items.iter().any(|i| i.path.ends_with(absent)),
                "{absent} must not be emitted"
            );
        }
    }

    #[test]
    fn app_support_cache_roots_finds_chromium_leaves_never_data() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        let mk = |p: &str| {
            let f = h.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(&f, b"x").unwrap();
        };
        // Chromium/Electron cache leaves at various depths.
        mk("Library/Application Support/Google/Chrome/Default/Cache/data_0");
        mk("Library/Application Support/Google/Chrome/Default/Code Cache/js/x");
        mk("Library/Application Support/Google/Chrome/Default/Service Worker/CacheStorage/y");
        mk("Library/Application Support/Slack/GPUCache/z");
        // App DATA that must NEVER be matched.
        mk("Library/Application Support/Google/Chrome/Default/Local Storage/leveldb/d");
        mk("Library/Application Support/Spotify/PersistentCache/Storage/s");
        // "cache" in the name is not proof of a disposable cache — none of these
        // may be matched, nor anything inside them:
        mk("Library/Application Support/Spotify/PersistentCache/Cache/s"); // inside a skipped dir
        mk("Library/Application Support/Zed/languages/x/node_modules/lru-cache/index.js"); // a package
        mk("Library/Application Support/Google/DriveFS/1/content_cache/chunks.db"); // live store
        mk("Library/Application Support/GitTool/.cache/KEYREGISTRY"); // hidden app state
        mk("Library/Application Support/Blender/scripts/__pycache__/a.pyc"); // app bytecode

        let roots = super::app_support_cache_roots(h);
        for skipped in ["node_modules", "content_cache", ".cache", "__pycache__"] {
            assert!(
                !roots.iter().any(|r| r.to_string_lossy().contains(skipped)),
                "{skipped} must never be matched as a cache"
            );
        }
        let has = |suffix: &str| roots.iter().any(|r| r.ends_with(suffix));
        assert!(has("Google/Chrome/Default/Cache"), "HTTP Cache leaf found");
        assert!(
            has("Google/Chrome/Default/Code Cache"),
            "Code Cache leaf found"
        );
        assert!(
            has("Google/Chrome/Default/Service Worker/CacheStorage"),
            "Service Worker CacheStorage leaf found (deepest level)"
        );
        assert!(has("Slack/GPUCache"), "Electron app leaf found");
        assert!(
            !roots
                .iter()
                .any(|r| r.to_string_lossy().contains("Local Storage")),
            "Local Storage (data) must be excluded"
        );
        assert!(
            !roots
                .iter()
                .any(|r| r.to_string_lossy().contains("PersistentCache")),
            "Spotify PersistentCache (offline media) must be excluded"
        );
        // A matched leaf is never descended into (one "Cache", not nested dupes).
        assert_eq!(roots.iter().filter(|r| r.ends_with("Cache")).count(), 1);
    }

    #[test]
    fn container_scanner_sweeps_caches_running_guard_and_tiers() {
        use super::*;
        use std::collections::HashSet;
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        let mk = |p: &str| {
            let f = h.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(&f, b"data").unwrap();
        };
        mk("Library/Containers/com.foo.bar/Data/Library/Caches/c1"); // not running → Safe
        mk("Library/Containers/com.run.ning/Data/Library/Caches/c2"); // running → Review (surfaced)
        mk("Library/Group Containers/GRP.com.foo/Library/Caches/g1"); // group → Review
                                                                      // App data alongside a cache — a regression guard that it's never emitted.
        mk("Library/Containers/com.foo.bar/Data/Library/Application Support/db");

        let ctx = ScanCtx {
            home: h.to_path_buf(),
            allowed_roots: vec![h.to_path_buf()],
            running_bundle_ids: HashSet::from(["com.run.ning".to_string()]),
            full_disk_access: true,
        };
        let mut items = Vec::new();
        ContainerCacheScanner
            .scan(&ctx, &CancelToken::new(), &mut |i| items.push(i))
            .unwrap();
        let has = |needle: &str| {
            items
                .iter()
                .any(|i| i.path.to_string_lossy().contains(needle))
        };
        assert!(
            has("com.foo.bar/Data/Library/Caches"),
            "non-running cache swept"
        );
        // Running sandboxed apps' caches are surfaced (Review), not skipped.
        let running = items
            .iter()
            .find(|i| i.path.to_string_lossy().contains("com.run.ning"))
            .expect("running sandboxed cache is surfaced, not skipped");
        assert_eq!(running.tier, SafetyTier::Review);
        assert!(has("GRP.com.foo/Library/Caches"), "group cache swept");
        assert!(
            items.iter().all(|i| i.path.ends_with("Caches")),
            "ONLY Caches dirs are emitted — never app data"
        );
        assert!(!has("Application Support"));
        let foo = items
            .iter()
            .find(|i| i.path.to_string_lossy().contains("com.foo.bar"))
            .unwrap();
        assert_eq!(foo.tier, SafetyTier::Safe);
        let grp = items
            .iter()
            .find(|i| i.path.to_string_lossy().contains("GRP.com.foo"))
            .unwrap();
        assert_eq!(grp.tier, SafetyTier::Review);
    }

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
