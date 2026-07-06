//! Fixture-based tests: a tempdir home, real `Info.plist` files written via
//! the plist crate, and planted remnants/orphans.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use tabibu_engine::{CancelToken, Category, CleanupItem, SafetyTier, ScanCtx, Scanner};
use tabibu_uninstall::{bundle_id_of, find_remnants, installed_apps, OrphanScanner};

fn make_app(dir: &Path, name: &str, bundle_id: &str) -> PathBuf {
    let app = dir.join(format!("{name}.app"));
    let contents = app.join("Contents");
    fs::create_dir_all(&contents).unwrap();
    let mut info = plist::Dictionary::new();
    info.insert(
        "CFBundleIdentifier".into(),
        plist::Value::String(bundle_id.into()),
    );
    info.insert("CFBundleName".into(), plist::Value::String(name.into()));
    plist::Value::Dictionary(info)
        .to_file_xml(contents.join("Info.plist"))
        .unwrap();
    app
}

fn ctx_for(home: &Path) -> ScanCtx {
    ScanCtx {
        home: home.to_path_buf(),
        allowed_roots: vec![home.to_path_buf()],
        running_bundle_ids: HashSet::new(),
        full_disk_access: true,
    }
}

fn run(scanner: &dyn Scanner, ctx: &ScanCtx) -> Vec<CleanupItem> {
    let mut items = Vec::new();
    scanner
        .scan(ctx, &CancelToken::new(), &mut |item| items.push(item))
        .unwrap();
    items
}

// ---------------------------------------------------------------- bundle id

#[test]
fn bundle_id_of_reads_cfbundleidentifier() {
    let tmp = tempfile::tempdir().unwrap();
    let app = make_app(tmp.path(), "FooBar", "com.acme.foobar");
    assert_eq!(bundle_id_of(&app), Some("com.acme.foobar".to_owned()));
}

#[test]
fn bundle_id_of_handles_missing_and_malformed() {
    let tmp = tempfile::tempdir().unwrap();
    // No Contents/Info.plist at all.
    let bare = tmp.path().join("Bare.app");
    fs::create_dir_all(&bare).unwrap();
    assert_eq!(bundle_id_of(&bare), None);
    // Garbage plist.
    let broken = tmp.path().join("Broken.app");
    fs::create_dir_all(broken.join("Contents")).unwrap();
    fs::write(broken.join("Contents/Info.plist"), "not a plist").unwrap();
    assert_eq!(bundle_id_of(&broken), None);
    // Valid plist without the key.
    let keyless = tmp.path().join("Keyless.app");
    fs::create_dir_all(keyless.join("Contents")).unwrap();
    plist::Value::Dictionary(plist::Dictionary::new())
        .to_file_xml(keyless.join("Contents/Info.plist"))
        .unwrap();
    assert_eq!(bundle_id_of(&keyless), None);
}

#[test]
fn installed_apps_scans_top_level_and_one_sublevel() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    make_app(root, "Alpha", "com.acme.alpha");
    let utilities = root.join("Utilities");
    fs::create_dir_all(&utilities).unwrap();
    make_app(&utilities, "Beta", "com.acme.beta");
    // Two levels deep: must NOT be found.
    let deep = root.join("Sub/Deeper");
    fs::create_dir_all(&deep).unwrap();
    make_app(&deep, "Gamma", "com.acme.gamma");
    // Malformed app: skipped quietly.
    fs::create_dir_all(root.join("NoPlist.app")).unwrap();

    let mut ids: Vec<String> = installed_apps(&[root.to_path_buf()])
        .into_iter()
        .map(|(_, id)| id)
        .collect();
    ids.sort();
    assert_eq!(ids, ["com.acme.alpha", "com.acme.beta"]);
}

// ------------------------------------------------------------ remnant hunt

fn plant(home: &Path, rel: &str, as_dir: bool) -> PathBuf {
    let path = home.join(rel);
    if as_dir {
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("payload.bin"), b"0123456789").unwrap();
    } else {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"0123456789").unwrap();
    }
    path
}

