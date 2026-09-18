# Module spec: RAW preview pipeline (`raw/`, `exif.rs`, `pipeline.rs`, `loupe.rs`, `viewassets.rs`)

## Purpose

Turn a folder of RAW files into displayable images at interactive speed
without ever decoding RAW sensor data on the hot path (ADR 0001): read the
camera's embedded JPEGs, and only them.

## Behaviour

### Inputs and outputs

- In: file paths from the catalog; priority hints from the UI — the visible
  range, the loupe position.
- Out: per image, up to three assets delivered as events — `Thumb` (320 px,
  the grid), `Mid` (the 1616-class preview: large grid cells, loupe fit on
  displays up to ~2K, the transit rung) and `FullRes` (1:1 pixels) —
  climbed by the ladder rule below.

### Reading a file

- **Targeted reads on the TIFF-shaped hot path.** Never read, or map, the
  whole file for a classic-TIFF container (every `.ARW`; NEF, CR2 and DNG
  too): only the IFD tables and the byte range of the chosen embedded JPEG.
  The EXIF summary comes from the in-tree TIFF walker (`raw/tiff.rs`,
  `raw/jpeg_exif.rs`), not from rawler: rawler's `RawSource` mmaps the
  entire file and its per-process `mmap_lock` serialized every import
  worker (2026-07-27: the EXIF pass peaked at ~500 files/s and DEGRADED
  with more threads while the seek+read path scaled to 1,557/s; over FUSE
  mounts — ntfs-3g backup drives, card readers — a real 1,450-ARW folder
  took 99–133 s to import against ~3 s with the walker; per-file EXIF
  1.71 ms → 5 µs). The walker keeps rawler's vendor normalization ("SONY"
  → "Sony"), so summaries are byte-stable across the swap.
