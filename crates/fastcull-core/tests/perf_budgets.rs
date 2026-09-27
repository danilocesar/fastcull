//! Performance budgets from specs/01-architecture.md, enforced as tests.
//!
//! These run ONLY in release mode (`cargo test --release`): debug-build
//! decode speed is meaningless for the budgets, so under debug_assertions
//! every test prints a skip note and passes. CI runs them in a dedicated
//! advisory release step. Thresholds are the enforced column of the spec
//! table; they bind on an idle run of the development machine (issue #27),
//! and were set ~2x looser than the original baselines to absorb variance.
//!
//! Every row prints one `BUDGET-MEDIAN` line BEFORE its assertion, and the
//! CI step runs with `--nocapture`: the step never fails its job, so its
//! numbers are read from the log or not at all, and a line printed after
//! the assertion would vanish exactly when the row is red
//! (01-architecture.md, "Performance budgets"; brief 008).

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

/// The build's target directory — where the I/O-touching budgets put their
/// fixtures, so 1,000 files never land on the development machine's tmpfs
/// `/tmp` (whose quota this repo has exhausted before). `CARGO_TARGET_DIR`
/// wins when the caller set one: the gate runs a validator and a QE agent
/// in their own target dirs, and a fixture written outside them is invisible
/// to their cleanup. A relative override resolves against the WORKSPACE
/// root, not the test's cwd — cargo runs a test binary from its package
/// directory, so a bare `target-qe-1` would otherwise land one level deep.
fn target_dir() -> PathBuf {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    // `join` with an absolute path replaces the base, so both forms work.
    workspace.join(std::env::var_os("CARGO_TARGET_DIR").unwrap_or_else(|| "target".into()))
}

/// A fixture directory that is deleted even when an assertion panics.
///
/// Without it a red budget leaks its 1,000 files under the target
/// directory, which nothing else cleans up (`cargo clean` aside).
struct Fixture {
    dir: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
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
        eprintln!(
            "BUDGET-MEDIAN {:.3} ms (open+EXIF {name})",
            med.as_secs_f64() * 1000.0
        );
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
        eprintln!(
            "BUDGET-MEDIAN {:.1} ms (grid thumb {name})",
            med.as_secs_f64() * 1000.0
        );
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
    eprintln!(
        "BUDGET-MEDIAN {:.1} ms (full-res portrait o8 + rotate)",
        med.as_secs_f64() * 1000.0
    );
    assert!(
        med < Duration::from_millis(350),
        "full-res decode+rotate median {med:?} (budget 350 ms)"
    );
}

/// The budget fixture's full-res embedded JPEG (`A1_full_lossless_compressed.ARW`,
/// 9,751,756 bytes, baseline 4:2:2, zero restart markers) — the bytes every
/// decode row below times, read once outside the timed region.
fn budget_fullres_bytes() -> Vec<u8> {
    let path = testdata(A1_FILES[1]);
    let mut file = std::fs::File::open(&path).unwrap();
    let previews = fastcull_core::raw::find_embedded_jpegs(&mut file).unwrap();
    let fullres = previews.fullres().unwrap().clone();
    fastcull_core::raw::read_jpeg(&mut file, &fullres).unwrap()
}

/// Five fresh runs of `decode` — a shipped loupe entry point, fresh buffers
/// every time as in the app — each asserted to come out `want` (oriented:
/// a row that timed the wrong shape would be measuring something else),
/// and their median.
fn median_decode(
    want: (u32, u32),
    decode: impl Fn() -> Result<(Vec<u8>, u32, u32), String>,
) -> Duration {
    let samples: Vec<Duration> = (0..5)
        .map(|_| {
            let t = Instant::now();
            let decoded = decode().unwrap();
            let elapsed = t.elapsed();
            assert_eq!((decoded.1, decoded.2), want, "the row must time this shape");
            std::hint::black_box(decoded);
            elapsed
        })
        .collect();
    median(samples)
}

