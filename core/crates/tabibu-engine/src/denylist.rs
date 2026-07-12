//! Hard denylist: paths no scanner may ever return, no reclaimer may ever
//! touch. SIP makes some of these immutable anyway; the rest hold data whose
//! loss would be catastrophic. Property-tested in `tests/denylist_prop.rs`.

use std::path::{Component, Path, PathBuf};

/// Absolute path prefixes that are always protected, regardless of user.
const SYSTEM_DENY: &[&str] = &[
    "/System",
    "/bin",
    "/sbin",
    "/usr/bin",
    "/usr/sbin",
    "/usr/lib",
    "/usr/libexec",
    "/usr/share",
    "/usr/standalone",
    "/private/var/db",
    "/Library/Apple",
];

/// Home-relative prefixes that are always protected (user data / live stores).
const HOME_DENY: &[&str] = &[
    "Documents",
    "Desktop",
    "Pictures",
    "Movies", // media libraries — sensitive, non-disposable user data
    "Music",
    "Library/Mail",
    "Library/Messages",
    "Library/Mobile Documents", // iCloud Drive
    "Library/Photos",
    "Library/Keychains",
    "Library/Application Support/MobileSync", // device backups: surfaced read-only, never reclaimed
];

/// Why a path is protected, suitable for logging/tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenyReason {
    SystemPath,
    UserData,
    Traversal,
    Root,
}

fn starts_with(path: &Path, prefix: &Path) -> bool {
    path.strip_prefix(prefix).is_ok()
}

/// On macOS `/var`, `/tmp`, and `/etc` are symlinks into `/private`, so the
/// same location has two absolute spellings. Every check here is lexical —
/// normalize the alias to its `/private` form so both spellings judge
/// identically (deny `/var/db/…` like `/private/var/db/…`, and match a
/// `/tmp/…` allowed root against a `/private/tmp/…` item).
fn normalize(path: &Path) -> PathBuf {
    for alias in ["var", "tmp", "etc"] {
        if let Ok(rest) = path.strip_prefix(Path::new("/").join(alias)) {
            return Path::new("/private").join(alias).join(rest);
        }
    }
    path.to_path_buf()
}

/// Returns the reason a path is denied, or `None` if it is permissible.
/// `home` is the user's home directory (injected for testability).
#[must_use]
pub fn denied(path: &Path, home: &Path) -> Option<DenyReason> {
    // Relative paths and any `..` component are rejected outright: a path
    // must be judged by where it truly points.
    if !path.is_absolute() || path.components().any(|c| c == Component::ParentDir) {
        return Some(DenyReason::Traversal);
    }
    // Both sides of every comparison get the alias treatment: `home` can
    // itself live under an alias (e.g. a /var/folders test home).
    let path = normalize(path);
    let home = normalize(home);
    if path == Path::new("/") || path == home {
        return Some(DenyReason::Root);
    }
    for p in SYSTEM_DENY {
        if starts_with(&path, Path::new(p)) {
            return Some(DenyReason::SystemPath);
        }
    }
    for rel in HOME_DENY {
        if starts_with(&path, &home.join(rel)) {
            return Some(DenyReason::UserData);
        }
    }
    None
}

/// `true` if `path` is inside at least one allowed root *and* not denied.
/// This is the invariant every scanner's output must satisfy.
#[must_use]
pub fn permitted(path: &Path, allowed_roots: &[PathBuf], home: &Path) -> bool {
    if denied(path, home).is_some() {
        return false;
    }
    let path = normalize(path);
    allowed_roots
        .iter()
        .any(|r| starts_with(&path, &normalize(r)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> PathBuf {
        PathBuf::from("/Users/test")
    }

    #[test]
    fn system_paths_denied() {
        for p in ["/System/Library", "/usr/bin/ls", "/private/var/db/x"] {
            assert!(denied(Path::new(p), &home()).is_some(), "{p}");
        }
    }

    #[test]
    fn usr_local_is_not_denied() {
        assert_eq!(denied(Path::new("/usr/local/bin/tool"), &home()), None);
    }

    #[test]
    fn user_data_denied() {
        for p in [
            "/Users/test/Documents/thesis.pages",
            "/Users/test/Movies/wedding.mov",
            "/Users/test/Music/album/track.flac",
            "/Users/test/Library/Mail/V10/x",
            "/Users/test/Library/Mobile Documents/com~apple~CloudDocs/a",
        ] {
            assert!(denied(Path::new(p), &home()).is_some(), "{p}");
        }
    }

    #[test]
    fn caches_permitted_within_roots() {
        let roots = vec![PathBuf::from("/Users/test/Library/Caches")];
        assert!(permitted(
            Path::new("/Users/test/Library/Caches/com.foo/x"),
            &roots,
            &home()
        ));
        // outside the allowed roots → not permitted even though not denied
        assert!(!permitted(
            Path::new("/Users/test/Downloads/a.dmg"),
            &roots,
            &home()
        ));
    }

    #[test]
    fn private_aliases_judge_identically() {
        // /var is a symlink to /private/var: both spellings must be denied.
        assert_eq!(
            denied(Path::new("/var/db/dslocal/x"), &home()),
            Some(DenyReason::SystemPath)
        );
        // …and allowed-root matching must cross the alias in both directions.
        let tmp_root = vec![PathBuf::from("/tmp")];
        assert!(permitted(
            Path::new("/private/tmp/scratch"),
            &tmp_root,
            &home()
        ));
        let private_root = vec![PathBuf::from("/private/var/folders/ab")];
        assert!(permitted(
            Path::new("/var/folders/ab/T/x"),
            &private_root,
            &home()
        ));
    }

    #[test]
    fn traversal_and_relative_rejected() {
        assert_eq!(
            denied(
                Path::new("/Users/test/Library/Caches/../Documents"),
                &home()
            ),
            Some(DenyReason::Traversal)
        );
        assert_eq!(
            denied(Path::new("Library/Caches"), &home()),
            Some(DenyReason::Traversal)
        );
    }
}
