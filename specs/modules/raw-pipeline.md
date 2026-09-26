# Module spec: RAW preview pipeline (`raw/`, `exif.rs`, `pipeline.rs`, `loupe.rs`, `budget.rs`, `viewassets.rs`)

## Purpose

Turn a folder of RAW files into displayable images at interactive speed
without ever decoding RAW sensor data on the hot path (ADR 0001): read the
camera's embedded JPEGs, and only them — for the loupe, decoded by
libjpeg-turbo at the size the screen draws them (ADR 0005).

## Behaviour

### Inputs and outputs

- In: file paths from the catalog; priority hints from the UI — the visible
  range, the loupe position and the loupe's fit box.
- Out: per image, up to four assets — `Thumb` (320 px, the grid) from the
  pipeline, and from the loupe engine's own channel the `Mid` (the
  1616-class preview: large grid cells, loupe fit on displays up to ~2K),
  the SCREEN RUNG (the embedded full JPEG decoded at N/8 to the loupe's fit
  box: fit on wider displays) and `FullRes` (1:1 pixels) — climbed by the
  ladder rule below. Every loupe `Ready` event names its rung's kind and the
  request state its decode carried.

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
- **The decoder** (ADR 0005): every loupe rung — mid, screen rung,
  full-res — is decoded by libjpeg-turbo ≥ 3.0 through the `turbojpeg`
  crate, except a stream whose header says CMYK or YCCK (a print-ready bare
  JPEG, issue #8): libjpeg-turbo will not convert those to RGB, so zune-jpeg
  decodes them, at full scale, with no screen rung (brief 008 R14, Manager
  ruling 2026-09-26). The grid thumb stays on zune-jpeg: its
  source is the small preview, whose decode is a small share of the thumb's
  budget, so a swap would buy little and re-open the grid path's
  hostile-input surface for nothing visible; the one place scaling would pay
  there — a large bare JPEG's thumb, which decodes at full size — is its own
  change with its own measurement; the two decoders' chroma upsampling
  differs by an amount invisible at 320 px (the senior developer's call,
  brief 008 N7). zune-jpeg stays at 0.4, for the thumbs and the
  CMYK/YCCK route alike: 0.5.15 measured slower on the full-size decode
  (History, 2026-08-02).
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
separate DCT FitPreview — the screen rung below is a DCT-scaled decode
again, but as a rung of this ladder under its rules). A1 rungs: the 320 px
thumb → the 1616×1080 mid (~5 ms; covers fit on ≲ 1.9k-wide viewports
instantly) → the screen rung (fit on wider viewports) → the 8640×5760 full
(1:1 and every factor above fit; the shown image swaps in place when it
lands, never blocks). A rung is decoded only when it is the cheapest one
that serves the request: a request above fit climbs mid → full and never
pays the screen rung; a request for the fit box on a 4K viewport climbs
mid → screen rung and stops. The ladder applies to GRID CELLS too: any cell
wider than 320 × 1.25 physical px is served by the mid rung
(`LoupeEngine::want(range, cell_width)`); the UI-side bookkeeping is
`viewassets.rs::ViewAssets`, in core, whose `ensure()` also adopts
engine-cached images that emit no event (the pruned-and-revisited cell).
Scrolled-past want-requests are culled on every `want()` call, so visible
cells never starve behind stale backlog.

The engine (`loupe.rs`) has its own event channel and one worker per
physical core (The decode workers, below): one FOCUS-RESERVED lane and the
rest BACKLOG workers.

- The reserved lane takes only the focused index's job — or MANUFACTURES it.
  While the user travels (ui-grid.md's request states), the focused frame asks
  for no more than the fit box, at fit and above it (The ring, its request
  table), so once the user stops nothing in the system may yet ask for the
  real target, and this lane is the only thing that wakes on a timer to issue
  one. It acts only when the focused frame is short of the app's real target,
  not already in flight and not failed, and only after the focus has
  represented the same PENDING WORK for a ~250 ms debounce (`FOCUS_DEBOUNCE`):
  the clock re-arms when the focused index changes AND when its target
  escalates above the highest seen during the current focus tenure (both
  guards are load-bearing: without the in-flight guard a key release during
  the transit rung's decode queues a duplicate full-res job and a ~149 MB
  transient; without the sufficiency guard the lane spins push/pop forever
  holding the state mutex and freezes every worker). Full-res decodes must
  never queue behind a background thumbnail sweep — the rule the lane and the
  pool bypass serve. Transient focuses (the first frame during load, transit
  frames for ~60-150 ms) are left to the backlog workers, which need no
  debounce, so the lane is free at the FIRST settle after sub-debounce
  transits.
- **The idle cook** is the lane's second manufactured job (brief 008 R9,
  Manager rulings 2026-09-26): settled at fit on a WIDE viewport — the
  reference landscape A1 mid, 1616×1080, does not serve the fit box
  (`loupe::mid_serves_box`) — with the cursor's screen rung in hand, the
  file's best not yet cached and nothing queued or in flight for it, the lane
  queues the cursor's full-res after the same `FOCUS_DEBOUNCE`, pops it in the
  same call and decodes it uninterruptibly, never on a backlog worker; so `Z`
  after a stop at fit finds the cursor's full-res cooked or cooking while the
  ring keeps filling. It never re-arms the escalation clock and never passes
  through `revive_deferred`. "Wide" is one predicate, decided per VIEWPORT: a
  frame that needs a rung on a viewport the reference mid serves — a portrait
  A1 on a monitor turned to portrait (fit box ≈ 1440×2260), a bare panorama on
  a 1080p screen — takes its rung, with the cue, but gets no idle cook, so its
  `Z` after a stop waits for the full-res decode (Manager ruling 2026-09-26,
  revisited if the user ever culls on a rotated screen). On viewports the mid
  serves there is no idle cook; extending it there is a follow-up if the user
  asks — one background decode per stop (Manager, M2 2026-09-26).
- **The settled ring after a hold** (Manager ruling 2026-09-26, brief 008
  Q-I): the ring's settled requests are asked by the app's next focus, and
  the app refreshes when something lands; when the user stops on a frame whose
  real target is already in hand nothing lands, so the reserved lane — at the
  same settle, finding nothing to climb — asks for the settled ring itself,
  once per settle, and not at all when a settled focus of the app's own has
  already asked it, for an engine with a fit box; an engine without one
  keeps the behaviour before brief 008. When the focused frame does need its
  climb, the lane asks no ring: the climb's landing refreshes the app, whose
  settled focus asks it — SETTLED, then SETTLED-AND-IDLE (ui-grid.md).
  Without this a tap forward right after a hold at 1:1 would land soft
  whenever the hold ended on a frame already sharp.
- The reserved lane's flights ABANDON at rung boundaries when their index is
  no longer the focus; backlog flights are uninterruptible (a whole ladder in
  one flight — their neighbours are legitimate prefetch). The lane checks only
  BETWEEN rungs, so a focus change during a rung's decode waits out that rung
  — one decode, whose release cost 01-architecture.md's perf rows give. A
  decode itself is never interrupted.
- The ring — 2 behind and 15 ahead of the cursor, in view order, leaning
  the way of travel — has its own section below, with the queue order, the
  cull and the revival gate.
- The pixel cache — a byte-budget LRU sized from total RAM (Memory, below)
  — evicts the least recently focused images, never the focused one; mids,
  screen rungs and full-res frames share it, one slot per index. The app's
  texture rings evict by `transit::evict_ring` (ui-grid.md).
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

### The screen rung (issue #60; user decisions 2026-09-26, persona-validated)

The user's words (2026-08-29): *"When fast culling between several images
(press and hold the right arrow) while in fit or 1:1 screen, the quality
gets bad very quickly, maybe within 2 or 3 frames."* Between the mid and the
full sits the SCREEN RUNG: the embedded full JPEG decoded by libjpeg-turbo
with DCT scaling (ADR 0005) at the smallest N/8, N in 1..=7, whose ORIENTED
output serves the loupe's fit box under the 1.25 rule (brief 008).

- It is a rung like every other: orientation is applied to its pixels, it
  is published as its own `Ready` event, and it lives in the index's single
  cache slot (the mid's replacement, the full's predecessor), so it counts
  toward the pixel cache as the mid does. It is never the TOP rung however
  large: the `Ready` event names its KIND — `mid`, `screen` or `full` — and
  `terminal` stays "the file's best possible rung", so the app never infers
  top-rung-ness from a size (a 3240 px rung exceeds `MID_RUNG_MAX_LONG`, and
  a size test would adopt it as 1:1 and read the zoom ceiling from it). The
  kind is what the decoder RAN — a scale below 8/8 is `screen` — never a
  comparison with an IFD's size claim, which a file can under-state.
