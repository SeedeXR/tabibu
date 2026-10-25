//! Fast parallel filesystem traversal and size-tree construction for Tabibu.
//!
//! This crate provides two entry points:
//!
//! - [`size_tree`] — builds a [`DirNode`] tree rooted at a path, with every
//!   directory's `size_bytes` equal to the sum of its descendants. Children
//!   are sorted by size, largest first. An optional `max_depth` limits how
//!   deep the *retained* tree is; sizes below the cutoff still aggregate
//!   into their ancestors.
//! - [`dir_size`] — convenience wrapper returning just the total size.
//!
//! # Concurrency model
//!
//! Traversal recurses on rayon's work-stealing pool: each directory's
//! entries are processed with a parallel iterator, and subdirectories
//! recurse on whichever worker picks them up. There is no shared mutable
//! state; each subtree returns its result up the call tree.
//!
//! # Symlinks
//!
//! Symbolic links are **never** followed. Every entry is inspected with
//! [`std::fs::symlink_metadata`], so a link contributes the size of the
//! link object itself and cycles cannot occur.
//!
//! # Errors and cancellation
//!
//! Permission-denied and transient I/O errors on individual entries are
//! skipped; only an unreadable *root* is reported as [`WalkError::Root`].
//! The [`CancelToken`] is checked at every directory boundary and a
//! cancelled walk returns [`WalkError::Cancelled`] promptly.

use std::fs::{self, Metadata};
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use serde::Serialize;
pub use tabibu_engine::CancelToken;

/// Errors returned by the traversal functions.
#[derive(Debug, thiserror::Error)]
pub enum WalkError {
    /// The walk was cancelled via its [`CancelToken`].
    #[error("walk cancelled")]
    Cancelled,
    /// The root path itself could not be read. Errors on entries *below*
    /// the root are skipped, never fatal.
    #[error("cannot read root {path}: {source}")]
    Root {
        /// The root path that failed.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },
}

/// One node of a size tree produced by [`size_tree`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DirNode {
    /// Path of this entry.
    pub path: PathBuf,
    /// Total LOGICAL size in bytes (sum of file `len()`). For directories this
    /// is the aggregate of all descendants, including those pruned by
    /// `max_depth`. Matches Finder's "Get Info" folder size, but can far exceed
    /// real disk usage (sparse files, APFS clones, iCloud dataless placeholders
    /// all report full length) — for "what's using my disk" use `alloc_bytes`.
    pub size_bytes: u64,
    /// Total PHYSICAL size in bytes actually allocated on disk (sum of
    /// `st_blocks * 512`), the `du`-style number. This is what a space/disk view
    /// should show — it reflects real consumption and won't exceed the volume
    /// (sparse/clone/dataless files allocate few or no blocks). Aggregated like
    /// `size_bytes`. (Hardlinks are not de-duplicated, so it can still read a few
    /// percent above `du` on hardlink-heavy trees.)
    pub alloc_bytes: u64,
    /// Number of descendant entries (files + subdirectories) under this node,
    /// counting the whole subtree even where `max_depth` pruned the retained
    /// children. 0 for a file. Powers the "N items" counts in the UI and the
    /// removal summary. Aggregated like `size_bytes`, independent of pruning.
    pub count: u64,
    /// `true` if this node is a directory.
    pub is_dir: bool,
    /// Child nodes, sorted by `alloc_bytes` (physical) descending — the order
    /// callers display. Empty for files, unreadable directories, and directories
    /// beyond `max_depth`.
    pub children: Vec<DirNode>,
}

