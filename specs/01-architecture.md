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
#76 commit at 2 m 19 s against 9 m 10 s (4.0×) — and CI pays it once per
change of the `rust-cache` KEY, which since brief 004 carries a component
computed from the root manifest's `[profile]` tables (brief 003 had made
it a hand-bumped version number earlier the same day; next paragraph,
with the retraction of what this sentence said before 2026-09-06; the
`save-if: main` rule still means a pull request after a rustc release or
a profile change pays it on every push until main saves under the new
key). PR #80 landed the line
and its first run
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
ruling on Q1, option a). The first CACHED run since the
profile line (filled 2026-09-06, Manager, closing brief 003 and 004):
PR #84's run 34056017285, restoring the computed-key pair the main run of
brief 004 saved — `Cache hit for: v1-rust-profile-a66a9ea8-…`, `full
match: true` on both jobs — took **ubuntu-latest 14 m 59 s** (`Tests`
1 m 22 s, the headless-X release screenshot pass 11 m 22 s, clippy 14 s) and **windows-latest 35 m 40 s** (`Tests` 11 m 56 s, the Windows release screenshot pass 13 m 34 s, clippy 44 s),
against 17 m 33 s / 33 m 48 s cached before the line and 27-35 / 58-72
min cold after it. (The placeholder's history: dated 2026-09-05; re-dated
2026-09-06 by brief 003 because none could exist under the old key, and
again by brief 004 because the `v1-rust-test-…` pair main run
34018510289 saved was never restored — brief 004 put the profile
component into the key before any run asked for it; a MAIN run's figure,
which includes the post step, is the like-for-like one and is recorded
when the next main run lands.)
Incremental builds
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

