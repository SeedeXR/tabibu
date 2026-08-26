//! The only mutating path in the product. Enforces, in order:
//! 1. every target passes the denylist + allowed-roots check again,
//! 2. tier rules (`Delete`/`Truncate` only for `Safe` items),
//! 3. undo manifest durably on disk before the first mutation,
//! 4. measured — not estimated — reclaimed bytes in the report.

use crate::denylist::{self, DenyReason};
use crate::item::{Category, CleanupItem, ReclaimAction, SafetyTier};
use crate::protect;
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

/// Move `path` to the Trash quietly. macOS's default trash mechanism (the
/// `trash` crate's `Finder`/AppleScript backend) plays the Finder trash sound
/// on EVERY call and spawns `osascript` each time — so reclaiming hundreds of
/// cache items machine-guns that sound and is slow. `NsFileManager`
/// (`trashItemAtURL`) trashes the same items — still recoverable from the
/// Trash — with no sound and no per-item process spawn. Centralized here so
/// every reclaim flow (junk, duplicates, uninstaller) is quiet and fast.
///
/// # Errors
/// Any failure moving `path` to the Trash.
pub fn move_to_trash(path: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        use trash::macos::{DeleteMethod, TrashContextExtMacos};
        let mut ctx = trash::TrashContext::default();
        ctx.set_delete_method(DeleteMethod::NsFileManager);
        ctx.delete(path).map_err(std::io::Error::other)
    }
    #[cfg(not(target_os = "macos"))]
    {
        trash::delete(path).map_err(std::io::Error::other)
    }
}

fn perform(item: &CleanupItem) -> std::io::Result<()> {
    match item.action {
        ReclaimAction::Trash => move_to_trash(&item.path),
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
    // User-managed protected paths (shared with the app), loaded once. Anything
    // overlapping an entry is skipped exactly like a denylisted path.
    let user_protected = protect::load(&ctx.home);
    let mut to_act: Vec<&CleanupItem> = Vec::new();
    for item in &selected {
        // A recognized rebuildable dev artifact (`DevCache`) may be reclaimed even
        // from a user-data dir like ~/Documents or ~/Desktop — that's where many
        // developers keep projects, the folder is not user data, and the move is
        // reversible (Trash). System paths, path traversal, and the user's own
        // protected list are NEVER overridden — only the user-data (`UserData`)
        // denial is, and only for a `DevCache` item being moved to the Trash.
        let deny = denylist::denied(&item.path, &ctx.home);
        let dev_override = item.category == Category::DevCache
            && item.action == ReclaimAction::Trash
            && matches!(deny, Some(DenyReason::UserData));
        if !denylist::within_roots(&item.path, &ctx.allowed_roots)
            || (deny.is_some() && !dev_override)
            || protect::is_protected(&item.path, &user_protected)
        {
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
                manifest.mark_completed(idx);
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
    // One durable write for all completion flags (per-item rewrites were
    // O(n²) bytes + an fsync storm). Failure must not abort a done reclaim;
    // entries staying incomplete on disk errs in the safe direction.
    let _ = manifest.persist();
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::Category;
    use crate::scanner::ScanCtx;
    use std::collections::HashSet;

    /// Safety regression: an item under a user-protected path is skipped and the
    /// file survives, even though it is otherwise a valid, selected Safe target.
    #[test]
    fn user_protected_paths_are_never_reclaimed() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        let keep_dir = home.join("keep");
        std::fs::create_dir_all(&keep_dir).unwrap();
        let file = keep_dir.join("data.bin");
        std::fs::write(&file, b"important").unwrap();

        protect::add(&home, &keep_dir).unwrap();

        let ctx = ScanCtx {
            home: home.clone(),
            allowed_roots: vec![home.clone()],
            running_bundle_ids: HashSet::new(),
            full_disk_access: false,
        };
        let item = CleanupItem::new(
            file.clone(),
            Category::UserCache,
            9,
            SafetyTier::Safe,
            "cache",
        );
        let report = reclaim(&ctx, &[item], &home.join("undo")).unwrap();

        assert_eq!(report.succeeded, 0);
        assert_eq!(report.failed, 1);
        assert!(file.exists(), "protected file must NOT be trashed");
    }

    /// A rebuildable dev artifact (`DevCache`) under a user-data dir like
    /// ~/Documents IS reclaimable (developers keep projects there; reversible
    /// Trash) — but any OTHER item in that denied tree stays protected.
    #[test]
    fn dev_artifacts_are_reclaimable_from_user_data_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        let ctx = ScanCtx {
            home: home.clone(),
            allowed_roots: vec![home.clone()],
            running_bundle_ids: HashSet::new(),
            full_disk_access: false,
        };

        // DevCache + Trash under ~/Documents → the override lets it through.
        let art = home.join("Documents/proj/node_modules");
        std::fs::create_dir_all(&art).unwrap();
        std::fs::write(art.join("f"), b"x").unwrap();
        let dev = CleanupItem::new(art, Category::DevCache, 1, SafetyTier::Safe, "node_modules");
        let r = reclaim(&ctx, &[dev], &home.join("undo")).unwrap();
        assert_eq!(
            r.failed, 0,
            "a DevCache artifact under Documents is not skipped"
        );
        assert_eq!(r.succeeded, 1);

        // Control: a NON-DevCache item in the same denied tree stays protected.
        let keep = home.join("Documents/proj/thesis.txt");
        std::fs::write(&keep, b"y").unwrap();
        let item = CleanupItem::new(
            keep.clone(),
            Category::LargeOldFile,
            1,
            SafetyTier::Safe,
            "big",
        );
        let r2 = reclaim(&ctx, &[item], &home.join("undo")).unwrap();
        assert_eq!(r2.failed, 1, "non-DevCache under Documents is refused");
        assert!(keep.exists(), "the non-artifact file is untouched");
    }
}
