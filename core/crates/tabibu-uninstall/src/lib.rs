//! tabibu-uninstall — app-remnant hunting, orphaned support data, unused
//! apps, and stale binaries.
//!
//! Everything in this crate is **read-only** (per the engine contract) and
//! deliberately conservative: a false positive here destroys unrelated user
//! data, so uncertain matches are reported at a higher [`SafetyTier`] or
//! omitted entirely.
//!
//! [`SafetyTier`]: tabibu_engine::SafetyTier

mod apps;
mod fsutil;
mod orphan;
mod remnants;
mod stale;
mod unused;

pub use apps::{bundle_id_of, installed_apps};
pub use orphan::OrphanScanner;
pub use remnants::find_remnants;
pub use stale::StaleBinaryScanner;
pub use unused::{last_used, UnusedAppScanner};

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::SystemTime;
use tabibu_engine::Scanner;

/// All uninstall-domain scanners, ready for the engine's `run_scanner`.
///
/// `installed` is the set of bundle IDs of currently installed apps (see
/// [`installed_apps`]); `apps` carries per-app last-used dates resolved by
/// the shell (see [`last_used`]).
#[must_use]
#[allow(clippy::implicit_hasher)] // scanners store std-hashed sets; generic hashers buy nothing here
pub fn scanners(
    installed: HashSet<String>,
    apps: Vec<(PathBuf, String, Option<SystemTime>)>,
) -> Vec<Box<dyn Scanner>> {
    vec![
        Box::new(OrphanScanner::new(installed)),
        Box::new(UnusedAppScanner::new(apps)),
        Box::new(StaleBinaryScanner::new()),
    ]
}
