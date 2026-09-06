# Architecture

## Crates

```
fastcull-core   ALL logic. No UI dependencies. Every behavior unit-testable.
fastcull-cli    Headless driver over core. Integration tests exec this binary.
fastcull-app    Thin Slint shell: maps core state -> Slint models, forwards input.
```

Rule: if a piece of code can live in `fastcull-core`, it must. The app crate contains
no business logic, no file I/O, no metadata knowledge — only model bridging and
`.slint` UI definitions. Reviewers reject logic in the app crate.

**Windows subsystems (issue #40, 2026-08-03)**: `fastcull-app` is built with
`#![windows_subsystem = "windows"]` — a console-subsystem exe double-clicked
in Explorer gets a console window allocated next to the app window, and
closing that console kills the process (`CTRL_CLOSE_EVENT`). To keep terminal
diagnostics working (`FASTCULL_TRACE=1`, usage errors, the drive harness —
docs/faq.md tells bug reporters to run from a terminal), `main()` first calls
`AttachConsole(ATTACH_PARENT_PROCESS)`: Windows then replaces NULL std
handles with the parent console's (GetStdHandle "Attach/detach behavior"),
and Rust's std re-queries the handle per write, so `eprintln!` reaches the
console with no further rebinding. Explicitly redirected/piped handles
(`2> trace.txt`, the screenshot tests' `Stdio::piped()`) are passed via
`STARTF_USESTDHANDLES` and honored regardless of subsystem — attach never
clobbers them. Accepted trade-off: a successful attach ties that launch to
the terminal's lifetime — closing the terminal (CTRL_CLOSE_EVENT) or a
Ctrl+C typed at its prompt terminates the app, which is standard for
console-attached processes and fine for a diagnostics run (documented in
docs/faq.md; a Ctrl+C handler is considered with the panic-visibility work,
issue #44). A double-click launch attaches to nothing and has no such
coupling. `fastcull-cli` deliberately stays console-subsystem: it is a
terminal tool. CI asserts both PE subsystem fields on every Windows build
(ci.yml "Verify Windows artifact": app = 2/GUI, cli = 3/console).

## Core modules (one spec each in `modules/`)

| Module | File | Responsibility |
|---|---|---|
| catalog | `catalog.rs` | folder scan, `ImageRecord`, session state |
| raw | `raw/` | rawler wrapper, preview extraction, A1 full-res extractor |
| pipeline | `pipeline.rs` | priority thread pool: visible > prefetch > background |
| cache | `cache.rs` | SQLite: thumbs + EXIF keyed by (path, size, mtime) |
| xmp | `xmp.rs` | sidecar read/merge/write, darktable field mapping |
| iptc | `iptc.rs` | IPTC model, templates, variable expansion |
| burst | `burst.rs` | burst grouping |
| fileops | `fileops.rs` | copy/rename engine with sidecar lockstep |
| filter | `filter.rs` | filter/sort predicates over the session |
| clip | `clip.rs`, `clip/qt.rs` | export frames as video: cadence from capture timestamps, Motion JPEG `.mov` muxer (in-tree), derived-output contract (ADR 0004) |
| transit | `transit.rs` | loupe render ladder + full-res ring eviction, as pure decision functions (spec'd in `ui-grid.md`) |

## Data flow

```
folder open
  └─ catalog: scan dir entries (instant) ──► session with placeholder records
       └─ pipeline: for each file (priority-ordered)
            ├─ cache hit (path,size,mtime)? ──► thumb+EXIF from SQLite
            └─ miss: raw: read preview bytes ─► decode ─► resize ─► cache ─► UI
user input (pick/IPTC edit)
  └─ session mutation ──► xmp: debounced sidecar write (≤1 s after last change)
copy picks
  └─ fileops: plan (rename template) ─► copy RAW+sidecar ─► verify ─► report
```

## Threading model

- **Main/UI thread**: Slint event loop only. Never blocks on I/O or decode —
  and as of the user decision 2026-08-02, **"decode" includes ALL pixel
  work**: JPEG decoding, full-frame copies into texture buffers, and
  downscaling. The M2-era deviations that budgeted such work per refresh
  (~32 thumb decodes, 2 full-res adoptions) are retired, not grandfathered:
  every texture is PREPARED on the texture-preparation worker below, and
  the UI thread only wraps a finished `SharedPixelBuffer` into a
  `slint::Image` (O(1)) and renders it. Measured motivation: a full-res
  adoption copied 149 MB on the UI thread (15-25 ms spikes at 1:1 walking,
  right at the 16.6 ms frame budget), and a 5k import spent ~0.93 s of UI
  time decoding thumbnails (perf investigation 2026-07-27; issue #30).
- **Texture-preparation worker** ("the kitchen"; app crate — presentation
  plumbing, not business logic, so rule 5 keeps it out of core): ONE
  dedicated thread owning every pixels→texture conversion — thumb JPEG
  decode, the full-res SharedPixelBuffer fill, native-size wraps of the
  engine's mid rung, and full→mid downscales. Priority Full > Wrap >
  Thumb > Mid (the full-res fill is the sharpness-on-stop tail; Wrap
  feeds the transit hold). Completions NUDGE the event loop
  (`invoke_from_event_loop` → a window callback), so adoption happens as
  soon as the UI is idle; the 33 ms pump drain is the fallback, and
  adoption is UNBUDGETED (rationing O(1) wraps would turn "one tick
  later" into a visible trickle-in — persona condition).
  `SharedPixelBuffer` is atomically refcounted and `Send`; `slint::Image`
  is not, so the final wrap is the one step that stays on the UI thread.
  Staleness: MID requests are culled to the visible set at each
  submission wave; Thumb/Wrap/Full requests are deliberately NOT culled —
  thumb bytes are MOVED into their jobs (completing them preserves the
  work), and Full/Wrap serve the loupe, whose own focus/want logic
  already decides what is asked for. One worker BY DESIGN: a second
  would take a core from the decode pool that gates stop-to-sharp
  (persona IN-MY-WAY on two).
- **Pipeline pool**: rayon pool (num_cpus) executing decode jobs from a priority
  queue. Priorities: (1) visible cells, (2) loupe neighbors — ±2 at rest, and
  ±2/±8 oriented by travel while the user holds a key (ui-grid.md transit
  contract), (3) sequential background fill. Reprioritization on scroll/zoom is O(changed cells).
- **Sidecar writer**: single dedicated thread, debounced queue — sidecar writes are
  ordered and never lost (flush on session close, panic-safe via Drop).
- Core ↔ UI communication: core exposes a `SessionEvent` stream (thumb ready,
  metadata loaded, pick changed…); the app crate translates events into Slint model
  updates on the UI thread. No shared mutable state across that boundary.

## Performance budgets (regression-tested)

Enforcement: `crates/fastcull-core/tests/perf_budgets.rs` (release-mode
tests), run in every local gate round **on an idle development machine** —
that is the machine class the thresholds bind on (issue #27 decision,
2026-08-02). They are wall-clock numbers, so a loaded machine fails them
without any regression existing: measured on the dev laptop (i7-8665U,
4 cores / 8 threads, 2026-08-02), the full-res row is still green with
2 of 8 logical CPUs busy (~326 ms) and red with 4 busy (~528 ms).
Thermal state shifts the same bands: immediately after a long release
build the 2-busy case measured red (~411 ms), and green again (~313 ms)
after cooldown. A red under load is a measurement, not a verdict —
re-run idle before treating it as a failing change. The CI step is advisory-only (`continue-on-error`, user decision
2026-07-25): shared virtualized runners cannot meaningfully gate
wall-clock budgets. Skipped in debug builds, where a wall clock is not
the shipped number: since 2026-09-05 dependencies compile optimised in the
dev profile too, but the workspace crates' own code — the rotate kernel,
the pipeline, the kitchen fills — stays at opt-level 0 (see "Build
profiles" below; corrected 2026-09-05, senior-developer plan). Numbers for humans: criterion benches in
`crates/fastcull-core/benches/hot_path.rs` (`cargo bench -p fastcull-core`).

Thresholds were set ~2× looser than the decode-bound baselines to absorb
variance (the EXIF row has huge headroom on purpose — anything near 1 ms
means a whole-file read or mmap snuck back; the folder-scan row sits 20×
above its idle median, keeping the number the catalog-cache criterion always
carried — what it can and cannot prove is spelled out under the table). The
original baselines were measured on a 32-thread machine retired 2026-07-28;
since then the development machine is an i7-8665U laptop (4 cores /
8 threads). Both
columns are kept: the historical baseline for provenance, the laptop idle
medians as the numbers a gate round actually compares against today. The
thresholds themselves are untouched by this rewrite (the last one to
change was the EXIF row, tightened 10 ms → 1 ms on 2026-07-27, after the
in-tree-walker fix) — the laptop meets them with headroom since the issue-#27 orientation rework (PR #32), whose spec
record lives in `modules/raw-pipeline.md`. Note the full-res row now
includes the orientation-8 rotate (the shipped `loupe::decode_oriented`
path); the 130–150 ms baseline predates that and timed the decode alone.

| Operation | 32-thread baseline (retired 2026-07-28) | i7-8665U laptop, idle (2026-08-02) | Threshold (enforced) |
|---|---|---|---|
| open+EXIF (in-tree walker, A1) | ~5 µs | ~12 µs | < 1 ms |
| grid thumb: extract+decode+resize | 7–11 ms | 12–14 ms | < 25 ms |
| full-res 8640×5760 decode+rotate | 130–150 ms (decode only) | 250–280 ms | < 350 ms |
| pipeline throughput (all cores) | ~1,500 files/s (post-2026-07-27 EXIF fix; was ~300 mmap-capped) | ~265 files/s | > 60 files/s (4-core runner) |
| video export, 30 A1 frames (327 MB) | — (M9, 2026-08-27) | ~527 ms | < 2 s |
| folder scan, 1,000-entry dir (placeholders) | — (moved here 2026-08-30, issue #59) | ~2.5 ms | < 50 ms |

The video-export and folder-scan rows measure whole operations that decode
nothing at all (unlike the open+EXIF row, which is decode-free but times one
step of the decode path). Each guards a change of KIND rather than a slow
drift, and each states below exactly which kind, because a wall clock proves
less than it looks like it does.

The video-export row is I/O-BOUND: the export copies embedded JPEGs byte
for byte and decodes nothing, so a number walking towards the threshold
means something started decoding, scaling or buffering the frames — which
is exactly what video-export.md forbids.
It writes into `target/` on purpose; `/tmp` is a RAM filesystem on the
development machine and would measure nothing.

The folder-scan row is FILESYSTEM-METADATA-bound: `Session::open` is one
`read_dir` plus two `stat`s per RAW-extension entry, one per unpaired JPEG
and none for anything else, so the number is the runner's syscall throughput
and the row's job is to keep that count LINEAR — an O(N²) walk or a
per-entry re-sort misses the threshold at once. It is deliberately NOT the
guard for "no file contents are read": measured 2026-08-30, adding an open +
4-byte read per entry only roughly doubles the median (2.5 ms → ~5 ms),
because a 4-byte stub opens in ~3 µs on a warm cache. That claim belongs to
the clock-free unit test named below, which fails on that same mutant.

What the budget times is WARM-CACHE metadata throughput: its own untimed
warm-up scan pulls the directory and its 1,000 inodes into the page cache
first, so the timed region is syscalls rather than storage (tmpfs and btrfs
measured under ~1.5 ms apart on 2026-08-30 — immaterial against the 50 ms
threshold). Its fixture is created and deleted outside the timed region and
lives under `target/` for housekeeping, not for a disk measurement: `/tmp`
is a RAM filesystem on the development machine whose quota this repo has
exhausted before.

The row moved here on 2026-08-30 (issue #59) from
`catalog::tests::thousand_entry_scan_is_fast`, which asserted the same wall
clock inside the DEBUG unit run and therefore needed an 8× carve-out on
Windows CI for Defender: shared-runner flake, the class issue #58 removed
from the suffix walk. Only the clock moved — the structural claim (1,000
placeholders, no file contents read, the exact stat count) is asserted
clock-free in
`catalog::tests::thousand_entry_scan_yields_placeholders_without_reading_them`,
which runs in every debug workspace test run including CI's, except that its
no-read half is `cfg(unix)`-gated and so compiles out on the Windows job,
where that claim stays review-only.

## Build profiles (user decision 2026-09-05, issue #76)

The workspace `Cargo.toml` sets `[profile.dev.package."*"] opt-level = 2`:
every crate that is NOT a workspace member compiles optimised in the dev
profile — and therefore in `cargo test`, whose `test` profile inherits
`dev` — while `fastcull-core`, `fastcull-cli` and `fastcull-app` keep the
default dev `opt-level` 0 and stay debuggable. Nothing else in the profile
moves: `debug-assertions` and `overflow-checks` stay on for every crate
(the `cfg!(debug_assertions)` gates in the tests still fire: the #73
discussion ran the two then release-only tests on a binary built with the
line and both still printed their debug skip), and the release profile is
untouched. The user's words: "dependencies are not required to be compile
at debug mode. most of the time that's useless."

Why: the app's hot pixel work is in dependencies — `zune-jpeg` decodes the
embedded JPEGs (ADR 0001), `fast_image_resize` scales them, Slint's
software renderer and `jpeg-encoder` produce a `--screenshot` frame — and
at opt-level 0 a debug build decoded the A1's 8640×5760 full-res JPEG in
26-40 s on the Windows CI runner and 31 s on the development seat
(rotated; 13 s landscape), against the screenshot shutter's 60 s readiness
cap: ten Windows CI jobs failed that way on 2026-07-27 and 2026-08-01
(issues #33 and #76; `modules/ui-grid.md`, "Debug facilities"). Same
source, one profile line apart, measured on the development seat on
2026-09-05 (senior developer, #73 discussion): `decode_oriented` on
`A1_full_lossless_compressed.ARW` 31,020 ms → 1,720 ms rotated (13,000 →
819 ms landscape; release 373 ms); `window_resize_keeps_the_photo` under
the load recipe that reproduces the CI refusals — the test pinned to two
cores with `taskset -c 0,1` against six busy-loop spinners pinned to the
same two cores — 0 of 4 green → 4 of 4 green, readiness 4.1-10.1 s;
unloaded, all six full-res rungs of that script decoded in under 4 s where
the stock profile managed three in 17.4-17.7 s. In a `--start-11` run over
the three sample RAWs (the center-anchor script, idle seat, plan-time
measurement 2026-09-05) the first full-res rung landed 14.2 s after
launch without the line and 1.24-1.37 s with it (three runs), the third
rung at 14.8 s against 2.43-2.50 s, and the sharp 1:1 render at 16.5 s
against 2.7 s. The #76 commit re-measured the same script on the same
seat (developer 2026-09-05, three runs per side, the "before" built from
its own target directory): the first full-res rung landed at
15.30/17.20/15.77 s without the line and 1.15/1.24/1.36 s with it, the
cursor's own rung at 16.06/17.79/16.40 s against 1.39/1.44/1.46 s, and
the sharp 1:1 render at 17.88/19.62/18.39 s against 2.81/2.88/3.07 s —
the whole test 19.0-20.8 s against 3.6-3.8 s. Its loaded pair, same
recipe: 0 of 2 green before (both refusing with `full-res never adopted
for the 1:1 frame`) and 4 of 4 after, readiness 7.1-18.9 s. QE re-ran the
same script on the same seat as an independent sample (QE 2026-09-05,
D5): first full-res rung 14.73-15.96 s → 1.14-1.25 s, sharp 1:1 render
17.10-18.38 s → 2.03-2.66 s. Numbers are the seat's and the day's.

What it costs, and what was accepted: a cold debug build compiles every
dependency optimised once — the screenshot test binary, cold, on the
development seat on 2026-09-05: 2 m 17 s stock against 10 m 43 s with the
line (4.7×; idle apart from a few short test runs), re-measured by the
#76 commit at 2 m 19 s against 9 m 10 s (4.0×) — and CI's `rust-cache`
pays it once per toolchain change (its `save-if: main` rule means a pull
request after a rustc release pays it on every push until main
repopulates the cache). PR #80 landed the line and its first run
(33996087777, both jobs green, 2026-09-05) is that cold pass:
**ubuntu-latest 30 m 35 s** — clippy 6 m 24 s, `Tests` 11 m 53 s, the
headless-X release screenshot pass 10 m 27 s, perf budgets 58 s — and
**windows-latest 61 m 43 s** — clippy 12 m 44 s, `Tests` (the debug pass)
25 m 36 s, the Windows release screenshot pass 12 m 42 s, perf budgets
2 m 23 s, the release test-binary build 6 m 35 s. For scale, the last
cached `main` run before the line (33986518746) took 17 m 33 s on ubuntu
and 33 m 48 s on windows — cold against cached, so the pair bounds the
cost rather than isolating it. The Windows job finished 28 minutes under
its 90-minute `timeout-minutes`, so `ci.yml` was not touched (Manager's
ruling on Q1, option a). The first CACHED `main` run's durations: to be
filled by the Manager once one exists (2026-09-05). Incremental builds
of workspace code are unaffected. A measurement trap, recorded because it bit the
plan-time A/B (2026-09-05): two profile variants built into ONE target
directory get distinct test binaries but share the uplifted
`debug/fastcull-app` that `CARGO_BIN_EXE_fastcull-app` points at, so the
last build wins and a "stock" test binary spawns the optimised app — an
A/B across the profile line needs two target directories, or a rebuild
before every measurement. Not architecture: no interface, dependency,
thread or data flow changes, and the line is reversible by deletion — so
no ADR; ADR 0002's "one toolchain" consequence stands. Do not extend it:
no per-crate opt-level tweaks and no un-optimising a dependency for
debuggability without asking the user (senior-developer standing
directive, 2026-09-05).

What it changed in the test suite (`modules/ui-grid.md` for each): the
debug-profile margin under the 60 s readiness cap; the two release-only
gates that rested on the debug decode
(`transit_to_a_cold_frame_keeps_the_overlay_at_the_carried_center`,
`a_decode_failed_cursor_drops_to_fit_instead_of_masking_the_badge`) are
lifted, while the timing pins that bind in release by the perf-budgets
precedent keep their gates; the M1 test's landing dump moved from a
fixed clock to the sharp rung's own mark in the same commit, because the
lift exposed it as a wall-clock pin racing the debug KITCHEN — workspace
code, still at opt-level 0 — under the #76 load recipe
(`modules/ui-grid.md` for the mechanism and the counts); the Windows debug pass, which runs the whole
screenshot suite in this profile, stops waiting on the decode (estimated
~275 s across its `--start-11` scripts; the PR records the measured job
durations). The two-shot shutter of issue #77 was fixed FIRST, in its own
commit, because its cleanest reproduction is a stock-profile capture that
overruns the poll's 250 ms period: with the line the capture is shorter,
and the reproduction keeps only the scripts whose window is large enough
to overrun it (`modules/ui-grid.md`, "Debug facilities").

## Shutdown policy (recorded 2026-07-25)

On window close the app flushes the sidecar writer (the only durability-
critical work) and then calls `process::exit` WITHOUT joining pipeline/loupe
workers: they are read-only and the preview cache is WAL-crash-safe, while a
worker stuck in uninterruptible kernel I/O on a dying card once kept the
process alive through SIGKILL for minutes. An 8 s watchdog bounds even the
sidecar flush when the sidecars themselves live on dead storage (marks are
then lost with a stderr notice — the device is gone either way).

## Error philosophy

A single unreadable/corrupt file must never break a session: the record is flagged
`Failed(reason)`, shown as a badge in the grid, excluded from copy plans, and logged.
The pipeline continues.