/// Builds a size tree rooted at `root` using parallel traversal.
///
/// Children of every directory are sorted by `alloc_bytes` (physical, du-style)
/// descending — the order callers display.
/// `max_depth` bounds the depth of *retained* nodes, counting the root as
/// depth 0: with `Some(0)` the root has no children, with `Some(1)` only
/// the root's direct children are kept, and so on. Sizes beyond the cutoff
/// still aggregate into the retained ancestors. Symlinks are counted but
/// never followed.
///
/// # Errors
///
/// Returns [`WalkError::Cancelled`] if `cancel` is triggered, or
/// [`WalkError::Root`] if `root` itself cannot be read. Unreadable entries
/// below the root are silently skipped.
pub fn size_tree(
    root: &Path,
    cancel: &CancelToken,
    max_depth: Option<usize>,
) -> Result<DirNode, WalkError> {
    let meta = fs::symlink_metadata(root).map_err(|source| WalkError::Root {
        path: root.to_path_buf(),
        source,
    })?;
    if !meta.is_dir() {
        return Ok(DirNode {
            path: root.to_path_buf(),
            size_bytes: meta.len(),
            alloc_bytes: alloc_of(&meta),
            count: 0,
            is_dir: false,
            children: Vec::new(),
        });
    }
    if cancel.is_cancelled() {
        return Err(WalkError::Cancelled);
    }
    let entries = read_entries(root).map_err(|source| WalkError::Root {
        path: root.to_path_buf(),
        source,
    })?;
    build_children(
        root.to_path_buf(),
        alloc_of(&meta),
        entries,
        0,
        max_depth,
        cancel,
    )
}

/// Returns the total size in bytes of everything under `root`.
///
/// Equivalent to `size_tree(root, cancel, Some(0))?.size_bytes`.
///
/// # Errors
///
/// Returns [`WalkError::Cancelled`] if `cancel` is triggered, or
/// [`WalkError::Root`] if `root` itself cannot be read.
pub fn dir_size(root: &Path, cancel: &CancelToken) -> Result<u64, WalkError> {
    size_tree(root, cancel, Some(0)).map(|node| node.size_bytes)
}

/// Physical bytes a file occupies on disk — `st_blocks` are 512-byte units, the
/// `du`-style measure. Near-0 for sparse, APFS-cloned, or iCloud-dataless files
/// whose `len()` reports full logical size, so summing this gives a disk total
/// that reflects real consumption instead of exceeding the volume.
#[cfg(unix)]
fn alloc_of(meta: &Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.blocks().saturating_mul(512)
}
#[cfg(not(unix))]
fn alloc_of(meta: &Metadata) -> u64 {
    meta.len()
}

/// Reads a directory, returning the path and `symlink_metadata` of each
/// entry. Entries whose metadata cannot be read are skipped; an error is
/// returned only if the directory itself cannot be opened.
fn read_entries(dir: &Path) -> std::io::Result<Vec<(PathBuf, Metadata)>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir)? {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        out.push((path, meta));
    }
    Ok(out)
}

/// Builds the [`DirNode`] for a directory at `depth` whose entries have
/// already been read. `own_alloc` is the directory inode's own allocated bytes
/// (so `alloc_bytes` counts the directory itself, like `du`, not just its
/// contents). Children are processed in parallel.
fn build_children(
    path: PathBuf,
    own_alloc: u64,
    entries: Vec<(PathBuf, Metadata)>,
    depth: usize,
    max_depth: Option<usize>,
    cancel: &CancelToken,
) -> Result<DirNode, WalkError> {
    let mut children = entries
        .into_par_iter()
        .map(|(child_path, meta)| {
            if meta.is_dir() {
                build_dir(child_path, alloc_of(&meta), depth + 1, max_depth, cancel)
            } else {
                Ok(DirNode {
                    path: child_path,
                    size_bytes: meta.len(),
                    alloc_bytes: alloc_of(&meta),
                    count: 0,
                    is_dir: false,
                    children: Vec::new(),
                })
            }
        })
        .collect::<Result<Vec<_>, WalkError>>()?;
    let size_bytes = children.iter().map(|c| c.size_bytes).sum();
    let alloc_bytes = own_alloc + children.iter().map(|c| c.alloc_bytes).sum::<u64>();
    // Every child is one entry; each adds its own subtree's entries too. Summed
    // before any `max_depth` pruning so the count reflects the whole subtree.
    let count = children.len() as u64 + children.iter().map(|c| c.count).sum::<u64>();
    // Sort by PHYSICAL size — that's what callers display/scale on (the space
    // view, treemap, bubbles, CLI tree), so rows stay largest-first by the number
    // actually shown. Logical size breaks ties for a stable order.
    children.sort_unstable_by_key(|c| {
        (
            std::cmp::Reverse(c.alloc_bytes),
            std::cmp::Reverse(c.size_bytes),
        )
    });
    if max_depth.is_some_and(|m| depth >= m) {
        children.clear();
    }
    Ok(DirNode {
        path,
        size_bytes,
        alloc_bytes,
        count,
        is_dir: true,
        children,
    })
}