/// Budget: the full-res decode, LANDSCAPE (no rotate) < 280 ms — THE SIMD
/// CANARY (brief 008, ADR 0005; 01-architecture.md perf table). A
/// libjpeg-turbo built without its SIMD kernels decodes this frame at
/// ~308 ms into a pre-faulted buffer on a cool start (the benchmark's
/// `JSIMD_FORCENONE=1` probe, 157.5 ms with AVX2 in the same run), so the
/// threshold sits at 1.5x the saturated 186 ms median rather than the
/// table's usual 2x: a 2x threshold would pass the SIMD-less decoder.
/// `require-simd` stops a SIMD-less BUILD; this row is the runtime half.
#[test]
fn budget_fullres_landscape_decode_under_280ms() {
    let Some(_serial) = measure_serially() else {
        return;
    };
    let bytes = budget_fullres_bytes();
    let med = median_decode((8640, 5760), || {
        fastcull_core::loupe::decode_oriented(&bytes, 1)
    });
    eprintln!(
        "BUDGET-MEDIAN {:.1} ms (full-res landscape o1)",
        med.as_secs_f64() * 1000.0
    );
    assert!(
        med < Duration::from_millis(280),
        "full-res landscape decode median {med:?} (budget 280 ms — the SIMD canary)"
    );
}

/// The two screen-rung rows' threshold, in ms: AT MOST 0.9x the IDLE
/// landscape full-res median (01-architecture.md, the rung rows' rule), so
/// that a "rung" that silently became a full decode plus a resize — a
/// change of KIND — is red, as the EXIF row catches a whole-file read.
/// Set by brief 008 step 1 (2026-09-26) from its measurement: the idle
/// landscape full-res median was 168.1 ms (release, the development
/// laptop, three runs at least two minutes apart, medians 168.1 / 168.2 /
/// 167.8 ms), 0.9x of it is 151.3, rounded DOWN to a multiple of 5 ms:
/// 150. It replaced the spec's provisional 155 (0.9x the ~172 ms
/// cool-start estimate). The rungs measured 117.5 ms (3/8) and 113.8 ms
/// (2/8 + rotate) in the same runs.
const RUNG_ROW_MS: u64 = 150;

/// Budget: the 4K LANDSCAPE screen rung — the A1's full at 3/8, 3240x2160,
/// orientation 1 (no transpose; the 3x3 reduced IDCT, which has no SIMD
/// path) — under the kind guard, `RUNG_ROW_MS` (brief 008, A9).
#[test]
fn budget_screen_rung_3_8_landscape_under_the_kind_guard() {
    let Some(_serial) = measure_serially() else {
        return;
    };
    let bytes = budget_fullres_bytes();
    let med = median_decode((3240, 2160), || {
        fastcull_core::loupe::decode_scaled_oriented(&bytes, 1, 3)
    });
    eprintln!(
        "BUDGET-MEDIAN {:.1} ms (screen rung 3/8 landscape o1)",
        med.as_secs_f64() * 1000.0
    );
    assert!(
        med < Duration::from_millis(RUNG_ROW_MS),
        "3/8 screen-rung decode median {med:?} (budget {RUNG_ROW_MS} ms, at most 0.9x the \
         landscape full-res median: over it, the rung is no longer a rung)"
    );
}

/// Budget: the 4K PORTRAIT screen rung — the A1's full at 2/8 (2160x1440
/// decoded) with orientation 8 forced as the full-res row does, rotated to
/// 1440x2160 single-threaded below `orient.rs`'s 32 MiB parallel
/// threshold — under the kind guard, `RUNG_ROW_MS` (brief 008, A9).
#[test]
fn budget_screen_rung_2_8_portrait_under_the_kind_guard() {
    let Some(_serial) = measure_serially() else {
        return;
    };
    let bytes = budget_fullres_bytes();
    let med = median_decode((1440, 2160), || {
        fastcull_core::loupe::decode_scaled_oriented(&bytes, 8, 2)
    });
    eprintln!(
        "BUDGET-MEDIAN {:.1} ms (screen rung 2/8 portrait o8)",
        med.as_secs_f64() * 1000.0
    );
    assert!(
        med < Duration::from_millis(RUNG_ROW_MS),
        "2/8 portrait screen-rung decode+rotate median {med:?} (budget {RUNG_ROW_MS} ms, at \
         most 0.9x the landscape full-res median: over it, the rung is no longer a rung)"
    );
}

