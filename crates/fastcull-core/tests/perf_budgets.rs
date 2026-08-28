//! Performance budgets from specs/01-architecture.md, enforced as tests.
//!
//! These run ONLY in release mode (`cargo test --release`): debug-build
//! decode speed is meaningless for the budgets, so under debug_assertions
//! every test prints a skip note and passes. CI runs them in a dedicated
//! advisory release step. Thresholds are the enforced column of the spec
//! table; they bind on an idle run of the development machine (issue #27),
//! and were set ~2x looser than the original baselines to absorb variance.

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

/// Budget: open + EXIF read < 1 ms per file. Tightened from 10 ms
/// after the 2026-07-27 perf fix (in-tree walker, ~5 µs measured):
/// anything near the old budget means a whole-file read or mmap snuck
/// back into the metadata pass — the exact regression this pins out.
#[test]
fn budget_open_exif_under_1ms() {
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
            med < Duration::from_millis(1),
            "{name}: open+EXIF median {med:?} (budget 1 ms)"
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
            // THE code path the loupe actually runs (decode_into a
            // pre-faulted buffer; transpose scratch prepared on spare
            // cores while the serial Huffman decode runs) — the old test
            // replicated `decode()` + rotate here and so measured a path
            // the app does not ship. Fresh buffers every iteration, same
            // as the app: no pooling is being smuggled into the number.
            // Portrait frames pay the soft-rotation too (validator M-2:
            // orientation cost must live inside the budget, and portrait
            // is the COMMON case for a pro shooter). Fixtures are
            // landscape, so force the rotate path explicitly.
            let rotated = fastcull_core::loupe::decode_oriented(&bytes, 8).unwrap();
            std::hint::black_box(rotated);
            t.elapsed()
        })
        .collect();
    let med = median(samples);
    eprintln!("BUDGET-MEDIAN {}", med.as_secs_f64() * 1000.0);
    assert!(
        med < Duration::from_millis(350),
        "full-res decode+rotate median {med:?} (budget 350 ms)"
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

/// Budget: 30 A1 frames exported as one Motion JPEG video in < 2 s
/// (video-export.md, M9).
///
/// The whole operation is I/O: 30 embedded JPEGs (~344 MB) are copied
/// byte for byte out of the RAWs, hashed on the way, and then the
/// finished file is read back and re-hashed. Nothing is decoded, so a
/// number creeping past this budget means something started decoding,
/// scaling or buffering the samples — which is precisely the change this
/// feature exists NOT to make.
///
/// The 30 sources cycle over the three reference RAWs rather than
/// creating 30 fixture files: the export reads them as 30 independent
/// frames, and a Windows runner is not asked to copy 2.4 GB of RAWs to
/// build a fixture it will delete.
///
/// The output goes under `target/`, i.e. onto a REAL disk. `/tmp` is a
/// RAM filesystem on the development machine and would make an
/// I/O-bound budget measure nothing.
#[test]
fn budget_video_export_30_frames_under_2s() {
    let Some(_serial) = measure_serially() else {
        return;
    };
    // Scoped by process AND thread, like `testutil::scratch_dir` in the
    // library: the gate runs a validator and a QE agent in parallel, in
    // different target dirs, and one shared path here means one run's
    // `remove_dir_all` deleting the other's 327 MB export mid-measurement
    // (validator finding, 2026-08-28).
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target")
        .join(format!(
            "perf-clip-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    let sources: Vec<fastcull_core::clip::ClipSource> = (0..30)
        .map(|i| fastcull_core::clip::ClipSource {
            id: i,
            path: testdata(A1_FILES[i % A1_FILES.len()]),
            name: format!("DSC{:05}.ARW", 5000 + i),
            // 33 ms apart: a 30 fps burst, the reference workload.
            time_ms: Some(i as i64 * 33),
            has_subsec: true,
        })
        .collect();

    let samples: Vec<Duration> = (0..3)
        .map(|_| {
            let plan = fastcull_core::clip::plan(
                &sources,
                &dir,
                fastcull_core::fileops::ClashPolicy::Overwrite,
            )
            .expect("the plan must build");
            assert_eq!(plan.frames.len(), 30, "all 30 frames must be exportable");
            let t = Instant::now();
            let (handle, rx) = fastcull_core::clip::execute(plan);
            let mut report = None;
            for event in &rx {
                if let fastcull_core::clip::ClipEvent::Finished(r) = event {
                    report = Some(r);
                }
            }
            let elapsed = t.elapsed();
            drop(handle);
            let report = report.expect("the export must finish");
            assert!(
                report.earned_the_green_light(),
                "the budget must measure a VERIFIED export, not a failed one: {report:?}"
            );
            elapsed
        })
        .collect();
    let med = median(samples);
    let bytes: u64 = std::fs::read_dir(&dir)
        .map(|d| {
            d.filter_map(|e| e.ok())
                .filter_map(|e| e.metadata().ok())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0);
    std::fs::remove_dir_all(&dir).ok();
    eprintln!(
        "BUDGET-MEDIAN {:.0} ms for {} MB",
        med.as_secs_f64() * 1000.0,
        bytes >> 20
    );
    assert!(
        med < Duration::from_secs(2),
        "30-frame video export median {med:?} for {} MB (budget 2 s)",
        bytes >> 20
    );
}