- **rawler stays in exactly two roles**: the RAW-decode fallback, and the
  EXIF fallback for non-classic-TIFF containers (CR3, RAF, X3F —
  best-effort, 00-overview.md). A walker-rejected header goes to rawler's
  parser, confining the mmap cost to those files, and to garbage files,
  which pay one bounded rawler attempt before erroring. Known exposure
  (issue #89): a RAW-named file the walker rejects is pre-faulted whole by
  rawler's `MAP_POPULATE`.
- The walker is in-tree and from scratch because rawler 0.7 does not
  expose the A1's full-res JPEG (`full_image()` returns 1616×1080). It
  works on any `Read + Seek` (which is what makes the counting-reader
  budget tests possible), reads only IFD tables and JPEG headers, and is
  hardened against hostile files: offset cycles, entry-count bombs,
  out-of-range offsets. BigTIFF (magic 43) is rejected as not-TIFF.
  Nothing is upstreamed without the user's approval (hard rule 2).
- **Asset sources.** The grid thumb: the largest embedded preview ≤ ~2 MP
  (A1: the 1616×1080), decoded with zune-jpeg and SIMD-resized
  (`fast_image_resize`) to 320 px. Full-res: the largest embedded JPEG
  (A1: the 8640×5760 `JpgFromRaw`).
- **Fallback chain** when a source is missing (non-A1 cameras): full-res
  JPEG → the mid preview upscaled → a half-size RAW decode via rawler
  (background priority only, with a "rendered from RAW" badge event) →
  `Failed(reason)`.
- **Bare JPEG sources** (issue #8): a `.jpg`/`.jpeg` session file IS its
  own single whole-file "embedded preview" (`find_embedded_jpegs` returns
  one candidate at offset 0), so the thumb and loupe ladder work
  format-agnostically. That rung is TERMINAL: the loupe `Ready` event says
  so, and the app adopts a terminal mid-class-or-smaller texture as the top
  rung so the zoom ceiling is knowable (small JPEGs — ≤ 2048 px long edge,
  phone and web files — dead-ended the zoom path otherwise). A > 2 MP
  JPEG's first thumb decode costs full resolution: the 25 ms ARW thumb
  budget does not apply, and the cache absorbs re-opens. The extension
  decides the EXIF path while the JPEG signature decides the preview path:
  a JPEG renamed `.ARW` gets thumbnails by signature and an empty rawler
  summary; an ARW renamed `.jpg` gets previews by TIFF walk and an empty
  JPEG summary — both degrade, neither errors. A JPEG's EXIF (capture time,
  SubSec, make/model/serial, orientation) comes from the APP1 `Exif\0\0`
  block through the same hardened walker; an absent or hostile APP1
  degrades to an empty summary and orientation 1, never an error. Sony
  JPEG maker notes are out of scope in v1: JPEGs group by the generic time
  path.

### The loupe ladder

Display the best already-loaded asset immediately, and cook a higher rung
ONLY when the display size exceeds the loaded asset by more than 25 %
(`UPSCALE_THRESHOLD = 1.25`; user decision 2026-07-25, replacing the
separate DCT FitPreview). A1 rungs: the 320 px thumb → the 1616×1080 mid
(~5 ms; covers fit on ≲ 1.9k-wide viewports instantly) → the 8640×5760 full
(~140 ms in release, cooked in the background for 1:1 and wider displays;
the shown image swaps in place when it lands, never blocks). The ladder
applies to GRID CELLS too: any cell wider than 320 × 1.25 physical px is
served by the mid rung (`LoupeEngine::want(range, cell_width)`); the
UI-side bookkeeping is `viewassets.rs::ViewAssets`, in core, whose
`ensure()` also adopts engine-cached images that emit no event (the
pruned-and-revisited cell). Scrolled-past want-requests are culled on every
`want()` call, so visible cells never starve behind stale backlog.

The engine (`loupe.rs`) has its own event channel and three workers: two
BACKLOG workers and one FOCUS-RESERVED lane.

- The reserved lane takes only the focused index's job — or MANUFACTURES
  it. While the user travels (ui-grid.md's transit contract), requests are
  capped at the mid rung, so once the user stops there is no full-res
  request anywhere in the system, and this lane is the only thing that
  wakes on a timer to issue one. It acts only when the focused frame is
  short of the app's real target, not already in flight and not failed,
  and only after the focus has represented the same PENDING WORK for a
  ~250 ms debounce (`FOCUS_DEBOUNCE`): the clock re-arms when the focused
  index changes AND when its target escalates above the highest seen
  during the current focus tenure (both guards are load-bearing: without
  the in-flight guard a key release during the transit mid's decode queues a
  duplicate full-res job and a ~149 MB transient; without the sufficiency
  guard the lane spins push/pop forever holding the state mutex and freezes
  all three workers). Full-res decodes must never queue behind a background
  thumbnail sweep — the rule the lane and the pool bypass serve. Transient
  focuses (the first frame
  during load, transit frames for ~60-150 ms) are left to the backlog
  workers, which need no debounce, so the lane is free at the FIRST settle
  after sub-debounce transits.
- The reserved lane's flights ABANDON at rung boundaries when their index
  is no longer the focus; backlog flights are uninterruptible (mid → full
  in one flight — their neighbours are legitimate prefetch). The lane
  checks only BETWEEN rungs, so a focus change during a rung's decode
  waits out that rung — ~140 ms in release, about 1-2 s in a debug build
  since 2026-09-05 (~30 s before dependencies compiled optimised, issue
  #76). A decode itself is never interrupted.
- **The ring is in VIEW order** (`set_view`; issue #46): ±`PREFETCH` (2)
  when settled, `TRANSIT_BEHIND`/`TRANSIT_AHEAD` (2/8, leaning the way of
  travel) while moving; an engine whose consumer never calls `set_view`
  keeps identity order — the pre-#46 behaviour, which the pre-#46 core tests
  still pin. A deferred upgrade — an in-flight index whose
  wanted rung grew mid-decode — is revived at land time only while the
  index is still inside ±`PREFETCH`, and a ring neighbour never outranks
  the focused frame's own pending work (a stale revival at top priority
  once captured both workers for frames the cursor had left and starved
  the current frame past the shutter's 60 s cap). A deferred upgrade for a
  frame 3-8 away is therefore dropped rather than revived — harmless today,
  since transit never escalates a target, and a trap for any future
  widening. A dropped upgrade loses nothing: the next refresh re-requests
  it (`focus()` at the loupe, `want()`/`ensure()` for grid cells).
- A byte-budget LRU (default 2 GiB) evicts the least recently focused
  images, never the focused one. The app's view-distance eviction of
  full-res TEXTURES is `transit::evict_fullres` (ui-grid.md).
- turbojpeg DCT scaling is a recorded future optimization only (~35–45 %
  off the cook; the ladder already hides that latency).
- The lane's three rules each answer a starvation that shipped once: a
  debounce-less reservation was captured by transient focuses; an
  index-change-only clock was beaten by rest-then-escalate (~20 % of the
  time, QE); a lane with no boundary check committed to a frame the user
  had left (the double-settle, which fired on the v0.4.0 release-commit
  Windows run). The first failed validation, the second failed QE, and the third was
  caught by the screenshot shutter's 60 s cap in the Windows debug pass, while a stock-profile full-res decode took
  26-40 s. With dependencies optimised in debug the cap catches only a
  stall of tens of seconds, and that sensitivity is spent deliberately
  (user decision 2026-09-05): the ladder's contracts are pinned by their
  own tests — `transit::render_rung`'s table, the engine's unit tests, the
  driven no-drop tests of ui-grid.md — never by the cap's timing.

### Orientation (user requirement 2026-07-25)

Embedded previews are stored in sensor orientation; the EXIF Orientation
tag (IFD0 0x0112) says how to display them. FastCull soft-rotates, like
Photo Mechanic: the walker extracts the tag, and it is applied to the
DECODED PIXELS of every rung — thumb, mid, full-res — before display. RAW
files and sidecars are never modified. All 8 values (rotations and
mirrored forms) are handled. The thumb cache stores post-rotation pixels
(the schema bump when this landed invalidated older thumbs wholesale).

The rotate is a hot loop and is engineered as one (`raw/orient.rs`; issue
#27, 2026-08-02; every constant pinned by a measured sweep on real
8640×5760 pixels, recorded in the module header). Mirrors and 180°
(orientations 2-4) run IN PLACE — no second 149 MB buffer. Transposes (5-8)
walk 64 px cache tiles under scoped threads capped at 8, with
bounds-check-free writes: 236 → 28-31 ms on the 8-core laptop,
byte-identical to a reference implementation for all 8 orientations at
sizes exercising partial tiles and partial thread bands. An `unsafe`
pointer kernel measured 25 ms and was REJECTED: ~4 ms is not worth the
crate's first `unsafe` block.

The full-res decode path (`loupe::decode_oriented` — THE hot path, public
so `perf_budgets` measures the shipped code) pays its page faults off the
critical path: the A1 full-res JPEG is baseline with ZERO restart markers,
so its Huffman decode is strictly serial and seven cores idle for ~220 ms
while it runs. The decode goes `decode_into` a pre-faulted buffer (~30 ms
saved over `decode()`'s own allocation), and the transpose's output buffer
is allocated and pre-faulted on a spare thread DURING the decode
(`raw::Scratch`). Peak memory is unchanged — the same two buffers exist
either way; only WHEN their faults are paid moves. Measured end to end on
the budget test: 518 ms untouched → 277 ms, inside the 350 ms budget with
headroom on the very laptop where issue #27 declared it unpassable. Buffer
POOLING (288 ms) stays excluded: three workers × 149 MB of resident pool is
a memory decision this does not need. zune-jpeg 0.5.15 measured a
regression on this workload (267-279 ms vs 0.4.21's 247-252) and the
decoder stays on 0.4.

### Hostile-input bounds (issue #31, 2026-08-02)

Decode buffers are sized from HEADER claims before one byte of scan data
is validated, and in a crafted file every claim is attacker-controlled, so
both sides of the decode are capped and stream completeness is checked
before allocation:

- **Input**: `MAX_EMBEDDED_JPEG_LEN` (256 MB, `raw/mod.rs`) caps what
  `read_jpeg` will allocate for a declared payload length.
- **Output**: `MAX_DECODED_PIXELS` (500,000,000, `raw/mod.rs`) caps what
  SOF dimensions may size — checked in `decode_oriented` right after
  `decode_headers`, before the decode buffer, the prefault pass or the
  transpose scratch exist. 500 MP is ~10× the A1's 49.8 MP and ~3× the
  largest shipping sensor, with room for stitched panoramas served as bare
  JPEGs; the JPEG format ceiling (65535×65535) would commit ~12.9 GB of RGB
  per buffer, and a sub-KB stream claiming 30000×30000 measured 5.29 GB
  RSS on the pre-fix path. The thumb/mid decode keeps zune's default
  16384-per-side limit (268 MP, already stricter); the pixel cap lives on
  the loupe path, the only one that lifts the per-side limits (it must
  accept panorama-wide bare JPEGs).
- **Truncation**: zune-jpeg 0.4 zero-fills missing scan data and reports a
  truncated stream as SUCCESS, and exposes no bytes-consumed accessor.
  Completeness is checked on the raw bytes
  (`raw/jpeg.rs::scan_is_terminated`): inside entropy-coded data every 0xFF
  is either stuffed (FF 00) or a real marker, so a genuine FF D9 at or
  after the first SOS is an EOI. The search runs backwards from the tail —
  intact camera files end with EOI, so the hot path pays effectively
  nothing — and pre-SOS APP1 segments (EXIF thumbnails are whole JPEGs)
  never vouch for the main scan. Applied in `decode_oriented` AND the
  grid-thumb decode.
- **Residual, accepted**: a crafted stream carrying plausible dimensions, a
  valid EOI and too little entropy data still decodes as a mostly-blank
  "success" — detecting that needs decoder cooperation neither zune 0.4
  nor 0.5 offers; and in a MULTI-SCAN (progressive) stream the table
  segments between scans may legitimately contain a literal FF D9, so a
  truncated progressive stream can pass the check. Both are bounded blank
  successes, never a giant allocation. (0.5.15's strict mode rejects the
  plain no-EOI truncation but not these, and is the perf regression above.)

All rejections flow through the existing `LoupeEvent::Failed` /
`SessionEvent::Failed`, so the UI shows the Failed badge (ui-grid.md) and
subsequent jobs are unaffected.

### The adaptive read pool (user requirement 2026-07-25)

Thirty-two simultaneous readers once drove a microSD into minute-long
kernel I/O queues and blocked shutdown; a fixed limit of 4 fixed the hang
but cannot react when the medium degrades further mid-session. A pool
manager owns the release of read workers and adapts their number to the
medium's measured behaviour:

- It owns `(limit, in_flight)`: a worker acquires before entering a read
  section and waits while `in_flight >= limit`. Decode stays fully
  parallel and unmanaged.
- **Floor 4** — the empirically proven-safe value, always available; NAS
  and network mounts are never throttled below it. **Cap = CPU core
  count**, earned probe by probe. The initial limit is the floor. (Local
  NVMe is decode-bound — fixed-4 measured 350/333 files/s against fixed-8's
  313 — so growth is for latency-bound sources, not local throughput.)
- **Probes**: at most one outstanding; the first read granted while none
  is outstanding becomes the probe. The probe paces GROWTH (one decision
  per completed probe); shrink signals come from the whole in-flight set.
  Timings are pure in-permit read time — queue time is NEVER included
  (measuring wait creates a positive-feedback collapse). Only the
  preview-read section (open + IFD walk + `read_jpeg`) feeds the
  controller; the EXIF section is pool-managed but not sampled; cache hits
  bypass the pool; reads larger than 2 MB feed NO decision — neither
  completion nor stall — or the non-A1 full-res-as-grid fallback would
  stall-shrink a healthy medium to the floor (the size is known before the
  bulk read, so the probe is neutralized as soon as its payload is chosen).
- **Control** (AIMD with a hysteresis dead band): probe < 200 ms → +1
  (clamped at the cap); probe > 500 ms → HALVE (clamped at the floor);
  otherwise hold. Halving, not −1: recovering from a warm-cache-pumped
  limit of 32 on a suddenly slow card takes 3 halvings, not 28 steps.
- **Growth requires "the loader is not stuck", literally** (live incident
  2026-07-25: warm 0 ms page-cache probes pumped the limit 4 → 22 while
  every cold read sat wedged — fast probes have survivorship bias, stuck
  reads never report; issue #1 tracks the class): a fast probe grows the limit only when no other
  in-flight non-excluded read is older than the grow threshold. An
  excluded (> 2 MB) read neither vouches nor indicts: a genuinely wedged
  large read vetoes nothing and triggers no shrink — accepted residual.
- **Stall watching covers EVERY in-flight read**, not just the probe (in
  the original incident reads did not come back slow — they did not come
  back). If the oldest non-excluded in-flight read exceeds the shrink
  threshold, the manager halves WITHOUT waiting for a completion, checked
  on every pool touch plus a periodic re-check by blocked waiters. Shrinks
  are throttled to one per shrink-threshold window: a persistent wedge
  walks cap → floor in ~3 windows (~1.5 s) with no cascade. Blind spot,
  recorded: if the limit equals the worker count and every worker is
  wedged inside a read, no thread touches the
  pool until the first read returns, so the cascade starts late — harm
  bounded to the reads already in flight.
- Retirement is non-preemptive: a shrink only lowers the limit; reads in
  progress finish. Release is priority-aware: waiters queue with a (job
  priority, arrival) ticket and a freed or grown slot goes to the lowest —
  a visible thumbnail before background prefetch even at the floor.
  Growing the limit wakes ALL waiters (a lost-wakeup hazard, once bitten).
- **Override**: `FASTCULL_MAX_READERS=N` replaces the adaptive cap. N at or
  below the floor lowers the floor too (`=1` pins a single reader, `=4`
  restores the old fixed behaviour); N above 4 sets the ceiling to exactly
  N, including above the core count (QE observed 94 readers with `=999` on
  32 cores — useful for saturating a high-latency NAS, self-inflicted
  otherwise). An env var, not a CLI flag, so the app and the CLI honour the
  same knob; unset is fully adaptive (`FASTCULL_NO_CACHE`, by contrast, is
  app-only; the CLI has `--no-cache`).
- Every limit change is logged to stderr, the diagnostics channel:
  `fastcull: read pool N -> M workers (probe read X ms | read stalled for
  X ms; K reading)`, K being the reads actually in flight. Steady state
  logs nothing (a clamped no-op change is not printed).
- **Scope**: the thumbnail pipeline only. Loupe full-res reads bypass the
  pool (user decision 2026-07-25: "full-res should bypass it, as full-res
  has priority"; a 12 MB read would also poison a latency-threshold
  controller). Risk on record: at the floor on a dying card an ungated
  loupe read can still hit the card hard — revisit if the hang class ever
  reappears via the loupe path.
- What the user answered (2026-07-25): NAS culling IS part of the workflow
  — the floor and the core-count cap serve it (a relative-baseline signal
  stays a recorded option if the absolute thresholds prove wrong on the
  NAS); culling while ingesting, "usually no" — the shrink path is a
  safety net; no status-bar "slow storage" hint — mooted by the floor.

### The priority queue

- Three levels: `Visible` > `Prefetch` (the loupe ring — ±2 at rest, ±2/±8
  by travel) > `Background` (sequential file order: cold-cache and
  card-reader friendly).
- Scroll and zoom call `set_visible(range)`; already-queued jobs are
  reprioritized, not re-enqueued. In-flight jobs are never cancelled
  mid-decode (they are ≤ 150 ms). Duplicate requests for the same (image,
  asset) coalesce.

### Memory

- Thumbs: unbounded (≈ 200 KB each; 5,000 images ≈ 1 GB worst case —
  acceptable; the SQLite cache lets us evict and reload cheaply if this
  ever pinches; issue #2 is the residency-window request).
- Full-res decodes: the engine's byte-budget LRU, 2 GiB by default;
  mid-rung textures count toward it.

## Contracts

- `LoupeEngine`: `focus(index, display_long)`, `want(range, cell_width)`,
  `set_view` (deferred revival is internal); events `Ready` (with the `terminal` flag)
  and `Failed`; constants `PREFETCH = 2`, `TRANSIT_BEHIND = 2`,
  `TRANSIT_AHEAD = 8`, `FOCUS_DEBOUNCE` (~250 ms), `MID_RUNG_MAX_LONG =
  2048`, `UPSCALE_THRESHOLD = 1.25`.
- `loupe::decode_oriented` is the perf-budget target; `raw/mod.rs` holds
  `MAX_EMBEDDED_JPEG_LEN`, `MAX_DECODED_PIXELS` and `GRID_SOURCE_MAX_PIXELS`.
- `ExifSummary` (`exif.rs`): make, model, serial, capture time, subsec, the
  Sony sequence number; `sort_key()` normalizes subseconds to three digits.
- The budget rows of 01-architecture.md bind this module: open+EXIF < 1 ms,
  grid thumb < 25 ms, full-res decode+rotate < 350 ms, throughput > 60
  files/s.
- Trace marks (test-harness.md): `thumb bytes idx N` (the pipeline read the
  embedded JPEG), `thumb landed idx N` (the kitchen decoded it), `loupe
  ready idx N long L`; the read pool's stderr line.
- What the transit contract asks of this engine — the request states and
  the settle guarantee in the reserved lane — is specified in ui-grid.md;
  the engine rules above are what make it hold.

## Acceptance criteria

- [x] **The zoom-quality gate** (user mandate 2026-07-25): `tests/zoom_walk.rs`
      — the 2-column forward-walk repro and the fast-scroll starvation
      variant — MUST pass in release against the real A1 files before any
      zoom-quality problem is declared fixed:
      `walking_at_two_columns_never_leaves_an_image_below_its_rung`,
      `fast_scroll_backlog_does_not_starve_final_window`.
- [x] Each of the 3 A1 files: the grid thumb comes from the 1616×1080
      preview, full-res is 8640×5760 —
      `tests/pipeline.rs::a1_files_produce_320px_thumbs_and_metadata`,
      `tests/embedded_jpeg.rs`.
- [x] No test observes a read over 20 MB of a 100 MB A1 file on the grid
      path — `tests/embedded_jpeg.rs` (a counting reader asserts
      `bytes_read <= 20 * 1024 * 1024`).
- [x] A truncated or garbage preview yields `Failed` and does not poison the
      pipeline — `tests/pipeline.rs::corrupt_file_fails_alone_others_complete`,
      `pipeline::tests::truncated_bare_jpeg_yields_failed_not_a_blank_thumb`.
- [x] Hostile decode dimensions (issue #31): a sub-KB stream claiming
      30000×30000 is rejected before any pixel allocation on both
      orientation paths, and a scan cut off before EOI yields `Failed`,
      never a blank success, on the loupe and grid-thumb paths —
      `raw::tests::decoded_pixel_cap_boundaries`,
      `raw::tests::read_jpeg_rejects_implausible_length`,
      `raw::jpeg::tests::scan_termination_detects_truncation`,
      `loupe::tests::decode_oriented_rejects_a_truncated_scan`,
      `loupe::tests::truncated_full_rung_keeps_the_good_mid_and_no_failed_badge`,
      `pipeline::tests::truncated_bare_jpeg_yields_failed_not_a_blank_thumb`.
- [x] `set_visible` promotion: with a saturated queue a newly visible
      image's thumb arrives before ≥ 90 % of background items —
      `tests/pipeline.rs::promoted_jobs_finish_before_background_bulk`.
- [x] The budgets of 01-architecture.md are enforced by release-mode tests —
      `tests/perf_budgets.rs`: `budget_open_exif_under_1ms`,
      `budget_grid_thumb_under_25ms`, `budget_fullres_decode_under_350ms`,
      `budget_pipeline_throughput_over_60_per_sec`,
      `budget_video_export_30_frames_under_2s`,
      `budget_folder_scan_1000_entries_under_50ms`; the criterion benches of
      `benches/hot_path.rs` give the numbers for humans.
- [x] The read pool: clamp arithmetic in a pure struct with clock-free unit
      tests; the clocked decisions (thresholds, dead band, growth veto,
      stall, shrink throttle) at the pool level with test-injected
      thresholds whose margins are ≥ 100 ms from any boundary, so they stay
      reliable on loaded runners — growth veto by a stuck read, dead-band
      hold, stall halving without completion, one shrink per window,
      large-read exclusion from every decision, priority handoff, and the
      grant invariant `concurrent readers <= the limit at grant time <=
      cap` (readers granted before a non-preemptive shrink may transiently
      exceed the new lower limit, by design) — `pipeline.rs`
      `pool_warm_probe_cannot_outvote_stuck_read`, `pool_dead_band_holds`,
      `pool_cap_is_at_least_the_floor`, `pool_concurrency_never_exceeds_limit`,
      `pool_releases_highest_priority_waiter_first`,
      `pool_probe_grows_shrinks_and_excludes_large_reads`,
      `pool_large_probe_never_stall_shrinks`,
      `pool_slow_completion_shrinks_without_other_touches`,
      `pool_stalled_probe_shrinks_once`, `test_controller`.
- [x] All 8 orientations byte-identical to the reference implementation at
      sizes with partial tiles and partial thread bands; `decode_oriented`
      actually rotates — `tests/loupe.rs` `decode_oriented_actually_rotates` and
      the `raw/orient.rs` unit tests.

## History

- 2026-09-17 — Rewritten (brief 007); the seven M1-era boxes had been
  ticked the same day against the tests that hold them. The old text's
  "decoded with turbojpeg" for the full-res source was wrong — zune-jpeg
  decodes it and turbojpeg is not a dependency — and was dropped rather than
  moved. The pre-rewrite
  text is `specs/history/raw-pipeline.md`.
- 2026-09-12 — Issue #89 found: rawler's `MAP_POPULATE` on a
  walker-rejected file (brief 006's plan).
- 2026-09-05 — Dependencies compile optimised in debug (issue #76): the
  shutter's cap no longer resolves a doubled decode; the ladder's contracts
  are pinned by their tests, by decision.
- 2026-08-11 — The render ladder and the full-res eviction moved into core
  as `transit` (ui-grid.md).
- 2026-08-02 — The orientation rework (issue #27, PR #32: 518 → 277 ms);
  the hostile-input bounds (issue #31); the transit request states
  (2026-08-01, ui-grid.md).
- 2026-07-27 — The soft-transit contract (issue #21) and the reserved
  lane's debounced worker (`986b36f`, `53907bd`, v0.4.0); the lane's other
  two rules followed the QE and CI findings of the days after.
- 2026-07-27 — The in-tree EXIF walker replaces rawler on the hot path
  (the `mmap_lock` serialization; v0.4.0).
- 2026-07-26 — Bare JPEG sources (issue #8).
- 2026-07-25 — The loupe ladder, soft-rotation, the adaptive read pool and
  the full-res bypass (user decisions); the pool's design review.
- 2026-07-24 — M1 and ADR 0001.
