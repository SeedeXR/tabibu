//! Undo manifest: written to disk *before* any mutation, fsynced, then
//! persisted once more after the reclaim with the completion flags. Flags are
//! tracked in memory during the run — a crash mid-reclaim leaves entries
//! marked incomplete, which errs in the safe direction (undo may attempt an
//! item that was actually reclaimed and simply find it already in the Trash;
//! it never assumes an untouched item was reclaimed). Rewriting + fsyncing
//! the whole manifest per item was O(n²) bytes on multi-thousand-item runs.

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

    /// Mark the entry at `index` completed (in memory — call [`Self::persist`]
    /// once after the reclaim loop to write the flags out).
    pub fn mark_completed(&mut self, index: usize) {
        if let Some(e) = self.entries.get_mut(index) {
            e.completed = true;
        }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.file_path
    }

    /// Durably (re)write the manifest: tmp file, fsync, atomic rename.
    ///
    /// # Errors
    /// Any I/O failure writing the manifest file.
    pub fn persist(&self) -> std::io::Result<()> {
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

        m.mark_completed(0);
        m.persist().unwrap();
        let reloaded = UndoManifest::load(m.path()).unwrap();
        assert!(reloaded.entries[0].completed);
    }
}
