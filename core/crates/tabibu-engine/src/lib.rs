//! Tabibu engine: shared types, safety invariants, and orchestration.
//!
//! The backbone contract of the product (engineering guide §4):
//! - Scanning is **read-only** ([`Scanner`]); reclaiming is **mutating with
//!   undo** ([`reclaim`]). The two never mix.
//! - No scanner output may escape the hard [`denylist`]. Property-tested.
//! - Anything below `Safe` is trashed, never deleted. An undo manifest is
//!   written to disk *before* the first mutation.
//! - Reported bytes are measured post-op, never estimated.

pub mod cancel;
pub mod denylist;
pub mod item;
pub mod orchestrate;
pub mod reclaim;
pub mod scanner;
pub mod undo;

pub use cancel::CancelToken;
pub use item::{Category, CleanupItem, ReclaimAction, SafetyTier};
pub use orchestrate::{smart_scan, ScannerOutcome, SmartScanReport};
pub use reclaim::{reclaim, ReclaimError, ReclaimReport};
pub use scanner::{ScanCtx, ScanError, Scanner};
