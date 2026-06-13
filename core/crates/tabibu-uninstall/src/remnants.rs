//! Remnant hunt: leftovers of a specific app being uninstalled.

use crate::fsutil::size_of;
use std::fs;
use std::path::{Path, PathBuf};
use tabibu_engine::{Category, CleanupItem, SafetyTier, ScanCtx};

/// App names shorter than this never fuzzy-match: short names ("Go", "Arc")
/// collide with far too many unrelated directories.
const MIN_FUZZY_NAME_LEN: usize = 4;

/// Hunt `~/Library` (relative to `ctx.home`) for leftovers of the app
/// identified by `bundle_id` / `app_name`. Read-only.
///
/// Matching is deliberately conservative:
/// - exact bundle-id matches → [`SafetyTier::Review`];
/// - fuzzy matches (directory name case-insensitively equals `app_name`) →
///   [`SafetyTier::Risky`], and only for names of at least 4 characters;
/// - an empty bundle id, or one without a `.`, matches nothing at all.
#[must_use]
pub fn find_remnants(bundle_id: &str, app_name: &str, ctx: &ScanCtx) -> Vec<CleanupItem> {
    let mut items = Vec::new();
    // A degenerate bundle id would prefix/substring-match half the Library.
    if bundle_id.is_empty() || !bundle_id.contains('.') {
        return items;
    }
    let lib = ctx.home.join("Library");
    let fuzzy_allowed = app_name.chars().count() >= MIN_FUZZY_NAME_LEN;

    // Directories matched by exact bundle id or fuzzy app name.
    for sub in ["Application Support", "Caches", "Logs"] {
        scan_named(
            &lib.join(sub),
            bundle_id,
            app_name,
            fuzzy_allowed,
            &mut items,
        );
    }
    // `<bundle_id>*.plist`, anchored at a `.` boundary after the id.
    for sub in ["Preferences", "LaunchAgents"] {
        scan_prefixed_plists(&lib.join(sub), bundle_id, app_name, &mut items);
    }
    // Fixed, fully-derived paths: exact bundle-id matches by construction.
    for path in [
        lib.join("Containers").join(bundle_id),
        lib.join("Saved Application State")
            .join(format!("{bundle_id}.savedState")),
        lib.join("WebKit").join(bundle_id),
        lib.join("HTTPStorages").join(bundle_id),
    ] {
        if path.symlink_metadata().is_ok() {
            items.push(exact_match(path, app_name));
        }
    }
    // Group containers are team-id prefixed, e.g. `ABCDE12345.com.foo.bar`.
    scan_group_containers(
        &lib.join("Group Containers"),
        bundle_id,
        app_name,
        &mut items,
    );
    items
}

fn exact_match(path: PathBuf, app_name: &str) -> CleanupItem {
    let size = size_of(&path);
    CleanupItem::new(
        path,
        Category::AppRemnant,
        size,
        SafetyTier::Review,
        format!("Created by {app_name}"),
    )
}

fn fuzzy_match(path: PathBuf, app_name: &str) -> CleanupItem {
    let size = size_of(&path);
    CleanupItem::new(
        path,
        Category::AppRemnant,
        size,
        SafetyTier::Risky,
        format!("Name matches {app_name} — verify before removing"),
    )
}

/// Entries named exactly `bundle_id` (any kind), or directories whose name
/// case-insensitively equals `app_name` (fuzzy, when allowed).
fn scan_named(
    dir: &Path,
    bundle_id: &str,
    app_name: &str,
    fuzzy_allowed: bool,
    items: &mut Vec<CleanupItem>,
) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name == bundle_id {
            items.push(exact_match(entry.path(), app_name));
        } else if fuzzy_allowed
            && name.to_lowercase() == app_name.to_lowercase()
            && entry.file_type().is_ok_and(|ft| ft.is_dir())
        {
            items.push(fuzzy_match(entry.path(), app_name));
        }
    }
}

/// Files matching `<bundle_id>(.<anything>)?.plist`. The character after the
/// bundle id must be a `.` so `com.foo.app` never claims `com.foo.apple.plist`.
fn scan_prefixed_plists(dir: &Path, bundle_id: &str, app_name: &str, items: &mut Vec<CleanupItem>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(rest) = name.strip_prefix(bundle_id) else {
            continue;
        };
        if rest.starts_with('.') && has_plist_suffix(rest) {
            items.push(exact_match(entry.path(), app_name));
        }
    }
}

/// Case-insensitive `.plist` suffix check (macOS filesystems are usually
/// case-insensitive).
fn has_plist_suffix(name: &str) -> bool {
    const SUFFIX: &str = ".plist";
    name.get(name.len().wrapping_sub(SUFFIX.len())..)
        .is_some_and(|tail| tail.eq_ignore_ascii_case(SUFFIX))
}

/// Entries containing `bundle_id` delimited by `.` (or string edges), the
/// shape of team-id-prefixed group container names.
fn scan_group_containers(
    dir: &Path,
    bundle_id: &str,
    app_name: &str,
    items: &mut Vec<CleanupItem>,
) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if contains_at_dot_boundary(name, bundle_id) {
            items.push(exact_match(entry.path(), app_name));
        }
    }
}

fn contains_at_dot_boundary(haystack: &str, needle: &str) -> bool {
    haystack.match_indices(needle).any(|(start, found)| {
        let end = start + found.len();
        let before_ok = start == 0 || haystack.as_bytes()[start - 1] == b'.';
        let after_ok = end == haystack.len() || haystack.as_bytes()[end] == b'.';
        before_ok && after_ok
    })
}

#[cfg(test)]
mod tests {
    use super::contains_at_dot_boundary;

    #[test]
    fn dot_boundary_matching() {
        assert!(contains_at_dot_boundary("com.foo.bar", "com.foo.bar"));
        assert!(contains_at_dot_boundary("ABCDE.com.foo.bar", "com.foo.bar"));
        assert!(contains_at_dot_boundary(
            "ABCDE.com.foo.bar.shared",
            "com.foo.bar"
        ));
        assert!(!contains_at_dot_boundary(
            "ABCDE.com.foo.barbaz",
            "com.foo.bar"
        ));
        assert!(!contains_at_dot_boundary("ABCDEcom.foo.bar", "com.foo.bar"));
        assert!(!contains_at_dot_boundary("unrelated", "com.foo.bar"));
    }
}
