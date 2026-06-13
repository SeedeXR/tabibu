//! Installed-app discovery: bundle-id extraction and `.app` enumeration.

use std::fs;
use std::path::{Path, PathBuf};

/// Read `CFBundleIdentifier` from `<app>/Contents/Info.plist`.
///
/// Returns `None` for missing, unreadable, or malformed plists — never
/// guesses an identifier.
#[must_use]
pub fn bundle_id_of(app_path: &Path) -> Option<String> {
    let info = app_path.join("Contents").join("Info.plist");
    let value = plist::Value::from_file(info).ok()?;
    value
        .as_dictionary()?
        .get("CFBundleIdentifier")?
        .as_string()
        .map(str::to_owned)
}

/// Enumerate `*.app` bundles in the given roots (e.g. `/Applications` and
/// `~/Applications`): top level plus one level of subdirectories (such as
/// `/Applications/Utilities`). Unreadable or malformed entries are skipped
/// quietly.
#[must_use]
pub fn installed_apps(roots: &[PathBuf]) -> Vec<(PathBuf, String)> {
    let mut found = Vec::new();
    for root in roots {
        let Ok(entries) = fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if is_app_bundle(&path) {
                push_app(&mut found, path);
            } else if path.is_dir() {
                // One level of subdirectories only.
                let Ok(sub_entries) = fs::read_dir(&path) else {
                    continue;
                };
                for sub_entry in sub_entries.flatten() {
                    let sub_path = sub_entry.path();
                    if is_app_bundle(&sub_path) {
                        push_app(&mut found, sub_path);
                    }
                }
            }
        }
    }
    found
}

fn push_app(found: &mut Vec<(PathBuf, String)>, path: PathBuf) {
    if let Some(id) = bundle_id_of(&path) {
        found.push((path, id));
    }
}

fn is_app_bundle(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == "app") && path.is_dir()
}