**The CI cache key and the profile** (brief 003; senior-developer plan
2026-09-06; the computed rule below: brief 004, senior-developer plan
2026-09-06). Until 2026-09-06 the paragraph above said that CI's
`rust-cache` "pays it once per toolchain change (its `save-if: main` rule
means a pull request after a rustc release pays it on every push until
main repopulates the cache)", and for a profile change that was WRONG
(corrected 2026-09-06, brief 003): main cannot repopulate a cache whose
key it hits. What `Swatinem/rust-cache@v2` keys on, read from its source
at the commit the `v2` tag resolves to (v2.9.2, 6323deb — the same commit
the job log's "Download action repository" line names; `src/config.ts`):
`prefix-key` (default `v0-rust`), then the `key` input when one is set —
appended directly after the prefix (79-82), so it is in the restore key
and the primary key alike; brief 004 sets it — then the job id,
`os.type()` and `os.arch()` (lines 73-95); then eight hex digits of a
SHA-1 over `rustc -vV` of every
installed toolchain and every environment variable whose name starts
with `CARGO`, `CC`, `CFLAGS`, `CXX`, `CMAKE` or `RUST` (104-131 — this
much is the restore key); then eight digits of a SHA-1 over the
`.cargo/config.toml` and `rust-toolchain{,.toml}` files, the manifests of
the workspace MEMBERS that `cargo metadata --no-deps` lists (parsed,
`package.version` and path-dependency fields zeroed, the rest stringified
whole) and the `Cargo.lock` entries that carry a `source` or `checksum`
(156-261; `workspace.ts` 19-46): `v0-rust-test-Linux-x64-91e3cbda-aacf1ed2`
and `v0-rust-test-Windows_NT-x64-8918a2f9-aacf1ed2` here. A `[profile]`
table counts only in the workspace root ("profiles for the non root
package will be ignored, specify profiles at the workspace root" —
cargo's own warning, reproduced 2026-09-06), and this root `Cargo.toml`
is a virtual manifest with no `[package]`: not a member, never read. Both
jobs' `Lockfiles considered` list `Cargo.lock` and the three
`crates/*/Cargo.toml`, nothing else. So the #76 line did not move the
key. Measured (2026-09-06): the key on both jobs is unchanged since
2026-09-04; the main run that landed the line (34007321960, f132a87)
restored the stock-profile entry (`Cache hit for:
v0-rust-test-Linux-x64-91e3cbda-aacf1ed2`, `full match: true`), rebuilt
the dev-profile dependencies — 238 `Compiling` lines in the clippy step
and 454 in `Tests`, against 1 and 3 on the last cached run
(33986518746) — and its post step logged `Cache up-to-date.` and saved
nothing (`save.ts` 25-28: a full-match restore leaves no state to save
from); the release half of the entry stayed valid because the release
profile did not change (the release screenshot step compiled 2 crates on
both runs); the next main run (34013585441, 1d66e09) did exactly the
same — `full match: true`, 237 and 454 `Compiling` lines, `Cache
up-to-date.`, ubuntu 26 m 05 s; its Windows job likewise, `full match:
true`, 228 and 337, `Cache up-to-date.` at log line 2239, 58 m 32 s
(QE 2026-09-06, D4) — two of two main runs, both jobs, under the
unchanged key. Every run since #76 paid the same cold dev build —
26 m 17 s / 58 m 29 s (the first main run), 30 m 05 s / 59 m 08 s (PR
#81, 34011097952),
30 m 53 s / 60 m 09 s (PR #80's second run, 34004719857), ubuntu /
windows — while `gh cache list` held only the two 2026-09-04 entries
(1,903,351,905 B Linux, 1,769,851,005 B Windows) and two 240 MB RAW
entries: 4,153,874,110 B of the 10 GB limit. The first fix (brief 003,
R1, PR #82, 2026-09-06) was `prefix-key: v1-rust` in `ci.yml` — a
version number bumped BY HAND whenever a `[profile]` table in the root
`Cargo.toml` changed, and for nothing else: rustc releases, `RUSTFLAGS`,
the lockfile, the member manifests, `.cargo/config.toml` and
`rust-toolchain` move the key by themselves. It moved the key (AC1 of
brief 003, below) and it was retired the same day (brief 004; the user,
2026-09-06: "make a decision based on best practices regarding ci"; the
Manager's decision on that basis): a cache key is derived from the
inputs that shape the build and never maintained by hand, because the
hand rule's failure is silent — a forgotten bump keeps every run green
and 10-25 minutes slower per job, which is how #76 cost five cold runs
across two days before anyone counted `Compiling` lines.

**The computed rule** (brief 004, R1-R2; senior-developer plan
2026-09-06). A `shell: bash` step named `Hash the root manifest's
[profile] tables`, `id: profile`, runs on both runners before the
rust-cache step. It parses the root `Cargo.toml` with Python's `tomllib`
(3.11+; `python3` on Linux, `python` on Windows — the runner images ship
3.12: ubuntu-24.04 `20260831.293` has 3.12.3, windows-2025-vs2026
`20260824.214` has 3.12.10, read from the images' software lists at the
versions the job logs name, and the step prints the version it found so
a drift is visible; no `setup-python`), takes the document's `profile`
table — `{}` when there is none — serialises it as canonical JSON (keys
sorted at every level, no whitespace: `json.dumps(profile,
sort_keys=True, separators=(",", ":"))`) and exposes the first eight hex
digits of its SHA-256 as the step output `hash`, after checking in the
step that it IS eight hex digits. The rust-cache step receives it as
`key: profile-${{ steps.profile.outputs.hash }}`, which the action
appends directly after the prefix (`config.ts` 79-82), before the job
id, so it is part of the `.. Prefix:` line, the restore key (133) and
the primary key (263) alike: a profile change misses both keys, which
is the wanted outcome (a restore-key partial match would hand cargo a
target the action pre-cleans on a mismatch, `restore.ts` 50-56, and
cargo rebuilds under the new flags anyway; only the registry half would
be reused, worth at most the crate downloads). `[profile]` is the one
root-manifest table that shapes the artifacts the entry keeps — the
dependencies' — since the save step removes the workspace members' own
artifacts before saving (`save.ts` 41-50 with
`getPackagesOutsideWorkspaceRoot`, `cleanup.ts` 62-79), so
`[workspace.package]` and `[workspace.lints]` shape nothing cached, and
a `[patch]` table changes `Cargo.lock`, which the action hashes. For the
manifest as it stands the component is `profile-a66a9ea8` — the
canonical JSON is
`{"dev":{"package":{"*":{"opt-level":2}}},"dist":{"inherits":"release","lto":"thin"},"release":{"lto":"thin","opt-level":3}}`
(development seat, Python 3.14.6, 2026-09-06; the PR run confirms it on
both runners, AC1) — and the whole key
`v1-rust-profile-a66a9ea8-test-Linux-x64-<env8>-<lock8>`. What moves it
and what does not, each a mutant on a copy of the manifest run through
the step's own command (senior-developer plan 2026-09-06, under
`.qe-scratch/pipeline-004/mutants/`; re-run by QE for AC2): `opt-level =
2` → `3` in `[profile.dev.package."*"]`, the #76 shape, moves it to
`1395f519`; a `[workspace.package] version` bump, a comment edit, a
reordering of the profile tables and of the keys inside one, and a CRLF
copy of the same file do not (all `a66a9ea8` — the last because the hash
is over the parsed document, not the bytes, which is also why both
runners compute the same component); a member manifest is not read by
the step at all (a dependency added to `fastcull-core`'s manifest left
it at `a66a9ea8` while the action's own lock hash moved from `aacf1ed2`
to `15007111`, `config.ts` 170-172, QE's `lockhash.py` from unit 003);
deleting every `[profile]` table hashes `{}` → `44136fa3`, a move, not
a failure; a manifest that does not parse fails the step, and the job,
with an `::error::` line and no output — never a default. The step is
reviewed under the test-integrity rule because its silent failure would
be #76 in a new form, and every way it could degrade to "the key never
moves" is loud or impossible: a missing interpreter or `tomllib`, a
parse error, an unset `$GITHUB_OUTPUT` or a hash that is not eight hex
digits each stop the job (`set -euo pipefail`, the in-step check; a
failing interpreter propagates through the `tr -d '\r'` that strips the
`\r\n` CPython writes to a pipe on Windows — `Python/pylifecycle.c`,
`create_stdio`, v3.12.10 lines 2454-2458); and a guard step `Assert the
profile hash reached the cache key`, between the hash step and the
rust-cache step, re-reads the same `steps.profile.outputs.hash`
expression through its `env` and fails the job unless it reads
`profile-<8 hex>` — so a renamed step id, a renamed output or a skipped
hash step cannot leave the key at a constant `profile-`. The residual:
an edit to the rust-cache `key:` line alone, which nothing in the run
detects; the `Cache Key:` reading (AC1) is the check for it, and the
value the guard prints is the line to compare it with. Offline it is one
command: a pyyaml assertion that the rust-cache step's `with.key` is
`profile-${{ steps.profile.outputs.hash }}` and that neither new step
carries an `if:` or `continue-on-error:` — the structural check QE ran
for brief 004 (QE 2026-09-06, D1); an in-run grep of the workflow's own
text would have to escape `${{`, which the runner substitutes before the
shell sees it. The residual also covers the `key:` line REPLACED by a
well-formed constant, where `Cache Key:` still looks right and only the
comparison with the guard's printed line catches it. `prefix-key:
v1-rust` stays as the escape hatch: it is bumped only for a change to
the action or to the key's format — a rust-cache release that hashes
differently, a change to the step's command — and NOT for a profile
change, which the computed component covers (AC4). Rejected, then and
now: a `key:` input of `hashFiles('Cargo.toml')`, which would also move
the key on every release's `[workspace.package] version` bump and on
every comment edit — a cold pair of jobs each time, silently (the
mutants above are exactly the edits it would have charged for); an
`awk` range over the manifest text (QE's first draft of the computed
rule, 2026-09-06: it captured the fourteen-line comment block between
`[profile.release]` and `[profile.dist]`, so a comment edit would have
moved the key — the reason the component is computed from a parsed
document, brief 004); a guard test in the Rust crates that goes red
when the profile changes without a bump (it turns one hand edit into
two); and moving the profile into `.cargo/config.toml`, which the
action does hash and cargo does honour (both reproduced 2026-09-06),
but which relocates the user decision recorded above and moves the key
on comment edits too. A key move costs once, and MORE than the post-#76
cold runs it was first compared with (corrected 2026-09-06, QE D1): a
run under a new key rebuilds BOTH halves of the entry, the release half
included, where a v0 run since #76 had restored a still-valid release
half — PR #82's run 34014978820 took 33 m 28 s / 1 h 09 m 58 s
(ubuntu / windows) with the release steps compiling 469+86 and
387+84+299 crates against 2+1 and 2+1+3 on the v0 runs, the debug steps
unchanged at 238/454 and 229/337; the first main run under the new
prefix (34018510289, d4da1ac) took 34 m 40 s / 57 m 43 s including its
saves (28 s / 2 m 15 s) — two Windows samples of one shape 12 minutes
apart, so the cold Windows figure is a range, not a number. The first
main run under a new key is cold and saves; pull requests before that
main run are cold too; the old key's entries sit orphaned until GitHub
evicts them ("not been accessed in over 7 days", or oldest access first
once the 10 GB limit is exceeded — GitHub's documented policy) or the
Manager deletes them (the v0 pair, 2026-09-06). On a pull request the
proof that the key moved is the restore step alone: `Cache Key:` shows
the component and the line after `... Restoring cache ...` is `No cache
found.`; the post step prints nothing on a pull request (`save-if`
false returns before any log line — `save.ts` 18-22), by design.

Acceptance (brief 003; a criterion is ticked by the commit that carries
its evidence):
- [x] **The rust-cache key moves with the dev profile (AC1).** Both CI
  checks green on the PR, and both jobs' restore step logs `Cache Key:`
  with a `v1-rust-…` key followed by `No cache found.` — a `v0-` there
  means the input did not take. Pinned by the PR run's job logs. Ticked
  (brief 004's spec commit, 2026-09-06): run 34014978820 — ubuntu job
  101437075173 green in 33 m 28 s, log lines 601-602 `Cache Key:` /
  `v1-rust-test-Linux-x64-91e3cbda-aacf1ed2`, 619-620 `... Restoring
  cache ...` / `No cache found.`; windows job 101437075249 green in
  1 h 09 m 58 s, lines 428-429
  `v1-rust-test-Windows_NT-x64-8918a2f9-aacf1ed2`, 446-447 `No cache
  found.`; no `Unexpected input`, no `Cache hit for: v1-`; and the two
  v0 entries' `lastAccessedAt` stayed at 05:17Z from run 34013585441 —
  the key moved, seen from outside the log too (QE 2026-09-06).
- [x] **The spec no longer claims main repopulates the cache under an
  unchanged key (AC2).** Pinned by the retraction above: a grep for
  "repopulates the cache" in this file finds only the correction.
- [x] **The first main run under the new prefix saves (AC3, the save
  half).** Ticked (brief 004's spec commit, 2026-09-06): run 34018510289
  (d4da1ac, both jobs green, ubuntu 34 m 40 s / windows 57 m 43 s) —
  both restore steps `No cache found.` under `v1-rust-test-…`; both post
  steps `... Saving cache ...` then `Sent 2028320243 of 2028320243
  (100.0%)` (ubuntu, 28 s) and `Sent 1746095594 of 1746095594 (100.0%)`
  (windows, 2 m 15 s); `gh cache list` shows
  `v1-rust-test-Linux-x64-91e3cbda-aacf1ed2` at 2,028,320,243 B
  (1.89 GiB) and `v1-rust-test-Windows_NT-x64-8918a2f9-aacf1ed2` at
  1,746,095,594 B (1.63 GiB), both on `refs/heads/main`; the two v0
  entries were deleted (Manager, Q2); `.../actions/cache/usage` reads
  4,255,087,037 B (3.96 GiB) over 4 entries against the 10 GB limit —
  the 5.61 GiB in brief 004's context was read while the Windows v0
  entry still existed (4,255,087,037 + 1,769,851,005 = 6,024,938,042 B),
  and its "1.88 / 1.62 GiB" are the same two sizes truncated rather than
  rounded (senior-developer 2026-09-06). No thrash: the four entries
  are under half the limit.
- [x] **… and the run after it is cached (AC3, the restore half).**
  TICKED 2026-09-06 by brief 004's AC3 (PR #84's run 34056017285 restored
  the computed-key pair; the durations are in the placeholder above).
  CORRECTED 2026-09-06 (brief 004): this half can no longer be ticked as
  written — no run restored the `v1-rust-test-…` pair before brief 004
  added the profile component to the key, so those two entries are
  orphans in their turn (deleted by the Manager once the computed key's
  pair exists, as the v0 pair was), and the first cached run is the one
  after brief 004's merge. Carried by brief 004's AC3 below, which ticks
  this line with it. The thrash rule stands: if the live pair plus the
  RAW caches exceed the limit and evict each other, that goes to the
  user with options, not solved here (brief 003, R4).

Acceptance (brief 004; a criterion is ticked by the commit that carries
its evidence):
- [x] **Both runners compute the component and the key carries it
  (AC1).** TICKED 2026-09-06 (Manager, closing commit): PR #83's run
  34043232886 — ubuntu job 101513620755 `Python 3.12.3`, `profile hash:
  a66a9ea8`, `rust-cache key component: profile-a66a9ea8`, `Cache Key:
  v1-rust-profile-a66a9ea8-test-Linux-x64-91e3cbda-aacf1ed2`, `No cache
  found.`; windows job 101513620796 `Python 3.12.10`, the same hash and
  component, `Cache Key: v1-rust-profile-a66a9ea8-test-Windows_NT-x64-
  8918a2f9-aacf1ed2`, `No cache found.`; no `Cache hit for: v1-rust-test-`,
  no `Unexpected input`; both green (27 m 49 s, 1 h 06 m 15 s). Both CI checks green on the PR; both jobs' `Hash the root
  manifest's [profile] tables` step logs `Python 3.12.x` and `profile
  hash: <8 hex>` — the same eight digits on both, `a66a9ea8` for the
  manifest as merged from d4da1ac — the guard step logs `rust-cache key
  component: profile-<same>`, the rust-cache step's `with:` echo shows
  `key: profile-<same>`, its `Cache Key:` is
  `v1-rust-profile-<same>-test-<OS>-<env8>-<lock8>` and the line after
  `... Restoring cache ...` is `No cache found.` (cold by design: no
  entry exists under a key with the component until a main run saves
  one). The failable readings: `profile-` with nothing after it in the
  key (the plumbing failed and the guard did not fire), a `Cache hit
  for: v1-rust-test-…` (the component did not take — the shape every
  run of main's `ci.yml` shows now that the `v1-rust-test-…` pair
  exists; that is this unit's old red), or a `Warning: Unexpected
  input(s)` line. Pinned by the PR run's job logs, read by QE; ticked,
  with the run id and the lines, by the Manager's closing spec commit.
- [x] **The mutants behave as stated (AC2).** TICKED 2026-09-06
  (Manager, closing commit; QE's report of PR #83, 17 fixtures plus the
  guard's 14 inputs and 4 loud paths). The step's own command,
  run on this seat by QE against copies of the manifest: `opt-level`
  2 → 3 moves the hash (`a66a9ea8` → `1395f519`); a version bump, a
  comment edit, a reordered table and a CRLF copy do not; a member
  manifest change does not touch it (the action's lock hash moves
  instead); no `[profile]` → `44136fa3`; a parse error → exit 1 with
  `::error::` and no output; the guard rejects `profile-`, a
  seven-digit and an upper-case value and accepts `profile-a66a9ea8`.
  Recorded in the QE report and the developer's commit message; ticked
  by the Manager's closing spec commit.
- [x] **The main run saves under the computed key and the run after it
  restores it (AC3).** TICKED 2026-09-06 (Manager, closing commit): main
  run 34047309575 (e879cff) saved `v1-rust-profile-a66a9ea8-test-Linux-
  x64-91e3cbda-aacf1ed2` (2,028,320,243 B) in its ubuntu post step (28 s)
  and `…-Windows_NT-x64-8918a2f9-aacf1ed2` (1,746,553,629 B) in its
  windows post step (4 m 18 s; the job 1 h 12 m 18 s, the longest Windows
  job measured, 17.7 minutes under the cap); the orphaned
  `v1-rust-test-…` pair was deleted the same day; usage 5.58 GiB of the
  10 GB limit; PR #84's run 34056017285 restored the pair on both jobs
  (`full match: true`) — the durations are in the placeholder above. Post-merge, not gating the PR: the merge run's
  post steps log `... Saving cache ...` and `Sent N of N (100.0%)` under
  `v1-rust-profile-a66a9ea8-…`; `gh cache list` shows the pair with
  sizes and the usage total against the limit; the orphaned
  `v1-rust-test-…` pair is deleted once the new pair exists; the
  following run restores both (`Cache hit for: v1-rust-profile-…`,
  `full match: true`) and its durations fill the placeholder above.
  Ticks brief 003's restore half with it.
- [x] **No sentence still says the prefix is bumped for a profile change
  (AC4).** `grep -n -i "bump" .github/workflows/ci.yml
  specs/01-architecture.md` finds only history (what brief 003 did and
  why it was retired), the action-or-format rule for `prefix-key`, the
  rejected alternatives and the mutants ("a version bump"), and hits
  about other things (the `v5` action bump, the RAW cache's `v1 -> v2`
  bump, a runner image that "bumped a default"). Ticked by the
  developer's `ci.yml` commit, whose grep is the evidence. Ticked
  (developer 2026-09-06): nine hits in `ci.yml` — 190 the retraction
  ("NOBODY BUMPS ANYTHING BY HAND"), 217 and 220 history (what brief
  003's comment said, and why a forgotten bump is silent), 255 the
  action-or-format rule ("and NOT for a profile change"), 236 and 260
  the mutants and the rejected `hashFiles`, 102, 333 and 460 the `v5`
  action bump, the RAW cache's `v1 -> v2` bump and a runner image that
  bumped a default; and in this file 256, 350 and 358 history and its
  retraction, 400 and 532 the mutants, 427 the escape-hatch rule, 432
  and 440 the rejected alternatives, the remaining hits being this
  criterion's own text. Not one of them is a rule to bump the prefix
  for a profile change.

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