/// Budget: scanning a 1,000-entry folder < 50 ms (catalog-cache.md).
///
/// Moved here from `catalog::tests::thousand_entry_scan_is_fast`, which
/// asserted this wall clock inside the DEBUG unit-test run and so measured
/// the runner: it needed an 8x carve-out on Windows CI for Defender, the
/// same shared-runner flake class issue #58 removed from the suffix walk
/// (issue #59). The structural half of the old criterion — 1,000
/// placeholders, no file contents read — is now clock-free in
/// `catalog::tests::thousand_entry_scan_yields_placeholders_without_reading_them`.
///
/// What the scan does is one `read_dir` plus two `stat`s per RAW-extension
/// entry (one per unpaired JPEG, none for anything else), so this row is a
/// throughput witness for the syscall count staying LINEAR — an O(N^2) walk
/// or a per-entry re-sort shows up here immediately. What it does NOT catch,
/// measured 2026-08-30: adding an `open` + 4-byte read per entry only
/// roughly doubles the median (2.5 ms -> ~5 ms), nowhere near the threshold,
/// because opening a 4-byte stub on a warm page cache costs ~3 us. The
/// structural claim "no file contents are read" is therefore owned by
/// `catalog::tests::thousand_entry_scan_yields_placeholders_without_reading_them`
/// (which fails on that same mutant), not by this clock. The threshold keeps
/// the number the spec criterion always carried.
///
/// What is being timed is WARM-CACHE metadata throughput: the untimed
/// warm-up scan below pulls the whole directory and its 1,000 inodes into
/// the page cache, so the timed region is syscalls, not storage — measured
/// 2026-08-30 over 14 alternating rounds, tmpfs and the btrfs `target/`
/// stay under ~1.5 ms apart, immaterial against the 50 ms threshold (the
/// number does not depend on which one it ran on). The fixture still lives
/// under the target directory rather than `/tmp`, but for a housekeeping
/// reason, not a measurement one: `/tmp` is a RAM filesystem on the
/// development machine, this repo has exhausted its quota before, and
/// 1,000 files have no business landing there. Creation and deletion are
/// outside the timed region either way.
#[test]
fn budget_folder_scan_1000_entries_under_50ms() {
    let Some(_serial) = measure_serially() else {
        return;
    };
    // Scoped by process AND thread, like the video-export fixture: the gate
    // runs a validator and a QE agent in parallel, and one shared path here
    // means one run's `remove_dir_all` deleting the other's fixture
    // mid-measurement.
    let fixture = Fixture {
        dir: target_dir().join(format!(
            "perf-scan-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        )),
    };
    let dir = &fixture.dir;
    std::fs::remove_dir_all(dir).ok();
    std::fs::create_dir_all(dir).unwrap();
    for i in 0..1000 {
        std::fs::write(dir.join(format!("DSC{i:05}.ARW")), b"stub").unwrap();
    }

    // Untimed warm-up: the first scan pays for 1,000 inodes still cold from
    // the writes above, which is fixture cost, not scan cost.
    let warm = fastcull_core::catalog::Session::open(dir).expect("the fixture must scan");
    assert_eq!(
        warm.images.len(),
        1000,
        "the fixture must hold 1,000 images"
    );
    drop(warm);

    let samples: Vec<Duration> = (0..5)
        .map(|_| {
            let t = Instant::now();
            let session = fastcull_core::catalog::Session::open(dir).expect("the scan must work");
            let elapsed = t.elapsed();
            std::hint::black_box(session);
            elapsed
        })
        .collect();
    let med = median(samples);
    eprintln!(
        "BUDGET-MEDIAN {:.1} ms (folder scan, 1,000 entries)",
        med.as_secs_f64() * 1000.0
    );
    assert!(
        med < Duration::from_millis(50),
        "1,000-entry folder scan median {med:?} (budget 50 ms)"
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
    // One timed run, so the row's number is that run's rate, not a median;
    // the tag stays `BUDGET-MEDIAN` so one grep finds every row.
    eprintln!(
        "BUDGET-MEDIAN {rate:.0} files/s (pipeline throughput: one run of {total} files on \
         {threads} threads)"
    );
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
    let dir = target_dir().join(format!(
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
