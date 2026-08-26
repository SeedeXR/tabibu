//! User-managed protected paths: a single safety list honored by BOTH the app
//! and the CLI. It is consulted in [`crate::reclaim`] — the one mutating path in
//! the product — so anything overlapping a protected entry is never trashed or
//! deleted, whichever front-end asked. The list is a plain newline-delimited
//! file under the user's config dir; `home` is injected (never read from env
//! here) so it stays testable, exactly like the [`crate::denylist`].

use std::io;
use std::path::{Path, PathBuf};

/// `<home>/.config/tabibu/protected.list` — the shared protected-paths file.
#[must_use]
pub fn protected_file(home: &Path) -> PathBuf {
    home.join(".config").join("tabibu").join("protected.list")
}

/// Load the protected paths. Blank lines and `#` comments are ignored; a missing
/// or unreadable file yields an empty list — an unreadable list must never brick
/// cleanup, it just means nothing extra is protected.
#[must_use]
pub fn load(home: &Path) -> Vec<PathBuf> {
    let Ok(text) = std::fs::read_to_string(protected_file(home)) else {
        return Vec::new();
    };
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(PathBuf::from)
        .collect()
}

/// True if `path` overlaps any protected entry — it equals the entry, lies
/// inside it, OR contains it. Overlap in *either* direction is refused:
/// trashing an ancestor would take a protected child down with it, so that is
/// blocked too. Comparison is lexical/component-wise (`/foobar` does not match
/// `/foo`).
#[must_use]
pub fn is_protected(path: &Path, list: &[PathBuf]) -> bool {
    list.iter()
        .any(|p| path.starts_with(p) || p.starts_with(path))
}

/// Add `path` to the list (idempotent). Returns whether it was newly added.
///
/// # Errors
/// If the config directory or file cannot be created/written.
pub fn add(home: &Path, path: &Path) -> io::Result<bool> {
    let mut list = load(home);
    if list.iter().any(|p| p == path) {
        return Ok(false);
    }
    list.push(path.to_path_buf());
    write(home, &list)?;
    Ok(true)
}

/// Remove `path` from the list. Returns whether it was present.
///
/// # Errors
/// If the file cannot be rewritten.
pub fn remove(home: &Path, path: &Path) -> io::Result<bool> {
    let mut list = load(home);
    let before = list.len();
    list.retain(|p| p != path);
    if list.len() == before {
        return Ok(false);
    }
    write(home, &list)?;
    Ok(true)
}

fn write(home: &Path, list: &[PathBuf]) -> io::Result<()> {
    let file = protected_file(home);
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let body: String = list.iter().map(|p| format!("{}\n", p.display())).collect();
    std::fs::write(file, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_load_remove_roundtrip() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        assert!(load(home).is_empty());

        let p = PathBuf::from("/Users/x/Projects/keep");
        assert!(add(home, &p).unwrap()); // newly added
        assert!(!add(home, &p).unwrap()); // idempotent
        assert_eq!(load(home), vec![p.clone()]);

        assert!(remove(home, &p).unwrap());
        assert!(!remove(home, &p).unwrap()); // already gone
        assert!(load(home).is_empty());
    }

    #[test]
    fn overlap_is_refused_in_both_directions() {
        let list = vec![PathBuf::from("/Users/x/keep")];
        assert!(is_protected(Path::new("/Users/x/keep"), &list)); // exact
        assert!(is_protected(Path::new("/Users/x/keep/sub/file"), &list)); // inside
        assert!(is_protected(Path::new("/Users/x"), &list)); // ancestor of protected
        assert!(!is_protected(Path::new("/Users/x/keepsake"), &list)); // no false prefix
        assert!(!is_protected(Path::new("/Users/y"), &list)); // unrelated
    }

    #[test]
    fn comments_and_blanks_are_ignored() {
        let home = tempfile::tempdir().unwrap();
        let file = protected_file(home.path());
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "# a comment\n\n  /Users/x/keep  \n").unwrap();
        assert_eq!(load(home.path()), vec![PathBuf::from("/Users/x/keep")]);
    }
}
