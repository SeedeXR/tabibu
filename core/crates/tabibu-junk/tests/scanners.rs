//! Integration tests for the junk scanners, all run against constructed
//! fixture homes so the real user home is never touched.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use tabibu_engine::{
    CancelToken, Category, CleanupItem, ReclaimAction, SafetyTier, ScanCtx, ScanError, Scanner,
};
use tabibu_junk::{
    empty_trash_dirs, scanners, trash_total_size, DevCacheScanner, LogScanner, TempScanner,
    TrashScanner, UserCacheScanner,
};

fn make_ctx(home: &Path) -> ScanCtx {
    ScanCtx {
        home: home.to_path_buf(),
        allowed_roots: vec![home.to_path_buf()],
        running_bundle_ids: HashSet::new(),
        full_disk_access: false,
    }
}

fn write_file(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, bytes).unwrap();
}

/// Backdate an entry's mtime by `days`. Works for files and directories
/// (futimens accepts a read-only descriptor on macOS).
fn set_age_days(path: &Path, days: u64) {
    let mtime = SystemTime::now() - Duration::from_secs(days * 24 * 60 * 60);
    let file = fs::File::open(path).unwrap();
    file.set_modified(mtime).unwrap();
}

fn run(scanner: &dyn Scanner, ctx: &ScanCtx) -> Vec<CleanupItem> {
    let cancel = CancelToken::new();
    let mut items = Vec::new();
    scanner
        .scan(ctx, &cancel, &mut |item| items.push(item))
        .unwrap();
    items
}

fn assert_all_under(items: &[CleanupItem], root: &Path) {
    for item in items {
        assert!(
            item.path.starts_with(root),
            "item {} escapes fixture root {}",
            item.path.display(),
            root.display()
        );
    }
}

// ---------------------------------------------------------------------------
// TrashScanner
// ---------------------------------------------------------------------------

#[test]
fn trash_lists_top_level_entries_with_recursive_sizes() {
    let home = tempfile::tempdir().unwrap();
    write_file(&home.path().join(".Trash/loose.txt"), b"12345");
    write_file(&home.path().join(".Trash/Folder/a.bin"), b"123");
    write_file(&home.path().join(".Trash/Folder/nested/b.bin"), b"1234");

    let ctx = make_ctx(home.path());
    // Inject an empty volumes root so the test never reads the real /Volumes.
    let scanner = TrashScanner::with_volumes_root(home.path().join("no-volumes"));
    let mut items = run(&scanner, &ctx);
    items.sort_by(|a, b| a.path.cmp(&b.path));

    assert_eq!(items.len(), 2);
    assert_all_under(&items, home.path());

    let folder = &items[0];
    assert_eq!(folder.path, home.path().join(".Trash/Folder"));
    assert_eq!(folder.size_bytes, 7);
    let loose = &items[1];
    assert_eq!(loose.path, home.path().join(".Trash/loose.txt"));
    assert_eq!(loose.size_bytes, 5);
    for item in &items {
        assert_eq!(item.category, Category::Trash);
        assert_eq!(item.tier, SafetyTier::Safe);
    }
}

#[test]
fn trash_missing_dir_yields_no_items() {
    let home = tempfile::tempdir().unwrap();
    let ctx = make_ctx(home.path());
    let scanner = TrashScanner::with_volumes_root(home.path().join("no-volumes"));
    assert!(run(&scanner, &ctx).is_empty());
}

#[test]
fn trash_includes_per_volume_trashes() {
    use std::os::unix::fs::MetadataExt;
    let home = tempfile::tempdir().unwrap();
    let uid = std::fs::metadata(home.path()).unwrap().uid();
    // A fake mounted volume with a per-uid trash holding one deleted file.
    let vols = home.path().join("Volumes");
    let vtrash = vols.join("BackupDrive/.Trashes").join(uid.to_string());
    write_file(&vtrash.join("old-export.zip"), b"abcdef");

    let ctx = make_ctx(home.path());
    let scanner = TrashScanner::with_volumes_root(vols);
    let items = run(&scanner, &ctx);

    assert_eq!(
        items.len(),
        1,
        "the external-volume trashed file should be found"
    );
    assert_eq!(items[0].path, vtrash.join("old-export.zip"));
    assert_eq!(items[0].size_bytes, 6);
    assert_eq!(items[0].tier, SafetyTier::Safe);
}

