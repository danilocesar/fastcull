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

Measured baselines on the 32-thread reference machine; CI thresholds set looser (~2× for decode-bound rows; the EXIF row has huge headroom on purpose — anything near 1 ms means a whole-file read or mmap snuck back)
to absorb runner variance. Enforcement: `crates/fastcull-core/tests/perf_budgets.rs`
(release-mode tests) on REFERENCE HARDWARE — they run in every local gate
round. The CI step is advisory-only (`continue-on-error`, user decision
2026-07-25): shared virtualized runners cannot meaningfully gate wall-clock
budgets. Skipped in debug builds where decode timing is meaningless. Numbers for humans: criterion benches in
`crates/fastcull-core/benches/hot_path.rs` (`cargo bench -p fastcull-core`).

| Operation | Baseline | CI threshold |
|---|---|---|
| open+EXIF (in-tree walker, A1) | ~5 µs | < 1 ms |
| grid thumb: extract+decode+resize | 7–11 ms | < 25 ms |
| full-res 8640×5760 decode | 130–150 ms | < 350 ms |
| pipeline throughput (all cores) | ~1,500 files/s (post-2026-07-27 EXIF fix; was ~300 mmap-capped) | > 60 files/s (4-core runner) |

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
