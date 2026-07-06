//! tabibu-uninstall — app-remnant hunting and orphaned support data.
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

pub use apps::{bundle_id_of, installed_apps};
pub use orphan::OrphanScanner;
pub use remnants::find_remnants;
