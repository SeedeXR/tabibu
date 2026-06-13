//! Small filesystem helpers shared across the crate.

use std::fs;
use std::path::Path;

/// Recursive size in bytes via `symlink_metadata`: symlinks count as the
/// link itself and are never followed. Unreadable entries count as zero —
/// sizing must never abort a scan.
pub(crate) fn size_of(path: &Path) -> u64 {
    let Ok(meta) = path.symlink_metadata() else {
        return 0;
    };
    if !meta.is_dir() {
        return meta.len();
    }
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    entries.flatten().map(|entry| size_of(&entry.path())).sum()
}
