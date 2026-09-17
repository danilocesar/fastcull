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

## Core modules (every file in `fastcull-core/src`, and the spec in `modules/` that owns it)

| Module | File | Responsibility | Spec |
|---|---|---|---|
| catalog | `catalog.rs` | folder scan, `ImageRecord`, session state | catalog-cache |
| cache | `cache.rs` | SQLite: thumbs + EXIF keyed by (path, size, mtime) | catalog-cache |
| raw | `raw/` | the in-tree TIFF/IFD walker (`tiff.rs`, `endian.rs`), embedded-JPEG discovery and hostile-header bounds (`jpeg.rs`), bare-JPEG EXIF (`jpeg_exif.rs`), the Sony maker-note reader (`sony.rs`), the orientation kernel (`orient.rs`); rawler only as the RAW-decode and non-TIFF EXIF fallback | raw-pipeline |
| exif | `exif.rs` | `ExifSummary` and the capture-time sort key, read through the walker | raw-pipeline |
| pipeline | `pipeline.rs` | priority thread pool: visible > prefetch > background | raw-pipeline |
| loupe | `loupe.rs` | the loupe engine: two backlog workers + one focus-reserved lane, the rung ladder, the full-res byte-budget LRU | raw-pipeline (ladder), ui-grid (transit contract) |
| viewassets | `viewassets.rs` | which rung the UI holds per grid cell; adopts engine-cached rungs that emit no event | raw-pipeline |
| transit | `transit.rs` | loupe render ladder + full-res ring eviction, as pure decision functions | ui-grid |
| zoompan | `zoompan.rs` | the ×1.5 zoom ladder and pan-anchor math | ui-grid |
| pointer | `pointer.rs` | the pointer state machine: (state, input) → (state, action) | ui-grid |
| grid | `grid.rs` | grid layout, the windowed model's visible range, the re-sort reveal | ui-grid |
| selection | `selection.rs` | the multi-selection: batch, spans, burst spans, the collapse rule | ui-grid, burst-grouping |
| filter | `filter.rs` | filter/sort predicates over the session, the cursor rules | ui-grid |
| burst | `burst.rs` | burst grouping | burst-grouping |
| xmp | `xmp.rs` | sidecar read/merge/write, darktable field mapping | xmp-sidecars |
| sidecar_writer | `sidecar_writer.rs` | the dedicated debounced writer thread | xmp-sidecars |
| iptc | `iptc.rs` | IPTC model, templates, variable expansion | iptc-templates |
| fileops | `fileops.rs` | copy/rename engine with sidecar lockstep, the clash question | fileops |
| clip | `clip.rs`, `clip/qt.rs` | export frames as video: cadence from capture timestamps, Motion JPEG `.mov` muxer (in-tree), derived-output contract (ADR 0004) | video-export |

(The table listed 11 of 19 files until 2026-09-17 — `exif`, `loupe`, `viewassets`,
`zoompan`, `pointer`, `grid`, `selection` and `sidecar_writer` had no row — and
described `raw/` as a "rawler wrapper", which it stopped being on 2026-07-27.)

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
every crate that is not a workspace member compiles optimised in the dev
profile — and so in `cargo test`, whose profile inherits `dev` — while
`fastcull-core`, `fastcull-cli` and `fastcull-app` keep opt-level 0 and
stay debuggable. `debug-assertions` and `overflow-checks` stay on for every
crate; the release profile is untouched. The user's words: "dependencies
are not required to be compile at debug mode. most of the time that's
useless."

