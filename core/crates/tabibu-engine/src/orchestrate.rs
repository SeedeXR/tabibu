//! Smart Scan orchestration: run many scanners concurrently, stream every
//! item through the denylist guard into one sink, and report per-scanner
//! outcomes honestly (a failing scanner never hides the others' results).

use crate::scanner::{run_scanner, ScanCtx, Scanner};
use crate::{CancelToken, CleanupItem};
use std::sync::Mutex;

/// Outcome of one scanner within a combined scan.
#[derive(Debug)]
pub struct ScannerOutcome {
    pub scanner_id: &'static str,
    pub items_emitted: u64,
    pub guard_rejected: u64,
    pub error: Option<String>,
}

/// Combined, honest report of a multi-scanner run.
#[derive(Debug, Default)]
pub struct SmartScanReport {
    pub outcomes: Vec<ScannerOutcome>,
    pub cancelled: bool,
}

/// Run all `scanners` concurrently (one thread each — scanners are I/O bound
/// and internally parallel where it matters). `sink` is called from multiple
/// threads under a mutex; it must be fast (enqueue, don't process).
///
/// # Panics
/// Only if a sink callback panics and poisons the internal mutexes — sinks
/// must not panic.
pub fn smart_scan(
    scanners: &[Box<dyn Scanner>],
    ctx: &ScanCtx,
    cancel: &CancelToken,
    sink: &(dyn Fn(CleanupItem) + Sync),
) -> SmartScanReport {
    let report = Mutex::new(SmartScanReport::default());

    std::thread::scope(|scope| {
        for scanner in scanners {
            let report = &report;
            scope.spawn(move || {
                let counted = Mutex::new(0u64);
                let mut guarded_sink = |item: CleanupItem| {
                    *counted.lock().expect("sink counter poisoned") += 1;
                    sink(item);
                };
                let result = run_scanner(scanner.as_ref(), ctx, cancel, &mut guarded_sink);
                let items_emitted = *counted.lock().expect("sink counter poisoned");
                let mut r = report.lock().expect("report poisoned");
                match result {
                    Ok(rejected) => r.outcomes.push(ScannerOutcome {
                        scanner_id: scanner.id(),
                        items_emitted,
                        guard_rejected: rejected,
                        error: None,
                    }),
                    Err(e) => {
                        if cancel.is_cancelled() {
                            r.cancelled = true;
                        }
                        r.outcomes.push(ScannerOutcome {
                            scanner_id: scanner.id(),
                            items_emitted,
                            guard_rejected: 0,
                            error: Some(e.to_string()),
                        });
                    }
                }
            });
        }
    });

    report.into_inner().expect("report poisoned")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::ScanError;
    use crate::{Category, SafetyTier};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct FakeScanner {
        id: &'static str,
        emit: Vec<PathBuf>,
        fail: bool,
    }
    impl Scanner for FakeScanner {
        fn id(&self) -> &'static str {
            self.id
        }
        fn scan(
            &self,
            _ctx: &ScanCtx,
            _cancel: &CancelToken,
            sink: &mut dyn FnMut(CleanupItem),
        ) -> Result<(), ScanError> {
            for p in &self.emit {
                sink(CleanupItem::new(
                    p.clone(),
                    Category::Temp,
                    1,
                    SafetyTier::Safe,
                    "t",
                ));
            }
            if self.fail {
                return Err(ScanError::Other("boom".into()));
            }
            Ok(())
        }
    }

    fn ctx() -> ScanCtx {
        ScanCtx {
            home: PathBuf::from("/Users/test"),
            allowed_roots: vec![PathBuf::from("/Users/test/Library/Caches")],
            running_bundle_ids: std::collections::HashSet::new(),
            full_disk_access: true,
        }
    }

    #[test]
    fn aggregates_streams_and_isolates_failures() {
        let ok_path = PathBuf::from("/Users/test/Library/Caches/a");
        let bad_path = PathBuf::from("/Users/test/Documents/secret");
        let scanners: Vec<Box<dyn Scanner>> = vec![
            Box::new(FakeScanner {
                id: "good",
                emit: vec![ok_path.clone(); 3],
                fail: false,
            }),
            Box::new(FakeScanner {
                id: "leaky",
                emit: vec![bad_path],
                fail: false,
            }),
            Box::new(FakeScanner {
                id: "broken",
                emit: vec![ok_path],
                fail: true,
            }),
        ];
        let seen = AtomicU64::new(0);
        let report = smart_scan(&scanners, &ctx(), &CancelToken::new(), &|_item| {
            seen.fetch_add(1, Ordering::Relaxed);
        });

        assert_eq!(report.outcomes.len(), 3);
        assert_eq!(
            seen.load(Ordering::Relaxed),
            4,
            "3 good + 1 from broken-before-failure"
        );
        let by_id = |id: &str| report.outcomes.iter().find(|o| o.scanner_id == id).unwrap();
        assert_eq!(
            by_id("leaky").guard_rejected,
            1,
            "denied path dropped by guard"
        );
        assert_eq!(by_id("leaky").items_emitted, 0);
        assert!(
            by_id("broken").error.is_some(),
            "failure reported, not hidden"
        );
        assert!(!report.cancelled);
    }
}
