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

- **Main/UI thread**: Slint event loop only. Never blocks on I/O or decode.
- **Pipeline pool**: rayon pool (num_cpus) executing decode jobs from a priority
  queue. Priorities: (1) visible cells, (2) loupe neighbors ±2, (3) sequential
  background fill. Reprioritization on scroll/zoom is O(changed cells).
- **Sidecar writer**: single dedicated thread, debounced queue — sidecar writes are
  ordered and never lost (flush on session close, panic-safe via Drop).
- Core ↔ UI communication: core exposes a `SessionEvent` stream (thumb ready,
  metadata loaded, pick changed…); the app crate translates events into Slint model
  updates on the UI thread. No shared mutable state across that boundary.

## Performance budgets (regression-tested)

Measured baselines on the 32-thread reference machine; CI thresholds set ~2× looser
to absorb runner variance. Enforcement: `crates/fastcull-core/tests/perf_budgets.rs`
(release-mode tests) on REFERENCE HARDWARE — they run in every local gate
round. The CI step is advisory-only (`continue-on-error`, user decision
2026-07-25): shared virtualized runners cannot meaningfully gate wall-clock
budgets. Skipped in debug builds where decode timing is meaningless. Numbers for humans: criterion benches in
`crates/fastcull-core/benches/hot_path.rs` (`cargo bench -p fastcull-core`).

| Operation | Baseline | CI threshold |
|---|---|---|
| open+EXIF (rawler, A1) | ~2 ms | < 10 ms |
| grid thumb: extract+decode+resize | 7–11 ms | < 25 ms |
| full-res 8640×5760 decode | 130–150 ms | < 350 ms |
| pipeline throughput (all cores) | ~300 files/s | > 60 files/s (4-core runner) |

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
