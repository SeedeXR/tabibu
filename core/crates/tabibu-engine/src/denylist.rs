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

/// Home-relative prefixes that are always protected (user data / live stores),
/// with the class that decides whether `reclaim` may ever waive the denial.
const HOME_DENY: &[(&str, DenyReason)] = &[
    ("Documents", DenyReason::UserData),
    ("Desktop", DenyReason::UserData),
    ("Pictures", DenyReason::UserData),
    ("Movies", DenyReason::UserData), // media libraries — sensitive, non-disposable user data
    ("Music", DenyReason::UserData),
    ("Library/Mobile Documents", DenyReason::UserData), // iCloud Drive
    ("Library/Mail", DenyReason::ReviewOnly),
    ("Library/Messages", DenyReason::ReviewOnly),
    ("Library/Photos", DenyReason::ReviewOnly),
    (
        "Library/Application Support/MobileSync",
        DenyReason::ReviewOnly,
    ), // device backups
    ("Library/Keychains", DenyReason::Secrets),
];

/// Why a path is protected, suitable for logging/tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenyReason {
    SystemPath,
    /// The user's own files (Documents, Desktop, media, iCloud Drive). `reclaim`
    /// waives this for an explicit, reversible Trash of a `DevCache` item.
    UserData,
    /// A live personal store (Mail, Messages, Photos support, device backups):
    /// waived like `UserData`, but ONLY when the caller attests the user
    /// confirmed the exact list on a review screen.
    ReviewOnly,
    /// Credentials (`~/Library/Keychains`). Never overridable, by anything.
    Secrets,
    Traversal,
    Root,
    /// Not protected itself, but an ANCESTOR of something that is (`/usr`,
    /// `/Library`, `/Users`, `~/Library`, …) — removing it would take the
    /// protected location with it. Never overridable.
    ContainsProtected,
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
///
/// Judges the path as spelled AND, when it exists, as the filesystem resolves
/// it: APFS is case-insensitive (`~/library/keychains` IS the keychain folder)
/// and symlinks reach protected places by other names (`/Volumes/Macintosh HD`
/// → `/`, an app container's `Data/Library/Keychains` → `~/Library/Keychains`).
/// A final-component symlink is judged as the link itself — trashing a link
/// never touches its target.
#[must_use]
pub fn denied(path: &Path, home: &Path) -> Option<DenyReason> {
    denied_lexical(path, home).or_else(|| {
        let real = resolved(path)?;
        let home = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
        denied_lexical(&real, &home)
    })
}

/// The on-disk spelling of `path` (case and symlinks resolved, except a final
/// symlink, which stays the link). `None` if nothing on the way exists.
fn resolved(path: &Path) -> Option<PathBuf> {
    let is_link = std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink());
    if !is_link {
        if let Ok(p) = std::fs::canonicalize(path) {
            return Some(p);
        }
    }
    Some(
        std::fs::canonicalize(path.parent()?)
            .ok()?
            .join(path.file_name()?),
    )
}

/// [`denied`] on the spelling alone, with no filesystem access. Only for a
/// walker that reports on-disk names and never follows symlinks (devscan),
/// where resolving every visited folder would be wasted work. Anything that
/// MUTATES must use [`denied`].
#[must_use]
pub fn denied_lexical(path: &Path, home: &Path) -> Option<DenyReason> {
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
    for (rel, reason) in HOME_DENY {
        if starts_with(&path, &home.join(rel)) {
            return Some(*reason);
        }
    }
    // A path that CONTAINS a protected location is protected too: `/usr` holds
    // `/usr/bin`, `/Library` holds `/Library/Apple`, `/Users` holds the home
    // folder, `~/Library` holds Mail and Keychains. Without this only OS flags
    // stood between a whole-disk selection and trashing those parents.
    let contains = |protected: &Path| starts_with(protected, &path);
    if contains(&home)
        || SYSTEM_DENY.iter().any(|p| contains(Path::new(p)))
        || HOME_DENY.iter().any(|(rel, _)| contains(&home.join(rel)))
    {
        return Some(DenyReason::ContainsProtected);
    }
    None
}

/// `true` if `path` is inside at least one allowed root *and* not denied.
/// This is the invariant every scanner's output must satisfy.
#[must_use]
pub fn permitted(path: &Path, allowed_roots: &[PathBuf], home: &Path) -> bool {
    denied(path, home).is_none() && within_roots(path, allowed_roots)
}

/// Whether `path` lies within one of `allowed_roots` (alias-normalized), with no
/// denylist judgement. `reclaim` uses this together with [`denied`] so a narrow,
/// reversible exception (rebuildable dev artifacts under a user-data dir) can be
/// permitted without widening the general [`permitted`] rule.
#[must_use]
pub fn within_roots(path: &Path, allowed_roots: &[PathBuf]) -> bool {
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
    fn ancestors_of_protected_locations_are_denied() {
        for p in [
            "/usr",                                    // holds /usr/bin
            "/private",                                // holds /private/var/db
            "/var",                                    // alias of /private/var
            "/Library",                                // holds /Library/Apple
            "/Users",                                  // holds the home folder
            "/Users/test/Library",                     // holds Mail, Keychains
            "/Users/test/Library/Application Support", // holds MobileSync
        ] {
            assert_eq!(
                denied(Path::new(p), &home()),
                Some(DenyReason::ContainsProtected),
                "{p}"
            );
        }
        // Siblings and unrelated parents stay permissible.
        for p in [
            "/Applications",
            "/opt",
            "/Users/test/Library/Caches",
            "/Users/other",
        ] {
            assert_eq!(denied(Path::new(p), &home()), None, "{p}");
        }
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

    /// The class decides what `reclaim` may waive: Keychains never, Mail and
    /// Messages only after review, ordinary user folders on an explicit pick.
    #[test]
    fn home_stores_carry_their_class() {
        for (p, want) in [
            ("/Users/test/Library/Keychains", DenyReason::Secrets),
            (
                "/Users/test/Library/Keychains/login.keychain-db",
                DenyReason::Secrets,
            ),
            ("/Users/test/Library/Mail", DenyReason::ReviewOnly),
            (
                "/Users/test/Library/Messages/chat.db",
                DenyReason::ReviewOnly,
            ),
            (
                "/Users/test/Library/Photos/Libraries",
                DenyReason::ReviewOnly,
            ),
            (
                "/Users/test/Library/Application Support/MobileSync/Backup",
                DenyReason::ReviewOnly,
            ),
            ("/Users/test/Documents/a.txt", DenyReason::UserData),
        ] {
            assert_eq!(denied(Path::new(p), &home()), Some(want), "{p}");
        }
    }

    /// Other spellings of a protected place are judged by where they point:
    /// a case variant on a case-insensitive volume, and a symlinked parent.
    #[test]
    fn spellings_are_judged_on_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        let keys = home.join("Library/Keychains");
        std::fs::create_dir_all(&keys).unwrap();
        std::fs::write(keys.join("login.keychain-db"), b"k").unwrap();
        std::os::unix::fs::symlink(&keys, home.join("alias")).unwrap();

        let via_link = home.join("alias/login.keychain-db");
        assert_eq!(denied_lexical(&via_link, &home), None);
        assert_eq!(denied(&via_link, &home), Some(DenyReason::Secrets));
        // The link itself is not the keychain: trashing it removes only the link.
        assert_eq!(denied(&home.join("alias"), &home), None);

        let case = home.join("library/keychains");
        if case.exists() {
            // case-insensitive volume (the macOS default)
            assert_eq!(denied(&case, &home), Some(DenyReason::Secrets));
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
