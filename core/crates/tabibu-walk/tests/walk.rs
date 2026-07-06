//! Integration tests for tabibu-walk against a sequential reference
//! implementation and adversarial fixtures (symlink cycles, unreadable
//! directories, cancellation).

use std::fs::{self, File};
use std::io::Write;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::Path;

use tabibu_walk::{dir_size, size_tree, CancelToken, DirNode, WalkError};

fn write_file(path: &Path, len: usize) {
    let mut file = File::create(path).unwrap();
    file.write_all(&vec![7u8; len]).unwrap();
}

/// Sequential reference: total apparent size of everything under `path`,
/// using `symlink_metadata` (never following links), skipping unreadable
/// entries.
fn ref_size(path: &Path) -> u64 {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return 0;
    };
    if !meta.is_dir() {
        return meta.len();
    }
    let Ok(rd) = fs::read_dir(path) else {
        return 0;
    };
    rd.filter_map(Result::ok).map(|e| ref_size(&e.path())).sum()
}

/// Builds a moderately nested fixture: 3 levels, mixed file sizes, plus a
/// symlink to a file (which must be counted as the link, not the target).
fn build_fixture(root: &Path) {
    for a in 0..4 {
        let da = root.join(format!("a{a}"));
        fs::create_dir(&da).unwrap();
        write_file(&da.join("top.bin"), 100 * (a + 1));
        for b in 0..3 {
            let db = da.join(format!("b{b}"));
            fs::create_dir(&db).unwrap();
            for c in 0..5 {
                write_file(&db.join(format!("f{c}.bin")), 10 * (c + 1) + a + b);
            }
        }
    }
    write_file(&root.join("loose.bin"), 4321);
    symlink(root.join("loose.bin"), root.join("link-to-loose")).unwrap();
}

fn assert_sorted_desc(node: &DirNode) {
    assert!(
        node.children
            .windows(2)
            .all(|w| w[0].size_bytes >= w[1].size_bytes),
        "children of {} not sorted descending",
        node.path.display()
    );
    for child in &node.children {
        assert_sorted_desc(child);
    }
}

/// Asserts every node's size equals the sequential size of its subtree.
fn assert_matches_reference(node: &DirNode) {
    assert_eq!(
        node.size_bytes,
        ref_size(&node.path),
        "{}",
        node.path.display()
    );
    for child in &node.children {
        assert_matches_reference(child);
    }
}

#[test]
fn size_tree_matches_sequential_reference() {
    let dir = tempfile::tempdir().unwrap();
    build_fixture(dir.path());
    let node = size_tree(dir.path(), &CancelToken::new(), None).unwrap();
    assert_eq!(node.size_bytes, ref_size(dir.path()));
    assert!(node.is_dir);
    assert_sorted_desc(&node);
    assert_matches_reference(&node);
}

#[test]
fn symlink_cycle_terminates_and_links_not_followed() {
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("sub");
    fs::create_dir(&sub).unwrap();
    write_file(&sub.join("data.bin"), 500);
    // Cycle: sub/loop -> root, plus a link to a large file elsewhere.
    symlink(dir.path(), sub.join("loop")).unwrap();
    let big = dir.path().join("big.bin");
    write_file(&big, 100_000);
    symlink(&big, sub.join("link-to-big")).unwrap();

    // Must terminate (no infinite recursion) and match the reference,
    // which also refuses to follow links.
    let node = size_tree(dir.path(), &CancelToken::new(), None).unwrap();
    assert_eq!(node.size_bytes, ref_size(dir.path()));

    // The sub directory counts only its file plus the two link objects —
    // far less than if either link were traversed.
    let sub_node = node.children.iter().find(|c| c.path == sub).unwrap();
    assert!(sub_node.size_bytes < 500 + 1024, "links were followed");
}

#[test]
fn precancelled_token_stops_immediately() {
    let dir = tempfile::tempdir().unwrap();
    write_file(&dir.path().join("f"), 10);
    let cancel = CancelToken::new();
    cancel.cancel();
    assert!(matches!(
        size_tree(dir.path(), &cancel, None),
        Err(WalkError::Cancelled)
    ));
    assert!(matches!(
        dir_size(dir.path(), &cancel),
        Err(WalkError::Cancelled)
    ));
}

#[test]
fn max_depth_aggregates_but_prunes_children() {
    let dir = tempfile::tempdir().unwrap();
    build_fixture(dir.path());
    let full = size_tree(dir.path(), &CancelToken::new(), None).unwrap();

    // Depth 0: root only, same total size.
    let d0 = size_tree(dir.path(), &CancelToken::new(), Some(0)).unwrap();
    assert_eq!(d0.size_bytes, full.size_bytes);
    assert!(d0.children.is_empty());

    // Depth 1: direct children kept with full aggregate sizes, but no
    // grandchildren.
    let d1 = size_tree(dir.path(), &CancelToken::new(), Some(1)).unwrap();
    assert_eq!(d1.size_bytes, full.size_bytes);
    assert_eq!(d1.children.len(), full.children.len());
    for (pruned, kept) in d1.children.iter().zip(full.children.iter()) {
        assert_eq!(pruned.path, kept.path);
        assert_eq!(pruned.size_bytes, kept.size_bytes);
        assert!(pruned.children.is_empty());
    }

    // dir_size agrees with the full tree.
    assert_eq!(
        dir_size(dir.path(), &CancelToken::new()).unwrap(),
        full.size_bytes
    );
}

#[test]
fn unreadable_subdir_is_skipped_not_fatal() {
    let dir = tempfile::tempdir().unwrap();
    write_file(&dir.path().join("visible.bin"), 300);
    let locked = dir.path().join("locked");
    fs::create_dir(&locked).unwrap();
    write_file(&locked.join("hidden.bin"), 9999);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

    // Running as root would ignore the permission bits; skip if so.
    if fs::read_dir(&locked).is_ok() {
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        eprintln!("skipping: permissions not enforced (running as root?)");
        return;
    }

    let node = size_tree(dir.path(), &CancelToken::new(), None).unwrap();
    // The hidden file is not counted; the locked dir appears as empty.
    assert_eq!(node.size_bytes, 300);
    let locked_node = node.children.iter().find(|c| c.path == locked).unwrap();
    assert!(locked_node.is_dir);
    assert_eq!(locked_node.size_bytes, 0);
    assert!(locked_node.children.is_empty());

    // The *root* being unreadable is fatal, by contrast.
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let err = size_tree(&locked, &CancelToken::new(), None).unwrap_err();
    assert!(matches!(err, WalkError::Root { .. }));
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
}
