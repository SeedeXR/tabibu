//! tabibu-dupes — three-stage duplicate-file funnel.
//!
//! Finding duplicates byte-for-byte is expensive, so the work is funnelled
//! through three stages, each strictly cheaper than the next and each
//! discarding every file that can no longer be part of a duplicate set:
//!
//! 1. **Size** — group candidates by exact byte length (one `stat` per file).
//!    Singleton groups are dropped.
//! 2. **Sample hash** — `blake3` over the first and last 16 KiB (whole
//!    content once for files under 32 KiB). Subgroup and drop singletons.
//!    Parallelised across files with `rayon`.
//! 3. **Full hash** — streaming `blake3` over the entire content with a
//!    1 MiB read buffer. Groups that survive are true duplicate sets.
//!
//! Files that vanish or fail to read mid-scan are silently dropped from
//! their group; one bad file never poisons the rest of the run.

use rayon::prelude::*;
use serde::Serialize;
use std::collections::HashMap;
use std::fs::{self, File};
use std::hash::Hash;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use tabibu_engine::{CancelToken, Category, CleanupItem, SafetyTier};

/// Bytes sampled from each end of a file in stage 2.
const SAMPLE_LEN: usize = 16 * 1024;
const SAMPLE_LEN_U64: u64 = 16 * 1024;
const SAMPLE_LEN_I64: i64 = 16 * 1024;
/// Read-buffer size for the stage-3 streaming hash.
const FULL_HASH_BUF_LEN: usize = 1024 * 1024;

/// Errors produced by the duplicate funnel.
///
/// Per-file IO failures are *not* errors — affected files are dropped from
/// their group. Only cancellation and an unreadable walk root surface here.
#[derive(Debug, thiserror::Error)]
pub enum DupeError {
    /// The scan was cancelled via [`CancelToken`].
    #[error("duplicate scan cancelled")]
    Cancelled,
    /// The walk root itself could not be read.
    #[error("failed to read {path}: {source}")]
    Io {
        /// Path that failed.
        path: PathBuf,
        /// Underlying IO error.
        #[source]
        source: io::Error,
    },
}

/// Configuration for the duplicate funnel.
#[derive(Debug, Clone)]
pub struct DupeConfig {
    /// Files smaller than this (in bytes) are ignored entirely.
    pub min_size: u64,
}

impl Default for DupeConfig {
    fn default() -> Self {
        Self { min_size: 4096 }
    }
}

/// A confirmed set of byte-identical files.
///
/// `paths` is sorted by modification time, newest first, so `paths[0]` is
/// the default keeper.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DuplicateGroup {
    /// Size of each member file in bytes.
    pub size_bytes: u64,
    /// Lower-case hex of the full-content `blake3` hash.
    pub hash_hex: String,
    /// Member paths, newest modification time first.
    pub paths: Vec<PathBuf>,
}

/// A candidate carried through the funnel stages.
#[derive(Debug)]
struct Candidate {
    path: PathBuf,
    size: u64,
    mtime: SystemTime,
}

