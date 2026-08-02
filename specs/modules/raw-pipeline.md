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

1. Targeted `seek`+`read` on the TIFF-shaped hot path — never read (or
   map) the whole file for any classic-TIFF container (every `.ARW`;
   also NEF/CR2/DNG); only IFD/metadata tables and the byte ranges of
   the chosen embedded JPEG. The EXIF summary uses the in-tree TIFF
   walker (`raw/jpeg_exif.rs::read_tiff_exif` — an ARW IS a TIFF), NOT
   rawler: rawler's `RawSource` mmaps the entire file, and the
   per-process `mmap_lock` serialized every import worker (perf
   investigation 2026-07-27 — the EXIF pass peaked at ~500 files/s and
   DEGRADED with more threads while the seek+read thumb path scaled to
   1,557/s; over FUSE mounts (ntfs-3g backup drives, card readers)
   each mmap page fault is a userspace round trip and a real 1,450-ARW
   folder took 99–133 s to import vs ~3 s with the walker; per-file
   EXIF cost 1.71 ms → 5 µs). The walker preserves rawler's vendor
   normalization ("SONY" → "Sony") so summaries are byte-stable across
   the swap. rawler remains in exactly two roles: the RAW-decode
   fallback, and the EXIF-metadata FALLBACK for non-classic-TIFF
   containers (CR3/RAF/X3F — see 00-overview.md's best-effort clause):
   a walker-rejected header falls back to rawler's parser, confining
   the mmap cost to those rare files (and to garbage files, which pay
   one bounded rawler attempt before erroring exactly as pre-fix).
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
     offsets) — properties rawler's path-based API doesn't offer. rawler
     remains the RAW-decode fallback and the non-classic-TIFF EXIF
     fallback (the hot-path EXIF summary moved to the in-tree walker,
     2026-07-27 — see step 1). BigTIFF (magic 43)
     containers are rejected as not-TIFF. Do NOT upstream anything without
     explicit user approval.
   - **Loupe asset ladder (user decision 2026-07-25, replaces the separate
     DCT FitPreview)**: display the best already-loaded asset immediately,
     and cook a higher-resolution one ONLY when the display size exceeds the
     loaded asset by more than 25% (the `UPSCALE_THRESHOLD = 1.25` rule).
     Rungs for the A1: 320 px thumb → 1616×1080 mid preview (~5 ms decode —
     covers fit view on ≲1.9k-wide viewports instantly) → 8640×5760 full
     (~140 ms, cooked in background for 1:1 and large displays; the shown
     image swaps in place when ready, never blocks). Note that "the ring" is no longer one width: `focus` prefetches
     ±`PREFETCH` (2) when settled but `TRANSIT_BEHIND`/`TRANSIT_AHEAD`
     (2/8, oriented by travel) while moving, whereas `revive_deferred`
     still gates revival on ±`PREFETCH`. A deferred upgrade for a frame
     between 3 and 8 away is therefore dropped rather than revived —
     harmless today (transit never escalates a target, so those entries
     are already sufficient) but a trap for any future widening.
     Loupe assets are served
     by a dedicated engine (`loupe.rs`) with its own event channel —
     two backlog workers plus one FOCUS-RESERVED worker. The reserved
     worker takes ONLY the focused index's job — or, since the
     transit/settled change (ui-grid.md, 2026-08-01), MANUFACTURES
     that job: while travelling, requests are capped at the mid rung,
     so once the user stops there is no full-res request anywhere in
     the system and this lane is the only thing that wakes on a timer
     to issue one. It does so only when the focused frame is short of
     the app's real target, is not already in flight, and has not
     failed. It acts only after the focus has represented the same
     PENDING WORK for a ~250 ms debounce: the clock re-arms when the focused index changes AND
     when the focused index's target escalates — above the HIGHEST
     target seen during the current focus tenure (a full-res climb
     freshly queued for a frame the cursor has been resting on is new
     work — QE proved the rest-then-escalate shape re-captured the
     lane ~20% of the time when only index changes re-armed). BACKLOG
     ladder flights are uninterruptible (mid→full in one flight, no
     intent recheck between rungs — their in-flight neighbors are
     legitimate prefetch); only the RESERVED lane rechecks between
     rungs and abandons when its index stopped being the focus (see
     below); the cursor legitimately rests on the
     first frame during load and touches transit frames for
     ~60-150 ms, and without the debounce any of those would capture
     the lane for a full multi-second debug decode. Transient focuses
     are left to the backlog workers (no debounce there — idle
     capacity still serves a fresh focus instantly), so the lane is
     free at the FIRST settle after sub-debounce transits and that
     frame's ladder starts within ~250 ms regardless of backlog
     commitment. The reserved lane's own flights ABANDON at rung
     boundaries when their index is no longer the focus (the lane
     serves the focus, only ever the focus; backlog workers never
     abandon — their neighbors are legitimate prefetch): without this,
     a stall-stretched transient hold that passed the debounce
     committed the lane to a full multi-second climb of a frame the
     user left — the double-settle residual, which fired for real on
     the v0.4.0 release-commit Windows run (bunched drive timers held
     an intermediate frame ~2 s; the settled frame then missed the
     60 s shutter cap). Remaining residual (accepted): the lane checks
     only BETWEEN rungs, so a focus change during a single rung's
     decode waits out that one rung (~30 s worst case in debug, ~140 ms
     release) — decode itself stays uninterruptible. (History: a
     debounce-less reservation failed validation for the
     transient-capture; an index-change-only clock failed QE for the
     rest-then-escalate capture; a boundary-check-less lane failed on
     the release-commit CI run for the double-settle) —
     full-res decodes must never queue behind a background thumbnail sweep.
     turbojpeg DCT scaling is a recorded FUTURE optimization only (saves
     ~35–45% on the cook; the ladder already hides that latency).
     Issue #21 (2026-07-27): while the top rung cooks, the loupe renders
     the mid rung UPSCALED at the carried factor with a visible cue —
     see ui-grid.md's revised quality rule; the ladder itself is
     unchanged (the landing frame's focus() preempts transit backlog,
     want-culling drops scrolled-past requests). Deferred upgrades (an
     in-flight index whose wanted rung grew mid-decode) are revived at
     land time ONLY while the index is still inside the focused
     prefetch ring, and a ring neighbor never outranks the focused
     frame's own pending work — a stale revival at top priority
     captured both loupe workers for full decodes of frames the cursor
     had left and starved the current frame past the screenshot
     shutter's 60 s cap (Windows CI, 2026-07-27). A dropped upgrade
     loses nothing: the next refresh re-requests it if still needed —
     focus() at the loupe, want()/ensure() for grid cells.
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