Why: the hot pixel work is in dependencies — `zune-jpeg` decodes the
embedded JPEGs, `fast_image_resize` scales them, Slint's software renderer
and `jpeg-encoder` produce a `--screenshot` frame — and at opt-level 0 a
debug build decoded the A1's full-res JPEG in 26-40 s on the Windows CI
runner and 31 s on the development seat, against the screenshot shutter's
60 s readiness cap; ten Windows CI jobs failed that way (issues #33, #76).
With the line the same decode lands in about 1.2-1.7 s in debug (release:
373 ms).

What it costs: a cold debug build compiles every dependency optimised once
— 4-4.7× the stock cold build on the development seat (2 m 17 s → 10 m 43 s
for the screenshot test binary) — and CI pays it once per change of the
rust-cache key (below). Incremental builds of workspace code are
unaffected. Two rules: an A/B across the profile line needs two target
directories (both variants share one `debug/fastcull-app`, which is what
`CARGO_BIN_EXE_fastcull-app` points at, so the last build wins); and the
line is not extended — no per-crate opt-level tweaks, no un-optimising a
dependency for debuggability without asking the user (senior-developer
standing directive, 2026-09-05). Not an ADR: no interface, dependency,
thread or data-flow change, and reversible by deletion.

What it changed in the test suite (ui-grid.md, test-harness.md): the
debug-profile margin under the 60 s cap; two release-only gates that rested
on the debug decode were lifted; the M1 transit test's landing dump moved
from a fixed clock to the sharp rung's own mark; the two-shot shutter of
issue #77 was fixed first, in its own commit. The measurements behind every
sentence above are in `specs/history/01-architecture.md`.

## The CI cache key (briefs 003 and 004, 2026-09-06)

`Swatinem/rust-cache` keys on the toolchain, the `CARGO`/`RUST`/`CC`-family
environment, `.cargo/config.toml`, `rust-toolchain`, the workspace MEMBERS'
manifests and the lockfile — never the virtual root manifest. So a
`[profile]` change alone never moved the key, and the #76 line cost five
cold CI runs across two days before anyone counted `Compiling` lines; a
hand-bumped prefix (brief 003) fixed it for a day and was retired the same
day, because a forgotten bump is silent.

The rule (brief 004): a `shell: bash` step before rust-cache parses the root
`Cargo.toml` with Python's `tomllib`, serialises its `[profile]` table as
canonical JSON (`{}` when there is none), and exposes the first eight hex
digits of its SHA-256 as `key: profile-<hash>`, which the action puts in the
restore key and the primary key alike; a parse error fails the job, and a
guard step fails it unless the component reads `profile-<8 hex>`. A comment
edit, a version bump, a reordered table or a CRLF copy do not move the key;
an `opt-level` change does. `prefix-key: v1-rust` is bumped only for a
change to the action or to the key's format, never for a profile change.
Rejected: `hashFiles('Cargo.toml')` (moves on every comment and version
bump), an `awk` range over the manifest text (captured comments), a Rust
guard test (turns one hand edit into two), and moving the profile into
`.cargo/config.toml` (relocates the user decision, moves on comments too).

A key move costs one cold pair of jobs — 27-35 min ubuntu and 58-72 min
windows, against 15 and 36 min cached — and only a main run saves
(`save-if: main`); pull requests before that main run are cold too. The
Manager deletes the orphaned entries once the new pair exists and checks
usage against the 10 GB limit (5.6 GiB over four entries, 2026-09-06);
thrash goes to the user with options.

Acceptance (briefs 003 and 004; every box ticked 2026-09-06 — the run ids,
job numbers and log lines are verbatim in `specs/history/01-architecture.md`):
- [x] The rust-cache key moves with the dev profile (003 AC1; run 34014978820).
- [x] The spec no longer claims main repopulates the cache under an unchanged
      key (003 AC2).
- [x] The first main run under the new prefix saves, and the run after it
      restores (003 AC3; 34018510289, then 004's AC3).
- [x] Both runners compute the component and the key carries it (004 AC1;
      run 34043232886, `profile-a66a9ea8` on both).
- [x] The mutants behave as stated (004 AC2; 17 fixtures, the guard's 14
      inputs and 4 loud paths).
- [x] The main run saves under the computed key and the run after it
      restores it (004 AC3; 34047309575, then PR #84's 34056017285 —
      `full match: true`, ubuntu 14 m 59 s, windows 35 m 40 s).
- [x] No sentence still says the prefix is bumped for a profile change
      (004 AC4).

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
