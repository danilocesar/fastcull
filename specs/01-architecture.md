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
wall-clock budgets. Skipped in debug builds where decode timing is
meaningless. Numbers for humans: criterion benches in
`crates/fastcull-core/benches/hot_path.rs` (`cargo bench -p fastcull-core`).

Thresholds were set ~2× looser than the decode-bound baselines to absorb
variance (the EXIF row has huge headroom on purpose — anything near 1 ms
means a whole-file read or mmap snuck back). The original baselines were
measured on a 32-thread machine retired 2026-07-28; since then the
development machine is an i7-8665U laptop (4 cores / 8 threads). Both
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
