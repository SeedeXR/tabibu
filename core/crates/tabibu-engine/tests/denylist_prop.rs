//! Property tests for the product's core invariant (engineering guide §4.3):
//! *no returned path escapes the allowed roots*, and the guarded sink drops
//! anything a buggy/malicious scanner tries to smuggle through.

use proptest::prelude::*;
use std::path::{Path, PathBuf};
use tabibu_engine::scanner::{run_scanner, ScanCtx, Scanner};
use tabibu_engine::{denylist, CancelToken, Category, CleanupItem, SafetyTier};

fn test_ctx() -> ScanCtx {
    ScanCtx {
        home: PathBuf::from("/Users/test"),
        allowed_roots: vec![
            PathBuf::from("/Users/test/Library/Caches"),
            PathBuf::from("/Users/test/.Trash"),
            PathBuf::from("/private/var/folders/zz"),
        ],
        running_bundle_ids: std::collections::HashSet::new(),
        full_disk_access: true,
    }
}

/// Arbitrary absolute-ish path built from hostile and benign segments.
fn arb_path() -> impl Strategy<Value = PathBuf> {
    let segment = prop_oneof![
        "[a-zA-Z0-9._ -]{1,12}",
        Just("..".to_string()),
        Just("Documents".to_string()),
        Just("Library".to_string()),
        Just("Caches".to_string()),
        Just("System".to_string()),
        Just("Mail".to_string()),
    ];
    (prop::bool::ANY, prop::collection::vec(segment, 1..6)).prop_map(|(abs, segs)| {
        let mut p = PathBuf::from(if abs { "/" } else { "" });
        // Bias half the cases to start inside an allowed root so both
        // branches of the invariant get real coverage.
        if segs.len() % 2 == 0 {
            p = PathBuf::from("/Users/test/Library/Caches");
        }
        for s in &segs {
            p.push(s);
        }
        p
    })
}

/// A scanner that emits whatever path it is told to — the adversary.
struct EvilScanner(PathBuf);
impl Scanner for EvilScanner {
    fn id(&self) -> &'static str {
        "evil"
    }
    fn scan(
        &self,
        _ctx: &ScanCtx,
        _cancel: &CancelToken,
        sink: &mut dyn FnMut(CleanupItem),
    ) -> Result<(), tabibu_engine::ScanError> {
        sink(CleanupItem::new(
            self.0.clone(),
            Category::UserCache,
            1,
            SafetyTier::Safe,
            "prop test",
        ));
        Ok(())
    }
}

proptest! {
    /// Whatever the scanner emits, everything that reaches the sink is
    /// inside an allowed root and not denied.
    #[test]
    fn guarded_sink_never_leaks(path in arb_path()) {
        let ctx = test_ctx();
        let mut emitted: Vec<CleanupItem> = Vec::new();
        let mut sink = |item: CleanupItem| emitted.push(item);
        run_scanner(&EvilScanner(path), &ctx, &CancelToken::new(), &mut sink).unwrap();

        for item in &emitted {
            prop_assert!(denylist::permitted(&item.path, &ctx.allowed_roots, &ctx.home));
            prop_assert!(ctx.allowed_roots.iter().any(|r| item.path.starts_with(r)));
        }
    }

    /// `permitted` is internally consistent: it never returns true for a
    /// denied path, a relative path, or a path with `..`.
    #[test]
    fn permitted_implies_clean(path in arb_path()) {
        let ctx = test_ctx();
        if denylist::permitted(&path, &ctx.allowed_roots, &ctx.home) {
            prop_assert!(path.is_absolute());
            prop_assert!(!path.components().any(|c| c.as_os_str() == ".."));
            prop_assert!(denylist::denied(&path, &ctx.home).is_none());
        }
    }
}

#[test]
fn deny_examples_stay_denied_even_inside_roots() {
    // Belt-and-braces: even if someone widens allowed_roots to $HOME/Library,
    // Mail and iCloud must still be refused.
    let ctx = ScanCtx {
        allowed_roots: vec![PathBuf::from("/Users/test/Library")],
        ..test_ctx()
    };
    for p in [
        "/Users/test/Library/Mail/V10/INBOX.mbox",
        "/Users/test/Library/Mobile Documents/com~apple~CloudDocs/notes.txt",
        "/Users/test/Library/Keychains/login.keychain-db",
    ] {
        assert!(
            !denylist::permitted(Path::new(p), &ctx.allowed_roots, &ctx.home),
            "{p} must never be permitted"
        );
    }
}
