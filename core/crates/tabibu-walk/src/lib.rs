//! Fast parallel filesystem traversal and size-tree construction for Tabibu.
//!
//! This crate provides three entry points:
//!
//! - [`size_tree`] — builds a [`DirNode`] tree rooted at a path, with every
//!   directory's `size_bytes` equal to the sum of its descendants. Children
//!   are sorted by size, largest first. An optional `max_depth` limits how
//!   deep the *retained* tree is; sizes below the cutoff still aggregate
//!   into their ancestors.
//! - [`walk_files`] — streams every regular file under a root to a caller
//!   supplied callback, in parallel. Used by the dedupe and old-files
//!   scanners.
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
    /// Total size in bytes. For directories this is the aggregate of all
    /// descendants, including those pruned by `max_depth`.
    pub size_bytes: u64,
    /// `true` if this node is a directory.
    pub is_dir: bool,
    /// Child nodes, sorted by `size_bytes` descending. Empty for files,
    /// for unreadable directories, and for directories beyond `max_depth`.
    pub children: Vec<DirNode>,
}

/// Builds a size tree rooted at `root` using parallel traversal.
///
/// Children of every directory are sorted by `size_bytes` descending.
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
    build_children(root.to_path_buf(), entries, 0, max_depth, cancel)
}

/// Visits every regular file under `root` in parallel, invoking `f` with
/// the file's path and its [`symlink_metadata`](fs::symlink_metadata).
///
/// Symlinks are never followed and are not reported (a symlink is not a
/// regular file). If `root` is itself a regular file, `f` is invoked once
/// for it. The callback runs concurrently on rayon workers and must be
/// `Sync`.
///
/// # Errors
///
/// Returns [`WalkError::Cancelled`] if `cancel` is triggered, or
/// [`WalkError::Root`] if `root` itself cannot be read. Unreadable entries
/// below the root are silently skipped.
pub fn walk_files(
    root: &Path,
    cancel: &CancelToken,
    f: &(dyn Fn(&Path, &Metadata) + Sync),
) -> Result<(), WalkError> {
    let meta = fs::symlink_metadata(root).map_err(|source| WalkError::Root {
        path: root.to_path_buf(),
        source,
    })?;
    if cancel.is_cancelled() {
        return Err(WalkError::Cancelled);
    }
    if meta.is_file() {
        f(root, &meta);
        return Ok(());
    }
    if !meta.is_dir() {
        return Ok(()); // symlink or special file at the root: nothing to do
    }
    let entries = read_entries(root).map_err(|source| WalkError::Root {
        path: root.to_path_buf(),
        source,
    })?;
    walk_entries(entries, cancel, f)
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
/// already been read. Children are processed in parallel.
fn build_children(
    path: PathBuf,
    entries: Vec<(PathBuf, Metadata)>,
    depth: usize,
    max_depth: Option<usize>,
    cancel: &CancelToken,
) -> Result<DirNode, WalkError> {
    let mut children = entries
        .into_par_iter()
        .map(|(child_path, meta)| {
            if meta.is_dir() {
                build_dir(child_path, depth + 1, max_depth, cancel)
            } else {
                Ok(DirNode {
                    path: child_path,
                    size_bytes: meta.len(),
                    is_dir: false,
                    children: Vec::new(),
                })
            }
        })
        .collect::<Result<Vec<_>, WalkError>>()?;
    let size_bytes = children.iter().map(|c| c.size_bytes).sum();
    children.sort_unstable_by_key(|c| std::cmp::Reverse(c.size_bytes));
    if max_depth.is_some_and(|m| depth >= m) {
        children.clear();
    }
    Ok(DirNode {
        path,
        size_bytes,
        is_dir: true,
        children,
    })
}

/// Recursive worker for [`size_tree`] below the root: unreadable
/// directories yield an empty node instead of an error.
fn build_dir(
    path: PathBuf,
    depth: usize,
    max_depth: Option<usize>,
    cancel: &CancelToken,
) -> Result<DirNode, WalkError> {
    if cancel.is_cancelled() {
        return Err(WalkError::Cancelled);
    }
    let entries = read_entries(&path).unwrap_or_default();
    build_children(path, entries, depth, max_depth, cancel)
}

/// Recursive worker for [`walk_files`].
fn walk_entries(
    entries: Vec<(PathBuf, Metadata)>,
    cancel: &CancelToken,
    f: &(dyn Fn(&Path, &Metadata) + Sync),
) -> Result<(), WalkError> {
    entries.into_par_iter().try_for_each(|(path, meta)| {
        if meta.is_dir() {
            if cancel.is_cancelled() {
                return Err(WalkError::Cancelled);
            }
            let Ok(children) = read_entries(&path) else {
                return Ok(()); // unreadable subdirectory: skip
            };
            walk_entries(children, cancel, f)
        } else {
            if meta.is_file() {
                f(&path, &meta);
            }
            Ok(())
        }
    })
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
            is_dir: true,
            children: Vec::new(),
        };
        let json = serde_json::to_string(&node).unwrap();
        assert!(json.contains("\"size_bytes\":7"));
        assert!(json.contains("\"is_dir\":true"));
    }
}