**Bare JPEG sources (issue #8)**: a `.jpg`/`.jpeg` session file IS its
own single whole-file "embedded preview" (`find_embedded_jpegs` returns
one candidate at offset 0), so the thumb/loupe ladder works
format-agnostically — the grid and loupe sources both resolve to the
file itself. Single-rung consequences (recorded at the gate): the loupe
`Ready` event carries a `terminal` flag (this rung is the file's best),
and the app adopts a TERMINAL mid-class-or-smaller texture as the top
rung so the zoom ceiling is knowable — without it, small JPEGs
(≤ 2048 px long edge: phone/web/export files) dead-ended the zoom path.
A > 2 MP JPEG's only grid source is the whole file, so its FIRST thumb
decode costs full resolution (the 25 ms ARW thumb budget does not apply
to JPEG sources; the cache absorbs re-opens). Extension decides the
EXIF path while the JPEG signature decides the preview path: a JPEG
renamed `.ARW` gets thumbnails by signature and an empty rawler
summary; an ARW renamed `.jpg` gets previews by TIFF walk and an empty
JPEG summary — both degrade, neither errors. Its EXIF (capture time, SubSec,
make/model/serial, Orientation) comes from the APP1 `Exif\0\0` TIFF
block via the in-tree hardened walker (`raw/jpeg_exif.rs` — rawler has
no JPEG path); an absent or hostile APP1 degrades to an empty summary
and orientation 1, never an error. Sony JPEG maker notes (burst
sequence) are out of scope in v1: JPEGs group via the generic time path.

Embedded previews are stored in sensor orientation; the EXIF Orientation tag
(IFD0 0x0112) says how to display them. Photo Mechanic soft-rotates and so
does FastCull: orientation is extracted by the TIFF walker and applied to the
DECODED PIXELS of every rung (thumb, mid, full-res) before display — RAW
files and sidecars are never modified. All 8 EXIF orientation values are
handled (rotations + mirrored forms). Cache note: the thumb cache stores
post-rotation pixels, so introducing this bumped the cache schema version
(pre-orientation thumbs invalidate wholesale).

**The rotate is a hot loop and is engineered as one** (`raw/orient.rs`,
issue #27 rework 2026-08-02; every constant pinned by a measured sweep on
real 8640×5760 pixels, recorded in the module header). Mirrors/180°
(orientations 2-4) run IN PLACE — no second 149 MB buffer. Transposes
(5-8) walk 64 px cache tiles under scoped threads capped at 8, with
bounds-check-free writes via `chunks_exact_mut`: 236 ms → **28-31 ms** on
the 8-core dev laptop, byte-identical output pinned against a reference
implementation for all 8 orientations at sizes exercising partial tiles
and partial thread bands. An `unsafe` pointer kernel measured 25 ms and
was REJECTED: ~4 ms is not worth the crate's first `unsafe` block.

**The full-res decode path pays its page faults off the critical path**
(`loupe::decode_oriented`, THE hot path — public so `perf_budgets`
measures the shipped code rather than a re-implementation): the A1
full-res JPEG is baseline with ZERO restart markers (verified by parsing
— the Huffman decode is strictly serial, so parallel decode is
impossible), meaning seven cores idle for ~220 ms while it runs. The
decode goes `decode_into` a pre-faulted buffer (saves ~30 ms vs
`decode()`'s internal allocation), and the transpose's output buffer is
allocated and pre-faulted on a spare thread DURING the decode
(`raw::Scratch`). Peak memory is unchanged — the same two buffers exist
either way; only WHEN their page faults are paid moves. Measured
end-to-end on the budget test, interleaved, medians of 5 rounds:
untouched 518 ms / cache-tiled-only (the superseded PR #28 approach)
309 ms / **this design 277 ms** — inside the 350 ms budget with headroom,
on the very laptop where issue #27 declared it unpassable. Buffer POOLING
(288 ms measured by the PR #28 research) remains deliberately excluded:
three workers × 149 MB of resident pool is a real memory decision, and
this gets under the budget without it. Also measured and rejected:
zune-jpeg 0.5.15 (267-279 ms vs 0.4.21's 247-252 — a regression on this
workload).

## Hostile-input bounds (issue #31, 2026-08-02)

Decode buffers are sized from HEADER claims before one byte of scan data is
validated, and in a crafted file every claim is attacker-controlled. Both
sides of the decode are therefore capped, and stream completeness is checked
before allocation:

- **Input side**: `MAX_EMBEDDED_JPEG_LEN` (256 MB, `raw/mod.rs`) caps what
  `read_jpeg` will allocate for a declared payload length.
- **Output side**: `MAX_DECODED_PIXELS` (500,000,000, `raw/mod.rs`) caps
  what SOF dimensions may size — checked in `loupe::decode_oriented` right
  after `decode_headers`, before the decode buffer, the prefault pass, or
  the transpose `Scratch` exist. 500 MP is ~10x the A1's 49.8 MP and ~3x
  the largest shipping sensor (150 MP medium format), with room for
  stitched panoramas served as bare JPEGs; the JPEG format ceiling
  (65535x65535 = 4.29 GP) would commit ~12.9 GB of RGB per buffer, and a
  sub-KB stream claiming 30000x30000 measured 5.29 GB RSS on the pre-fix
  path. The thumb/mid pipeline decode keeps zune's default 16384-per-side
  limit (268 MP — already stricter than this cap), so the pixel cap lives
  on the loupe path, the only one that lifts the per-side limits (it must
  accept panorama-wide bare JPEGs).
- **Truncation**: zune-jpeg 0.4 zero-fills missing scan data and reports a
  truncated stream as SUCCESS — its overread counter stops growing at the
  first zero-fill refill, so even strict mode's premature-end check can
  never fire — and it exposes no bytes-consumed accessor. For the record
  (gate measurement, 2026-08-02): 0.5.15 in strict mode DOES reject the
  plain no-EOI truncation ("premature end of buffer") but does NOT catch
  the EOI-appended residual shape below, and it remains a measured perf
  regression on this workload (above) — staying on 0.4 keeps the perf,
  and the byte-level check below covers the same truncation class the
  upgrade would have. Completeness is checked on the raw bytes
  (`raw/jpeg.rs::scan_is_terminated`): inside entropy-coded data proper,
  every 0xFF is either stuffed (FF 00) or a real marker, so a genuine
  FF D9 pair at or after the first SOS is an EOI.
  The search runs backwards from the tail — intact camera files end with
  EOI, so the hot path pays effectively nothing. Pre-SOS APP1 segments
  (EXIF thumbnails are whole JPEGs, EOI included) never vouch for the
  main scan. Applied in `decode_oriented` AND the grid-thumb decode
  (this spec's truncated-preview-yields-Failed criterion).
- **Residual gap (accepted, recorded on issue #31)**: a crafted stream
  carrying plausible dimensions, a valid EOI, and too-little entropy data
  still decodes as a mostly-blank "success" — detecting that requires
  decoder cooperation (bytes consumed vs. expected) that neither zune 0.4
  nor 0.5 offers. Same class, second shape: in MULTI-SCAN (progressive)
  streams, post-SOS table segments between scans (DHT etc.) are not
  entropy-coded and may legitimately contain a literal FF D9 pair, so a
  truncated progressive stream can pass the completeness check. Both
  shapes are bounded blank-successes, never a giant allocation: the caps
  bound the memory; real-world corruption (cut-off baseline camera files)
  is caught by the termination check.

All rejections flow through the existing error paths — `LoupeEvent::Failed`
/ `SessionEvent::Failed` — so the UI shows the Failed badge (ui-grid.md),
and subsequent jobs are unaffected.

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
- **Override** (user request 2026-07-25): `FASTCULL_MAX_READERS=N` replaces
  the adaptive cap for debugging/testing. N at or below the floor also lowers
  the floor — `=1` pins a single reader, `=4` restores the old fixed-4
  behavior; `>4` sets the ceiling to exactly N, **including above the core
  count** (it is an override, not merely a limiter: QE observed 94 readers
  with `=999` on 32 cores — useful for saturating high-latency NAS mounts,
  self-inflicted otherwise). Unset = fully adaptive. Env var (not a CLI
  flag) so the app and the CLI honor the same knob (verified in both;
  `FASTCULL_NO_CACHE`, by contrast, is app-only — the CLI uses
  `--no-cache`).
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
  full-res has priority") — the loupe workers (2 backlog + 1
  focus-reserved) stay ungated; a 12 MB
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
- [ ] Hostile decode dimensions (issue #31): a sub-KB stream whose SOF claims
      30000x30000 is rejected before any pixel allocation (unit-tested on
      `decode_oriented`, both orientation paths), and a scan cut off before
      EOI yields `Failed` — never a blank "success" — on both the loupe and
      grid-thumb decode paths.
- [ ] `set_visible` promotion: with a saturated queue, a newly visible image's thumb
      arrives before ≥90% of background items (deterministic test with a fake
      2-thread pool and instrumented job order).
- [ ] The budgets in `01-architecture.md` are enforced by release-mode tests
      (`tests/perf_budgets.rs`, dedicated CI step); criterion benches
      (`benches/hot_path.rs`) provide the numbers for humans.
