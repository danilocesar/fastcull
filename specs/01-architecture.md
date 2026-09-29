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

**The Linux allocator (brief 008, Manager ruling 2026-09-27)**: on Linux
with glibc, `main()` first calls `mallopt(M_MMAP_THRESHOLD, 4 MiB)` through
the `libc` crate — a dependency of the app on that target alone — so glibc
maps large buffers on their own and returns them when they are freed,
instead of growing its arenas for them and keeping them; the rule, what it
covers and its residuals are
`modules/raw-pipeline.md`'s ("Memory", The Linux allocator). It is the app
crate's one `unsafe` call on Linux, as `AttachConsole` above is its one on
Windows, and it lives in the app because it configures the process from the
process's own `main`, which core does not own: core keeps the value
(`budget::MMAP_THRESHOLD`), and core's one `unsafe` block stays the Windows
total-RAM probe (`budget.rs`). The dependency behaviours it rests on —
glibc's dynamic threshold, which an explicit one switches off, and Slint's
pixel-buffer copies going through Rust's global allocator (i-slint-core
1.17.1, `sharedvector.rs` 61-62 and 348-360, reached from
`SharedPixelBuffer::clone_from_slice`) — are recorded in the canaries of
`crates/fastcull-app/Cargo.toml`, so an upgrade re-reads them. No
`#[global_allocator]` is set: one would take those copies and the
decoders' buffers out of glibc's hands, and this rule with them.

## Core modules (every file in `fastcull-core/src`, and the spec in `modules/` that owns it)

