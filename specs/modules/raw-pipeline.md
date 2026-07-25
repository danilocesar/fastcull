# Module spec: RAW preview pipeline (`raw/` + `pipeline.rs`)

## Purpose

Turn a folder of RAW files into displayable images at interactive speed without ever
decoding RAW sensor data on the hot path.

## Inputs / outputs

- In: file paths from `catalog`; priority hints from the UI (visible range, loupe pos).
- Out: per image, up to three assets delivered as `SessionEvent`s:
  `Thumb` (320 px, grid), `Mid` (1616-class, large grid cells + loupe fit),
  `FullRes` (1:1 pixels) — climbed per the 25% ladder rule below.

## Extraction strategy (per file)

1. Open with `rawler::RawSource` + `get_decoder` — never read the whole file; only
   IFD/metadata and the byte ranges of the chosen embedded JPEG.
2. Asset sources, in order of preference:
   - **Grid thumb**: largest embedded preview ≤ ~2 MP (A1: the 1616×1080), decoded
     with zune-jpeg, SIMD-resized (`fast_image_resize`) to 320 px.
   - **FullRes**: largest embedded JPEG (A1: 8640×5760 `JpgFromRaw`) decoded with
     turbojpeg. **Known gap**: rawler 0.7 does not expose the A1 full-res JPEG
     (`full_image()` returns 1616×1080). Discovery therefore uses an in-tree,
     from-scratch minimal TIFF/IFD walker (`raw/tiff.rs`) rather than rawler's
     TIFF parser: it operates on any `Read + Seek` (enabling the counting-reader
     budget tests), reads only IFD tables and JPEG headers, and is hardened
     against hostile files (offset cycles, entry-count bombs, out-of-range
     offsets) — properties rawler's path-based API doesn't offer. rawler remains
     the EXIF/metadata and RAW-decode-fallback dependency. BigTIFF (magic 43)
     containers are rejected as not-TIFF. Do NOT upstream anything without
     explicit user approval.
   - **Loupe asset ladder (user decision 2026-07-25, replaces the separate
     DCT FitPreview)**: display the best already-loaded asset immediately,
     and cook a higher-resolution one ONLY when the display size exceeds the
     loaded asset by more than 25% (the `UPSCALE_THRESHOLD = 1.25` rule).
     Rungs for the A1: 320 px thumb → 1616×1080 mid preview (~5 ms decode —
     covers fit view on ≲1.9k-wide viewports instantly) → 8640×5760 full
     (~140 ms, cooked in background for 1:1 and large displays; the shown
     image swaps in place when ready, never blocks). Loupe assets are served
     by a dedicated 2-worker engine (`loupe.rs`) with its own event channel —
     full-res decodes must never queue behind a background thumbnail sweep.
     turbojpeg DCT scaling is a recorded FUTURE optimization only (saves
     ~35–45% on the cook; the ladder already hides that latency).
     The ladder applies to GRID CELLS too (user bug 2026-07-25): any cell
     wider than 320 × 1.25 physical px is served by the mid rung via
     `LoupeEngine::want(range, cell_width)`; UI-side bookkeeping lives in
     core (`viewassets.rs::ViewAssets`) so it is testable — `ensure()` also
     adopts engine-cached images that emit no event (the pruned-and-
     revisited-cell bug). Scrolled-past want-requests are CULLED on every
     want() call so visible cells never starve behind stale backlog.
3. Fallback chain when a source is missing (non-A1 cameras): full-res JPEG → mid
   preview upscaled → half-size RAW decode via rawler (background priority only,
   with a "rendered from RAW" badge event) → `Failed(reason)`.

## Orientation (user requirement 2026-07-25)

Embedded previews are stored in sensor orientation; the EXIF Orientation tag
(IFD0 0x0112) says how to display them. Photo Mechanic soft-rotates and so
does FastCull: orientation is extracted by the TIFF walker and applied to the
DECODED PIXELS of every rung (thumb, mid, full-res) before display — RAW
files and sidecars are never modified. All 8 EXIF orientation values are
handled (rotations + mirrored forms). Cache note: the thumb cache stores
post-rotation pixels, so introducing this bumped the cache schema version
(pre-orientation thumbs invalidate wholesale).

## Adaptive read pool (user requirement 2026-07-25, replaces the fixed 4-permit gate)

History: 32 simultaneous readers drove a microSD into minute-long kernel I/O
queues (blk_mq) and blocked shutdown — slow removable media serves few streams
well. A fixed limit of 4 fixed the hang but cannot react when the medium
degrades further mid-session (e.g. a concurrent multi-GB transfer to the same
card). Requirement (user, 2026-07-25): a **pool manager** owns the release
of read workers and adapts their number to the medium's measured behavior.

