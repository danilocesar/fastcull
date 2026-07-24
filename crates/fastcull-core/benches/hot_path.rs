//! Criterion benchmarks for the culling hot path (specs/01-architecture.md).
//!
//! These produce the numbers; the enforcement lives in
//! `tests/perf_budgets.rs` (release-mode test thresholds). Run with
//! `cargo bench -p fastcull-core` after `testdata/fetch.sh`.

use std::path::PathBuf;

use criterion::{criterion_group, criterion_main, Criterion};
use fastcull_core::pipeline::{make_grid_thumb, JobSpec};

fn testdata(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/raws")
        .join(name);
    assert!(
        path.is_file(),
        "missing test file {path:?} — run testdata/fetch.sh first"
    );
    path
}

fn bench_hot_path(c: &mut Criterion) {
    let lossless = testdata("A1_full_lossless_compressed.ARW");

    c.bench_function("open_exif_a1", |b| {
        b.iter(|| fastcull_core::exif::read_exif_summary(&lossless).unwrap())
    });

    let spec = {
        let md = std::fs::metadata(&lossless).unwrap();
        JobSpec {
            path: lossless.clone(),
            size: md.len(),
            mtime: md.modified().ok(),
        }
    };
    c.bench_function("grid_thumb_a1", |b| {
        b.iter(|| make_grid_thumb(&spec).unwrap())
    });

    let fullres_bytes = {
        let mut f = std::fs::File::open(&lossless).unwrap();
        let previews = fastcull_core::raw::find_embedded_jpegs(&mut f).unwrap();
        let fr = previews.fullres().unwrap().clone();
        fastcull_core::raw::read_jpeg(&mut f, &fr).unwrap()
    };
    let mut group = c.benchmark_group("fullres");
    group.sample_size(10);
    group.bench_function("decode_8640x5760", |b| {
        b.iter(|| {
            let mut d = zune_jpeg::JpegDecoder::new(&fullres_bytes);
            d.decode().unwrap()
        })
    });
    group.finish();
}

criterion_group!(benches, bench_hot_path);
criterion_main!(benches);
