//! The only mutating path in the product. Enforces, in order:
//! 1. every target passes the denylist + allowed-roots check again,
//! 2. tier rules (`Delete`/`Truncate` only for `Safe` items),
//! 3. undo manifest durably on disk before the first mutation,
//! 4. measured — not estimated — reclaimed bytes in the report.

use crate::denylist;
use crate::item::{CleanupItem, ReclaimAction, SafetyTier};
use crate::scanner::ScanCtx;
use crate::undo::{ManifestEntry, UndoManifest};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum ReclaimError {
    #[error("denied path refused: {0}")]
    Denied(PathBuf),
    #[error("action {action:?} not allowed for tier {tier:?} ({path})")]
    TierViolation {
        path: PathBuf,
        tier: SafetyTier,
        action: ReclaimAction,
    },
    #[error("could not write undo manifest: {0}")]
    Manifest(#[source] std::io::Error),
}

/// Per-item outcome, reported honestly (partial failures are normal).
#[derive(Debug)]
pub struct ItemOutcome {
    pub path: PathBuf,
    pub reclaimed_bytes: u64,
    pub error: Option<String>,
}

#[derive(Debug, Default)]
pub struct ReclaimReport {
    /// Sum of bytes actually freed, measured per item post-op.
    pub reclaimed_bytes: u64,
    pub succeeded: usize,
    pub failed: usize,
    pub outcomes: Vec<ItemOutcome>,
    pub manifest_path: Option<PathBuf>,
}

fn size_on_disk(path: &Path) -> u64 {
    fn walk(p: &Path) -> u64 {
        let Ok(meta) = fs::symlink_metadata(p) else {
            return 0;
        };
        if meta.is_dir() {
            fs::read_dir(p)
                .map(|rd| rd.flatten().map(|e| walk(&e.path())).sum())
                .unwrap_or(0)
        } else {
            meta.len()
        }
    }
    walk(path)
}

fn perform(item: &CleanupItem) -> std::io::Result<()> {
    match item.action {
        ReclaimAction::Trash => trash::delete(&item.path).map_err(std::io::Error::other),
        ReclaimAction::Delete => {
            let meta = fs::symlink_metadata(&item.path)?;
            if meta.is_dir() {
                fs::remove_dir_all(&item.path)
            } else {
                fs::remove_file(&item.path)
            }
        }
        ReclaimAction::Truncate => fs::File::options()
            .write(true)
            .truncate(true)
            .open(&item.path)
            .map(|_| ()),
    }
}

/// Reclaim the **selected** items. Fails fast on contract violations
/// (denylist, tier rules, manifest write); per-item I/O failures are
/// recorded and skipped, never hidden.
///
/// # Errors
/// [`ReclaimError::Denied`] / [`ReclaimError::TierViolation`] if any selected
/// item violates the contract (nothing is touched in that case), and
/// [`ReclaimError::Manifest`] if the undo manifest cannot be written.
pub fn reclaim(
    ctx: &ScanCtx,
    items: &[CleanupItem],
    undo_dir: &Path,
) -> Result<ReclaimReport, ReclaimError> {
    let selected: Vec<&CleanupItem> = items.iter().filter(|i| i.selected).collect();

    // 1+2: validate the whole batch before touching anything.
    for item in &selected {
        if !denylist::permitted(&item.path, &ctx.allowed_roots, &ctx.home) {
            return Err(ReclaimError::Denied(item.path.clone()));
        }
        if item.tier != SafetyTier::Safe && item.action != ReclaimAction::Trash {
            return Err(ReclaimError::TierViolation {
                path: item.path.clone(),
                tier: item.tier,
                action: item.action,
            });
        }
    }

    // 3: manifest on disk before the first mutation.
    let entries = selected
        .iter()
        .map(|i| ManifestEntry {
            path: i.path.clone(),
            category: i.category,
            size_bytes: i.size_bytes,
            tier: i.tier,
            action: i.action,
            completed: false,
        })
        .collect();
    let mut manifest = UndoManifest::create(undo_dir, entries).map_err(ReclaimError::Manifest)?;

    // 4: act, measuring true before/after sizes per item.
    let mut report = ReclaimReport {
        manifest_path: Some(manifest.path().to_path_buf()),
        ..ReclaimReport::default()
    };
    for (idx, item) in selected.iter().enumerate() {
        let before = size_on_disk(&item.path);
        match perform(item) {
            Ok(()) => {
                let after = size_on_disk(&item.path); // 0 unless truncate left the file
                let freed = before.saturating_sub(after);
                report.reclaimed_bytes += freed;
                report.succeeded += 1;
                report.outcomes.push(ItemOutcome {
                    path: item.path.clone(),
                    reclaimed_bytes: freed,
                    error: None,
                });
                // Manifest update failing must not abort a half-done reclaim;
                // the entry simply stays marked incomplete, which is truthful.
                let _ = manifest.mark_completed(idx);
            }
            Err(e) => {
                report.failed += 1;
                report.outcomes.push(ItemOutcome {
                    path: item.path.clone(),
                    reclaimed_bytes: 0,
                    error: Some(e.to_string()),
                });
            }
        }
    }
    Ok(report)
}