/// Runs the full three-stage funnel over `files`.
///
/// Each confirmed group is streamed to `on_group` as it is finalised, and
/// the complete list is also returned (sorted by size descending, then
/// hash, for deterministic output). Files below `cfg.min_size`, non-regular
/// files, and files that error mid-read are skipped silently.
///
/// # Errors
///
/// Returns [`DupeError::Cancelled`] if `cancel` is triggered; cancellation
/// is checked between stages and inside the parallel hashing loops.
pub fn find_duplicates(
    files: &[PathBuf],
    cfg: &DupeConfig,
    cancel: &CancelToken,
    on_group: &(dyn Fn(&DuplicateGroup) + Sync),
) -> Result<Vec<DuplicateGroup>, DupeError> {
    // Stage 1: group by exact size.
    let mut by_size: HashMap<u64, Vec<Candidate>> = HashMap::new();
    for path in files {
        if cancel.is_cancelled() {
            return Err(DupeError::Cancelled);
        }
        // Vanished or unreadable files are simply not candidates.
        let Ok(meta) = fs::symlink_metadata(path) else {
            continue;
        };
        if !meta.is_file() || meta.len() < cfg.min_size {
            continue;
        }
        by_size.entry(meta.len()).or_default().push(Candidate {
            path: path.clone(),
            size: meta.len(),
            mtime: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
        });
    }
    let survivors = prune_singletons(by_size);
    if cancel.is_cancelled() {
        return Err(DupeError::Cancelled);
    }

    // Stage 2: head + tail sample hash, in parallel.
    let sampled: Vec<((u64, [u8; 32]), Candidate)> = survivors
        .into_par_iter()
        .filter_map(|c| {
            if cancel.is_cancelled() {
                return None;
            }
            let hash = sample_hash(&c.path, c.size).ok()?;
            Some(((c.size, *hash.as_bytes()), c))
        })
        .collect();
    if cancel.is_cancelled() {
        return Err(DupeError::Cancelled);
    }
    let survivors = prune_singletons(collect_groups(sampled));

    // Stage 3: full streaming content hash, in parallel.
    let hashed: Vec<((u64, [u8; 32]), Candidate)> = survivors
        .into_par_iter()
        .filter_map(|c| {
            if cancel.is_cancelled() {
                return None;
            }
            match full_hash(&c.path, cancel) {
                Ok(Some(hash)) => Some(((c.size, *hash.as_bytes()), c)),
                // IO error or cancelled mid-file: drop the file silently;
                // cancellation is re-checked after the loop.
                Ok(None) | Err(_) => None,
            }
        })
        .collect();
    if cancel.is_cancelled() {
        return Err(DupeError::Cancelled);
    }

    let mut groups: Vec<DuplicateGroup> = collect_groups(hashed)
        .into_iter()
        .filter(|(_, members)| members.len() > 1)
        .map(|((size, hash_bytes), mut members)| {
            // Newest first; tie-break on path for determinism.
            members.sort_by(|a, b| b.mtime.cmp(&a.mtime).then_with(|| a.path.cmp(&b.path)));
            DuplicateGroup {
                size_bytes: size,
                hash_hex: blake3::Hash::from_bytes(hash_bytes).to_hex().to_string(),
                paths: members.into_iter().map(|c| c.path).collect(),
            }
        })
        .collect();
    groups.sort_by(|a, b| {
        b.size_bytes
            .cmp(&a.size_bytes)
            .then_with(|| a.hash_hex.cmp(&b.hash_hex))
    });

    for group in &groups {
        on_group(group);
    }
    Ok(groups)
}

