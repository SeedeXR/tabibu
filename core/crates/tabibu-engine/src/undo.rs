//! Undo manifest: written to disk *before* any mutation, fsynced, and
//! updated per-item as the reclaim proceeds, so a crash mid-reclaim leaves a
//! truthful record of exactly what was touched.

use crate::item::{Category, ReclaimAction, SafetyTier};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub path: PathBuf,
    pub category: Category,
    pub size_bytes: u64,
    pub tier: SafetyTier,
    pub action: ReclaimAction,
    /// Set once the action has actually been performed.
    pub completed: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UndoManifest {
    /// Seconds since the Unix epoch at creation.
    pub created_unix: u64,
    pub entries: Vec<ManifestEntry>,
    #[serde(skip)]
    file_path: PathBuf,
}

impl UndoManifest {
    /// Create and persist a manifest for `entries` under `dir` (created if
    /// missing). Returns only after the file is durably on disk — the
    /// "manifest before mutation" invariant.
    ///
    /// # Errors
    /// Any I/O failure creating `dir` or durably writing the manifest file.
    pub fn create(dir: &Path, entries: Vec<ManifestEntry>) -> std::io::Result<Self> {
        fs::create_dir_all(dir)?;
        let created_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| std::io::Error::other(e.to_string()))?
            .as_secs();
        let file_path = dir.join(format!("undo-{created_unix}-{}.json", std::process::id()));
        let manifest = Self {
            created_unix,
            entries,
            file_path,
        };
        manifest.persist()?;
        Ok(manifest)
    }

    /// Mark the entry at `index` completed and persist the change.
    ///
    /// # Errors
    /// Any I/O failure rewriting the manifest file (the in-memory flag is
    /// still set; callers may treat this as non-fatal).
    pub fn mark_completed(&mut self, index: usize) -> std::io::Result<()> {
        if let Some(e) = self.entries.get_mut(index) {
            e.completed = true;
        }
        self.persist()
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.file_path
    }

    fn persist(&self) -> std::io::Result<()> {
        let tmp = self.file_path.with_extension("json.tmp");
        let mut f = File::create(&tmp)?;
        serde_json::to_writer_pretty(&mut f, self)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        f.sync_all()?;
        fs::rename(&tmp, &self.file_path)?;
        Ok(())
    }

    /// Load a previously written manifest (for the restore/undo UI).
    ///
    /// # Errors
    /// I/O failure reading the file, or a deserialization failure (reported
    /// as `io::Error` with the serde message) if the file is corrupt.
    pub fn load(path: &Path) -> std::io::Result<Self> {
        let data = fs::read(path)?;
        let mut m: Self =
            serde_json::from_slice(&data).map_err(|e| std::io::Error::other(e.to_string()))?;
        m.file_path = path.to_path_buf();
        Ok(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_persists_before_and_during() {
        let dir = tempfile::tempdir().unwrap();
        let entries = vec![ManifestEntry {
            path: PathBuf::from("/Users/test/Library/Caches/x"),
            category: Category::UserCache,
            size_bytes: 42,
            tier: SafetyTier::Safe,
            action: ReclaimAction::Trash,
            completed: false,
        }];
        let mut m = UndoManifest::create(dir.path(), entries).unwrap();
        assert!(m.path().exists(), "manifest must exist before any mutation");

        m.mark_completed(0).unwrap();
        let reloaded = UndoManifest::load(m.path()).unwrap();
        assert!(reloaded.entries[0].completed);
    }
}