// ---------------------------------------------------------------------------
// UserCacheScanner
// ---------------------------------------------------------------------------

#[test]
fn user_cache_tiers_and_running_guard() {
    let home = tempfile::tempdir().unwrap();
    let caches = home.path().join("Library/Caches");
    write_file(&caches.join("com.example.app/data.bin"), b"0123456789"); // 10 B
    write_file(&caches.join("com.running.app/data.bin"), b"xx");
    write_file(&caches.join("com.google.Chrome/blob"), b"abc");
    write_file(&caches.join("WeirdFolder/junk"), b"abcd");
    write_file(&caches.join("loose-file"), b"ignored"); // not a directory

    let mut ctx = make_ctx(home.path());
    ctx.running_bundle_ids.insert("com.running.app".to_owned());

    let mut items = run(&UserCacheScanner, &ctx);
    items.sort_by(|a, b| a.path.cmp(&b.path));

    assert_eq!(
        items.len(),
        3,
        "running app cache and loose file must be skipped"
    );
    assert_all_under(&items, home.path());
    assert!(
        !items.iter().any(|i| i.path.ends_with("com.running.app")),
        "running-process guard failed"
    );

    let example = items
        .iter()
        .find(|i| i.path.ends_with("com.example.app"))
        .unwrap();
    assert_eq!(example.tier, SafetyTier::Safe);
    assert_eq!(example.size_bytes, 10);
    assert_eq!(example.reason, "Cache for com.example.app (not running)");

    let chrome = items
        .iter()
        .find(|i| i.path.ends_with("com.google.Chrome"))
        .unwrap();
    assert_eq!(chrome.tier, SafetyTier::Safe);
    assert!(chrome.reason.contains("Browser cache"));

    let weird = items
        .iter()
        .find(|i| i.path.ends_with("WeirdFolder"))
        .unwrap();
    assert_eq!(weird.tier, SafetyTier::Review);
    assert_eq!(weird.size_bytes, 4);
    for item in &items {
        assert_eq!(item.category, Category::UserCache);
    }
}

// ---------------------------------------------------------------------------
// DevCacheScanner
// ---------------------------------------------------------------------------

#[test]
fn dev_cache_reports_only_existing_locations() {
    let home = tempfile::tempdir().unwrap();
    let derived = home.path().join("Library/Developer/Xcode/DerivedData");
    write_file(&derived.join("MyApp-abc/Build/out.o"), b"123456"); // 6 B
    let npm = home.path().join(".npm/_cacache");
    write_file(&npm.join("content-v2/blob"), b"12"); // 2 B
    let brew = home.path().join("Library/Caches/Homebrew");
    fs::create_dir_all(&brew).unwrap(); // empty dir still reported, size 0
                                        // Hidden dot-directory caches (surfaced via Cmd+Shift+G in Finder).
    let xdg = home.path().join(".cache");
    write_file(&xdg.join("uv/blob"), b"1234"); // 4 B
    let gradle = home.path().join(".gradle/caches");
    write_file(&gradle.join("modules-2/x.jar"), b"123"); // 3 B

    let ctx = make_ctx(home.path());
    let items = run(&DevCacheScanner, &ctx);

    assert_eq!(
        items.len(),
        5,
        "non-existent dev caches must be skipped silently; hidden caches included"
    );
    assert_all_under(&items, home.path());

    let derived_item = items.iter().find(|i| i.path == derived).unwrap();
    assert_eq!(derived_item.tier, SafetyTier::Review);
    assert_eq!(derived_item.size_bytes, 6);
    assert_eq!(
        derived_item.reason,
        "Xcode build products — regenerated on next build"
    );

    let npm_item = items.iter().find(|i| i.path == npm).unwrap();
    assert_eq!(npm_item.tier, SafetyTier::Safe);
    assert_eq!(npm_item.size_bytes, 2);

    let brew_item = items.iter().find(|i| i.path == brew).unwrap();
    assert_eq!(brew_item.tier, SafetyTier::Safe);
    assert_eq!(brew_item.size_bytes, 0);

    // Hidden caches: found, Review tier (broad → never auto-selected).
    let xdg_item = items.iter().find(|i| i.path == xdg).unwrap();
    assert_eq!(xdg_item.tier, SafetyTier::Review);
    assert_eq!(xdg_item.size_bytes, 4);
    let gradle_item = items.iter().find(|i| i.path == gradle).unwrap();
    assert_eq!(gradle_item.tier, SafetyTier::Review);
    assert_eq!(gradle_item.size_bytes, 3);
    for item in &items {
        assert_eq!(item.category, Category::DevCache);
    }
}