/// Walks `root` (without following symlinks) and collects regular files of
/// at least `min_size` bytes, sorted by path. Unreadable subdirectories and
/// entries are skipped silently.
///
/// # Errors
///
/// Returns [`DupeError::Io`] if `root` itself cannot be read, and
/// [`DupeError::Cancelled`] if `cancel` is triggered mid-walk.
pub fn collect_candidates(
    root: &Path,
    min_size: u64,
    cancel: &CancelToken,
) -> Result<Vec<PathBuf>, DupeError> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    let mut at_root = true;
    while let Some(dir) = stack.pop() {
        if cancel.is_cancelled() {
            return Err(DupeError::Cancelled);
        }
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(source) if at_root => return Err(DupeError::Io { path: dir, source }),
            Err(_) => continue,
        };
        at_root = false;
        for entry in entries.flatten() {
            // `file_type` and `metadata` on a `DirEntry` do not follow
            // symlinks, so links are never traversed or collected.
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                stack.push(entry.path());
            } else if file_type.is_file() {
                if let Ok(meta) = entry.metadata() {
                    if meta.len() >= min_size {
                        out.push(entry.path());
                    }
                }
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Converts a confirmed group into reviewable cleanup items: every path
/// *except* the newest copy (`paths[0]`) becomes a [`CleanupItem`] in
/// [`Category::Duplicate`] at [`SafetyTier::Review`]. Duplicates are never
/// auto-selected or hard-deleted; the reason text names the kept copy.
#[must_use]
pub fn to_cleanup_items(group: &DuplicateGroup) -> Vec<CleanupItem> {
    let Some((kept, extras)) = group.paths.split_first() else {
        return Vec::new();
    };
    extras
        .iter()
        .map(|path| {
            CleanupItem::new(
                path.clone(),
                Category::Duplicate,
                group.size_bytes,
                SafetyTier::Review,
                format!("Duplicate of {}", kept.display()),
            )
        })
        .collect()
}

/// Buckets keyed candidates into a map.
fn collect_groups<K: Eq + Hash>(pairs: Vec<(K, Candidate)>) -> HashMap<K, Vec<Candidate>> {
    let mut map: HashMap<K, Vec<Candidate>> = HashMap::new();
    for (key, candidate) in pairs {
        map.entry(key).or_default().push(candidate);
    }
    map
}

/// Drops groups that can no longer contain a duplicate pair.
fn prune_singletons<K>(map: HashMap<K, Vec<Candidate>>) -> Vec<Candidate> {
    map.into_values()
        .filter(|members| members.len() > 1)
        .flatten()
        .collect()
}

/// Stage-2 hash: first and last 16 KiB, or the whole file when it is
/// smaller than 32 KiB (so nothing is hashed twice).
fn sample_hash(path: &Path, size: u64) -> io::Result<blake3::Hash> {
    let mut file = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    if size < 2 * SAMPLE_LEN_U64 {
        io::copy(&mut file, &mut hasher)?;
    } else {
        let mut buf = vec![0u8; SAMPLE_LEN];
        file.read_exact(&mut buf)?;
        hasher.update(&buf);
        file.seek(SeekFrom::End(-SAMPLE_LEN_I64))?;
        file.read_exact(&mut buf)?;
        hasher.update(&buf);
    }
    Ok(hasher.finalize())
}

/// Stage-3 hash: full content, streamed through a 1 MiB buffer with a
/// cancellation check per chunk. Returns `Ok(None)` when cancelled.
fn full_hash(path: &Path, cancel: &CancelToken) -> io::Result<Option<blake3::Hash>> {
    let mut file = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; FULL_HASH_BUF_LEN];
    loop {
        if cancel.is_cancelled() {
            return Ok(None);
        }
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                hasher.update(&buf[..n]);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(Some(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::fs::OpenOptions;
    use std::sync::Mutex;
    use std::time::Duration;
    use tempfile::TempDir;

    fn write_file(dir: &TempDir, name: &str, contents: &[u8]) -> PathBuf {
        let path = dir.path().join(name);
        fs::write(&path, contents).unwrap();
        path
    }

    fn set_mtime(path: &Path, secs_after_epoch: u64) {
        let file = OpenOptions::new().write(true).open(path).unwrap();
        file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(secs_after_epoch))
            .unwrap();
    }

    fn run(files: &[PathBuf], min_size: u64) -> Vec<DuplicateGroup> {
        let cfg = DupeConfig { min_size };
        find_duplicates(files, &cfg, &CancelToken::new(), &|_| {}).unwrap()
    }

    #[test]
    fn detects_known_duplicates_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let payload = vec![7u8; 8192];
        let original = write_file(&dir, "original.bin", &payload);
        let copy = write_file(&dir, "copy.bin", &payload);
        let unrelated = write_file(&dir, "unrelated.bin", &vec![9u8; 5000]);

        let groups = run(&[original.clone(), copy.clone(), unrelated], 1);
        assert_eq!(groups.len(), 1);
        let group = &groups[0];
        assert_eq!(group.size_bytes, 8192);
        assert_eq!(group.hash_hex, blake3::hash(&payload).to_hex().to_string());
        let mut found = group.paths.clone();
        found.sort();
        let mut expected = vec![original, copy];
        expected.sort();
        assert_eq!(found, expected);
    }

    #[test]
    fn same_size_differing_beyond_sample_window_not_grouped() {
        // 64 KiB files identical in the first and last 16 KiB but differing
        // at offset 32 KiB: stage 2 cannot tell them apart, stage 3 must.
        let dir = tempfile::tempdir().unwrap();
        let base = vec![0xAA_u8; 64 * 1024];
        let mut tweaked = base.clone();
        tweaked[32 * 1024] = 0xBB;

        let original = write_file(&dir, "first.bin", &base);
        let copy = write_file(&dir, "second.bin", &base);
        let decoy = write_file(&dir, "decoy.bin", &tweaked);

        let groups = run(&[original.clone(), copy.clone(), decoy.clone()], 1);
        assert_eq!(groups.len(), 1, "decoy must be split off by stage 3");
        assert!(!groups[0].paths.contains(&decoy));
        assert!(groups[0].paths.contains(&original));
        assert!(groups[0].paths.contains(&copy));
    }

    #[test]
    fn small_files_under_32k_grouped() {
        let dir = tempfile::tempdir().unwrap();
        let payload = b"tiny but mighty".repeat(20); // 300 bytes
        let original = write_file(&dir, "small_one.txt", &payload);
        let copy = write_file(&dir, "small_two.txt", &payload);
        let other = write_file(&dir, "small_other.txt", &b"different cont.".repeat(20));

        let groups = run(&[original, copy, other], 1);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].paths.len(), 2);
    }

    #[test]
    fn min_size_respected_and_defaults_to_4096() {
        assert_eq!(DupeConfig::default().min_size, 4096);
        let dir = tempfile::tempdir().unwrap();
        let payload = vec![1u8; 1024];
        let files = vec![
            write_file(&dir, "below_a.bin", &payload),
            write_file(&dir, "below_b.bin", &payload),
        ];
        assert!(run(&files, 4096).is_empty(), "1 KiB dupes below min_size");
        assert_eq!(run(&files, 1).len(), 1, "same files found with min_size 1");
    }

    #[test]
    fn paths_sorted_by_mtime_descending() {
        let dir = tempfile::tempdir().unwrap();
        let payload = vec![3u8; 6000];
        let oldest = write_file(&dir, "oldest.bin", &payload);
        let newest = write_file(&dir, "newest.bin", &payload);
        let middle = write_file(&dir, "middle.bin", &payload);
        set_mtime(&oldest, 1_000_000);
        set_mtime(&middle, 2_000_000);
        set_mtime(&newest, 3_000_000);

        let groups = run(&[oldest.clone(), newest.clone(), middle.clone()], 1);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].paths, vec![newest, middle, oldest]);
    }

    #[test]
    fn to_cleanup_items_spares_newest_and_is_review_tier() {
        let dir = tempfile::tempdir().unwrap();
        let payload = vec![4u8; 6000];
        let oldest = write_file(&dir, "old_copy.bin", &payload);
        let newest = write_file(&dir, "new_copy.bin", &payload);
        set_mtime(&oldest, 1_000_000);
        set_mtime(&newest, 2_000_000);

        let groups = run(&[oldest.clone(), newest.clone()], 1);
        let items = to_cleanup_items(&groups[0]);
        assert_eq!(items.len(), 1);
        let item = &items[0];
        assert_eq!(item.path, oldest);
        assert_eq!(item.category, Category::Duplicate);
        assert_eq!(item.tier, SafetyTier::Review);
        assert_eq!(item.size_bytes, 6000);
        assert!(!item.selected, "duplicates must never be auto-selected");
        assert!(item.reason.starts_with("Duplicate of "));
        assert!(item.reason.contains(&newest.display().to_string()));
    }

    #[test]
    fn streams_groups_via_callback() {
        let dir = tempfile::tempdir().unwrap();
        let payload_one = vec![5u8; 5000];
        let payload_two = vec![6u8; 7000];
        let files = vec![
            write_file(&dir, "p1_a.bin", &payload_one),
            write_file(&dir, "p1_b.bin", &payload_one),
            write_file(&dir, "p2_a.bin", &payload_two),
            write_file(&dir, "p2_b.bin", &payload_two),
        ];
        let streamed: Mutex<Vec<DuplicateGroup>> = Mutex::new(Vec::new());
        let cfg = DupeConfig { min_size: 1 };
        let returned = find_duplicates(&files, &cfg, &CancelToken::new(), &|g| {
            streamed.lock().unwrap().push(g.clone());
        })
        .unwrap();
        assert_eq!(returned.len(), 2);
        assert_eq!(*streamed.lock().unwrap(), returned);
    }

    #[test]
    fn cancellation_returns_cancelled_error() {
        let dir = tempfile::tempdir().unwrap();
        let payload = vec![8u8; 5000];
        let files = vec![
            write_file(&dir, "c_a.bin", &payload),
            write_file(&dir, "c_b.bin", &payload),
        ];
        let cancel = CancelToken::new();
        cancel.cancel();
        let cfg = DupeConfig { min_size: 1 };
        let result = find_duplicates(&files, &cfg, &cancel, &|_| {});
        assert!(matches!(result, Err(DupeError::Cancelled)));
        assert!(matches!(
            collect_candidates(dir.path(), 1, &cancel),
            Err(DupeError::Cancelled)
        ));
    }

    #[test]
    fn vanished_or_missing_files_are_dropped_silently() {
        let dir = tempfile::tempdir().unwrap();
        let payload = vec![2u8; 5000];
        let original = write_file(&dir, "real_a.bin", &payload);
        let copy = write_file(&dir, "real_b.bin", &payload);
        let ghost = dir.path().join("never_existed.bin");

        let groups = run(&[original, copy, ghost], 1);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].paths.len(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn collect_candidates_walks_filters_and_skips_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a/b");
        fs::create_dir_all(&nested).unwrap();
        let big = write_file(&dir, "big.bin", &vec![1u8; 5000]);
        let deep = nested.join("deep.bin");
        fs::write(&deep, vec![2u8; 5000]).unwrap();
        write_file(&dir, "small.bin", &[3u8; 10]); // below min_size
        std::os::unix::fs::symlink(&big, dir.path().join("link.bin")).unwrap();

        let found = collect_candidates(dir.path(), 4096, &CancelToken::new()).unwrap();
        let mut expected = vec![big, deep];
        expected.sort();
        assert_eq!(found, expected);
    }

    #[test]
    fn collect_candidates_missing_root_errors() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("no_such_dir");
        let result = collect_candidates(&missing, 0, &CancelToken::new());
        assert!(matches!(result, Err(DupeError::Io { .. })));
    }

    #[test]
    fn duplicate_group_serializes() {
        let group = DuplicateGroup {
            size_bytes: 42,
            hash_hex: "abc123".to_owned(),
            paths: vec![PathBuf::from("/tmp/x"), PathBuf::from("/tmp/y")],
        };
        let value = serde_json::to_value(&group).unwrap();
        assert_eq!(value["size_bytes"], 42);
        assert_eq!(value["hash_hex"], "abc123");
        assert_eq!(value["paths"].as_array().unwrap().len(), 2);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(48))]

        #[test]
        fn grouped_iff_byte_identical(
            contents in proptest::collection::vec(
                proptest::collection::vec(any::<u8>(), 0..200),
                2..10,
            )
        ) {
            let dir = tempfile::tempdir().unwrap();
            let mut paths = Vec::new();
            for (i, c) in contents.iter().enumerate() {
                let path = dir.path().join(format!("f{i}.bin"));
                fs::write(&path, c).unwrap();
                paths.push(path);
            }
            let cfg = DupeConfig { min_size: 0 };
            let groups =
                find_duplicates(&paths, &cfg, &CancelToken::new(), &|_| {}).unwrap();

            let mut group_of: HashMap<&PathBuf, usize> = HashMap::new();
            for (idx, group) in groups.iter().enumerate() {
                for path in &group.paths {
                    group_of.insert(path, idx);
                }
            }
            for i in 0..paths.len() {
                for j in (i + 1)..paths.len() {
                    let identical = contents[i] == contents[j];
                    let same_group = match (group_of.get(&paths[i]), group_of.get(&paths[j])) {
                        (Some(a), Some(b)) => a == b,
                        _ => false,
                    };
                    prop_assert_eq!(same_group, identical, "files {} and {}", i, j);
                }
            }
        }
    }
}
