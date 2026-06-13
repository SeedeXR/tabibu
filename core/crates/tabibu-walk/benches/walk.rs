//! Criterion benchmark for `size_tree` over a ~5000-file fixture.
//!
//! The fixture (50 directories x 100 small files, plus nesting) is built
//! once in a tempdir and reused across iterations, so the benchmark
//! measures traversal and aggregation, not fixture setup.

use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

use criterion::{criterion_group, criterion_main, Criterion};
use tabibu_walk::{size_tree, CancelToken};

const DIRS: usize = 50;
const FILES_PER_DIR: usize = 100;

fn build_fixture(root: &Path) {
    for d in 0..DIRS {
        // Nest every tenth directory one level deeper for a bit of shape.
        let dir = if d % 10 == 0 {
            root.join(format!("outer{d}")).join("inner")
        } else {
            root.join(format!("dir{d}"))
        };
        fs::create_dir_all(&dir).unwrap();
        for f in 0..FILES_PER_DIR {
            let mut file = File::create(dir.join(format!("f{f}.bin"))).unwrap();
            file.write_all(&vec![0u8; 64 + (f % 7) * 32]).unwrap();
        }
    }
}

fn bench_size_tree(c: &mut Criterion) {
    let fixture = tempfile::tempdir().unwrap();
    build_fixture(fixture.path());
    let cancel = CancelToken::new();

    c.bench_function("size_tree_5k_files", |b| {
        b.iter(|| {
            let node = size_tree(fixture.path(), &cancel, None).unwrap();
            std::hint::black_box(node.size_bytes)
        });
    });

    c.bench_function("size_tree_5k_files_depth1", |b| {
        b.iter(|| {
            let node = size_tree(fixture.path(), &cancel, Some(1)).unwrap();
            std::hint::black_box(node.size_bytes)
        });
    });
}

criterion_group!(benches, bench_size_tree);
criterion_main!(benches);