Design (validator design review 2026-07-25: ADOPT-WITH-CHANGES, incorporated):

- **Pool manager**: owns `(limit, in_flight)`. A worker asks the manager for
  release before entering a read section; `acquire` waits while
  `in_flight >= limit`. Decode remains fully parallel and unmanaged.
- **Bounds** (user decisions 2026-07-25, second round): **floor 4** — the
  empirically proven-safe value is always available; NAS/network mounts must
  never be throttled below it. **Cap = CPU core count** ("if the loader is
  not stuck, we can add more up to the number of CPU cores"): growth beyond
  the floor is earned probe by probe, so slow media never sees the high end.
  Initial limit = floor 4, preserving the QE-verified cold-open behavior on
  healthy media. Local-media benchmark for the record (2026-07-25, reference
  machine, 300-job run): fixed-4 = 350/333 files/s vs fixed-8 = 313 files/s —
  local NVMe is decode-bound, so growth is for latency-bound sources
  (network mounts), not local throughput.
- **Probe measurement**: at most one *probe* read is outstanding at any time;
  the first read granted while no probe is outstanding becomes the probe.
  The probe paces GROWTH (one growth decision per completed probe); shrink
  signals come from the whole in-flight set (see stall watching below).
  All timings are **pure in-permit read time** (queue/wait time is NEVER
  included — measuring wait creates a positive-feedback collapse). Only
  probe-eligible reads are the preview-read section (open + IFD walk + `read_jpeg`)
  feeds the controller; the EXIF section is pool-managed but not sampled
  (rawler parse CPU would contaminate it). Cache hits bypass the pool and
  produce no samples. Reads larger than 2 MB feed NO decision — neither
  completion NOR stall (validator H1: the exclusion must cover every
  decision path, or the non-A1 full-res-as-grid fallback stall-shrinks a
  healthy medium to the floor); the size is known before the bulk read, so
  the probe is neutralized as soon as its payload is chosen.
- **Control rule** (AIMD with hysteresis dead-band): probe < 200 ms →
  release one more worker (+1, clamp cap); probe > 500 ms → **halve** the
  limit (clamp floor 4); otherwise hold. Halving (not −1) is required by
  the core-count cap: recovering from a warm-cache-pumped limit of 32 on a
  suddenly-slow card takes 3 halvings instead of 28 single steps.
- **Growth requires "the loader is not stuck" — literally** (live incident
  2026-07-25: warm 0 ms page-cache probes pumped the limit 4 → 22 while
  every cold read sat wedged on a saturated microSD; fast probes have
  survivorship bias — stuck reads never report). A fast probe grows the
  limit ONLY when no other in-flight NON-EXCLUDED read is older than the
  grow threshold: one wedged normal read anywhere vetoes all growth.
  Excluded (>2 MB) reads neither vouch nor indict — accepted residual: a
  genuinely wedged large read vetoes nothing and triggers no shrink (large
  reads legitimately run long; counting them would permanently veto growth
  on non-A1 fallback folders).
- **Stall watching covers EVERY in-flight read, not just the probe**
  (persona review: in the original incident reads didn't come back slow,
  they didn't come back at all). If the oldest non-excluded in-flight read
  exceeds the shrink threshold, the manager halves WITHOUT waiting for any
  completion — checked on every pool touch (acquires and releases alike)
  plus a periodic re-check by blocked waiters. Shrinks (stall or slow
  completion) are throttled to **one per shrink-threshold window**: a
  persistent wedge walks cap → floor in ~3 windows (~1.5 s), with no
  collapse cascade from many simultaneous slow observations. Known blind
  spot (recorded): if the limit equals the worker count and EVERY worker is
  wedged inside a read, no thread touches the pool until the first read
  returns, so the cascade starts late — harm is bounded to the reads
  already in flight (no new reads can be issued in that state), the same
  surface non-preemptive shrink already accepts.
- **Degradation bound**: cap to floor in ≤3 halvings (~1.5 s of sustained
  stall). Upward, +1 per fast probe from the floor; on fast media probes
  are milliseconds apart, so the ramp is invisible. The floor guarantees
  the proven baseline of 4 at all times — there is no "stuck at 1" state.
- **Debug visibility** (user request 2026-07-25): every limit change is
  logged to **stderr** (`eprintln`, consistent with all FastCull
  diagnostics; stdout belongs to CLI output) as
  `fastcull: read pool N -> M workers (probe read X ms | read stalled for
  X ms; K reading)` where K is the number of reads actually in flight at
  that moment (user request: show how many workers are actually alive).
  Steady state logs nothing (clamped no-op changes are not printed).
- **Retirement is non-preemptive**: a shrink only lowers `limit`; reads in
  progress always finish (never cancelled), the slot is simply not re-released.
- **Priority-aware release**: waiting workers queue with a
  (job priority, arrival seq) ticket; a freed or newly-grown slot goes to the
  lowest ticket — a visible thumbnail is released before background prefetch
  even at the floor. Growing the limit must wake ALL waiters
  (lost-wakeup hazard, previously bitten).
- **Scope**: thumbnail pipeline only. Loupe full-res reads BYPASS the pool
  on purpose (user decision 2026-07-25: "full-res should bypass it, as
  full-res has priority") — the 2 loupe workers stay ungated; a 12 MB
  full-res read would also poison a latency-threshold controller. Risk on
  record (persona): at the floor on a dying card, an ungated loupe full-res
  read can still hit the card hard — revisit if the hang class ever
  reappears via the loupe path.

Persona questions resolved by the user (2026-07-25):
- NAS/network culling IS part of his workflow → resolved by the **floor of
  4** (a healthy-but-distant source is never throttled below the proven
  baseline) plus the core-count cap (fast probes let latency-bound sources
  earn more streams). The relative-baseline signal stays a recorded future
  option if absolute thresholds prove wrong on his NAS in practice.
- Cull-while-ingesting: "usually no" — the shrink path is a safety net, not
  a daily-driver optimization.
- Status-bar "slow storage" hint: mooted by the floor (there is no
  pinned-at-1 state); stderr limit-change lines are the debug surface.

Testability requirements: clamp arithmetic (grow/halve, cap/floor) lives in
a pure struct with clock-free unit tests; the clocked decisions (thresholds,
dead-band, growth veto, stall, shrink throttle) are unit-tested at the pool
level with test-injected thresholds whose margins are WIDE relative to
scheduler noise (≥100 ms between a sleep and the boundary it must not
cross — pool tests must stay reliable on loaded CI runners). Covered
deterministically: growth veto by a stuck read, dead-band hold, stall
halving without completion, one-shrink-per-window throttle, large-read
exclusion from every decision, priority handoff, and the grant invariant —
`concurrent readers <= the limit at grant time <= cap` (during a
non-preemptive shrink, readers granted earlier may transiently exceed the
NEW lower limit; that is by design) — replacing the one-off manual fd proof
from the fixed-gate era.

## Priority queue contract

- Three levels: `Visible` > `Prefetch` (loupe ±2) > `Background` (sequential file
  order — cold-cache/card-reader friendly).
- Scroll/zoom calls `set_visible(range)`; already-queued jobs are reprioritized, not
  re-enqueued. In-flight jobs are never cancelled mid-decode (they're ≤150 ms).
- Duplicate requests for the same (image, asset) coalesce.

## Memory budget

- Thumbs: unbounded (≈200 KB each; 5,000 images ≈ 1 GB worst case — acceptable; the
  SQLite cache lets us evict and re-load cheaply if this ever pinches).
- FullRes decodes: LRU capped at 2 GiB (configurable). mid-rung textures count toward it.

## Acceptance criteria (tests)

- [ ] **MANDATORY zoom-quality gate (user mandate 2026-07-25)**:
      `tests/zoom_walk.rs` (the user's 2-column forward-walk repro + the
      fast-scroll starvation variant) MUST pass — in release mode, against
      the real A1 files — before ANY zoom-quality problem is declared fixed.

- [ ] For each of the 3 A1 test files: grid thumb is produced from the 1616×1080
      preview (assert source dimensions), FullRes is 8640×5760.
- [ ] No test may observe a read of more than 20 MB from a 100 MB A1 file for the
      grid path (instrument with a counting reader).
- [ ] A file with a truncated/garbage preview yields `Failed` and does not poison
      the pipeline (subsequent jobs complete).
- [ ] `set_visible` promotion: with a saturated queue, a newly visible image's thumb
      arrives before ≥90% of background items (deterministic test with a fake
      2-thread pool and instrumented job order).
- [ ] The budgets in `01-architecture.md` are enforced by release-mode tests
      (`tests/perf_budgets.rs`, dedicated CI step); criterion benches
      (`benches/hot_path.rs`) provide the numbers for humans.