// ---------------------------------------------------------------------------
// TempScanner
// ---------------------------------------------------------------------------

#[test]
fn temp_reports_only_stale_entries() {
    let home = tempfile::tempdir().unwrap();
    let tmp_items = home.path().join("Library/Caches/TemporaryItems");
    let stale = tmp_items.join("stale.tmp");
    write_file(&stale, b"12345678"); // 8 B
    set_age_days(&stale, 10);
    write_file(&tmp_items.join("fresh.tmp"), b"fresh");

    // Fake system temp dir: a tempfile dir lives under /var/folders on
    // macOS, so it passes the prefix check while staying a fixture we own.
    let fake_temp = tempfile::tempdir().unwrap();
    let stale_sys = fake_temp.path().join("old-blob");
    write_file(&stale_sys, b"123"); // 3 B
    set_age_days(&stale_sys, 8);
    write_file(&fake_temp.path().join("fresh-blob"), b"new");

    let ctx = make_ctx(home.path());
    let scanner = TempScanner::with_system_temp(fake_temp.path().to_path_buf());
    let items = run(&scanner, &ctx);

    assert_eq!(items.len(), 2, "fresh entries must not be reported");
    let canonical_fake = fake_temp.path().canonicalize().unwrap();
    for item in &items {
        assert_eq!(item.category, Category::Temp);
        assert_eq!(item.tier, SafetyTier::Review);
        assert!(
            item.path.starts_with(home.path()) || item.path.starts_with(&canonical_fake),
            "item {} escapes fixtures",
            item.path.display()
        );
    }
    let home_item = items.iter().find(|i| i.path == stale).unwrap();
    assert_eq!(home_item.size_bytes, 8);
    let sys_item = items
        .iter()
        .find(|i| i.path == canonical_fake.join("old-blob"))
        .unwrap();
    assert_eq!(sys_item.size_bytes, 3);
}

#[test]
fn temp_skips_system_dir_outside_var_folders() {
    let home = tempfile::tempdir().unwrap();

    // CARGO_TARGET_TMPDIR is under the project target/ dir, not /var/folders.
    let outside: PathBuf = [env!("CARGO_TARGET_TMPDIR"), "tabibu_junk_fake_temp"]
        .iter()
        .collect();
    let stale = outside.join("stale.tmp");
    write_file(&stale, b"old stuff");
    set_age_days(&stale, 30);
    assert!(!outside.canonicalize().unwrap().starts_with("/var/folders"));

    let ctx = make_ctx(home.path());
    let scanner = TempScanner::with_system_temp(outside);
    let items = run(&scanner, &ctx);
    assert!(
        items.is_empty(),
        "non-/var/folders temp dirs must be ignored"
    );
}

// ---------------------------------------------------------------------------
// LogScanner
// ---------------------------------------------------------------------------

