use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Safety tier of a cleanup candidate. Drives selection defaults and the
/// reclaim action: `Risky` is never auto-selected, and only `Safe` items may
/// ever be hard-deleted (everything else goes to the Trash).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SafetyTier {
    Safe,
    Review,
    Risky,
}

/// How an item is reclaimed. `Trash` is the default and the only action
/// permitted for `Review`/`Risky` tiers (enforced in [`crate::reclaim`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReclaimAction {
    Trash,
    Delete,
    Truncate,
}

/// Feature category an item belongs to; mirrors the review-UI grouping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Category {
    Trash,
    UserCache,
    DevCache,
    Temp,
    Log,
    Duplicate,
    LargeOldFile,
    AppRemnant,
    OrphanedSupport,
    UnusedApp,
    StaleBinary,
    Malware,
}

impl Category {
    /// Stable identifier used across the FFI boundary and in telemetry.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::Trash => "trash",
            Self::UserCache => "user_cache",
            Self::DevCache => "dev_cache",
            Self::Temp => "temp",
            Self::Log => "log",
            Self::Duplicate => "duplicate",
            Self::LargeOldFile => "large_old",
            Self::AppRemnant => "app_remnant",
            Self::OrphanedSupport => "orphan",
            Self::UnusedApp => "unused_app",
            Self::StaleBinary => "stale_binary",
            Self::Malware => "malware",
        }
    }
}

/// One reviewable cleanup candidate. `reason` is shown verbatim in the
/// review UI — write it for the user, not for the log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanupItem {
    pub path: PathBuf,
    pub category: Category,
    pub size_bytes: u64,
    pub tier: SafetyTier,
    pub reason: String,
    pub selected: bool,
    pub action: ReclaimAction,
}

impl CleanupItem {
    /// Standard constructor: selection and action defaults derived from the
    /// tier (`Safe` pre-selected; everything trashed by default).
    #[must_use]
    pub fn new(
        path: PathBuf,
        category: Category,
        size_bytes: u64,
        tier: SafetyTier,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            path,
            category,
            size_bytes,
            tier,
            reason: reason.into(),
            selected: tier == SafetyTier::Safe,
            action: ReclaimAction::Trash,
        }
    }
}
