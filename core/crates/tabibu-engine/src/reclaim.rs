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
#[derive(Debug, serde::Serialize)]
pub struct ItemOutcome {
    pub path: PathBuf,
    pub reclaimed_bytes: u64,
    pub error: Option<String>,
}

#[derive(Debug, Default, serde::Serialize)]
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

/// Reclaim the **selected** items.
///
/// Denied (protected) paths are skipped per item — recorded in the report as a
/// failed outcome and never touched — so a batch spanning protected and
/// unprotected locations still reclaims everything it safely can. A non-`Safe`
/// item with a destructive action is a programming error the UI never produces,
/// so that still fails fast. Per-item I/O failures are recorded, never hidden.
///
/// # Errors
/// [`ReclaimError::TierViolation`] if a selected item requests a destructive
/// action on a non-`Safe` tier (nothing is touched in that case), and
/// [`ReclaimError::Manifest`] if the undo manifest cannot be written.
pub fn reclaim(
    ctx: &ScanCtx,
    items: &[CleanupItem],
    undo_dir: &Path,
) -> Result<ReclaimReport, ReclaimError> {
    let selected: Vec<&CleanupItem> = items.iter().filter(|i| i.selected).collect();
    let mut report = ReclaimReport::default();

    // 1+2: validate. Denied paths are SKIPPED (recorded, never touched) rather
    // than aborting the whole batch — so a whole-home duplicate/leftover set
    // can reclaim everything outside protected folders while leaving the
    // protected copies untouched. A non-Safe item with a destructive action is
    // a programming error the UI never produces, so that still fails fast.
    let mut to_act: Vec<&CleanupItem> = Vec::new();
    for item in &selected {
        if !denylist::permitted(&item.path, &ctx.allowed_roots, &ctx.home) {
            report.failed += 1;
            report.outcomes.push(ItemOutcome {
                path: item.path.clone(),
                reclaimed_bytes: 0,
                error: Some("protected location — left untouched".to_string()),
            });
            continue;
        }
        if item.tier != SafetyTier::Safe && item.action != ReclaimAction::Trash {
            return Err(ReclaimError::TierViolation {
                path: item.path.clone(),
                tier: item.tier,
                action: item.action,
            });
        }
        to_act.push(item);
    }

    // Nothing actionable (all skipped / none selected): no manifest, no mutation.
    if to_act.is_empty() {
        return Ok(report);
    }

    // 3: manifest on disk before the first mutation (only the actionable items).
    let entries = to_act
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
    report.manifest_path = Some(manifest.path().to_path_buf());

    // 4: act, measuring true before/after sizes per item.
    for (idx, item) in to_act.iter().enumerate() {
        let before = size_on_disk(&item.path);
        match perform(item) {
            Ok(()) => {
                // Only `Truncate` leaves the path in place, so it's the only
                // action that needs a post-op walk; for `Trash`/`Delete` the
                // path is gone and a second walk would just measure 0 — re-using
                // `before` avoids re-walking a (possibly huge) tree for nothing.
                let freed = if item.action == ReclaimAction::Truncate {
                    before.saturating_sub(size_on_disk(&item.path))
                } else {
                    before
                };
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
