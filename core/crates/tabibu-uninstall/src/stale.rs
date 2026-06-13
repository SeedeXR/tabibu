//! Stale binaries: broken symlinks in user-managed `bin` directories.

use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;
use tabibu_engine::{CancelToken, Category, CleanupItem, SafetyTier, ScanCtx, ScanError, Scanner};

/// Reports files directly under the configured roots (by default
/// `/usr/local/bin` and `/opt/homebrew/bin`) that are **broken symlinks** —
/// a symlink whose target no longer exists. Real binary auditing defers to
/// brew; broken links are the safe, certain win.
///
/// The roots live outside `$HOME`; items are emitted regardless, and the
/// engine guard plus `ctx.allowed_roots` decide at runtime whether they pass.
#[derive(Debug)]
pub struct StaleBinaryScanner {
    roots: Vec<PathBuf>,
}

impl StaleBinaryScanner {
    /// Scanner over the default roots: `/usr/local/bin`, `/opt/homebrew/bin`.
    #[must_use]
    pub fn new() -> Self {
        Self::with_roots(vec![
            PathBuf::from("/usr/local/bin"),
            PathBuf::from("/opt/homebrew/bin"),
        ])
    }

    /// Scanner over custom roots (primarily for tests).
    #[must_use]
    pub fn with_roots(roots: Vec<PathBuf>) -> Self {
        Self { roots }
    }
}

impl Default for StaleBinaryScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl Scanner for StaleBinaryScanner {
    fn id(&self) -> &'static str {
        "stale_binary"
    }

    fn scan(
        &self,
        _ctx: &ScanCtx,
        cancel: &CancelToken,
        sink: &mut dyn FnMut(CleanupItem),
    ) -> Result<(), ScanError> {
        for root in &self.roots {
            if cancel.is_cancelled() {
                return Err(ScanError::Cancelled);
            }
            // Missing roots and permission errors are skipped quietly.
            let Ok(entries) = fs::read_dir(root) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(meta) = path.symlink_metadata() else {
                    continue;
                };
                if !meta.file_type().is_symlink() {
                    continue;
                }
                let Ok(target) = fs::read_link(&path) else {
                    continue;
                };
                // `fs::metadata` follows the link; NotFound means broken.
                // Any other error (e.g. permissions) is uncertain → skip.
                match fs::metadata(&path) {
                    Err(err) if err.kind() == ErrorKind::NotFound => {
                        sink(CleanupItem::new(
                            path,
                            Category::StaleBinary,
                            meta.len(),
                            SafetyTier::Review,
                            format!("Broken symlink → {}", target.display()),
                        ));
                    }
                    Ok(_) | Err(_) => {}
                }
            }
        }
        Ok(())
    }
}
