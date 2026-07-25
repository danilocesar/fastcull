//! Performance budgets from specs/01-architecture.md, enforced as tests.
//!
//! These run ONLY in release mode (`cargo test --release`): debug-build
//! decode speed is meaningless for the budgets, so under debug_assertions
//! every test prints a skip note and passes. CI runs them in a dedicated
//! release step. Thresholds are the CI column of the spec table (~2x looser
//! than the reference-machine baselines to absorb runner variance).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use fastcull_core::pipeline::{make_grid_thumb, JobSpec, Pipeline, SessionEvent};

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

const A1_FILES: [&str; 3] = [
    "A1_full_compressed.ARW",
    "A1_full_lossless_compressed.ARW",
    "A1_full_uncompressed.ARW",
];

/// Budget tests must never time each other's noise: cargo runs test fns on
/// parallel threads, and the throughput test saturates every core. Each test
/// takes this lock so measurements are serial no matter how cargo is invoked.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn measure_serially() -> Option<std::sync::MutexGuard<'static, ()>> {
    if cfg!(debug_assertions) {
        eprintln!("perf budget skipped: debug build (run with --release)");
        return None;
    }
    Some(
        SERIAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    )
}

fn spec_for(path: PathBuf) -> JobSpec {
    let md = std::fs::metadata(&path).unwrap();
    JobSpec {
        path,
        size: md.len(),
        mtime: md.modified().ok(),
    }
}

fn median(mut samples: Vec<Duration>) -> Duration {
    samples.sort();
    samples[samples.len() / 2]
}

/// Budget: open + EXIF read < 10 ms per file.
#[test]
fn budget_open_exif_under_10ms() {
    let Some(_serial) = measure_serially() else {
        return;
    };
    for name in A1_FILES {
        let path = testdata(name);
        let samples: Vec<Duration> = (0..7)
            .map(|_| {
                let t = Instant::now();
                fastcull_core::exif::read_exif_summary(&path).unwrap();
                t.elapsed()
            })
            .collect();
        let med = median(samples);
        assert!(
            med < Duration::from_millis(10),
            "{name}: open+EXIF median {med:?} (budget 10 ms)"
        );
    }
}

/// Budget: grid thumb (extract + decode + resize + encode) < 25 ms per file.
#[test]
fn budget_grid_thumb_under_25ms() {
    let Some(_serial) = measure_serially() else {
        return;
    };
    for name in A1_FILES {
        let spec = spec_for(testdata(name));
        let samples: Vec<Duration> = (0..7)
            .map(|_| {
                let t = Instant::now();
                make_grid_thumb(&spec).unwrap();
                t.elapsed()
            })
            .collect();
        let med = median(samples);
        assert!(
            med < Duration::from_millis(25),
            "{name}: grid thumb median {med:?} (budget 25 ms)"
        );
    }
}

/// Budget: full-resolution 8640x5760 embedded-JPEG decode < 350 ms.
#[test]
fn budget_fullres_decode_under_350ms() {
    let Some(_serial) = measure_serially() else {
        return;
    };
    let path = testdata(A1_FILES[1]);
    let mut file = std::fs::File::open(&path).unwrap();
    let previews = fastcull_core::raw::find_embedded_jpegs(&mut file).unwrap();
    let fullres = previews.fullres().unwrap().clone();
    let bytes = fastcull_core::raw::read_jpeg(&mut file, &fullres).unwrap();

    let samples: Vec<Duration> = (0..5)
        .map(|_| {
            let t = Instant::now();
            let mut d = zune_jpeg::JpegDecoder::new(&bytes);
            d.decode().unwrap();
            t.elapsed()
        })
        .collect();
    let med = median(samples);
    assert!(
        med < Duration::from_millis(350),
        "full-res decode median {med:?} (budget 350 ms)"
    );
}

/// Budget: cold pipeline throughput > 60 files/sec on all cores.
#[test]
fn budget_pipeline_throughput_over_60_per_sec() {
    let Some(_serial) = measure_serially() else {
        return;
    };
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    let jobs: Vec<JobSpec> = A1_FILES
        .into_iter()
        .map(|n| spec_for(testdata(n)))
        .cycle()
        .take(60)
        .collect();
    let total = jobs.len();
    let t = Instant::now();
    let (pipeline, rx) = Pipeline::start(jobs, None, threads);
    let mut done = 0;
    while done < total {
        match rx.recv_timeout(Duration::from_secs(60)).unwrap() {
            SessionEvent::ThumbReady { .. } | SessionEvent::Failed { .. } => done += 1,
            SessionEvent::MetadataReady { .. } | SessionEvent::Sidecar { .. } => {}
        }
    }
    let elapsed = t.elapsed();
    drop(pipeline);
    let rate = total as f64 / elapsed.as_secs_f64();
    assert!(
        rate > 60.0,
        "pipeline throughput {rate:.0} files/sec on {threads} threads (budget > 60)"
    );
}