| Module | File | Responsibility | Spec |
|---|---|---|---|
| catalog | `catalog.rs` | folder scan, `ImageRecord`, session state | catalog-cache |
| cache | `cache.rs` | SQLite: thumbs + EXIF keyed by (path, size, mtime) | catalog-cache |
| raw | `raw/` | the in-tree TIFF/IFD walker (`tiff.rs`, `endian.rs`), embedded-JPEG discovery and hostile-header bounds (`jpeg.rs`), bare-JPEG EXIF (`jpeg_exif.rs`), the Sony maker-note reader (`sony.rs`), the orientation kernel (`orient.rs`); rawler only as the RAW-decode and non-TIFF EXIF fallback | raw-pipeline |
| exif | `exif.rs` | `ExifSummary` and the capture-time sort key, read through the walker | raw-pipeline |
| pipeline | `pipeline.rs` | priority thread pool: visible > prefetch > background | raw-pipeline |
| loupe | `loupe.rs` | the loupe engine: one decode worker per physical core, one of them the focus-reserved lane; the rung ladder — mid, screen rung, full-res — decoded by libjpeg-turbo (ADR 0005; CMYK and YCCK streams by zune-jpeg); the one ring and the switch rule above fit; the pixel cache (the byte-budget LRU) | raw-pipeline (ladder, ring and what it asks, decoders, memory), ui-grid (the request states) |
| budget | `budget.rs` | the machine-derived loupe sizes: the pixel cache from total RAM, the decoder count from physical cores, total RAM and `FASTCULL_DECODERS`, the startup line; `MMAP_THRESHOLD`, the value the app's Linux allocator call sets (Crates, above); the Windows total-RAM probe, core's one `unsafe` block | raw-pipeline |
| viewassets | `viewassets.rs` | which rung the UI holds per grid cell; adopts engine-cached rungs that emit no event | raw-pipeline |
| transit | `transit.rs` | loupe render ladder (the cue at fit and the pill's minimum on-time included) + the eviction of the app's two texture rings by the engine's leaned windows, as pure decision functions | ui-grid |
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
  plumbing, not business logic, so rule 5 keeps it out of core): ONE dedicated
  thread owning every pixels→texture conversion — thumb JPEG decode, the
  full-res SharedPixelBuffer fill, native-size wraps of the engine's mid rung
  and screen rung, and full→mid downscales. Priority Full > Wrap > Thumb > Mid
  (the full-res fill is the sharpness-on-stop tail and the full-res ring's
  textures at 1:1; Wrap feeds the transit hold — the mid's native-size copy
  (~5 MB) and the screen rung's (21 MB on a 4K viewport) — and dedupes per
  index AND kind, so a queued mid wrap never swallows the rung's; a screen
  rung's copy, like a full-res fill, is cooked only while the loupe is up).
  Ahead of all four goes one kind of job: a queued Thumb for a frame inside
  the fill window — the full-res texture window around the cursor that the
  fill order carries, the one the staleness rule below culls Full fills by;
  with the order's cursor out of the view there is no window, and no thumb
  goes first — pops before any Full fill, the cursor's own included, and so
  before any Wrap. At the loupe a frame's thumb is its rescue rung, what it
  shows when the cursor reaches it before any loupe rung (ui-grid.md, "The
  render ladder"): queued behind the full-res ring's 149 MB fills, it can
  reach the screen after the cursor does, and the loupe then keeps the
  previous frame's pixels — the residual hold — while a thumb, a 320 px
  decode, delays a fill very little (Manager ruling 2026-09-28, brief 008
  step-6 review F1; a change to this priority contract with its own row in
  the kitchen's unit test, ui-grid.md's "A thumb inside the fill window pops
  before any full fill"). No priority interrupts a fill already cooking, so
  the thumbs also reach the kitchen well before the cursor does (ui-grid.md,
  Virtualization).
  Among Full fills the cursor's cooks first, then the nearest to it by view
  distance, ties toward the lean of the full-res texture window (forward
  when it has none) — the order the loupe engine decodes them in
  (raw-pipeline.md, "The ring"), so the nearest member, the one a tap
  reaches first, is never cooked last. The order is core's pure
  `transit::next_fill`, the mirror of the texture rings' victim rule
  (ui-grid.md, "The render ladder"), which the kitchen applies at each pop
  (Manager ruling 2026-09-26, brief 008). Completions NUDGE the event loop (`invoke_from_event_loop` → a
  window callback), so adoption happens as soon as the UI is idle; the
  33 ms pump drain is the fallback, and adoption is UNBUDGETED (rationing
  O(1) wraps would turn "one tick later" into a visible trickle-in — persona
  condition). `SharedPixelBuffer` is atomically refcounted and `Send`;
  `slint::Image` is not, so the final wrap is the one step that stays on the
  UI thread. Staleness: MID requests are culled to the visible set at each
  submission wave, and a queued FULL fill for a frame outside the full-res
  texture window (`LoupeEngine::texture_windows`, ui-grid.md) is culled at the
  next one, so a revisit never waits behind copies of frames already passed
  (brief 008, the redesign's G4); Thumb and Wrap requests, and Full fills
  inside the window, are deliberately NOT culled — thumb bytes are MOVED into
  their jobs (completing them preserves the work), and Full/Wrap serve the
  loupe, whose own focus/want logic already decides what is asked for. The
  Full-before-Wrap order stays: a hold that starts before the kitchen has
  cooked the full-res ring's fills can find them queued ahead of its first
  rung wrap, each a 149 MB copy (ui-grid.md A5 records the delay); if that
  ever bites, the lever is a Wrap that outranks ring Fulls while the transit
  latch is on — a change to this priority contract with its own row in the
  kitchen's unit test, never a silent reorder (brief 008). One worker BY
  DESIGN: a second would take a core from the decode pool that gates
  stop-to-sharp (persona IN-MY-WAY on two).
- **Pipeline pool**: one thread per logical core (`available_parallelism`,
  which the app hands `Pipeline::start`) executing the grid-thumb decode jobs
  from a priority queue. Priorities: (1) visible cells, (2) prefetch, (3)
  sequential background fill. Reprioritization on scroll/zoom is O(changed
  cells). The loupe's ring never runs here: it runs on the loupe engine's own
  workers, below.
- **Loupe workers**: the loupe engine's threads, one per physical core
  within the bounds raw-pipeline.md sets ("The decode workers"), one of
  them focus-reserved. They decode every loupe rung — mid, screen
  rung, full-res — soft-rotate it and serve the ring. Every pixel step of a
  rung runs off the UI thread: the workers decode and rotate, the kitchen
  fills, the UI thread only wraps (brief 008; the user decision of
  2026-08-02 above).
- **Sidecar writer**: single dedicated thread, debounced queue — sidecar writes are
  ordered and never lost (flush on session close, panic-safe via Drop).
- Core ↔ UI communication: core exposes a `SessionEvent` stream (thumb ready,
  metadata loaded, pick changed…); the app crate translates events into Slint model
  updates on the UI thread. No shared mutable state across that boundary.

(Changed 2026-09-26, brief 008: the kitchen popped the LATEST Full fill first
— the focused frame's, while the loupe's neighbours were ±2, but the farthest
member's under a full-res ring of fifteen ahead; and this section called the
pipeline pool a "rayon pool (num_cpus)", which it never was.)

(Changed 2026-09-28, brief 008 step-6 review F1: a thumb inside the fill
window waited, like every thumb, behind every queued Full fill and Wrap.)

## Performance budgets (regression-tested)

Enforced by `crates/fastcull-core/tests/perf_budgets.rs` — release-mode
tests, one per row, run in every local gate round **on an idle development
machine**: that is the machine class the thresholds bind on (issue #27,
2026-08-02). They are wall-clock numbers, so a loaded or hot machine fails
them without any regression existing (the full-res row measured green with 2
of 8 logical CPUs busy and red with 4, and red straight after a long release
build) — a red under load is a measurement, not a verdict; re-run idle
before treating it as a failing change. The CI step is advisory
(`continue-on-error`, user decision 2026-07-25: shared virtualized runners
cannot gate wall clocks), and it runs with `--nocapture`, so every row's
`BUDGET-MEDIAN` line reaches both jobs' logs, green or red: the step never
fails its job, so its numbers are read from the log or not at all — brief
008's unoptimised Windows decoder (Native dependencies, below) showed first
as red rows inside a green job (Manager ruling 2026-09-26, brief 008;
senior-developer review 2026-09-26, F1). Skipped in debug builds, where the
workspace crates' own code — the rotate kernel, the pipeline, the kitchen
fills — runs at opt-level 0 even though dependencies compile optimised since
2026-09-05 ("Build profiles" below). Criterion benches in
`crates/fastcull-core/benches/hot_path.rs` (`cargo bench -p fastcull-core`)
give the numbers for humans.

Thresholds sit ~2× above the decode-bound baselines to absorb variance; the
EXIF row has huge headroom on purpose (anything near 1 ms means a whole-file
read or an mmap snuck back), and the folder-scan row sits 20× above its
idle median. The baselines were measured on a 32-thread machine retired
2026-07-28; the development seat since is an i7-8665U laptop (4 cores /
8 threads), whose idle medians are what a gate round compares against
today. The thresholds themselves last moved on 2026-07-27 (the EXIF row,
10 ms → 1 ms, after the in-tree walker), and the laptop meets them with
headroom since the issue-#27 orientation rework (PR #32, recorded in
`modules/raw-pipeline.md`). The full-res row includes the orientation-8
rotate (the shipped `loupe::decode_oriented` path); the 130–150 ms baseline
predates that and timed the decode alone.

Since 2026-09-26 the loupe decodes with libjpeg-turbo (brief 008, ADR
0005), and three rows join the full-res one, each guarding a change of KIND
rather than sitting at ~2× its median. The landscape full-res row is the
SIMD canary: a libjpeg-turbo built without its SIMD kernels decodes that
frame in ~308 ms into a pre-faulted buffer (the benchmark's
`JSIMD_FORCENONE=1` probe), so its threshold, 280 ms, sits at 1.67× the
idle median and under that figure, where 2× would never fire. The two
screen-rung rows are the rungs a 4K viewport asks for — a landscape frame's
3/8 at orientation 1 and a portrait frame's 2/8 plus its orientation-8
rotate — and their threshold is at most 0.9× the idle landscape full-res
median (0.9 × 168.1 = 151.3 ms, rounded down to 150, `RUNG_ROW_MS`), so a
"rung" that silently became a full decode plus a resize is red — the EXIF
row's reasoning, applied to scaling. All three run on the budget fixture,
`A1_full_lossless_compressed.ARW`; a 3/8 rung with a transpose is the
5K-portrait shape, a number for humans in the benches, not a gate.

| Operation | 32-thread baseline (retired 2026-07-28) | i7-8665U laptop, idle (2026-08-02) | Threshold (enforced) |
|---|---|---|---|
| open+EXIF (in-tree walker, A1) | ~5 µs | ~12 µs | < 1 ms |
| grid thumb: extract+decode+resize | 7–11 ms | 12–14 ms | < 25 ms |
| full-res 8640×5760 decode+rotate (portrait, o8) | 130–150 ms (decode only) | 250–280 ms with zune-jpeg; 215.5 ms with libjpeg-turbo (2026-09-26) | < 350 ms |
| full-res 8640×5760 decode, landscape (o1) — the SIMD canary | — (added 2026-09-26, brief 008) | 168.1 ms (2026-09-26) | < 280 ms |
| screen rung 3/8 (3240×2160) decode, landscape (o1) — the 4K landscape rung | — (added 2026-09-26, brief 008) | 117.5 ms (2026-09-26) | < 150 ms |
| screen rung 2/8 (2160×1440) decode+rotate (portrait, o8) — the 4K portrait rung | — (added 2026-09-26, brief 008) | 113.8 ms (2026-09-26) | < 150 ms |
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

Why: the hot pixel work is in dependencies — libjpeg-turbo decodes the
loupe's rungs and `zune-jpeg` the grid thumbs, `fast_image_resize` scales
them, Slint's software renderer and `jpeg-encoder` produce a `--screenshot`
frame — and at opt-level 0 a debug build decoded the A1's full-res JPEG in
26-40 s on the Windows CI runner and 31 s on the development seat, against
the screenshot shutter's 60 s readiness cap; ten Windows CI jobs failed that
way (issues #33, #76). With the line the same decode lands in about 1.7 s
rotated and 0.8 s landscape in debug (release: 373 ms) — those figures are
zune-jpeg's, the loupe's decoder until 2026-09-26. libjpeg-turbo is a C
library, built by the `cmake` crate at the CMake profile cargo's opt-level
implies (opt-level 0 → `Debug`, 1–3 with debug info → `RelWithDebInfo`,
without → `Release`; `turbojpeg-sys` sets none of its own), so under this
line the dev profile builds it `RelWithDebInfo` and without the line it
would be an unoptimised `Debug` build (`-O0`; `/Od` under MSVC): the line
keeps the loupe's debug decode optimised — on the MSVC target only because
the workspace names that target's CMake generator, Ninja, without which the
crate picks a Visual Studio generator itself, strips every `/O` flag and
MSVC compiles the library unoptimised in every profile, release included
(Native dependencies, below; corrected 2026-09-26, senior-developer review
F1: this said the line alone kept the decode optimised).

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
issue #77 was fixed first, in its own commit.

## The CI cache key (briefs 003 and 004, 2026-09-06)

`Swatinem/rust-cache` keys on the toolchain, every environment variable
whose name starts with `CARGO`, `CC`, `CFLAGS`, `CXX`, `CMAKE` or `RUST`, `.cargo/config.toml`, `rust-toolchain`, the workspace MEMBERS'
manifests and the lockfile — never the virtual root manifest. So a
`[profile]` change alone never moved the key, and the #76 line cost five
cold CI runs across two days before anyone counted `Compiling` lines; a
hand-bumped prefix (brief 003) fixed it for a day and was retired the same
day, because a forgotten bump is silent. The lockfile, the members'
manifests and `.cargo/config.toml` are hashed into the full key only, after
the environment hash; the restore key stops before them, so a change to any
of them restores the newest entry under the unchanged restore key (`full
match: false`), and only a main run saves under the new key
(senior-developer review 2026-09-27, F6: run 36294797235's restore key
equals 36285260233's while its full key moved).

The rule (brief 004): a `shell: bash` step before rust-cache parses the root
`Cargo.toml` with Python's `tomllib`, serialises its `[profile]` table as
canonical JSON (keys sorted at every level, no whitespace; `{}` when there
is none) — only `[profile]`, because the save step drops the workspace
members' own artifacts, `[workspace.package]` and `[workspace.lints]` shape
nothing cached, and a `[patch]` table changes `Cargo.lock`, which the action
already hashes — and exposes the first eight hex
digits of its SHA-256 as `key: profile-<hash>`, which the action puts in the
restore key and the primary key alike; a parse error fails the job, and a
guard step fails it unless the component reads `profile-<8 hex>`. The
guard's residual: an edit to the rust-cache `key:` line alone — dropped, or
replaced by a well-formed constant — is invisible to it; the check is
comparing the restore step's `Cache Key:` with the guard's printed line
(AC1), or offline a pyyaml assertion that `with.key` is
`profile-${{ steps.profile.outputs.hash }}` and that neither new step
carries `if:` or `continue-on-error:`. On a pull request the proof that the
key moved is the restore step alone — `Cache Key:` shows the component and
`No cache found.` follows; the post step prints nothing on a PR. A comment
edit, a version bump, a reordered table or a CRLF copy do not move the key;
an `opt-level` change does. `prefix-key: v1-rust` is bumped only for a
change to the action or to the key's format, never for a profile change.
Rejected: `hashFiles('Cargo.toml')` (moves on every comment and version
bump), an `awk` range over the manifest text (captured comments), a Rust
guard test (turns one hand edit into two), and moving the profile into
`.cargo/config.toml` (relocates the user decision, moves on comments too).

A key move costs one cold pair of jobs — 28.5-39.5 min ubuntu and
53.9-77.8 min windows over the twenty cold runs of PR #93 (brief 008,
2026-09-26 to 09-29; 27-35 and 58-72 min when measured for briefs 003 and
004), against 15 and 36 min cached — and only a main run saves
(`save-if: main`); pull requests before that main run are cold too. A key
move is not the only way to lose the pair: GitHub evicts a cache entry
nobody has read for seven days, so after a week with no run on the
repository every pull request is cold until the next main run saves —
main's pair, last used 2026-09-18, was gone when PR #93 opened (the session
audit of brief 008, S3). The job cap clears a cold Windows job with 22 % of
it to spare (ci.yml's comment has the runs). The Manager deletes the
orphaned entries once the new pair exists and checks
usage against the 10 GB limit (3.96 GiB over four entries once the v0 pair
went, 5.6 GiB with the computed pair saved, 2026-09-06);
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

## Native dependencies (ADR 0005, brief 008, 2026-09-26)

FastCull links ONE C library: libjpeg-turbo ≥ 3.0, the loupe's decoder and
the source of its N/8 screen rung, through the `turbojpeg` crate over
`turbojpeg-sys` with the `cmake`, `pkg-config` and `require-simd` features
(ADR 0005 has the decision and the benchmark; `modules/raw-pipeline.md` the
rung).

- **Every seat that runs `cargo build` needs `cmake` and `nasm`** — the
  `cmake` crate drives the vendored libjpeg-turbo's own build, NASM
  assembles its SIMD kernels — or, on Linux, a system libjpeg-turbo 3.0 or
  newer found through `pkg-config` (`TURBOJPEG_SOURCE=pkg-config`; Fedora
  44 ships 3.1.3, Ubuntu 24.04's 2.1.x is too old). A Windows seat also
  needs `ninja` on its PATH, for the Ninja generator the MSVC target names
  (below), beside the Visual Studio C++ tools the MSVC toolchain always
  needed, of any version (corrected 2026-09-26, the generator re-ruling of
  brief 008: this said a Windows seat needs Visual Studio 2022). The
  default is the vendored sources, built and linked statically on both
  targets; README's build block (the Linux one) and `docs/index.md` name
  the requirement, and ADR 0002's "contributors need only rustup" is
  narrowed by it.
- **Without NASM the build FAILS**: `require-simd` passes
  `-DREQUIRE_SIMD=ON`, and libjpeg-turbo's `simd/CMakeLists.txt` then
  raises a CMake `FATAL_ERROR` instead of falling back to a C-only
  library. Deliberate: a SIMD-less decoder is 1.95× slower at full size
  and would break every promise of the loupe silently. The runtime half of
  the guard is the landscape full-res perf row above.
- **The C library follows cargo's opt-level — on MSVC only because the target
  names its CMake generator, Ninja** (Manager ruling 2026-09-26, brief 008,
  re-ruled the same day on the developer's evidence; senior-developer review
  2026-09-26, F1). The `cmake` crate maps cargo's opt-level to a CMake
  profile (Build profiles, above), and where it leaves CMake's own flags for
  that profile alone — on Linux, and on MSVC under any named generator — the
  #76 line keeps the library optimised in debug and a release build is
  CMake's `Release`. With no generator named on the MSVC target, the crate
  picks the Visual Studio generator it finds and sets the C flags of the
  configuration it builds — `CMAKE_C_FLAGS_<BUILD_TYPE>`, the other
  configurations keeping CMake's defaults — to the `cc` crate's, each `/O`
  flag stripped (cmake 0.1.58, `src/lib.rs` 652 and 723-768), and MSVC
  compiles libjpeg-turbo at its default, unoptimised, in every profile,
  release included. Setting `CFLAGS=/O2` instead would do nothing: the same
  stripping drops it from every flag variable the crate writes. So a
  workspace `.cargo/config.toml` names the generator in its `[env]` table,
  which cargo hands to every build script run inside the checkout, the
  release workflow's included:
  `CMAKE_GENERATOR_x86_64_pc_windows_msvc = "Ninja"`, a name the crate reads
  for that target only (`src/lib.rs` 510-513, 947-960). Ninja, not a Visual
  Studio name, because it holds whatever Visual Studio a seat carries: CI's
  `windows-latest` image carries Visual Studio 2026 only and dist's release
  runner, `windows-2022`, Visual Studio 2022 only (2026-09-26), so naming
  either version breaks the other build, while ninja is preinstalled on
  both. With Ninja on MSVC the crate
  hands CMake the `cc` crate's `cl.exe` as the compiler and cc's MSVC
  environment — the compiler's and the Windows SDK's tool directories on the
  PATH, `INCLUDE`, `LIB` — for the configure and the build (`src/lib.rs`
  780-806, 822-823, 837-838), so a plain shell builds it, with no Developer
  prompt; that environment carries no `ninja` (`find-msvc-tools` 0.1.9,
  `src/find_tools.rs` 806-834), and CMake's lookup for it
  (`CMakeNinjaFindMake.cmake`) has no install-directory hint like NASM's. So
  a Windows seat puts `ninja` on its PATH — Visual Studio's "C++ CMake tools
  for Windows" component carries one, on the PATH in a Developer PowerShell —
  and a seat without it fails the configure loudly (CMake: `unable to find a
  build program corresponding to "Ninja"`), never silently unoptimised. A
  seat may name another generator by setting
  `CMAKE_GENERATOR_x86_64_pc_windows_msvc` in its own environment — `[env]`
  never overrides a variable the environment already holds, while a plain
  `CMAKE_GENERATOR` loses to the target's own name — and any named generator,
  a Visual Studio one included, keeps CMake's flags. Only a cargo command run
  inside the checkout reads the file: a `cargo install --git`, or a cargo
  command run from outside the checkout (`--manifest-path` included), does not
  (cargo's configuration discovery), so on Windows it builds the library
  unoptimised unless the seat sets the variable itself — build from a checkout
  (senior-developer review 2026-09-27, F7). A new name takes effect
  only where `turbojpeg-sys`'s build script runs afresh: neither crate
  declares the variable a rerun trigger, so a tree that built the library
  under another generator keeps that build until `cargo clean -p
  turbojpeg-sys`, and when the script does re-run over such a tree, CMake
  refuses the old cache ("Does not match the generator used previously"). The
  file is part of rust-cache's full key but not of its restore key (The CI
  cache key, above), so an edit to it makes no run cold: the next run
  restores the newest entry under the unchanged restore key, `full match:
  false`. The check: "Verify Windows artifact" reads each `turbojpeg-sys`
  build's `CMakeCache.txt` and its build output and fails, printing the
  lines, the generator each build used among them, unless the flags of the
  configuration the cache's `CMAKE_BUILD_TYPE` names carry `/O2` and the
  output says `WITH_SIMD = 1` — for the release build
  `CMAKE_C_FLAGS_RELEASE`, while `[profile.release]` carries no debug info:
  with debug info the crate builds `RelWithDebInfo`, and under its own pick
  of generator overrides that configuration's flags while
  `CMAKE_C_FLAGS_RELEASE` keeps CMake's default `/O2`, so a check reading it
  would pass an unoptimised library. (Corrected 2026-09-26: this bullet read
  "the #76 line is what keeps it optimised in debug" and said nothing of
  MSVC, where the first Windows artifact of brief 008 carried the library
  unoptimised. Corrected again the same day, the generator re-ruling of brief
  008: the name was `"Visual Studio 17 2022"`, a seat without Visual Studio
  2022 was to fail the configure, and the line was to move when the runner
  images moved — they already had, and no one Visual Studio is on both the CI
  and the release image; and this said that landing or editing the file costs
  one cold pair of CI jobs, which rust-cache's restore key does not bear
  out.)
- **CI** installs `nasm` on both jobs — `apt-get` on ubuntu, `choco install
  nasm` on Windows, which CMake finds in `C:\Program Files\NASM` without a
  PATH step — and `cmake` is on both runner images, `ninja` on the Windows
  images CI and the release build on; both jobs build the library from
  source, its cold cost paid once per cache key like every dependency. The
  Windows executables carry it statically (the crate links
  `turbojpeg-static` on MSVC), and "Verify Windows artifact" checks that
  neither exe imports `turbojpeg.dll` or `jpeg62.dll`, beside the
  `VCRUNTIME140` and subsystem checks. The release workflow gets `nasm` from
  `dist-workspace.toml` (`[dist.dependencies.apt]` and
  `[dist.dependencies.chocolatey]`), keys the workflow reads at release time,
  so `release.yml` does not change (RELEASING.md), and RELEASING.md names the
  requirement, ninja on the Windows runner included. No pull-request run
  builds the release artifacts (the release workflow's PR run is `dist plan`
  only), so the release job's nasm, and its Ninja build against Visual
  Studio 2022 where CI's is against 2026, are review-verified until the
  first release (the generator re-ruling of brief 008, 2026-09-26).
- **Licences**: libjpeg-turbo is IJG and BSD-3-Clause, with zlib on the SIMD
  sources — all in `about.toml`'s accepted list, and README already carries
  the IJG attribution sentence. `cargo about` keys on the crates' SPDX
  expressions (`Unlicense OR MIT` for both `turbojpeg` crates), not on the
  C sources a `-sys` crate vendors, so `about.toml` carries a clarification
  for `turbojpeg-sys` naming the three vendored texts, each checksummed:
  `LICENSE.md` (BSD-3-Clause), `README.ijg`'s LEGAL ISSUES (IJG) and
  `simd/nasm/jsimdext.inc`'s notice (zlib). cargo-about 0.9.2 does not fail
  when a checksum no longer matches: it exits 0 with a warning and lists
  the crate under MIT with an unrelated notice, so a test reads the
  generated `THIRD-PARTY-LICENSES.md` and requires the three licences with
  their texts (Manager ruling 2026-09-26).
- **The version canary**: the libjpeg-turbo behaviours the loupe depends
  on are recorded beside the dependency in
  `crates/fastcull-core/Cargo.toml`, in the manner of the Slint canaries in
  `crates/fastcull-app/Cargo.toml`, so an upgrade re-reads them —
  `tj3Decompress8` returns −1 whenever a decode emitted any warning and the
  safe crate maps it to `Err` (the residual gap's only guard on the loupe
  path); that `Err` carries only the text of the decode's first message —
  libjpeg reports its first warning, a fatal error replaces it — and every
  scanline is written before a warning's −1, which is how the loupe sorts
  complaints and keeps a kept-class message's image, by the vendored message
  texts (raw-pipeline.md, "The decoder's complaints"); at a scan's end the
  Huffman decoder drops, uncounted, the bytes its bit buffer read ahead,
  where at a restart marker it counts them, so a few junk bytes before EOI
  raise no message at all, and its arithmetic decoder meets a marker in the
  data without a warning; the header read fails on any warning, an ICC
  chunk's included, since TurboJPEG saves APP2 markers by default (and,
  beside the zune-jpeg dependency, that its 0.4 default options are strict,
  refusing two or more bytes between header segments; that it refuses an
  unknown Adobe transform in either mode; and that its default scan limit,
  100, bounds the loupe's zune-jpeg route); the header read allocates no
  image buffer; the reduced-size IDCTs have SIMD only at 4×4 and 2×2, so
  the 3/8 rung runs a C 3×3 IDCT; a lossless stream cannot be DCT-scaled;
  CMYK and YCCK are refused for RGB
  output ("Unsupported color conversion request"), which is why those
  streams go through zune-jpeg; `TJPARAM_SCANLIMIT` defaults to no limit,
  which is why the loupe sets 100, zune-jpeg 0.4's own default; the `cmake`
  crate's opt-level → CMake profile mapping; that crate's override of the C
  flags of the configuration it builds, `/O` stripped, when it picks the
  MSVC generator itself, which any named generator skips — why the MSVC
  target names one (senior-developer review 2026-09-26, F1); the order it
  reads the name in, the target's own before a plain `CMAKE_GENERATOR`;
  with Ninja on MSVC, its handing CMake cc's `cl.exe` and cc's MSVC
  environment, which carries no `ninja`; that neither it nor
  `turbojpeg-sys` declares the name a rerun trigger, so a changed name
  rebuilds nothing already built; and libjpeg-turbo's own default of the
  static C runtime on MSVC (`WITH_CRT_DLL` off, `CMakeLists.txt` 400-427),
  the runtime every MSVC build links (the next bullet; the generator
  re-ruling of brief 008, 2026-09-26; corrected 2026-09-28, QE round 1 of
  brief 008, D4: this named only the artifact's `crt-static` build, and the
  other builds linked the runtime DLL beside it).
- **Every MSVC build links the static C runtime** (QE 2026-09-28, D4):
  libjpeg-turbo builds with the static runtime, its own default, and a Rust
  build that links the runtime DLL — the MSVC target's default — puts both
  runtimes in one executable, which MSVC's linker reports as LNK4098
  ("defaultlib 'LIBCMT' conflicts with use of other libs"). So the workspace
  `.cargo/config.toml` sets `-C target-feature=+crt-static` for every MSVC
  target, and CI's test, screenshot and perf builds, a plain `cargo build` on
  a Windows seat and the artifact link one runtime, the static one; the
  artifact's step and the release workflow (dist's `msvc-crt-static`) also
  set the flag in `RUSTFLAGS`, which replaces the file's, so a shipped
  executable is static whatever the file says. CI's "Verify every Windows
  build links the static C runtime" fails on an executable under the test
  builds' directories that imports `VCRUNTIME140`, which a static build
  cannot carry. On every other target the file's table is inert.
- **No upstream contribution** (hard rule 2): the crates are used as
  published; a patch, if ever needed, stays in-tree.

Acceptance (brief 008; each box is ticked by the commit or run that
carries its evidence):
- [x] CI green on both runners with the C dependency, libjpeg-turbo compiled
      optimised on both targets (brief 008 A10): both checks green on the
      PR; the Windows job builds libjpeg-turbo from source with cmake,
      ninja and nasm, and its artifact passes the `VCRUNTIME140`, subsystem
      and no-`turbojpeg.dll`-import checks and runs `fastcull-cli.exe
      --version`; the library is compiled optimised on both targets — on
      Windows by the CMake-cache check in the verify step, seen red on the
      runner one commit ahead of the generator's fix (Manager ruling
      2026-09-26) and green after it with every cache it prints naming
      `CMAKE_GENERATOR=Ninja`, and by that run's Windows perf log, where the
      3/8 rung's median is below the landscape full-res median (the step is
      advisory, so the log is read, not asserted); and on Linux
      review-verified, the
      `cmake` crate overriding no configuration's flags there; the ubuntu
      job installs nasm and builds from source. Review-verified: the import
      check's red (no local seat builds a dynamic MSVC binary) and the
      release job's nasm and Ninja build (no PR run builds release
      artifacts; the first release after brief 008 is its proof). The
      user's test of that artifact on the desktop with real folders
      precedes any release (user decision 2026-09-26). Met so far by run
      36294797235 (82f69ff): both checks green, the CMake-cache check green
      on every `turbojpeg-sys` cache, each naming `CMAKE_GENERATOR=Ninja`,
      after it was red on the same runner at d4cc7b7 and 53a4248, and the
      3/8 rung's Windows median below the landscape full-res median (brief
      008's decisions log). The release plumbing — `dist-workspace.toml`'s
      nasm for apt and chocolatey, and RELEASING.md naming nasm and, on
      Windows, ninja — landed with brief 008's step 6 (`dist generate
      --check` reports `release.yml` unchanged). Ticked with run 36390256213
      (1ece4ce), the first CI run after it, which step 6's review verified
      against every condition above (Manager ruling 2026-09-28).
      (Changed 2026-09-26, senior-developer review F1: the box
      asked only for green checks and a running artifact, which brief 008's
      first run met with the library compiled unoptimised on Windows.
      Changed again the same day, the generator re-ruling of brief 008: the
      generator is Ninja, and the perf-log reading is the proof that ruling
      names. Corrected 2026-09-27, senior-developer review F5: the open
      reason read "ticked with the run id", which no longer said what was
      missing once run 36294797235 had met the Windows half, and invited a
      tick while the release plumbing the box calls review-verified was not
      yet in the tree.)
- [x] The licence file carries libjpeg-turbo's notices (brief 008 A15):
      `turbojpeg-sys` listed under the IJG, BSD-3-Clause and zlib licences
      with their notice texts; red on a file regenerated after the
      clarification fell back —
      `the_licence_file_carries_libjpeg_turbos_notices`. Ticked by the
      step-2b commit, which carries the test; the fallback's red is in its
      message.
- [x] Every Windows build links one C runtime, the static one (QE round 1 of
      brief 008, D4): no build of the Windows job links libjpeg-turbo's
      static runtime beside the runtime DLL — CI's "Verify every Windows
      build links the static C runtime" reads every executable under
      `target/debug` and `target/release` and fails on one that imports
      `VCRUNTIME140`, and the job's log carries no LNK4098. The guard was
      pushed one commit ahead of the fix and is red there by construction
      (every test build dynamic). Ticked 2026-09-29 (M10): the guard red on
      run 36477292291, green on run 36486218733 with no LNK4098 in the log.

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