- **The fit box** is the loupe's N=1 cell in physical pixels, which the app
  supplies on every refresh at the loupe (`set_fit_box`), the way it
  supplies the view order. A request at fit asks for the box (`focus_fit`),
  and the ladder serves it with the cheapest rung: the mid when its ORIENTED
  size serves the box, else the screen rung, else the full.
- **The factor rule** is the BOX rule: the frame's oriented extent scaled to
  fit the box is what the screen shows, and the rung is the smallest N/8
  whose oriented output × 1.25 covers that extent. A frame whose full JPEG
  already fits within the box × 1.25 gets no rung, and the full is the
  target; N = 8 is the full. The mid is tried first, so on viewports up to
  ~2K the mid serves fit and nothing changes on the decode path. For the A1
  (8640×5760, mid 1616×1080):

  | viewport (physical px) | landscape frame | portrait frame (orientation 5–8) |
  |---|---|---|
  | 1920×1080 | the mid | the mid |
  | 2560×1440 (QHD) | 2/8 (2160×1440) | the mid (1080×1616 oriented covers the 960×1440 extent) |
  | 3840×2160 (4K) | 3/8 (3240×2160) | 2/8 (1440×2160 oriented) |
  | 5120×2880 (5K) | 4/8 (4320×2880) | 3/8 (2160×3240 oriented) |

  A box between two factors takes the next one up ("the smallest N that
  serves", never "the nearest"). A portrait frame uses its ROTATED extent: a
  long-edge rule would ask 3/8 for a portrait A1 on 4K, where 2/8 serves. A
  bare JPEG (issue #8, one candidate = the whole file) gets a rung by the same
  rule, and only its full-scale decode is `terminal`. A lossless stream, which
  libjpeg-turbo cannot scale, decodes full-scale with no rung (developer
  2026-09-26, brief 008 step 1), and so does a CMYK or YCCK stream (Manager
  ruling 2026-09-26, brief 008 R14).
- **The factor follows the viewport**, never a constant: a resize, a panel
  toggle or a move to another display re-keys the box at the next refresh; a
  cached rung that no longer serves the new box is re-requested at the new
  factor, and until it lands it is a SOFT rung, shown with the cue — never
  presented as sharp (brief 008 R3, Manager decision 2026-09-26).
- **An engine with no fit box** — a consumer that never calls
  `set_fit_box`, as the older core tests — asks in transit for the mid over
  the ring, as `MID_RUNG_TARGET` (1616), a size the mid serves, never
  `MID_RUNG_MAX_LONG`'s 2048, which it does not (1.25 × 1616 = 2020);
  settled, for the app's target over ±`PREFETCH`, which is then its ring in
  force; and it has no switch rule, which needs the box to know what "above
  fit" is (Manager ruling Q2, 2026-09-26). The app supplies a box at every
  refresh at the loupe; before the first layout it has none, and a request
  at fit is then the mid.

### The decode workers (user decision 2026-09-26, Manager M6)

The loupe engine runs one decode worker per PHYSICAL core, not per logical one
— on this serial, Huffman-bound decode a hyperthread adds little throughput
and doubles the per-frame latency (brief 008's benchmark) — with a floor of 3
(two backlog workers and the focus-reserved lane, which stays) and a cap of
16: the ring holds 18 frames, so more than 18 decoders could never all be
busy, and sixteen is the core count of the one culling machine whose CPU is
known, the Ryzen AI Max+ 395; a larger machine only waits a second round of
decodes for the ring's last frames. The machine's RAM caps them too, at half
its total RAM in GiB rounded down — `⌊total ÷ 2 GiB⌋` of the total "Memory"
reads, so a 16-core machine that reports 31.x GiB, as a 32 GB one does, runs
15 — though never below the floor of 3: each decoder holds up to two decoded
full-res frames and the JPEG it reads at once, and the cap stops the whole-app
worst case ("Memory", below) from growing with the cores where the RAM is
small (Manager rulings 2026-09-26, brief 008 Q4 and Q-D). The count comes from
`num_cpus::get_physical` — on Linux `/proc/cpuinfo`'s `cpu cores` summed per
`physical id`, on Windows `GetLogicalProcessorInformation`'s
`RelationProcessorCore` entries, and on both the logical count when the
topology is unreadable — and the rule reads a zero or missing count as 4
(Manager rulings 2026-09-26). `FASTCULL_DECODERS=N` replaces the count, in the
mould of `FASTCULL_MAX_READERS`: a testing and diagnosis switch, an
environment variable so a release build honours it, taken as given above
either cap and below the floor down to 2 — one backlog worker beside the
reserved lane, the least that still reads ahead, so 1 reads as 2 — and
ignored, with a stderr line naming it, when it is not a positive integer
(Manager ruling 2026-09-26, brief 008 R5). The persona's "cores − 1, never all
cores" is recorded; ui-grid.md A5's p90 frame interval is where the jitter it
feared would show.

### The ring (user decision 2026-09-26)

- ONE ring shape at every factor, travelling and settled alike: `RING_BEHIND`
  = 2 frames behind the cursor and `RING_AHEAD` = 15 ahead, in VIEW
  order (`set_view`; issue #46), leaning the way of travel by the latch
  `note_focus` sets at an index change and never re-derives per call
  (ui-grid.md, "Transit and settled"). It is fixed, never derived from the
  machine — the user's words: *"What if we do 2 behind and 15 in front? I
  feel we are overly complicating things."* The folder's ends clamp it, and
  above fit the pixel cache may shorten its far end (Above fit, below);
  nothing else changes it. A reversal re-leans on the very next focus; frames
  behind the ring stay in the pixel cache while it has room (the persona: the
  compare loop steps 1–3 back). An engine whose consumer never calls
  `set_view` keeps identity order — the pre-#46 behaviour, which the pre-#46
  core tests still pin.
- **What each position asks for** — stated here once; ui-grid.md's request
  states ("Transit and settled") say which row applies:

  | | the focused frame | the members behind | the members ahead |
  |---|---|---|---|
  | at fit, travelling or settled | the fit box | the fit box | the fit box |
  | above fit, settled | the top rung | full-res | full-res |
  | above fit, travelling (a hold) | the fit box | the fit box | full-res, or the fit box by the switch rule |

  The fit box is served by the cheapest rung that serves it: the screen rung
  on a wide viewport, the mid on displays up to ~2K. At fit a stop asks for
  what the hold already asked, so it costs nothing when the decoders kept
  ahead. Above fit, the members beyond the cache's clamp ask for nothing
  (Above fit, below). Settled at fit on a wide viewport, the reserved lane
  adds the cursor's full-res (the idle cook). An engine with no fit box: The
  screen rung, above.
- **The ring in force** is the ring around the current cursor as this
  section and the next shape it: 2 behind and 15 ahead in view order, leaned
  by the latch and clamped at the folder's ends; above fit, shortened at its
  far end by the cache's clamp; for an engine with no fit box, ±`PREFETCH`
  while settled. The cull and the revival gate read it and nothing else.
- **Order in the queue**: the focused frame's own work first, always — its
  entry at the back of the queue (popped next by a backlog worker), and the
  reserved lane's manufactured jobs (the settle climb, the idle cook) on the
  lane that serves only it; then ring members nearest first, and at equal
  distance the one in the travel direction first. A ring member never
  outranks the focused frame's pending work (the 2026-07-27 starvation
  rule).
- **Culling**: on every focus the queued — never in-flight — focus-origin
  entries whose view position falls outside the ring in force are dropped,
  so a reversal culls what leaned the wrong way and a stretched gap
  mid-hold costs at most the decodes in flight. Grid wants keep their own
  cull.
- **Revival**: a deferred upgrade — an in-flight index whose wanted rung
  grew mid-decode — is revived at land time only while the index is inside
  the ring in force, and at no more than what its position there asks for
  now — so during a hold above fit a frame the cursor has reached or passed
  revives at the fit box, never at full-res; and a ring neighbour never
  outranks the focused frame's own pending work: a stale revival at top
  priority once captured both workers for frames the cursor had left and
  starved the current frame past the shutter's 60 s cap. A dropped upgrade
  loses nothing: the next refresh re-requests it (`focus()` at the loupe,
  `want()`/`ensure()` for grid cells).
- **The request state travels with the decode**: every queue entry carries the
  request state — `transit` or `settled` — of the focus that last scheduled or
  re-targeted it: a focus that schedules an index replaces its queued entry,
  so the latest state and target win, while one whose request the cache
  already serves leaves the entry untouched (the hold's re-plan aside, Above
  fit); an in-flight decode keeps the state it started with; a revived entry
  keeps the state of the focus whose target was deferred, stored beside the
  target, never the mode at revival; and a merge (an in-flight index's
  deferred target, merged with `max`) changes the state only when the target
  grows. The `Ready` event carries the state out of core beside the rung's
  kind — an instrument, not behaviour: under a transit capped at the mid no
  screen rung ever lands `transit` (ui-grid.md A5, gate 2).
- **Nearest-first when the decoders fall behind at fit**: once the key
  outruns the aggregate rung rate, every decode beyond the frontier starts
  on a frame the cursor reaches before it lands, so the frames a hold sees
  at the rung are the runway plus the frontier race — a decode-rate figure,
  never a gate (issue #27). The lever, if a decode-bound seat ever matters,
  is a transit LEAD — pop the member latency ÷ key period ahead of the
  cursor instead of the nearest — a change to this queue order with its own
  clock-free row, for a later unit, never a silent reorder.
- Not done here (a later unit): preparing the `]` target — the next burst's
  first frame — while the user rests (Manager 2026-09-26).

### Above fit: the full-res ring and the switch rule (user decision 2026-09-26; the switch rule is the persona's, adopted by the Manager, M2)

- **Above fit the ring in force is the FULL-RES ring**, clamped at its far
  end so that the pixel cache's figure holds its frames twice over — each
  frame's decoded pixels in the cache and its texture copy outside it — and
  the kitchen's fill in flight besides: the cursor and the two behind
  always, then as many of the fifteen ahead as `⌊(cache − 149,299,200) ÷
  (2 × 149,299,200)⌋` frames in all (149,299,200 B is the reference A1
  frame, 8640 × 5760 × 3 bytes) leaves room for, the far end first — the
  whole ring on a cache of 37 frames or more (5,524,070,400 B, about
  5.1 GiB: a machine with about 20.6 GiB of RAM), 10 ahead on a 4 GiB
  cache, 3 at the 2 GiB floor (brief 008, the redesign's G2; the texture
  copies counted, Manager ruling 2026-09-26, brief 008 Q4; the fill
  counted, Manager ruling 2026-09-26, brief 008 Q-G). A ring the cache
  cannot hold would be decoded and then evicted, and one whose textures
  the RAM cannot hold would take the whole app past it (Memory). The
  positions beyond the clamp are outside the ring in force: they ask for
  nothing, settled and during a hold alike, and the cull drops what they
  had queued.
- **Settled and tapping**, the whole full-res ring asks for full-res and the
  focused frame for the top rung (the table, The ring), so a tap forward at
  1:1 lands on a sharp frame once the ring has filled.
- **During a hold** the focused frame and the members behind ask for the fit
  box, never full-res: a full-res decode started for the frame the cursor is
  on lands after the cursor has left, and before transit such decodes swamped
  a hold (the 2026-08-01 finding, ui-grid.md History). The focused frame's
  full-res is kept when already cached or in flight — an in-flight decode is
  never re-targeted — and once the user stops, the reserved lane asks for the
  real target (the settle guarantee). At every focus of a hold the engine
  re-plans those frames whatever the cache holds: a QUEUED full-res entry for
  the focused frame or for a member behind is replaced by the fit-box request,
  or dropped when that rung is already in hand. Elsewhere a request the cache
  already serves leaves the queue untouched, and a stale full-res entry left
  that way would still be popped. So no full-res decode starts during a hold
  for the frame the cursor is on or for one it has passed. The members ahead
  ask for full-res while their full-res can reach the screen before the cursor
  does, and for the fit box when it cannot, by the switch rule below. The hold
  is never slowed, and the render shows the best rung in hand, cued
  (ui-grid.md). One rule decides the switch:
  1. **Step down once, at the decode.** When a worker is about to start a
     member's full-res decode, the engine compares the time the cursor needs
     to reach that member — its distance ahead at that moment, in view
     positions counted from 1 (the first member ahead), × the hold's key
     period, the interval between the last two index changes — with the
     full-res TIME-TO-SCREEN: for the latest full-res decode whose fill the
     app reports complete, the time from that decode's start to the app's
     report (`note_adopted`), the moment the frame is ready to draw — whether
     the app's ring then held the texture or it was at once that ring's
     victim, since either way it was ready to draw; a re-adoption of an
     already-cached frame measures nothing, nor does a decode whose fill the
     app culled (`note_dropped`); and a measurement exists only while the
     engine has a fit box — a decode started without one starts none, and
     the box going ends every open one unmeasured (Manager ruling
     2026-09-26, brief 008 Q-K). When the cursor would arrive first, that
     member and every member beyond it ask for the fit box — one boundary,
     in view positions, set before any of their full-res decodes starts, so
     the frames the cursor meets step from full-res to the fit-box rung once
     and never dip through a mid or a thumb that the switch caused. A
     full-res entry still queued at or beyond the boundary becomes a fit-box
     entry, or is dropped when that rung is already in hand; one already in
     flight lands.
  2. **Step up only from a complete ring with a free decoder.** At a focus,
     before it schedules anything: when every member ahead but the farthest —
     the newest, which a hold keeps renewing — holds its fit-box rung or
     better or has its decode in flight, no ring work waits in the queue and
     a backlog worker is free, the members from the first position beyond
     the ring's far end onward ask for full-res again — one boundary, a
     ring's length ahead of the cursor, so the full-res frames come back in
     one step. The persona's "the rung ring ahead is complete" is read as
     nothing of it still waiting to start: during a fast hold the members
     nearest the far end entered the ring a key period or two before, and a
     rung decode takes several key periods, so a rule that waited for their
     rungs to land could never step up on a wide viewport (Manager ruling
     2026-09-26, brief 008 Q-H). Its "the decoders are idle" is read as
     spare capacity: a decode is in flight at nearly every instant of a
     hold, so a rule that waited for none would never step up.
  3. **No pumping.** When a step-down's boundary falls less than one ring
     (`RING_AHEAD` frames) beyond the last step-up's boundary, the hold stays
     on the fit-box rung until it ends — a stop, or keys slower than four a
     second (`TRANSIT_GAP`).

  A reversal starts the rule afresh. The focused frame's own work outranks
  every ring member's, as everywhere. The rule's binding form — distances
  counted from 1, the time-to-screen ending at `note_adopted` whether the
  ring held the texture or not and never measured for a culled fill or
  without a box, "complete" read as nothing of the ring waiting to start,
  "idle" read as spare capacity, the lock counted between boundaries in view
  positions, the positions beyond the cache's clamp asking for nothing, the
  focused frame and the members behind asking for the fit box during a hold,
  re-planned whatever the cache holds — is the senior developer's, agreed by
  the Manager (brief 008, 2026-09-26; its Q-H and Q-K the same day).
- **Keeping up is measured when the frame is ready to draw** (brief 008, the
  redesign's G1): the time-to-screen includes the kitchen's 149 MB copy and
  the UI thread's adoption, so a kitchen that cannot fill a full-res frame
  per key steps the hold down even while the decoders keep up — its queue
  grows, and so does the time.
- **The GPU upload is outside the switch rule** (Manager ruling 2026-09-26, narrowing brief 008 G1): the shipping femtovg renderer uploads a texture at the frame's
  first draw, inside Slint's render pass on the UI thread, after the frame was
  ready to draw, and an upload that cannot keep up with the key costs
  displayed frames, not time-to-screen, which no single latency shows. Nor is
  it in any automated test: the suite renders in software (test-harness.md,
  `--screenshot`), which uploads nothing and offers no rendering notifier. So
  on a display that cannot take a full-res frame per key, a hold at 1:1 whose
  frames are full-res drops displayed frames while the switch rule keeps
  asking for full-res — the residual, on the machines where the decoders keep
  up. The user's Windows test of the CI build is the only check on it (brief
  008); the lever if it bites is the 1:1 crop upload (issue #60 part 4), never
  a slower hold.
- Before the session's first full-res fill completes at the loupe the
  time-to-screen is unknown, and the members ask for full-res: a hold at
  1:1 entered before that commits the full-res decodes the backlog workers
  start until then — one or two per worker, since that fill completes a
  kitchen job after the first decode lands and the workers start their
  second decodes at that landing — before the rule can judge them, and the
  frames the cursor meets meanwhile show the best rung in hand, cued — the
  residual, accepted. A fill the app culls because the cursor has passed its
  frame measures nothing, so on a machine whose kitchen, not its decoders,
  falls behind, the rule learns the delay from the fills that were cooked,
  later than it would from every fill; and a report for an older fill of an
  index can end a newer decode's measurement of it — one measurement lost or
  taken early, never a stale one (Manager ruling 2026-09-26, brief 008 Q-K).

### Orientation (user requirement 2026-07-25)

Embedded previews are stored in sensor orientation; the EXIF Orientation
tag (IFD0 0x0112) says how to display them. FastCull soft-rotates, like
Photo Mechanic: the walker extracts the tag, and it is applied to the
DECODED PIXELS of every rung — thumb, mid, screen rung, full-res — before
display. RAW files and sidecars are never modified. All 8 values
(rotations and mirrored forms) are handled. The thumb cache stores
post-rotation pixels (the schema bump when this landed invalidated older
thumbs wholesale).

The rotate is a hot loop and is engineered as one (`raw/orient.rs`; issue
#27, 2026-08-02; every constant pinned by a measured sweep on real
8640×5760 pixels, recorded in the module header). Mirrors and 180°
(orientations 2-4) run IN PLACE — no second 149 MB buffer. Transposes (5-8)
walk 64 px cache tiles under scoped threads capped at 8, with
bounds-check-free writes: 236 → 28-31 ms on the development laptop (4 cores
/ 8 threads), byte-identical to a reference implementation for all 8
orientations at sizes exercising partial tiles and partial thread bands. An
`unsafe` pointer kernel measured 25 ms and was REJECTED: ~4 ms is not worth
the crate's first `unsafe` block.

**The parallel threshold does not move for the screen rung** (Manager
ruling 2026-09-26, brief 008): the transpose fans out only above
`PARALLEL_THRESHOLD_BYTES` (32 MiB), the bound that keeps mid-class images
and portrait thumbs single-threaded. A portrait A1 on a 4K viewport takes
the 2/8 rung (8.9 MiB), under the threshold; the only rotated 3/8 rung is a
portrait frame on a 5K viewport (20 MiB), also under it, where a lower
threshold would buy under a tenth of the decode on a seat nobody on the
project has. A 5K seat is the trigger to revisit it, measured as the 3/8 +
orientation-8 bench at 32 and at 16 MiB.

The full-res decode path (`loupe::decode_oriented` — THE hot path, public
so `perf_budgets` measures the shipped code) pays its page faults off the
critical path: the A1 full-res JPEG is baseline with ZERO restart markers,
so its Huffman decode is strictly serial and the other cores idle while it
runs. The decoder writes into a pre-faulted buffer the caller owns
(libjpeg-turbo's `Decompressor::decompress`, pitch 3 × width — the packed
RGB the kitchen's texture fill takes; zune-jpeg's `decode_into` saved
~30 ms over its own allocation the same way), and the transpose's output
buffer is allocated and pre-faulted on a spare thread DURING the decode
(`raw::Scratch`). Peak memory is unchanged — the same two buffers exist
either way; only WHEN their faults are paid moves. Measured end to end on
the budget test: 518 ms untouched → 277 ms (2026-08-02, zune-jpeg), inside
the 350 ms budget with headroom on the very laptop where issue #27 declared
it unpassable. The screen rung's scaled decode (`decode_scaled_oriented`)
shares the buffer and `Scratch` discipline. Buffer POOLING (288 ms) stays
excluded: a decoder per physical core × 149 MB of resident pool is a memory
decision this does not need.

### Hostile-input bounds (issue #31, 2026-08-02)

Decode buffers are sized from HEADER claims before one byte of scan data
is validated, and in a crafted file every claim is attacker-controlled, so
both sides of the decode are capped and stream completeness is checked
before allocation:

- **Input**: `MAX_EMBEDDED_JPEG_LEN` (256 MB, `raw/mod.rs`) caps what
  `read_jpeg` will allocate for a declared payload length.
- **Output**: `MAX_DECODED_PIXELS` (500,000,000, `raw/mod.rs`) caps what SOF
  dimensions may size — checked on the loupe path right after the header read
  (libjpeg-turbo's `tj3DecompressHeader`, which parses markers and allocates
  no image buffer), on the header's FULL dimensions before any scaling factor
  or decoder is chosen — so it binds the zune-jpeg route for CMYK and YCCK
  streams too — and before the decode buffer, the prefault pass or the
  transpose scratch exist. 500 MP is ~10× the A1's 49.8 MP and ~3× the
  largest shipping sensor, with room for stitched panoramas served as bare
  JPEGs; the JPEG format ceiling (65535×65535) would commit ~12.9 GB of RGB
  per buffer, and a sub-KB stream claiming 30000×30000 measured 5.29 GB RSS
  on the pre-fix path. The thumb/mid decode keeps zune's default
  16384-per-side limit (268 MP, already stricter); the pixel cap lives on
  the loupe path, the only one that lifts the per-side limits (it must accept
  panorama-wide bare JPEGs).
- **Truncation, the byte check** (`raw/jpeg.rs::scan_is_terminated`): inside
  entropy-coded data every 0xFF is either stuffed (FF 00) or a real marker,
  so a genuine FF D9 at or after the first SOS is an EOI. The search runs
  backwards from the tail — intact camera files end with EOI, so the hot
  path pays effectively nothing — and pre-SOS APP1 segments (EXIF
  thumbnails are whole JPEGs) never vouch for the main scan. Applied in the
  grid-thumb decode and on the loupe path (below). zune-jpeg 0.4 — the grid
  thumb, and the loupe's CMYK and YCCK route — zero-fills missing scan data,
  reports a truncated stream as SUCCESS and exposes no bytes-consumed
  accessor, so on those paths the byte check is the only guard.
- **Truncation on the loupe path**: two guards, in this order. The byte check
  runs after the header read and the pixel cap, before the decoder is chosen —
  so the CMYK and YCCK route gets it as well — and before any buffer is sized
  or any scan byte is decoded, so a hostile claim that is also cut short is
  named for its size (Manager ruling Q10, 2026-09-26); it spares the grey
  decode of the commonest field corruption, a cut-off copy, and names the
  cause ("truncated"), which the decoder's own message does not. Past it,
  libjpeg-turbo fails a short stream by its own return contract: its memory
  source inserts a fake EOI when the bytes run out (`JWRN_JPEG_EOF`,
  "Premature end of JPEG file"), its Huffman decoder warns on meeting a marker
  with data still to decode (`JWRN_HIT_MARKER`, "Corrupt JPEG data: premature
  end of data segment"), `tj3Decompress8` returns −1 whenever a decode emitted
  any warning, and the safe `turbojpeg` crate maps that to `Err` — a `Failed`
  badge over the grey-bottomed buffer, never a blank success (brief 008 R2).
  `TJPARAM_STOPONWARNING` and `TJPARAM_MAXPIXELS` are not set: the safe crate
  keeps its handle private, and setting them would take a raw-FFI decompressor
  in core to abort a crafted stream a little sooner and to duplicate our own
  pixel cap; the loupe uses the safe `Decompressor` as published (Manager
  ruling 2026-09-26). The scope, by design ("warnings are errors on the
  decode", brief 008 R2): a stream whose only warning is benign — another
  body's `JWRN_EXTRANEOUS_DATA`-class warning — fails on the loupe path; the
  A1's streams are warning-free. Two symptoms follow. A bare JPEG, whose
  stream is its only rung, shows the `Failed` badge in the loupe, and its
  zune-jpeg thumb shows only where zune tolerates the same fault — its strict
  mode refuses bytes between two header markers as libjpeg-turbo does. A RAW
  whose mid decodes but whose full JPEG carries the warning shows no badge:
  its full-res fails, and so does its screen rung, a scaled decode of the
  same stream, so the frame stays on its mid for the session — at 1:1, and
  at fit on a wide viewport — cued, and its one sign is the stderr line of a
  rung that fails over a good lower one (below; brief 008, the step-1
  review, Manager ruling 2026-09-26).
  (PENDING brief 008, other cameras: ruled 2026-09-26 into step 2 — a benign warning uses the buffer the library completed and prints one stderr line, and the truncation class stays fatal — so this scope, its two symptoms and docs/faq.md's Failed-badge answer are rewritten by the spec pass that precedes step 2's code.)
- **The scaled decode refuses a numerator outside 1..=8**: 9/8 and above would
  UPSCALE, which no rung may do (developer 2026-09-26, brief 008 step 1).
- **Residual, accepted — on the zune-jpeg paths only** (the grid thumb, and
  the loupe's CMYK and YCCK route): a crafted stream carrying plausible
  dimensions, a valid EOI and too little entropy data still decodes there as a
  mostly-blank "success" — detecting that needs decoder cooperation neither
  zune 0.4 nor 0.5 offers, while libjpeg-turbo warns on it and fails it
  (above); and in a MULTI-SCAN (progressive) stream the table segments between
  scans may legitimately contain a literal FF D9, so a truncated progressive
  stream can pass the byte check. Both are bounded blank successes, never a
  giant allocation. (0.5.15's strict mode rejects the plain no-EOI truncation
  but not these, and is the regression "The decoder", above, keeps out.)
- **Progressive scans: at most 100** (brief 008 R2; Manager ruling
  2026-09-26): the loupe's libjpeg-turbo decode sets
  `Decompressor::set_scan_limit(100)`, the bound zune-jpeg 0.4's default
  gave the whole loupe path before brief 008 and still gives the grid thumb
  and the loupe's CMYK and YCCK route — each scan of a progressive stream is
  a pass over every block of the components it covers, so a small crafted
  stream with thousands of scans would hold a decoder far longer than any
  real file (the exploit libjpeg-turbo documents for `TJPARAM_SCANLIMIT`,
  whose default is no limit). A stream over the limit is `Failed` with the
  library's message. A crafted stream reaches the loupe as a bare JPEG
  (issue #8) or inside a crafted RAW; the A1's embedded JPEGs are baseline,
  one scan each.

All rejections flow through the existing `LoupeEvent::Failed` /
`SessionEvent::Failed`, so the UI shows the Failed badge (ui-grid.md) and
subsequent jobs are unaffected — except a HIGHER rung's: a rung that fails
while the frame has a good lower rung, decoded in the same flight or already
cached, fails nothing. The ladder keeps the lower rung and memoizes it as the
highest rung it climbs for that file this session, so it never retries — an
engine-internal memo, never published as `terminal`, so the frame stays cued
wherever that rung does not serve — and prints one line on stderr, the
diagnostics channel, naming the file, the rung that failed and the decoder's
reason, so a fault that shows no badge is still seen (brief 008, the step-1
review; Manager ruling 2026-09-26).

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

- Three levels: `Visible` > `Prefetch` (a level `promote` offers; the app
  promotes only visible cells) > `Background` (sequential file order:
  cold-cache and card-reader friendly). The loupe's ring is the loupe
  engine's, on its own workers (The ring), never this pool's.
- Scroll and zoom call `set_visible(range)`; already-queued jobs are
  reprioritized, not re-enqueued. In-flight jobs are never cancelled
  mid-decode (they are ≤ 150 ms). Duplicate requests for the same (image,
  asset) coalesce.

### Memory (user decisions 2026-09-26)

- **The pixel cache** — the loupe engine's byte-budget LRU of decoded rungs,
  mids, screen rungs and full-res frames alike — is a quarter of the
  machine's TOTAL RAM, never below 2 GiB and never above 10 GiB: `cache =
  clamp(total ÷ 4, 2 GiB, 10 GiB)`. The user's words: *"The memory cache can
  grow more than that. Would a 10G limit help to have more textures live so
  it would be faster to move between images?"* Total RAM is the total the OS
  reports — `/proc/meminfo` `MemTotal` on Linux, `GlobalMemoryStatusEx`'s
  `ullTotalPhys` on Windows, read through core's one `unsafe` block, a
  `#[cfg(windows)]` call over `windows-sys` with its SAFETY comment (Manager
  ruling 2026-09-26; `sysinfo` refused, a large dependency on every seat for
  one number) — not the free figure, and it is read once, at startup: the
  cache never changes during a session. An unreadable, zero or absurd (over
  16 TiB) total is read as 8 GiB, the machine the 2 GiB floor already
  assumes — the cache's floor, and 4 decoders under the RAM cap (The decode
  workers) — said once on stderr (Manager ruling 2026-09-26, brief 008 Q-C).
  There is no setting and no
  override (#39 parked; the persona: "a toggle you can't see in the UI is
  worse than none" — if a setting ever ships it is ONE number, the memory
  FastCull may use).
- **The startup line**: the app prints once on stderr at startup, in the
  read pool's voice, `fastcull: loupe cache …` — the cache and where it came
  from (a quarter of the total, the floor, the cap, or the unreadable
  fallback), the ring (2 behind / 15 ahead, and the full-res ring ahead as
  the cache clamps it), the decoders and where they came from (physical
  cores, the RAM cap, `FASTCULL_DECODERS`, or the fallback), and the
  whole-app worst-case peak below for A1 frames on a 4K screen, "plus
  ~0.2 GB per 1,000 thumbnails" (brief 008, the redesign's G3).
- **Outside the cache**, bounded by the rings: the app's texture copies — the
  full-res ring's (up to 18 × 149 MB of A1 frames at 1:1, as many as the
  cache's clamp leaves in the ring), the screen-rung
  ring's (18 × 21 MB on a 4K viewport) and the mids (`MIDS_CAP` = 64, ~5 MB
  each) — and the kitchen's full-res fill in flight, one 149 MB frame at a
  time (one more for each finished fill the UI thread has not yet taken,
  which piles up only while that thread is busy); the decoders' transient
  buffers — per decoder the embedded JPEG it read (`read_jpeg` holds the
  whole stream for the decode: 9.8 to 12.3 MB for the reference A1 files)
  and up to two decoded full-res frames (a portrait frame and its rotate
  scratch); and the thumbs, unbounded (≈ 200 KB each; 5,000
  images ≈ 1 GB — acceptable; the SQLite cache lets us evict and reload
  cheaply if this ever pinches; issue #2 is the residency-window request).
- **The whole-app worst-case peak** — A1 frames, a 4K screen, a long session
  at 1:1 with the cache full, every decoder rotating a portrait frame and the
  kitchen filling a full-res texture, before thumbnails and before what the
  app holds with no photo open (its code, the interface toolkit, the graphics
  driver) — is the cache + the full-res ring's frames × 149,299,200 B + 18 ×
  20,995,200 B + 64 × 5,235,840 B + the decoders × (2 × 149,299,200 B +
  12,313,510 B) + 149,299,200 B for the kitchen's fill, where 12,313,510 B is
  the largest embedded full JPEG of the three reference A1 files, the input
  each decoder holds. It follows the RAM and the physical cores, under the two
  clamps — the full-res ring's texture copies and the kitchen's fill counted
  against the cache (Above fit) and the decoders capped at half the RAM in GiB
  (The decode workers). The decoders' buffers are 1.2, 2.3 and 4.6 GiB at 4, 8
  and 16 decoders:

  | total RAM | cache | full-res ring at 1:1 | texture copies, the fill included | decoders at 4 / 8 / 16 cores | peak at 4 / 8 / 16 cores |
  |---|---|---|---|---|---|
  | 8 GiB | 2 GiB (the floor) | 2 behind / 3 ahead | 1.6 GiB | 4 / 4 / 4 | 4.8 / 4.8 / 4.8 GiB (59.9 / 59.9 / 59.9 %) |
  | 16 GiB | 4 GiB | 2 / 10 | 2.6 GiB | 4 / 8 / 8 | 7.8 / 8.9 / 8.9 GiB (48.6 / 55.8 / 55.8 %) |
  | 32 GiB | 8 GiB | 2 / 15 | 3.3 GiB | 4 / 8 / 16 | 12.5 / 13.6 / 15.9 GiB (39.0 / 42.6 / 49.8 %) |
  | 64 GiB | 10 GiB (the cap) | 2 / 15 | 3.3 GiB | 4 / 8 / 16 | 14.5 / 15.6 / 17.9 GiB (22.6 / 24.4 / 28.0 %) |

  Every row is within 60 % of the total — the 8 GiB row at 59.9 %, inside the
  line by less than the formula's own precision, which counts no decoder's
  working rows and no allocator overhead (Manager ruling 2026-09-26, brief 008
  Q-G). At fit the full-res ring is not asked for (the idle cook's cursor
  frame aside), so the peak there is lower by most of the full-res copies. The
  rows are nominal sizes, and the app reads the total the OS reports, which is
  lower: a 32 GB machine reports 31 to 32 GiB, where a 16-core machine runs 15
  decoders, not 16 (The decode workers), and an 8 GB machine a little under 8
  GiB, so it runs 3 decoders, not the row's 4. Below a reported 8 GiB the
  cache's 2 GiB floor and the 3 decoders fix the peak at 4.5 GiB whatever the
  cores (4.0 GiB before brief 008, counted the same way), which is 60 % of a
  reported 7.51 GiB: an 8 GB machine that reports more stays within 60 %, and
  one that reports less — its graphics or its kernel keeping a larger share —
  does not: 60.1 % at a reported 7.5 GiB, 62.6 % at 7.2 GiB, 64.4 % at 7.0
  GiB, and past the whole RAM from a reported 4.5 GiB down (113 % of 4 GiB,
  where it was 100 %). Above a reported 7.51 GiB every total is within 60 % at
  any core count but in a band between the 8 and 12 GB classes that no common
  RAM size reports, 8.34 to 8.38 GiB, where a fourth frame ahead joins the
  ring and the peak reaches 60.2 %. The formula does not count what glibc's
  allocator keeps of freed decode buffers on Linux (Windows returns them;
  brief 008's decisions log)
  (PENDING brief 008 Q-J and the Linux allocator term: machines that report under 8 GiB, and whether that term is counted, bounded or released — both ruled, and this bullet and the figures docs/faq.md's memory answer and docs/culling.md's "At 1:1" paragraph take from it amended, before step 3 implements it).
  The relief is the runtime shrink below.
- Not in this unit (a later brief): shrinking the cache at runtime when the
  machine runs short — poll the free memory, shrink only, never grow back
  mid-session, and never take the cursor's rungs, the ring's far end first
  (brief 008, the first persona gate's G9) — and the thumbnail cap.

## Contracts

- `LoupeEngine`: `start(paths, cache_bytes)` — three decoders whatever the
  machine, the engine the older core tests run on every seat — and
  `start_with(paths, cache_bytes, decoders)`, the app's form, the count from
  `budget.rs`; `focus(index, display_long)` (the app's real target: above fit
  `u32::MAX`, the top rung — what a hold actually asks for the focused frame
  is the engine's, Above fit) and `focus_fit(index)` (the fit box);
  `want(range, cell_width)`; `set_view(order)`; `set_fit_box(box)` (the N=1
  cell in physical pixels; none before the first layout and whenever the app
  is not at the loupe — review-verified: the app's call is unconditional at
  every refresh); `texture_windows()` (the leaned windows of the app's two
  texture rings, ui-grid.md); `note_adopted(index, kind, held)` (the app's
  report that a fill it made for that index completed at the loupe, with
  whether its ring kept it — where the switch rule's time-to-screen ends,
  held or not) and `note_dropped(index)` (the app's report that it culled
  that index's queued full-res fill, which ends that decode's measurement
  unmeasured) (Manager ruling 2026-09-26, brief 008 Q-K); deferred revival
  is internal.
  Events: `Ready` — the image with its `RungKind` (`Mid`, `Screen`, `Full`),
  the `terminal` flag and the `RequestState` (`Transit`, `Settled`) — and
  `Failed`. Constants: `RING_BEHIND = 2`, `RING_AHEAD = 15`, `PREFETCH = 2`
  (the settled ring of an engine with no fit box), `FOCUS_DEBOUNCE` (~250 ms),
  `MID_RUNG_MAX_LONG = 2048`, `MID_RUNG_TARGET = 1616` (the transit request of
  an engine with no fit box), `UPSCALE_THRESHOLD = 1.25`,
  `DEFAULT_BUDGET_BYTES` (2 GiB, the cache's floor).
- The pure rules in `loupe.rs`, each table-tested: `serves_box` (the 1.25
  rule for a box — its one home; the app asks it too), `mid_serves_box` (the
  "wide viewport" predicate), `fits_box`, `rung_factor` (the box rule),
  `fit_rung` (mid, screen rung or full for one frame at fit) and
  `fullres_ring_ahead` (the full-res ring's cache clamp, which the startup
  line prints too); `scaled_dims` is the decoder's own ceiling division.
- `loupe::decode_oriented(bytes, orientation)` is the perf-budget target
  (full scale, then the soft-rotate); `loupe::decode_scaled_oriented(bytes,
  orientation, numerator)` is its N/8 sibling, numerator 1..=8; `raw/mod.rs`
  holds `MAX_EMBEDDED_JPEG_LEN`, `MAX_DECODED_PIXELS` and
  `GRID_SOURCE_MAX_PIXELS`.
- `budget.rs`: the pixel cache from total RAM, the decoder count from
  physical cores, total RAM and `FASTCULL_DECODERS`, the machine probe, and
  the startup line, whose `fastcull: loupe cache ` prefix tests read.
- `ExifSummary` (`exif.rs`): make, model, serial, capture time, subsec, the
  Sony sequence number; `sort_key()` normalizes subseconds to three digits.
- The budget rows of 01-architecture.md bind this module — open+EXIF, the
  grid thumb, the full-res decode+rotate, the landscape full-res decode (the
  SIMD canary), the two 4K screen-rung rows (a change of kind) and the
  pipeline throughput; their thresholds live there.
- Trace marks (test-harness.md): `thumb bytes idx N` (the pipeline read the
  embedded JPEG), `thumb landed idx N` (the kitchen decoded it), `loupe
  ready idx N long L kind K state S`; the read pool's stderr line; the
  startup line.
- The request states — TRANSIT, SETTLED and SETTLED-AND-IDLE, when each
  applies and the user requirement behind them — are ui-grid.md's ("Transit
  and settled"); what each state asks of this engine for every position of the
  ring is this spec's, once (The ring, its request table; Above fit), and so
  is the settle guarantee, the reserved lane's.

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

Brief 008 (the screen rung, issue #60; every box below is ticked by the
commit that lands its tests, and stays open until then):

- [ ] **The ring plan** (brief 008 A1): clock-free over the engine's plan —
      forward → 2 behind / 15 ahead; a reversal re-leans on the very next
      call; the edges clamp; a folder shorter than the ring → the whole
      folder; on a 3840×2160 box every member asks for the fit box, travelling
      and settled, both leans; above fit, full-res clamped by the cache with
      each frame counted twice, pixels and texture copy, and the kitchen's
      fill once (3 ahead on a 2 GiB cache, 10 on 4 GiB, 15 on 8 GiB — red
      under the clamp that left out the fill, which gives 4, 11 and 15, and
      under the one that counted the pixels alone, 11, 15 and 15), the
      positions beyond the clamp asking for nothing, settled and during a
      hold; an engine with no fit box keeps the mid in transit and ±`PREFETCH`
      settled. Red on the old 2 / 8 transit shape (the first test's depths)
      and on the old settled ±2 with a box (the second's settled rows) —
      `transit_ring_leans_in_the_direction_of_travel` (its depths move from
      `TRANSIT_BEHIND`/`TRANSIT_AHEAD` to `RING_BEHIND`/`RING_AHEAD`, the
      promise kept; its engine has no box, so its settled row stays
      ±`PREFETCH`), `the_ring_at_fit_asks_the_fit_box_travelling_and_settled`,
      `the_full_res_ring_is_clamped_by_the_cache`. Open: lands with the ring.
- [ ] **The queue order and the cull** (brief 008): at equal distance the
      member in the travel direction is popped first, both ways; a focus
      drops the queued focus-origin entries outside the ring in force and
      leaves grid entries and in-flight decodes alone —
      `ring_ties_break_toward_the_travel_direction`,
      `a_focus_culls_queued_entries_outside_the_ring_in_force`. Open: lands
      with the ring.
- [ ] **The rung factor follows the viewport and the frame** (brief 008
      A2): clock-free, the box rule over (fit box, frame, mid, orientation)
      — 3840×2160 → 3/8 landscape, 2/8 portrait; 2560×1440 → 2/8 landscape,
      the mid portrait; 1920×1080 → the mid; 5120×2880 → 4/8 landscape, 3/8
      portrait; a box between two factors → the next one up; a frame
      already within the box × 1.25 → no rung; a bare JPEG → the same rule;
      the mid's ORIENTED size decides whether it serves —
      `rung_factor_follows_the_viewport_and_the_frame`, with `serves_box`,
      `fits_box` and `rung_factor` asserted on the same rows. Open: lands
      with the rung.
- [ ] **The rung's kind comes from the decode; the ladder stops on oriented
      sizes** (brief 008): a scaled decode is `screen` and never `terminal`,
      even when an IFD under-claims its stream; a portrait mid that serves the
      box stops the ladder; a truncated full at fit keeps the good mid with no
      Failed badge; a cached rung that no longer serves a grown box is
      re-requested; a lossless stream decodes full-scale through the scaled
      entry point, both orientation paths —
      `the_rung_kind_comes_from_the_decode_not_the_ifd_claim`,
      `a_portrait_mid_that_serves_the_box_stops_the_ladder`,
      `truncated_full_rung_keeps_the_good_mid_and_no_failed_badge` (gains a
      run at fit),
      `a_cached_rung_that_no_longer_serves_the_box_is_re_requested`,
      `a_lossless_stream_decodes_full_scale_through_the_scaled_entry_point`.
      Open: lands with the rung.
- [ ] **The pixel cache and the decoders follow the machine** (brief 008 A4):
      clock-free — the cache over 4 / 8 / 16 / 32 / 64 GiB of total RAM → 2,
      2, 4, 8, 10 GiB, and an unreadable, zero or absurd total → 2 GiB; the
      decoders over 2 / 4 / 16 / 32 physical cores on 64 GiB → 3, 4, 16, 16, a
      zero or missing count → 4; the RAM cap on 16 cores → 3 at 6 GiB, 4 at 8
      GiB, 8 at 16 GiB, 15 at 31 GiB (what a 32 GB machine reports), 16 at 32
      GiB, and 4 with the total unreadable, zero or absurd (read as 8 GiB,
      Memory); `FASTCULL_DECODERS` wins, above both caps too and 1 read as 2,
      and a value that is not a positive integer is ignored with its stderr
      line; the startup line names the cache, the ring, the decoders, their
      sources and the peak, and the app prints it exactly once per run —
      asserted on the stderr of ui-grid.md A5's fit run; the engine spawns
      that many workers and reserves one lane —
      `the_pixel_cache_is_a_quarter_of_total_ram_between_2_and_10_gib`,
      `the_decoders_follow_the_physical_cores`,
      `the_decoders_are_capped_at_half_the_ram_in_gib`,
      `a_decoder_override_wins_and_a_bad_one_is_ignored`,
      `the_startup_line_names_the_cache_the_ring_the_decoders_and_the_peak`,
      `start_with_spawns_the_decoders_and_reserves_the_last`. Open: lands with
      the rules.
- [ ] **Old red first, the three mutants** (brief 008 A7): a transit capped
      at the mid is red clock-free — `transit_request_is_the_fit_box_and_never_the_full`
      handed a 3840×2160 box resolves to the mid class under it — and
      driven, where ui-grid.md A5's gate 2 reads 0; `revive_deferred` gating
      on ±`PREFETCH` is red on `revival_gates_on_the_ring_in_force` (+7
      revived and +16 dropped in transit and at fit, and the clamped edge
      above fit); a travel direction derived per call is red on
      `a_backward_hold_keeps_leaning_backward_across_refocus`, unchanged
      (the ring behind is 2, as before). Each mutant in a scratch worktree,
      its red named in the commit. Open: lands with the ring.
- [x] **Hostile inputs on the new path** (brief 008 A8): the existing tests
      pass on libjpeg-turbo; the 30000×30000 SOF is refused before any
      allocation through the scaled decode too, on both orientation paths;
      the cut-before-EOI stream is `Failed` through both entry points with a
      reason that says "truncated"; the short-scan stream with a valid EOI
      is `Failed` on the loupe path with the decoder's own message; a
      numerator outside 1..=8 is refused —
      `decode_oriented_rejects_implausible_header_dimensions`,
      `decode_oriented_rejects_a_truncated_scan` (both extended to the
      scaled entry point), `a_short_scan_with_a_valid_eoi_fails_on_the_loupe_path`,
      `decode_scaled_oriented_refuses_a_numerator_outside_1_to_8`,
      `scaled_dims_is_the_decoders_ceiling_division`; the mutants are of our
      code — the byte check deleted, the decoder's `Err` swallowed, the cap
      read on the scaled size — never of a library parameter. Ticked by
      the step-1 commit, which carries these tests; each mutant's red is in
      its message.
- [ ] **A progressive stream over 100 scans is refused on the loupe path**
      (brief 008 R2): a valid 101-scan progressive stream is `Failed`
      through both entry points and a 100-scan one decodes; red with the
      limit unset — `a_progressive_stream_over_100_scans_fails_on_the_loupe_path`.
      Open: lands with the limit, in the step-1 fix round.
- [ ] **CMYK and YCCK open in the loupe** (brief 008 A14): a CMYK and a YCCK
      bare JPEG decode through the loupe path with pixels, at full scale with
      no rung, never a Failed badge; red on the decoder swap without the
      zune-jpeg route. The route keeps the bounds (brief 008 R2): a CMYK
      stream cut before EOI is `Failed` with a reason that says "truncated",
      and a CMYK header claiming 30000×30000 is refused as "implausible"
      before any allocation; red with the byte check or the pixel cap moved
      after the route — `cmyk_and_ycck_streams_decode_on_the_loupe_path`.
      Open: lands with the route.
- [ ] **A rung that fails over a good lower one is named on stderr** (brief
      008, the step-1 review; Manager ruling 2026-09-26): a higher rung
      whose decode fails while a lower one is in hand — decoded in the same
      flight or already cached — leaves the lower rung shown, emits no
      `Failed` and prints one stderr line naming the file, the rung that
      failed and the decoder's reason; a ladder that climbs cleanly prints
      nothing; red with the line removed —
      `a_rung_that_fails_over_a_good_lower_one_is_named_on_stderr`. Open:
      the line lands in step 2.
- [ ] **Perf budgets** (brief 008 A9): the full-res row stays green with
      more headroom, and the three new rows are green on the idle
      development laptop — `budget_fullres_decode_under_350ms`,
      `budget_fullres_landscape_decode_under_280ms`,
      `budget_screen_rung_3_8_landscape_under_the_kind_guard`,
      `budget_screen_rung_2_8_portrait_under_the_kind_guard` (their
      thresholds, `RUNG_ROW_MS` among them, are 01-architecture.md's);
      `tests/zoom_walk.rs`, the mandatory zoom-quality gate above,
      passes in release against the real A1 files on the final tree. Open:
      ticked on the final tree's release run.
- [ ] **The idle cook** (brief 008): settled at fit on a 4K box with the
      cursor's screen rung in hand, the reserved lane's next job is the
      cursor's full-res; none on a box the reference mid serves (a 3/8 rung
      cached on a 1920×1080 box included), none while moving, none in
      flight, none without a box, none once the file's best is cached —
      `the_reserved_lane_cooks_the_cursors_full_at_fit_on_a_wide_viewport`.
      Open: lands with the rings.
- [ ] **The request state travels with the decode** (brief 008): a transit
      focus replaces a queued entry's state; a merge changes it only when
      the target grows; a revived entry keeps the deferred state —
      `the_request_state_travels_with_the_decode`. Open: lands with the
      rung.
- [ ] **The hold above fit** (brief 008 A13): engine-level and clock-free, the
      time-to-screen, the key period and the workers' state handed in —
      decoders that keep up leave every member ahead full-res; a member that
      cannot land before the cursor steps it and every member beyond it to the
      fit box when its decode would start, its distance counted from 1, a
      full-res entry queued beyond the boundary becoming a fit-box one and one
      in flight landing; the step up waits for every member ahead but the
      farthest to hold its rung or have it in flight, nothing queued and a
      backlog worker free, and starts beyond the ring's far end; the
      time-to-screen runs from a decode's start to the app's report that its
      fill completed, held by the ring or at once that ring's victim, a frame
      the cursor has passed included, while a culled fill or the box going
      ends it unmeasured and a decode published with no box starts no
      measurement; a step-down less than one ring past the last step-up holds
      the rung until the hold ends; a reversal starts afresh; during a hold
      the focused frame and the members behind ask for the fit box — an
      in-flight full-res kept, a queued one replaced, or dropped when the
      fit-box rung is in hand — and the settle then asks for the top rung; a
      deferred full-res target at or behind the cursor revives at the fit box;
      the positions beyond the cache's clamp ask for nothing —
      `the_switch_rule_steps_down_before_a_frame_it_cannot_land`,
      `the_switch_rule_steps_up_only_from_a_complete_ring_with_a_free_decoder`,
      `a_quick_second_step_down_holds_the_rung_until_the_hold_ends`,
      `a_hold_above_fit_asks_the_fit_box_for_the_focused_frame` (with a row
      whose fit-box rung is cached while its full-res is queued — the early
      return a request the cache serves takes),
      `revival_gates_on_the_ring_in_force` (its hold rows),
      `the_full_res_ring_is_clamped_by_the_cache` (its hold rows),
      `note_adopted_measures_a_full_res_frame_from_its_decode_start`. And a
      simulated 800-focus hold at 1:1 over the engine's own plan and queue,
      pops and landings interleaved, from a rest whose members hold their
      fit-box rungs from an earlier pass at fit and have their full-res
      queued, starts no full-res decode for the frame the cursor is on or one
      it has passed (the 2026-08-01 finding, ui-grid.md History), and fails
      unless it measured a time-to-screen and stepped down at least once, so a
      simulation that stopped measuring cannot pass —
      `a_hold_above_fit_never_starts_a_full_res_decode_the_cursor_has_reached`.
      The mutants, each red on its row: the focused frame asking for the top
      rung during a hold; the re-plan skipped when the fit-box rung is cached
      (red on the cached row and on the simulation); distances counted from 0;
      the revival at the stored target; a step-up that waits for nothing in
      flight; one that ignores the free worker; one that counts a member whose
      rung is in flight as missing (red on the in-flight row); the lock
      removed (red on
      `a_quick_second_step_down_holds_the_rung_until_the_hold_ends`); the
      positions beyond the clamp asking for the fit box (red on the clamp
      test's hold rows); a ring's victim measuring nothing, `note_dropped`
      doing nothing, and the measurements culled when their frame leaves the
      ring (red on the passed-frame row, and on the simulation, which then
      never measures). Driven, the hold's frames on screen per key stay at
      ui-grid.md A6's level in two 1:1 runs of
      `a_held_arrow_at_fit_on_4k_stays_at_the_rung_and_never_slows`: A6's own,
      on the seat's decoders, and one with `FASTCULL_DECODERS=2`, whose one
      backlog decoder falls behind the key on every seat on record
      (01-architecture.md's perf table); how many times each hold switches
      between full-res and the rung is a number for humans in brief 008's
      Outcome. What the user sees on the desktop — one step or a flicker — is
      the user's own test of the CI build (brief 008). Open: lands with the
      switch rule.
- [ ] **The settled ring after a hold** (brief 008, Manager ruling Q-I):
      clock-free — a stop above fit on a frame whose full-res is in hand, its
      ring members holding only their fit-box rungs, has the reserved lane
      queue the settled ring, full-res for every member in the ring's order,
      and ask nothing on its next wake; a stop on a frame that still needs
      its climb queues the climb and no member; a settled focus of the app's
      own leaves the lane nothing to ask; an engine with no fit box asks
      nothing — `a_settle_with_nothing_to_climb_asks_for_the_settled_ring`.
      Mutants: the lane's ask removed; the ring asked beside the climb; the
      once-per-settle guard removed (red on the second wake). Open: lands
      with the switch rule.
- [ ] **The RSS ceiling** (brief 008 A12): release, Linux only (symlinks, and
      `VmHWM` from `/proc/self/status`): an engine walk over 5,000 symlinks to
      the three A1 files at the seat's own cache (the cache rule over the
      seat's total RAM), holding and stopping at fit on a 3840×2160 box (the
      3/8 rung) and at 1:1 — each phase decoding at least 1.5 × the cache's
      worth of distinct frames at its rung before its reading, so the cache
      has filled and evicted — keeps `VmHWM` ≤ the cache + the decoders × 2 ×
      149,299,200 B + 200 MB (the engine alone, no textures), read during the
      walk as well as at its end; skipped, with the reason printed, when
      available RAM is under the cache + 2 GiB —
      `the_engine_walk_holds_the_rss_ceiling`. Open: lands with the cache
      rule.
- [ ] **Hard rule 1** (brief 008 A11): the RAW-write tests are unchanged and
      green; QE records `sha256sum testdata/raws/*.ARW` before and after its
      runs, and the listings match. Open: QE's rounds.

## History

- 2026-09-26 — The screen rung (brief 008, issue #60; ADR 0005): a held arrow
  at fit on a 4K viewport went soft after two or three frames, because transit
  asked the decoder for the 1616 px mid only and the fit cell showed it 2×
  upscaled with no cue. The loupe now decodes with libjpeg-turbo, fit asks for
  the fit box — decoded at N/8 on a wide viewport — and one fixed ring of
  2 behind / 15 ahead serves every factor, where the settled ring was ±2
  and the transit ring 2 behind / 8 ahead, with full-res above fit under
  the switch rule; the decoders, three until then, follow the physical cores;
  and the pixel cache, 2 GiB until then, is a quarter of total RAM between
  2 and 10 GiB — so the whole-app worst case, 3.8 GiB on every machine
  before (4.0 counted as the table now counts it), follows the RAM and the
  cores (Memory). Corrected the same day, before any code implemented these
  rules (Manager ruling 2026-09-26, brief 008 Q4): the first form of this
  change clamped the full-res ring at `⌊cache ÷ 149,299,200⌋` frames,
  counting the decoded pixels alone, and capped the decoders at 16 only, so
  its own table put the worst case at 72 / 85 / 113 % of an 8 GiB machine
  with 4 / 8 / 16 cores and 73 % of a 16 GiB one with 16; the ring now
  counts each frame's texture copy against the cache and the decoders stop
  at half the RAM in GiB. The formula also gained two terms the first form
  left out — the kitchen's full-res fill in flight and each decoder's input
  JPEG — which put the 8 GiB row at 61.7 % under those two clamps (brief 008
  Q4, its outcome), so the ring's clamp counts that fill too (brief 008 Q-G),
  and the table now says what a machine's reported total does below the
  nominal rows (Memory). The plan's other questions were ruled the same day,
  each cited where it applies (brief 008 Q-C, Q-D, Q-H, Q-I, Q-K); and the
  step-1 review added the stderr line of a rung that fails over a good lower
  one — a RAW whose full JPEG carries a benign warning shows no badge — and
  the progressive scan limit was restored (Hostile-input bounds): the loupe
  lost it when it left zune-jpeg, whose default refuses more than 100 scans,
  where the step-1 review had taken the exposure for one zune-jpeg shared.
  The GPU upload of a full-res frame is outside what the switch rule and the
  suite can see — a recorded residual (Above fit). What each request state
  asks for every position of the
  ring moved here from ui-grid.md, its one home (Contracts). The benchmark
  behind the decision — the development laptop, 2026-09-26 — is in brief 008's
  Context and ADR 0005; the idle medians after the swap are
  01-architecture.md's perf table. The design was redrawn by the user the same
  day; the first cut — a memory budget derived from FREE memory with an
  eleven-row table, `FASTCULL_MEMORY_MB`, `fastcull-cli budget`, a 6 / 12
  transit ring and a 2 / 6 look-ahead, the cache capped at 2 GiB because "a
  bigger LRU stores nothing a hold can use" — never reached `main` (brief
  008's decisions log). Corrected in place: "the 8-core laptop" is the 4-core
  / 8-thread laptop (it counted threads); "turbojpeg DCT scaling is a recorded
  future optimization only (~35–45 % off the cook; the ladder already hides
  that latency)" is retired — it is the screen rung, and on a 4K viewport the
  ladder hid nothing; the priority queue's `Prefetch` level was never the
  loupe's ring, which runs on the loupe engine's own workers; "Full-res
  decodes: the engine's byte-budget LRU …; mid-rung textures count toward it"
  — the LRU counts the engine's decoded rungs, mids among them, and never the
  app's texture copies, which the kitchen makes with `clone_from_slice` and
  the app's rings bound (Memory); the lossless clause's "review-verified: no
  lossless fixture can be encoded here" — the `turbojpeg` crate's own
  compressor encodes one (senior-developer review F2); the benign-warning
  scope's "while its zune-jpeg thumb shows" — zune's strict mode refuses some
  of the same faults (measured for this amendment); and the decodes a hold
  commits before the first measurement, "about one per worker", which is one
  or two, since the first measurement ends at the first fill's completion
  (senior-developer plan, brief 008 Q-K).
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
  (2026-08-01, ui-grid.md). zune-jpeg 0.5.15 measured a regression on the
  full-size decode (267–279 ms against 0.4.21's 247–252), and the decoder
  stayed on 0.4.
- 2026-07-27 — The soft-transit contract (issue #21) and the reserved
  lane's debounced worker (`986b36f`, `53907bd`, v0.4.0); the lane's other
  two rules followed the QE and CI findings of the days after.
- 2026-07-27 — The in-tree EXIF walker replaces rawler on the hot path
  (the `mmap_lock` serialization; v0.4.0).
- 2026-07-26 — Bare JPEG sources (issue #8).
- 2026-07-25 — The loupe ladder, soft-rotation, the adaptive read pool and
  the full-res bypass (user decisions); the pool's design review.
- 2026-07-24 — M1 and ADR 0001.