#[test]
fn log_emits_individual_stale_files_never_directories() {
    let home = tempfile::tempdir().unwrap();
    let logs = home.path().join("Library/Logs");

    let myapp = logs.join("MyApp");
    let old_a = myapp.join("old-a.log");
    write_file(&old_a, b"123456"); // 6 B stale
    set_age_days(&old_a, 45);
    let old_b = myapp.join("archive/old-b.log");
    write_file(&old_b, b"1234"); // 4 B stale, nested
    set_age_days(&old_b, 60);
    write_file(&myapp.join("today.log"), b"freshfresh"); // not stale

    write_file(&logs.join("OtherApp/recent.log"), b"fresh"); // all fresh

    let loose = logs.join("system.log");
    write_file(&loose, b"12"); // 2 B stale, directly under Logs
    set_age_days(&loose, 90);

    let ctx = make_ctx(home.path());
    let items = run(&LogScanner, &ctx);

    // Every item is a stale FILE — a directory item would drag current logs
    // (today.log) into an auto-selected Safe deletion.
    assert_eq!(
        items.len(),
        3,
        "each stale file is its own item; fresh files never appear"
    );
    assert_all_under(&items, home.path());
    for item in &items {
        assert!(
            item.path.is_file(),
            "never a directory: {}",
            item.path.display()
        );
        assert_eq!(item.category, Category::Log);
        assert_eq!(item.tier, SafetyTier::Safe);
        assert_eq!(item.action, ReclaimAction::Trash);
    }

    let a = items.iter().find(|i| i.path == old_a).unwrap();
    assert_eq!(a.size_bytes, 6);
    assert!(a.reason.contains("MyApp"));
    let b = items.iter().find(|i| i.path == old_b).unwrap();
    assert_eq!(b.size_bytes, 4, "nested stale files are found recursively");
    let loose_item = items.iter().find(|i| i.path == loose).unwrap();
    assert_eq!(loose_item.size_bytes, 2);
}

// ---------------------------------------------------------------------------
// Registry and cancellation
// ---------------------------------------------------------------------------

#[test]
fn registry_exposes_all_scanners() {
    let ids: Vec<&str> = scanners().iter().map(|s| s.id()).collect();
    assert_eq!(
        ids,
        [
            "trash",
            "user_cache",
            "container_cache",
            "dev_cache",
            "temp",
            "log",
            "large_old"
        ]
    );
}

#[test]
fn every_scanner_returns_cancelled_when_cancelled_up_front() {
    let home = tempfile::tempdir().unwrap();
    let ctx = make_ctx(home.path());
    let cancel = CancelToken::new();
    cancel.cancel();
    for scanner in scanners() {
        let err = scanner.scan(&ctx, &cancel, &mut |_| {}).unwrap_err();
        assert!(
            matches!(err, ScanError::Cancelled),
            "scanner {} returned {err:?} instead of Cancelled",
            scanner.id()
        );
    }
}

// ---------------------------------------------------------------------------
// empty_trash_dirs / trash_total_size
// ---------------------------------------------------------------------------

#[test]
fn empty_trash_deletes_everything_and_reports_freed_bytes() {
    let trash = tempfile::tempdir().unwrap();
    // A loose file (5 bytes) and a nested folder (3 + 4 = 7 bytes).
    write_file(&trash.path().join("loose.txt"), b"12345");
    write_file(&trash.path().join("sub/a.bin"), b"abc");
    write_file(&trash.path().join("sub/b.bin"), b"defg");
    let dirs = vec![trash.path().to_path_buf()];

    // Size before matches the bytes we'll free.
    let before = trash_total_size(&dirs, &CancelToken::new());
    assert_eq!(
        before, 12,
        "5 + 3 + 4 bytes across the two top-level entries"
    );

    let out = empty_trash_dirs(&dirs);
    assert_eq!(out.deleted_items, 2, "two top-level entries removed");
    assert_eq!(out.freed_bytes, 12);
    assert!(out.errors.is_empty(), "clean delete: {:?}", out.errors);
    // The trash dir itself remains but is now empty.
    assert_eq!(fs::read_dir(trash.path()).unwrap().count(), 0);
    assert_eq!(trash_total_size(&dirs, &CancelToken::new()), 0);
}

#[test]
fn empty_trash_missing_dir_is_a_noop() {
    let out = empty_trash_dirs(&[PathBuf::from("/no/such/trash/dir")]);
    assert_eq!(out.deleted_items, 0);
    assert_eq!(out.freed_bytes, 0);
    assert!(out.errors.is_empty());
}