#[test]
fn remnant_hunt_finds_planted_remnants_with_right_tiers() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let id = "com.acme.foobar";

    let exact_dirs = [
        plant(home, "Library/Application Support/com.acme.foobar", true),
        plant(home, "Library/Caches/com.acme.foobar", true),
        plant(home, "Library/Containers/com.acme.foobar", true),
        plant(
            home,
            "Library/Group Containers/ABCDE12345.com.acme.foobar",
            true,
        ),
        plant(
            home,
            "Library/Saved Application State/com.acme.foobar.savedState",
            true,
        ),
        plant(home, "Library/WebKit/com.acme.foobar", true),
        plant(home, "Library/HTTPStorages/com.acme.foobar", true),
        plant(home, "Library/Preferences/com.acme.foobar.plist", false),
        plant(
            home,
            "Library/Preferences/com.acme.foobar.helper.plist",
            false,
        ),
        plant(
            home,
            "Library/LaunchAgents/com.acme.foobar.agent.plist",
            false,
        ),
    ];
    let fuzzy_dirs = [
        plant(home, "Library/Application Support/foobar", true), // case-insensitive name
        plant(home, "Library/Logs/FooBar", true),
    ];
    // Decoys that must never match.
    plant(home, "Library/Application Support/com.other.thing", true);
    plant(home, "Library/Preferences/com.acme.foobarx.plist", false); // no dot boundary
    plant(home, "Library/Preferences/com.acme.foo.plist", false); // different id
    plant(
        home,
        "Library/Group Containers/ABCDE.com.acme.foobarbaz",
        true,
    );
    plant(home, "Library/Caches/FooBarOnFile", true); // name differs

    let items = find_remnants(id, "FooBar", &ctx_for(home));
    let by_path = |p: &Path| items.iter().find(|i| i.path == p);

    for path in &exact_dirs {
        let item = by_path(path).unwrap_or_else(|| panic!("missing exact remnant {path:?}"));
        assert_eq!(item.tier, SafetyTier::Review);
        assert_eq!(item.category, Category::AppRemnant);
        assert_eq!(item.reason, "Created by FooBar");
        assert!(item.size_bytes >= 10, "recursive size measured");
    }
    for path in &fuzzy_dirs {
        let item = by_path(path).unwrap_or_else(|| panic!("missing fuzzy remnant {path:?}"));
        assert_eq!(item.tier, SafetyTier::Risky);
        assert_eq!(item.reason, "Name matches FooBar — verify before removing");
    }
    assert_eq!(
        items.len(),
        exact_dirs.len() + fuzzy_dirs.len(),
        "no decoys leaked: {items:#?}"
    );
}

#[test]
fn remnant_hunt_refuses_fuzzy_for_short_names() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    plant(home, "Library/Application Support/foo", true); // would fuzzy-match "Foo"
    let exact = plant(home, "Library/Caches/com.acme.foo", true);

    let items = find_remnants("com.acme.foo", "Foo", &ctx_for(home));
    assert_eq!(items.len(), 1, "fuzzy refused for 3-char name: {items:#?}");
    assert_eq!(items[0].path, exact);
    assert_eq!(items[0].tier, SafetyTier::Review);
}

#[test]
fn remnant_hunt_refuses_degenerate_bundle_ids() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    plant(home, "Library/Application Support/anything", true);
    assert!(find_remnants("", "Whatever", &ctx_for(home)).is_empty());
    assert!(find_remnants("nodots", "Whatever", &ctx_for(home)).is_empty());
}

// ---------------------------------------------------------------- orphans

#[test]
fn orphan_scanner_flags_orphans_and_skips_installed_running_apple() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let orphan_support = plant(home, "Library/Application Support/com.gone.app", true);
    let orphan_cache = plant(home, "Library/Caches/org.dead.tool", true);
    plant(home, "Library/Application Support/com.installed.app", true); // installed
    plant(home, "Library/Application Support/com.running.app", true); // running
    plant(home, "Library/Application Support/com.apple.dt.Xcode", true); // Apple
    plant(home, "Library/Application Support/Slack", true); // not a bundle id
    plant(home, "Library/Application Support/io.two", true); // 2 segments
    plant(home, "Library/Application Support/xyz.foo.bar", true); // unknown TLD token
    plant(home, "Library/Caches/com.file.like.id", false); // file, not a dir

    let mut ctx = ctx_for(home);
    ctx.running_bundle_ids.insert("com.running.app".to_owned());
    let scanner = OrphanScanner::new(HashSet::from(["com.installed.app".to_owned()]));

    let mut items = run(&scanner, &ctx);
    items.sort_by(|a, b| a.path.cmp(&b.path));
    let paths: Vec<&Path> = items.iter().map(|i| i.path.as_path()).collect();
    assert_eq!(
        paths,
        [orphan_support.as_path(), orphan_cache.as_path()],
        "{items:#?}"
    );
    for item in &items {
        assert_eq!(item.tier, SafetyTier::Risky);
        assert_eq!(item.category, Category::OrphanedSupport);
    }
    assert_eq!(
        items[0].reason,
        "No installed app with bundle ID com.gone.app found"
    );
}
