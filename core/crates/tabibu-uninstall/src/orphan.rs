//! Orphaned support data: bundle-id-named directories with no matching app.

use crate::fsutil::size_of;
use std::collections::HashSet;
use std::fs;
use tabibu_engine::{CancelToken, Category, CleanupItem, SafetyTier, ScanCtx, ScanError, Scanner};

/// First segments accepted as "reverse-DNS-looking". Anything else (vendor
/// names, plain words) is skipped: we only flag what is unambiguously a
/// bundle id.
const TLD_TOKENS: &[&str] = &["com", "org", "io", "net", "dev", "app", "co"];

/// Library locations whose immediate subdirectories are inspected.
const SUPPORT_DIRS: &[&str] = &["Application Support", "Caches", "Containers"];

/// Flags subdirectories of `~/Library/{Application Support,Caches,Containers}`
/// whose name is a bundle id with no installed app behind it.
///
/// Conservative by design: only reverse-DNS names count, running apps are
/// exempt, and everything `com.apple.*` is skipped outright. Every hit is
/// [`SafetyTier::Risky`].
#[derive(Debug)]
pub struct OrphanScanner {
    installed: HashSet<String>,
}

impl OrphanScanner {
    /// `installed` is the set of bundle IDs currently installed (see
    /// [`crate::installed_apps`]).
    #[must_use]
    #[allow(clippy::implicit_hasher)] // stored as-is; generic hashers buy nothing
    pub fn new(installed: HashSet<String>) -> Self {
        Self { installed }
    }
}

impl Scanner for OrphanScanner {
    fn id(&self) -> &'static str {
        "orphan"
    }

    fn scan(
        &self,
        ctx: &ScanCtx,
        cancel: &CancelToken,
        sink: &mut dyn FnMut(CleanupItem),
    ) -> Result<(), ScanError> {
        let lib = ctx.home.join("Library");
        for sub in SUPPORT_DIRS {
            if cancel.is_cancelled() {
                return Err(ScanError::Cancelled);
            }
            // Permission errors and missing directories are skipped quietly.
            let Ok(entries) = fs::read_dir(lib.join(sub)) else {
                continue;
            };
            for entry in entries.flatten() {
                if !entry.file_type().is_ok_and(|ft| ft.is_dir()) {
                    continue;
                }
                let name = entry.file_name();
                let Some(name) = name.to_str() else { continue };
                if !looks_like_bundle_id(name)
                    || name.starts_with("com.apple.")
                    || self.installed.contains(name)
                    || ctx.running_bundle_ids.contains(name)
                {
                    continue;
                }
                let path = entry.path();
                let size = size_of(&path);
                sink(CleanupItem::new(
                    path,
                    Category::OrphanedSupport,
                    size,
                    SafetyTier::Risky,
                    format!("No installed app with bundle ID {name} found"),
                ));
            }
        }
        Ok(())
    }
}

/// Reverse-DNS shape: at least three non-empty dot-separated segments of
/// plain identifier characters, starting with a TLD-looking token.
fn looks_like_bundle_id(name: &str) -> bool {
    let segments: Vec<&str> = name.split('.').collect();
    if segments.len() < 3 {
        return false;
    }
    let Some(first) = segments.first() else {
        return false;
    };
    if !TLD_TOKENS.iter().any(|tld| first.eq_ignore_ascii_case(tld)) {
        return false;
    }
    segments.iter().all(|segment| {
        !segment.is_empty()
            && segment
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    })
}

#[cfg(test)]
mod tests {
    use super::looks_like_bundle_id;

    #[test]
    fn bundle_id_shape() {
        assert!(looks_like_bundle_id("com.foo.bar"));
        assert!(looks_like_bundle_id("io.github.some-tool"));
        assert!(looks_like_bundle_id("org.example.app.helper"));
        assert!(!looks_like_bundle_id("com.foo")); // two segments
        assert!(!looks_like_bundle_id("Slack")); // plain name
        assert!(!looks_like_bundle_id("xyz.foo.bar")); // unknown first token
        assert!(!looks_like_bundle_id("com..bar")); // empty segment
        assert!(!looks_like_bundle_id("com.foo bar.baz")); // whitespace
    }
}
