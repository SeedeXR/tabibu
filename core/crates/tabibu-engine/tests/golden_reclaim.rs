//! Golden-image reclaim test (engineering guide §8): snapshot a disk state,
//! reclaim, assert *exactly* the intended files changed. Uses `Delete` on
//! Safe items so the test never touches the real user Trash.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use tabibu_engine::scanner::ScanCtx;
use tabibu_engine::{reclaim, Category, CleanupItem, ReclaimAction, ReclaimError, SafetyTier};

fn walk(p: &Path, set: &mut BTreeSet<PathBuf>) {
    if let Ok(rd) = fs::read_dir(p) {
        for e in rd.flatten() {
            set.insert(e.path());
            if e.path().is_dir() {
                walk(&e.path(), set);
            }
        }
    }
}

fn snapshot(root: &Path) -> BTreeSet<PathBuf> {
    let mut set = BTreeSet::new();
    walk(root, &mut set);
    set
}

struct Fixture {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    caches: PathBuf,
    undo: PathBuf,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let caches = home.join("Library/Caches");
    fs::create_dir_all(caches.join("com.example.app")).unwrap();
    fs::write(caches.join("com.example.app/blob.bin"), vec![0u8; 4096]).unwrap();
    fs::write(caches.join("stray.tmp"), b"stale").unwrap();
    fs::create_dir_all(home.join("Documents")).unwrap();
    fs::write(home.join("Documents/thesis.txt"), b"precious").unwrap();
    let undo = tmp.path().join("undo");
    Fixture {
        home: home.clone(),
        caches,
        undo,
        _tmp: tmp,
    }
}

fn ctx(f: &Fixture) -> ScanCtx {
    ScanCtx {
        home: f.home.clone(),
        allowed_roots: vec![f.caches.clone()],
        running_bundle_ids: std::collections::HashSet::default(),
        full_disk_access: true,
    }
}

#[test]
fn reclaim_changes_exactly_the_intended_files() {
    let f = fixture();
    let target_dir = f.caches.join("com.example.app");
    let before = snapshot(&f.home);

    let items = vec![CleanupItem {
        action: ReclaimAction::Delete,
        ..CleanupItem::new(
            target_dir.clone(),
            Category::UserCache,
            4096,
            SafetyTier::Safe,
            "test",
        )
    }];
    let report = reclaim(&ctx(&f), &items, &f.undo).unwrap();

    assert_eq!(report.succeeded, 1);
    assert_eq!(report.failed, 0);
    assert_eq!(
        report.reclaimed_bytes, 4096,
        "measured bytes, exactly the blob"
    );
    assert!(report.manifest_path.as_deref().is_some_and(Path::exists));

    let after = snapshot(&f.home);
    let removed: BTreeSet<_> = before.difference(&after).collect();
    let expected: BTreeSet<PathBuf> = [target_dir.clone(), target_dir.join("blob.bin")]
        .into_iter()
        .collect();
    assert_eq!(
        removed,
        expected.iter().collect(),
        "exactly the intended paths removed"
    );
    assert!(f.home.join("Documents/thesis.txt").exists());
    assert!(
        f.caches.join("stray.tmp").exists(),
        "unselected sibling untouched"
    );
}

#[test]
fn unselected_items_are_never_touched() {
    let f = fixture();
    let mut item = CleanupItem::new(
        f.caches.join("stray.tmp"),
        Category::Temp,
        5,
        SafetyTier::Safe,
        "test",
    );
    item.selected = false;
    item.action = ReclaimAction::Delete;
    let report = reclaim(&ctx(&f), &[item], &f.undo).unwrap();
    assert_eq!(report.succeeded + report.failed, 0);
    assert!(f.caches.join("stray.tmp").exists());
}

#[test]
fn denied_target_is_skipped_others_reclaimed() {
    // A denied (protected) path in the batch is SKIPPED and reported — never
    // touched — while the legitimate items still reclaim. (Previously a denied
    // path aborted the whole batch, which broke whole-home duplicate cleanup
    // whenever one copy lived in a protected folder.)
    let f = fixture();
    let doc = f.home.join("Documents/thesis.txt");
    let good = CleanupItem {
        action: ReclaimAction::Delete,
        ..CleanupItem::new(
            f.caches.join("stray.tmp"),
            Category::Temp,
            5,
            SafetyTier::Safe,
            "test",
        )
    };
    let denied = CleanupItem {
        action: ReclaimAction::Delete,
        ..CleanupItem::new(
            doc.clone(),
            Category::Temp,
            8,
            SafetyTier::Safe,
            "protected",
        )
    };
    let report = reclaim(&ctx(&f), &[good, denied], &f.undo).unwrap();

    // The legitimate item was reclaimed…
    assert_eq!(report.succeeded, 1);
    assert!(!f.caches.join("stray.tmp").exists());
    // …and the protected path was NEVER touched, but is reported as skipped.
    assert!(doc.exists(), "protected path must never be touched");
    assert_eq!(report.failed, 1);
    let skipped = report.outcomes.iter().find(|o| o.path == doc).unwrap();
    assert!(skipped.error.as_deref().unwrap_or("").contains("protected"));
}

#[test]
fn non_safe_tier_cannot_be_deleted() {
    let f = fixture();
    let item = CleanupItem {
        action: ReclaimAction::Delete,
        selected: true,
        ..CleanupItem::new(
            f.caches.join("stray.tmp"),
            Category::Temp,
            5,
            SafetyTier::Review,
            "test",
        )
    };
    let err = reclaim(&ctx(&f), &[item], &f.undo).unwrap_err();
    assert!(matches!(err, ReclaimError::TierViolation { .. }));
    assert!(f.caches.join("stray.tmp").exists());
}

#[test]
fn permission_denied_mid_batch_is_reported_not_hidden() {
    // Fault injection: make one target undeletable, assert honest partials.
    use std::os::unix::fs::PermissionsExt;
    let f = fixture();
    let locked_dir = f.caches.join("locked");
    fs::create_dir_all(&locked_dir).unwrap();
    fs::write(locked_dir.join("x"), b"x").unwrap();
    fs::set_permissions(&locked_dir, fs::Permissions::from_mode(0o555)).unwrap();

    let items = vec![
        CleanupItem {
            action: ReclaimAction::Delete,
            ..CleanupItem::new(
                locked_dir.join("x"),
                Category::Temp,
                1,
                SafetyTier::Safe,
                "will fail",
            )
        },
        CleanupItem {
            action: ReclaimAction::Delete,
            ..CleanupItem::new(
                f.caches.join("stray.tmp"),
                Category::Temp,
                5,
                SafetyTier::Safe,
                "will succeed",
            )
        },
    ];
    let report = reclaim(&ctx(&f), &items, &f.undo).unwrap();
    fs::set_permissions(&locked_dir, fs::Permissions::from_mode(0o755)).unwrap(); // cleanup

    assert_eq!(report.failed, 1);
    assert_eq!(report.succeeded, 1);
    assert!(report.outcomes[0].error.is_some());
    assert!(!f.caches.join("stray.tmp").exists());
}
