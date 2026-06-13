//! End-to-end benchmark for the three-stage duplicate funnel.
//!
//! Fixture: ~2000 files of 8 KiB each, ~30% of which belong to duplicate
//! pairs (1400 unique + 300 duplicated pairs = 2000 files, 600 dupes).
//! The fixture is built once in setup; only `find_duplicates` is timed.

use criterion::{criterion_group, criterion_main, Criterion};
use std::fs;
use std::hint::black_box;
use std::path::PathBuf;
use tabibu_dupes::{find_duplicates, DupeConfig};
use tabibu_engine::CancelToken;

const FILE_LEN: usize = 8 * 1024;
const UNIQUE_FILES: u64 = 1400;
const DUPE_PAIRS: u64 = 300;

/// Deterministic per-seed content: the seed's little-endian bytes cycled
/// to `FILE_LEN`, so distinct seeds yield distinct contents.
fn content(seed: u64) -> Vec<u8> {
    seed.to_le_bytes()
        .iter()
        .copied()
        .cycle()
        .take(FILE_LEN)
        .collect()
}

fn build_fixture(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for i in 0..UNIQUE_FILES {
        let path = dir.join(format!("unique_{i}.bin"));
        fs::write(&path, content(i)).unwrap();
        files.push(path);
    }
    for i in 0..DUPE_PAIRS {
        let payload = content(1_000_000 + i);
        for side in ["a", "b"] {
            let path = dir.join(format!("dupe_{i}_{side}.bin"));
            fs::write(&path, &payload).unwrap();
            files.push(path);
        }
    }
    files
}

fn bench_find_duplicates(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let files = build_fixture(dir.path());
    let cfg = DupeConfig::default();
    let cancel = CancelToken::new();

    c.bench_function("find_duplicates_2k_files_30pct_dupes", |b| {
        b.iter(|| {
            let groups = find_duplicates(black_box(&files), &cfg, &cancel, &|_| {}).unwrap();
            assert_eq!(groups.len(), usize::try_from(DUPE_PAIRS).unwrap());
            black_box(groups)
        });
    });
}

criterion_group!(benches, bench_find_duplicates);
criterion_main!(benches);
