//! Large & old files (guide §6.14): *surface* user files that are big and
//! stale — Downloads only, Review tier, never auto-selected. We only ever
//! suggest; the user decides.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};
use tabibu_engine::{CancelToken, Category, CleanupItem, SafetyTier, ScanCtx, ScanError, Scanner};

/// Files qualify when they are ≥ `LARGE_BYTES`, or ≥ `MEDIUM_BYTES` and
/// untouched for ≥ `STALE_DAYS`.
const LARGE_BYTES: u64 = 500 * 1024 * 1024;
const MEDIUM_BYTES: u64 = 50 * 1024 * 1024;
const STALE_DAYS: u64 = 180;

pub struct LargeOldScanner {
    /// Home-relative directories to inspect (default: Downloads).
    rel_roots: Vec<PathBuf>,
}

impl Default for LargeOldScanner {
    fn default() -> Self {
        Self {
            rel_roots: vec![PathBuf::from("Downloads")],
        }
    }
}

impl LargeOldScanner {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_rel_roots(rel_roots: Vec<PathBuf>) -> Self {
        Self { rel_roots }
    }
}

fn days_old(mtime: SystemTime) -> u64 {
    SystemTime::now()
        .duration_since(mtime)
        .unwrap_or(Duration::ZERO)
        .as_secs()
        / 86_400
}

impl Scanner for LargeOldScanner {
    fn id(&self) -> &'static str {
        "large_old"
    }

    fn scan(
        &self,
        ctx: &ScanCtx,
        cancel: &CancelToken,
        sink: &mut dyn FnMut(CleanupItem),
    ) -> Result<(), ScanError> {
        for rel in &self.rel_roots {
            if cancel.is_cancelled() {
                return Err(ScanError::Cancelled);
            }
            let root = ctx.home.join(rel);
            let Ok(entries) = std::fs::read_dir(&root) else {
                continue;
            };
            for entry in entries.flatten() {
                if cancel.is_cancelled() {
                    return Err(ScanError::Cancelled);
                }
                let Ok(meta) = entry.metadata() else { continue };
                if !meta.is_file() {
                    continue;
                }
                let size = meta.len();
                let age = meta.modified().map(days_old).unwrap_or(0);
                let qualifies = size >= LARGE_BYTES || (size >= MEDIUM_BYTES && age >= STALE_DAYS);
                if !qualifies {
                    continue;
                }
                let reason = if age >= STALE_DAYS {
                    format!("Large file not modified in {age} days")
                } else {
                    "Very large file — verify you still need it".to_string()
                };
                sink(CleanupItem::new(
                    entry.path(),
                    Category::LargeOldFile,
                    size,
                    SafetyTier::Review,
                    reason,
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::fs;

    fn ctx(home: &std::path::Path) -> ScanCtx {
        ScanCtx {
            home: home.to_path_buf(),
            allowed_roots: vec![home.join("Downloads")],
            running_bundle_ids: HashSet::new(),
            full_disk_access: true,
        }
    }

    #[test]
    fn surfaces_only_qualifying_files_at_review_tier() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dl = tmp.path().join("Downloads");
        fs::create_dir_all(&dl).expect("mkdir");
        // Big enough outright (sparse write to keep the test fast).
        let big = dl.join("huge.iso");
        let f = fs::File::create(&big).expect("create");
        f.set_len(LARGE_BYTES + 1).expect("set_len");
        // Small and fresh: must not appear.
        fs::write(dl.join("note.txt"), b"hi").expect("write");

        let mut found = Vec::new();
        let mut sink = |i: CleanupItem| found.push(i);
        LargeOldScanner::new()
            .scan(&ctx(tmp.path()), &CancelToken::new(), &mut sink)
            .expect("scan");

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, big);
        assert_eq!(found[0].tier, SafetyTier::Review);
        assert!(!found[0].selected, "Review tier is never pre-selected");
    }

    #[test]
    fn cancellation_honored() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(tmp.path().join("Downloads")).expect("mkdir");
        let cancel = CancelToken::new();
        cancel.cancel();
        let mut sink = |_| {};
        let err = LargeOldScanner::new().scan(&ctx(tmp.path()), &cancel, &mut sink);
        assert!(matches!(err, Err(ScanError::Cancelled)));
    }
}