/// Recursive worker for [`size_tree`] below the root: unreadable
/// directories yield an empty node instead of an error. `own_alloc` is this
/// directory inode's own allocated bytes, forwarded to [`build_children`].
fn build_dir(
    path: PathBuf,
    own_alloc: u64,
    depth: usize,
    max_depth: Option<usize>,
    cancel: &CancelToken,
) -> Result<DirNode, WalkError> {
    if cancel.is_cancelled() {
        return Err(WalkError::Cancelled);
    }
    let entries = read_entries(&path).unwrap_or_default();
    build_children(path, own_alloc, entries, depth, max_depth, cancel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;

    fn write_file(path: &Path, len: usize) {
        let mut file = File::create(path).unwrap();
        file.write_all(&vec![0u8; len]).unwrap();
    }

    #[test]
    fn file_root_is_leaf_node() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f.bin");
        write_file(&file, 123);
        let node = size_tree(&file, &CancelToken::new(), None).unwrap();
        assert_eq!(node.size_bytes, 123);
        assert!(!node.is_dir);
        assert!(node.children.is_empty());
    }

    #[test]
    fn children_sorted_descending() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("small"), 10);
        write_file(&dir.path().join("big"), 1000);
        write_file(&dir.path().join("mid"), 100);
        let node = size_tree(dir.path(), &CancelToken::new(), None).unwrap();
        let sizes: Vec<u64> = node.children.iter().map(|c| c.size_bytes).collect();
        assert_eq!(sizes, vec![1000, 100, 10]);
        assert_eq!(node.size_bytes, 1110);
        assert_eq!(node.count, 3, "three files = three descendant entries");
        assert!(
            node.children.iter().all(|c| c.count == 0),
            "files have no descendants"
        );
        // alloc_bytes is st_blocks*512 (du-style) — block-aligned, the number a
        // disk view should show instead of logical size_bytes.
        assert_eq!(
            node.alloc_bytes % 512,
            0,
            "allocated size is 512-block aligned"
        );
    }

    #[test]
    fn count_aggregates_nested_entries() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        write_file(&dir.path().join("top.bin"), 1);
        write_file(&sub.join("a.bin"), 1);
        write_file(&sub.join("b.bin"), 1);
        // entries: top.bin, sub, sub/a.bin, sub/b.bin = 4
        let node = size_tree(dir.path(), &CancelToken::new(), None).unwrap();
        assert_eq!(node.count, 4);
        let sub_node = node.children.iter().find(|c| c.is_dir).unwrap();
        assert_eq!(sub_node.count, 2);
    }

    #[test]
    fn missing_root_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let gone = dir.path().join("nope");
        let err = size_tree(&gone, &CancelToken::new(), None).unwrap_err();
        assert!(matches!(err, WalkError::Root { .. }));
    }

    #[test]
    fn dirnode_serializes() {
        let node = DirNode {
            path: PathBuf::from("/tmp/x"),
            size_bytes: 7,
            alloc_bytes: 4096,
            count: 0,
            is_dir: true,
            children: Vec::new(),
        };
        let json = serde_json::to_string(&node).unwrap();
        assert!(json.contains("\"size_bytes\":7"));
        assert!(json.contains("\"alloc_bytes\":4096"));
        assert!(json.contains("\"count\":0"));
        assert!(json.contains("\"is_dir\":true"));
    }
}
