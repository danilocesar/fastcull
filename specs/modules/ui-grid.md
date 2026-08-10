# Module spec: grid & loupe UI (`fastcull-app` + `filter.rs`)

## Purpose

The one continuous view: a zoomable virtualized grid that morphs from many columns
to a single-image loupe with 1:1 pixel zoom. Plus the filter/sort bar, pick badges,
burst badges, and the IPTC side panel shell.

## Zoom model (one axis, seamless)

Zoom levels: column count `N ∈ {12, 8, 6, 4, 3, 2, 1}` (Ctrl+scroll / `+`/`-`
step through; pinch later). At `N = 1` the view is the **loupe**:
- First stop: fit-to-screen (full-res asset GPU-scaled — see the recorded
  FitPreview fold in raw-pipeline.md). **Fit means the WHOLE frame is on
  screen** — the requirement, not an aspiration: see *One-column cell
  bounding* below.
- Further zoom-in: the ×1.5 ladder below, capped at 1:1 (FullRes asset as GPU
  texture, panning with drag; arrows NAVIGATE at every zoom level — they are
  never repurposed for panning, the burst focus-check loop depends on it).
- Zooming out from loupe returns to the grid **centered on the current image**.

### One-column cell bounding (bug found 2026-07-30, user-approved fix)

The loupe IS the grid at one column, so the fit view is an `N = 1` grid
cell. Cells are 3:2 (`CELL_ASPECT`) and span the grid width, which makes the
one-column cell TALLER than the viewport on any window wider than 1.5× the
grid area's height — i.e. every normal window. `scroll_to_reveal` top-aligns
a cell it cannot fit, so the bottom of every frame sat below the fold:
**measured 16.6 % hidden on a 1440×900 window, 23.4 % fullscreen on 1080p**,
with nothing on screen to say so. The shipped `docs/assets/fastcull-loupe.jpg`
shows it.

That silently contradicted this section ("fit-to-screen"), the pointer
contract's `Fit` state ("the whole image is on screen") and its drag row
("nothing is off-screen, so there is no pan axis"). Worse, issue #11 gave
the wheel to zoom and made drag inert at fit, so after it the hidden band
was unreachable by **any** input — a culling tool cannot show you 80 % of a
photograph and let you decide its fate.

Requirement: **at one column the cell is bounded by the grid viewport**
(`cell_height = min(cell_width / CELL_ASPECT, viewport_height - 2·CELL_GAP)`,
`GridLayout::new`), so the image contain-fits inside it with pillarbox bars
and the whole frame is on screen. Consequences, all intended:

- The photo renders ~17-23 % smaller in each dimension than the old
  fill-width crop. Persona verdict: pay it happily — completeness is what
  fit is *for*; sharpness is what the ×1.5 ladder and 1:1 are for.
- **Multi-column grids are NOT bounded.** Their cells are far shorter than
  the viewport anyway, and capping `N = 2` would shrink the side-by-side
  comparison pair for nothing (persona review).
- The bars stay pure black — no filmstrip, no histogram, no info panel
  (persona: an instant IN-MY-WAY).
- The `✓ copied` and `×N burst` cell badges, anchored to the cell bottom,
  become visible in the loupe again; they had been rendering below the fold
  while the app deliberately populated them at `N = 1`. **This is the
  intended loupe badge policy, not an accident of the new geometry**: the
  MARK is suppressed at `N = 1` (`pick: 0`) because the issue #20 pill owns
  state display and the grid's 40% reject dim must stay out of the loupe,
  while "already copied" and "burst of N" have no pill and are exactly what
  a last pass before bed wants to see on the full-screen frame (persona).
  One channel per fact: pill for the mark, cell badges for the rest.
- Pre-layout refreshes (issue #4) see a zero/negative viewport height; the
  bound is skipped there rather than collapsing the cell.
- Residual, accepted: the zoom OVERLAY covers the filter bar while the fit
  view does not, so the overlay's factor-1.0 extent is ~6 % larger than the
  rendered fit cell and the first ladder rung magnifies ~1.59× rather than
  exactly 1.5×. That is a size-only discontinuity; the *positional* lurch
  (the old fit was vertically off-centre by the crop) is gone. Making the
  ladder's 1.0 the fit cell itself is the follow-up if it ever shows.

### Loupe zoom ladder (user decisions 2026-07-25, persona-validated)

The user request that drove this: "+ should not be a big jump to 1:1 — slow
increase — and it must never show the corner of the image; keep the center
where it is."

- **Steps**: from fit, each `+` multiplies the zoom factor by **1.5**
  (fit → 1.5× → 2.25× → …), computed as `fit × 1.5ⁿ` so `-` retraces the
  ladder's stops with no drift (a stop within rounding distance of the
  1:1 ceiling folds into it rather than producing a duplicate press). A step that would exceed 1:1 lands
  **exactly at 1:1** (a `+` that visibly does almost nothing reads as a
  broken key). Zoom NEVER passes 1:1 (user: beyond that you are judging the
  embedded JPEG, not focus). When 1:1 ≤ fit (small file), `+` at fit does
  nothing — clamped, no flicker.
- **Anchor**: every zoom step (in or out, `+`/`-`/`Z`/click entry alike)
  keeps the **center of the currently visible region** fixed. At fit that
  equals the image center (fixes the corner-entry bug); after a pan it means
  repeated `+` stays on the subject the user panned to. Zooming out clamps
  the offset to image bounds as the frame approaches fit; at fit the offset
  is definitionally zero.
- **`Z`**: from fit → 1:1; from 1:1 OR any intermediate factor → back to fit
  (user decision: `Z` below 1:1 is the escape hatch, not "show me pixels").
  One keystroke each way, always.
- **Click-to-zoom**: a single click is always "center HERE" (user decision
  2026-07-25: "single clicks centralize the image in the clicked point"),
  and **double-click** is the gesture that reaches 1:1 (user decision
  2026-07-26, superseding the earlier "single click at fit jumps to 1:1"
  rule). Full gesture table in *Mouse & pointer contract* below — that
  section is the source of truth for anything the mouse does.
- **Persistence across images (contract, was accident)**: navigating or
  pick/reject-advancing to another image keeps BOTH the zoom factor and the
  pan center, carried as a **fractional center of the image** and clamped
  for differing dimensions/orientations (lock 1:1 on the eye, arrow through
  the burst, Y/N each frame). Returning to fit forgets the pan spot — a
  fresh zoom-in re-centers (a stale pan from three images ago is a trap).
  During held-arrow transit the carried factor/pan render from whatever
  rung exists (quality rule below) — the persistence promise holds
  visually across EVERY frame, not just the decoded ones (issue #21).
  Implementation rule (issue #6): the zoom overlay is a PERMANENT element
  whose visibility is toggled — never a conditional (`if`) element. A
  conditional is re-created on every texture gap during held-arrow
  navigation, and a freshly created element initializes its viewport
  before the offset write lands: one 0,0 frame per transition, a visible
  top-left stream under key repeat.
  **Single-writer rule (issue #46, superseding the read-back scheme)**:
  Rust is the ONLY writer of the overlay's viewport offsets. A drag is
  reported by the overlay's touch surface as an explicit `loupe-dragged`
  event, folded through the pointer machine into the pan centre, and the
  offsets are rewritten synchronously from that centre — there is no
  Flickable, no offset read-back, and no `capture_pan`. The retired
  read-back inferred "the user dragged" from displacement, which is the
  #16/#22 disease in a new organ: a fling's deceleration binding fed it
  ANIMATED offsets nobody was touching, and every refresh of the decay
  folded them into the pan centre as phantom drags until the carried
  centre was permanently lost (issue #46 M3, `pan fold` traces with no
  hand on the mouse). The doctrine holds here as everywhere: **intent is
  only ever claimed from a POSITIVE input signal — the drag event itself
  — never inferred from displacement**, because no elimination list of
  displacement causes stays complete.
- **Transit vs settled (user requirement 2026-08-01)**: the user's words —
  *"while I'm holding a key and rapidly moving between shots I don't need
  the image to be as good as possibile, I need it to move fast. feeling
  almost like a video. But when I release the key, then I want quality to
  be high."*
  The loupe therefore has three request states. They govern **what is
  ASKED of the decoder, never what is DISPLAYED** — the renderer always
  shows the best rung in cache.

  | state | trigger | request |
  |---|---|---|
  | TRANSIT | frame changes < `TRANSIT_GAP` (250 ms) apart | mid rung ONLY, wide ring biased in the direction of travel |
  | SETTLED | the user stops (see the timing note below) | the app's real target for the focused frame |
  | SETTLED-AND-IDLE | after that lands | full-res look-ahead on the ±`PREFETCH` neighbours |

  **Every ring is a VIEW-ORDER ring (issue #46).** The engine's rings —
  transit, settled, look-ahead, and the deferred-upgrade revival gate —
  are planned in view POSITIONS and mapped to image ids at request time
  (`LoupeEngine::set_view`; the app re-keys it on every view recompute,
  so a filter or sort change re-keys the ring the same tick). The old
  ±`PREFETCH` in image-id space was wrong the moment view order diverged
  from id order — a capture-time sort over interleaved filenames (two
  bodies; repeated capture times) is the everyday case — and there it
  warmed frames no arrow could reach while EVERY actual neighbor stayed
  cold: the deterministic per-step fit-flash of issue #46 M1. The
  travel-direction latch compares view positions for the same reason (a
  forward hold FALLS in id half the time on an interleaved view). The
  policy (ring widths, lean, transit capping) stays in core; the app
  supplies only the position↔id mapping it already owns. An engine whose
  consumer never calls `set_view` keeps identity order — the pre-#46
  behavior, exactly, which is what the pre-#46 core tests still pin.

  - **The geometry never changes.** Transit keeps the carried factor and
    pan centre; it does NOT drop to fit — **in every reachable path,
    including jumps** (`[`/`]`, PgUp/PgDn, Home/End land outside any
    ring; the thumb rung below covers them). The spec already learned
    this once — "the old drop-to-fit strobed the whole burst-transit
    loop and trained the user to tap instead of hold" — and zoom/pan
    persistence is what makes 1:1 burst comparison work at all. Until
    issue #46 this sentence was aspiration: with neither the full-res
    nor the mid rung in hand, the renderer's fallback arm dropped the
    overlay and the N=1 strip showed the whole next frame at fit for
    1.5–75 ms per step (one or two pump ticks) — the user's "shows it at
    0,0 briefly, then snaps back". The thumb rung and the residual hold
    (quality rule below) closed the gap; the renderer traces
    `loupe overlay dropped` if any future path re-opens it, and the
    regression tests grep for that line and assert `one2one` across a
    cook-widened transit.
  - **SETTLED-AND-IDLE is not optional.** Requesting only the focused frame
    on settle would make tap-stepping through a burst at 1:1 pay a full
    decode on every frame, forever. It needs no new code — it is the
    pre-existing behaviour, which is precisely the SETTLED behaviour.
  - **Same rule at every factor, fit included**, no threshold to learn.
    On displays up to ~2 K wide, holding at fit is unchanged: fit asks for
    less than the mid, so `transit_request` is a no-op and QE measured the
    trace as byte-identical to the old behaviour. **On wider displays (QHD,
    4K) transit DOES engage at fit** — fit there is 2560/3840 px, above
    what the mid serves — so a hold shows mids upscaled ~1.6–2.4× until
    release (QE 2026-08-01: 6/30 frames at full during a hold vs main's
    30/30, which cost main 7.2 s of CPU for frames never seen). This is the
    designed trade applied consistently, not an exemption failing: an
    earlier revision of this bullet claimed fit was ALWAYS a no-op, which
    was only true on the ≤2 K displays it had been measured on. The visible
    softness of an upscaled mid on a 4K monitor during a hold has not been
    eyeballed by the user — worth one look before anyone tunes constants
    around it.
  - **Direction is latched at the index change**, never re-derived per
    call. The app re-focuses the SAME index on every `refresh()`, and
    `refresh()` runs on every decode landing — of which transit produces
    one per ring member per frame. `index >= prev` is trivially true for
    all of those, so deriving direction per call flipped the ring forward
    within milliseconds of every backward step: a backward hold prefetched
    the frames the user was moving AWAY from, an effectively 21-wide ring
    doing half its work behind the user. Found by the gate, not by the
    tests, which is why `a_backward_hold_keeps_leaning_backward_across_refocus`
    now drives the real engine through `focus()` and simulates that
    re-focus storm.
  - **The transit request must be a rung the mid actually SERVES.**
    `serves` allows a 1.25x upscale, so a 1616 mid covers 2020 px:
    requesting `MID_RUNG_MAX_LONG` (2048) is 28 px too high and silently
    sends every transit frame up the ladder to full-res anyway. The first
    implementation did exactly that and measured as no improvement at all.
  - **The settle guarantee lives in the reserved lane, not in the app.**
    Transit asks only for the mid, so something must ask for the real
    target once the user stops — and the app cannot, because its refresh
    loop goes quiet exactly when nothing is decoding. The reserved worker
    already wakes on a timer. Its `in_flight` and sufficiency guards are
    both load-bearing: without the first, releasing the key while the
    transit mid is still decoding queues a duplicate full-res job (a
    worker and ~149 MB of transient); without the second, the lane spins
    push/pop forever **while holding the state mutex**, freezing all three
    workers.

  **Timing note — the settle is ~250 ms, not `SETTLE_DEBOUNCE`.**
  `SETTLE_DEBOUNCE` (150 ms) is what `in_transit` decays on, but it is not
  what the user feels. The settle guarantee runs in the reserved lane,
  which is gated first by `FOCUS_DEBOUNCE` (250 ms) on `focused_at` — and
  `note_focus` resets `focused_at` and `last_index_change` from the same
  index change, so the lane cannot act before 250 ms and the `settled`
  check inside it is always true when reached. It is kept as an explicit
  belt-and-braces statement of intent, not as live logic. QE measured the
  overhead at ~215 ms over a bare decode, and confirmed by injecting a
  poke at T+150 ms that the engine had NOT yet acted.
  An earlier draft of this section claimed the two debounces would
  otherwise "stack into most of a second"; that is arithmetically wrong —
  both are measured from the same origin, so they do not add.

  **The pill is shown throughout a hold.** An earlier draft claimed the
  soft-cue pill stays settle-only, on the reasoning that an 8 Hz flicker
  in peripheral vision would be worse than nothing. Nothing in the app is
  transit-aware, so that was never true: every transit frame renders
  through the soft branch and the "◌ loading" pill is up for the whole
  hold. Measured over a 24 s hold: on for 784 of 787 rendered frames, with
  **one** state change — so it is a steady pill, not a flicker, and the
  feared failure mode does not occur. Accepted as-is; "never leave a frame
  at rest unsharp without the cue" is still honoured.

  **Measured** on the 8-core development laptop, real A1 frames, cold
  cache, at 1:1. "On screen" counts distinct frames whose pixels actually
  reached the display while the key was down — not decode landings, which
  include the look-ahead frames the cursor never reaches (an earlier draft
  of this table conflated the two and overstated the short-burst figures).

  | held arrow | | before | after |
  |---|---|---|---|
  | 150 keys @ 40 ms | frames on screen | 12 of 150 | **139 of 150** |
  | | key→pixels, median | 119 ms | **2 ms** |
  | | key→pixels, p90 | 9.3 s | **3 ms** |
  | 800 keys @ 30 ms | frames on screen | — | **787 of 800** |
  | | full-res decodes during the hold | 182 | **2** |
  | 20 keys @ 120 ms | frames on screen | 9 | **18** |
  | | full-res on the frame stopped on | 988 ms | 1027 ms |

  The win is in **steady travel**, and it is large: a held arrow tracks the
  key frame-for-frame instead of showing one frame in twelve with a p90 of
  over nine seconds. Two honest limits:

  - **A short burst barely benefits.** Over only 20 keys at 40 ms the
    figure is 3-4 → 8 of 20, because the first ~340 ms of a hold from a
    cold loupe stalls identically on both sides, and that is most of an
    800 ms burst. Stop-to-sharp at that rate is a wash (829-890 ms before,
    801-935 ms after).
  - **Stop-to-sharp is ~40 ms slower at 120 ms repeats** — the settle,
    paid on every stop. Accepted: the user's priority was explicit and
    motion-first, and both figures are under a second.

  **Measured and rejected — an adaptive settle.** Since the debounce is
  pure stop latency, it was made to learn the user's repeat rate and wait
  1.25x the observed gap (60 ms floor). It sharpened 200 ms sooner and far
  more consistently (749 ms, spread 705-754) but cost five frames of
  smoothness: a 60 ms threshold is fragile to repeat jitter, and one long
  gap settles mid-hold and fires a full-res decode that blocks the frames
  behind it. A middle setting (2x, 100 ms floor) was worse on both axes and
  swung 8-18 frames across three runs. Dropped rather than tuned on that
  spread.

  Recorded gap (historical — closed 2026-08-02): at the time of this
  experiment stop-to-sharp was decode-bound — the full-res decode alone
  medianed 614 ms under the transit workload, and
  `budget_fullres_decode_under_350ms` failed on the `main` of that day
  (issue #27). Scheduling could not close it; the PR #32 orientation
  rework did (raw-pipeline.md — the budget now passes idle with headroom).

  **Known and deferred** (recorded per the CLAUDE.md gate; none is a spec
  acceptance criterion, and all predate or are unchanged by this change):

  - A `Y`/`N` cull chain faster than 4 marks/second is classified as
    travelling, so those frames are judged from the mid. Marking is a
    judgment workflow, not a travel one — but **DOCUMENTED AS INTENDED,
    user decision 2026-08-01**, closing the deferral: the measurement
    below shows the trade only exists at cadences where the old code
    showed BLANK frames, so excluding marking from transit would trade a
    soft-but-present frame for a missing one. If a rating-speed workflow
    ever makes this bite, the recorded fix is an exclusion keyed on the
    mark keys, not a wider `TRANSIT_GAP`.
    **The measurement** (QE 2026-08-01): at the actual 4/s cadence
    (250 ms gaps, exactly `TRANSIT_GAP`) BOTH sides judge every frame at
    full-res — nothing changes. The mid-judging regime begins only above
    ~4.2/s, where main is strictly worse: at 6.2–8/s the branch judges
    17/20 at mid with zero blanks, while main leaves 2–5 of 20 frames with
    NOTHING decoded at all. So the trade only exists at cadences where the
    old code showed blank frames; the knife-edge at exactly `TRANSIT_GAP`
    is the part worth a deliberate decision.
  - Wraparound direction latch: if the app ever wraps cursor 0 → count−1
    on backward travel, the position comparison in `note_focus` (view
    positions since issue #46; was `index >= prev` in id space) reads
    that one step as "forward" and leans the ring the wrong way for one
    refocus cycle. Self-corrects at the next step; no main-relative
    regression (main has no lean at all); recorded so a future
    wraparound feature knows to fix the latch with it.
  - Sharpness-on-stop variance: at the ENGINE level stop-to-sharp is
    371–408 ms with ±20 ms spread across hold lengths 4–64 (QE
    2026-08-01) — extremely consistent. The wider swings observed at the
    APP level (721–1047 ms across whole-app runs) are compositor/refresh
    overhead on top, not engine scheduling; measure at the right layer
    before tuning any constant against that number.
  - No hysteresis on `moving`: a single stretched gap > `TRANSIT_GAP`
    mid-hold drops back to SETTLED and fires a full-res ring that is never
    cancelled, precisely when the machine is already behind. Same mechanism
    on `main`; transit simply does not help across a hiccup.
  - The first focus of a hold is never transit, so entering one commits up
    to two uninterruptible full-res decodes (~600 ms each) at the moment
    the hold starts. This is what makes short bursts benefit least.
  - Transit queues with `focus_origin = true`, which `want()`'s cull
    deliberately spares, so leaving the loupe mid-hold leaves up to 11
    stale entries ahead of visible grid cells.
  - The settle guarantee's `Slot::Wait` when the frame is in flight is an
    untimed wait; today the app's refresh loop re-drives it, but a
    core-only consumer that calls `focus()` once has no such rescue.

- **Quality rule (revised by issue #21, user-approved 2026-07-27)**:
  intermediate factors are rendered from the **full-res rung** once
  cached (GPU-downscaled): ANY factor above fit requests the top rung
  outright (`display_long = u32::MAX`). While the top rung is still
  decoding, the view stays at the CARRIED factor and pan center,
  rendered from the mid rung upscaled — soft but positionally
  continuous (the old drop-to-fit strobed the whole burst-transit
  loop and trained the user to tap instead of hold). The rule is now:
  **never show upscaled pixels UNFLAGGED, and never leave a frame at
  rest unsharp without the cue** — any above-fit view rendered from
  below the top rung shows the top-left cue pill ("you are never
  silently looking at soft pixels"), removed atomically when the sharp
  texture swaps in place.
  **The soft ladder gained a bottom rung (issue #46), and the identity
  rule gained one bounded, recorded exception.** The above-fit render
  ladder is now: full-res (sharp) → mid rung (soft) → the cursor's own
  320 px grid THUMB (soft — ~25× mush at 1:1, and exactly right during
  transit: position and identity continuity is what the eye tracks at
  video speed, persona-reviewed MUST-HAVE) → residual HOLD. The old
  behavior below the mid — drop to fit — was the M1 fit-flash and is
  GONE from every reachable path. A decode-FAILED cursor image skips
  the thumb rescue (validator finding on the first cut): an image with
  a live thumb TEXTURE but no decodable loupe rung would otherwise sit
  at 1:1 behind a "◌ loading" pill that can never complete, hiding the
  strip's failed badge — fit plus the badge is the honest floor. The
  shape is unreachable as a static file (QE, gate round 2: the grid
  thumb and the loupe's first rung decode the same `grid_source()`
  bytes, so on disk they live or die together); it is the MID-SESSION
  route that is real — a file that dies on disk (or a stale cache's
  thumb for a since-corrupted file) after its thumb reached memory.
  One causally unavoidable transient is accepted: the first focus of a
  freshly dead file renders the thumb for the milliseconds until its
  decode attempt fails, because the failure does not exist as
  knowledge yet; the gate binds from the Failed event on.
  **Residual HOLD (the recorded exception; persona-reviewed USEFUL with
  the bound demanded and applied)**: when not even the thumb exists (a
  cold-start edge: the thumb pipeline has not served that image yet),
  the overlay keeps the PREVIOUS image's pixels at the carried
  geometry, cue pill on — the video-player dropped-frame convention;
  the alternatives were the fit strobe (the bug) or a black frame
  (retinal pumping in a dark room). This is knowingly a bounded breach
  of "never the previous frame": during the hold the mark badge and
  status bar name the NEW image over the old pixels (marks still land
  on the intended image — addressing is correct; only the judged
  pixels lag). The bound is double: a decode FAILURE of the cursor
  image drops to fit immediately (the strip owns the failed badge),
  and `OVERLAY_HOLD_CAP` (250 ms, one settle window) caps a wedged
  decode — never an unbounded wrong-pixels hold. The cap is PER
  CURSOR IMAGE (recorded, validator finding): a hold-arrow run across
  consecutively cold frames re-times it at each cursor change, so the
  same stale pixels can exceed 250 ms in aggregate across images in
  the wedged-decode pathology — the bound is on how long any one
  photograph can be misrepresented, not on the pixels' total tenure.
  In any healthy release-profile session the thumb or mid lands well
  inside the cap;
  a CONGESTED adoption queue (observed in debug-profile runs, where
  149 MB texture fills stack up behind the cook hold) can legitimately
  fire the cap first — the capped drop traces its reason
  (`loupe overlay dropped … (hold cap)`, distinct from the outlawed
  excuse-less `(no rung in hand)` form) and the overlay RE-RAISES the
  moment any rung of the cursor image lands. A cold ENTRY into zoom
  with no pixels of the image at all (nothing to hold) keeps the
  overlay down until the first rung lands — the pre-existing honest
  behavior, unchanged.
  **Recorded deferral (validator concern, gate 2026-08-09)**: the
  hold state machine (cap timing, failure gating, re-raise) and the
  view-distance full-res texture eviction live in the APP crate as
  stateful policy with no core unit pins — only the timing-sensitive
  integration tests cover them. Precedented (the render ladder was
  already app-side) but each #46-class bug so far lived exactly in
  untestable app-side state; the next transit-affecting change should
  force this block into core as a pure decision function. Deferred
  explicitly, not silently.
  An INFINITY-pinned desire (Z) during transit renders at the last
  RESOLVED factor (the carried magnification, not the sentinel); a
  VIRGIN pin (nothing resolved yet this session) renders the mid at its
  own native resolution, floored at fit — the most zoom the data
  truthfully supports at that instant (QE finding: the earlier
  undefined case left fit showing with a usable mid in hand). The soft
  source is the cursor's own mid rung or a warm sub-top texture the
  engine re-announced (revisits beyond the retained window). The
  magnification never carries across sessions. Same
  behavior at all factors (user decision — no special 1.5-2.25x
  handling). The landing frame's full-res preempts transit backlog via
  the existing focus/want-culling priority; sharpness-on-stop within
  ~300ms is the contract.
- `G`/Esc from an intermediate factor → grid at the previous grid zoom, the
  factor is discarded (re-entering the loupe starts at fit; persistence is
  for walking images INSIDE the loupe, not across grid round-trips).

## Mouse & pointer contract (state machine) — user request 2026-07-26, issue #11

The mouse means different things in the grid and in the loupe, and the
difference is not a pile of `if`s scattered through the app crate: **pointer
behavior is defined by an explicit state machine whose state is the zoom
level**. This section is the source of truth for every mouse gesture; the
transition table below is the specification the implementation's tests are
written against.

The driving user requirement (2026-07-26, verbatim intent): *in the
multi-image view the wheel scrolls the grid as it does today; once a single
image is shown the wheel stops scrolling and starts zooming; a click centers
the clicked point; a double-click goes to 1:1 with the clicked point
centered; click-and-drag moves the image once you are in the single-image
view or deeper; dragging in the multi-image view is reserved for later.*

### States

| State | Meaning |
|---|---|
| `Grid { columns: N }`, `N ∈ {12, 8, 6, 4, 3, 2}` | multi-image view |
| `Fit` | single image, zoom factor `1.0` (the whole image is on screen) |
| `Zoomed { factor }`, `1.0 < factor ≤ max` | single image, above fit; `factor == max` is 1:1 |

`N = 1` is not a grid state — one column IS the loupe, i.e. `Fit` or
`Zoomed`. The state machine holds no other state: marks, cursor, filter and
selection are untouched by it.

### Inputs

Raw Slint pointer events are normalized before they reach the machine:
`Wheel { notches, pos }`, `Click { pos }`, `DoubleClick { pos }`,
`DragStart { pos }`, `Drag { dx, dy }`, `DragEnd`. `pos` is a point in the
view area; the machine converts it to a fractional image coordinate via the
existing `zoompan::contain_click_frac`.

Explicitly NOT inputs of this feature (persona review 2026-07-26, user
decision): **Ctrl+wheel** ("no Ctrl+wheel yet" — grid Ctrl+scroll zoom stays
the M2 deferral, and in the loupe the modifier is ignored, i.e. reserved),
**right / middle / thumb buttons** (the user has no use for back/forward
buttons; they get an explicit reserved no-op so nobody grows a context menu
into the culling grid by accident). Pinch/trackpad gestures and momentum
scrolling are out of scope; they reuse this machine when they land.

### Transition table (the contract)

| Input | `Grid { N }` | `Fit` | `Zoomed { factor }` |
|---|---|---|---|
| Wheel up | scroll the view up; cursor unmoved (browsing) | **zoom in** one ladder stop → `Zoomed { 1.5 }`, anchored under the pointer | one ladder stop up, anchored under the pointer; caps exactly at 1:1 |
| Wheel down | scroll the view down; cursor unmoved | **nothing** (clamped — user decision 2026-07-26: the wheel never falls out of the loupe; `-`/`G`/`Esc` are the exits) | one ladder stop down, anchored under the pointer; a step landing on `1.0` → `Fit` |
| Ctrl+Wheel | grid zoom in/out — still the M2 deferral | **reserved**: the modifier is ignored, the plain-wheel row applies | **reserved**: the modifier is ignored, the plain-wheel row applies |
| Click | move the cursor to that cell + collapse the multi-selection (issue #7); Ctrl/Shift variants per the cursor contract | **nothing** — the whole image is on screen, and the keyboard ladder stays center-anchored (user decision 2026-07-26, Q5) | re-center the view on the clicked point; factor unchanged |
| Double-click | **open that image in the loupe at fit** (user decision 2026-07-26 — the first click has already moved the cursor there, so this is purely "enter the loupe"); the previous grid zoom is remembered for `G`/`Esc` | → **1:1 with the clicked point centered** | → **1:1 with the clicked point centered** (already at 1:1: re-center only) |
| Drag | scroll the view (Flickable kinetic drag, today's behavior — **kept**); rubber-band multi-select is the reserved future gesture | **nothing** — nothing is off-screen, so there is no pan axis | **pan the image**, 1:1 with pointer motion, clamped so the image never detaches from the viewport edges; **release stops the image dead — no fling, no inertia** (issue #46, see below) |

Rules that the table alone does not carry:

- **The wheel no longer browses images in the loupe — knowingly** (user
  decision 2026-07-26 after persona review). Until now, at `N = 1` the view
  was a one-column strip and wheel-scrolling stepped to the next image with
  the cursor following (the "cursor follows scrolling" exception in the
  cursor contract). The user confirmed using that gesture AND chose to
  replace it with zoom. Consequence, spelled out so nobody re-discovers it
  as a bug: **inside the loupe, moving between images is keyboard-only** —
  arrows / PgUp / PgDn / Home / End, `Y`/`N` auto-advance, `[`/`]`. The
  cursor contract's 1-column exception survives only for the scrollbar-drag
  route, and is reworded accordingly.
- **A click at fit does not arm the next zoom** (user decision 2026-07-26,
  Q5 — resolving a contradiction between this section and the Loupe zoom
  ladder above). `+`/`-`/`Z` stay center-anchored at every factor,
  including immediately after a click at fit. The click at fit therefore
  stores nothing and does nothing; the only pointer-anchored zoom route is
  the wheel, which uses the pointer's live position and needs no click.
- **Wheel anchor is the pointer, not the center** (user decision
  2026-07-26): the image point under the cursor stays under the cursor as
  the factor changes — you wheel toward an eye without clicking first. This
  deliberately differs from `+`/`-`/`Z`, which keep the *view center* fixed
  (Loupe zoom ladder above); both are correct, because a key has no
  position and the wheel does. When the pan clamp makes the anchor
  impossible (image edge), the clamp wins and the anchor drifts — the image
  never detaches from an edge.
- **One notch = one ladder stop.** The wheel walks the identical `1.5ⁿ`
  stops as `+`/`-` (`zoompan::ladder_up`/`ladder_down`), so wheel and keys
  can never desync. High-resolution / kinetic wheels accumulate delta and
  emit one stop per notch-equivalent — never one stop per delta event.
- **Click/double-click need no timer.** Slint fires `clicked` before
  `double-clicked`, and single-click's action (center on P) is a strict
  prefix of double-click's (center on P, then go to 1:1 at P) — so the
  intermediate state is invisible and no click needs to be held back
  waiting for a possible second one. **Why the target point survives the
  prefix** (recorded 2026-07-30; expression updated for the issue #46
  restructure — it is a cancellation, not an accident anyone should
  have to re-derive): the two `clicked` calls re-centre the view and
  `refresh()` rewrites `loupe-vx/vy` SYNCHRONOUSLY, so by the time
  `double-clicked` evaluates `zoomed-img.x + mouse-x` (the image's `x`
  carries the pan offset since the Flickable's removal — the same sum
  the old `+ loupe-vx` term spelled explicitly) its frozen `mouse-x`
  and the new offset cancel exactly and the machine recovers the point
  actually pressed. This holds only while that refresh is synchronous —
  if the pan write is ever deferred to a timer or animated, the 1:1
  landing point silently moves by roughly `max/factor ×` the click
  offset (most of the viewport on an A1 frame).
- **Drag beats click.** A click fires only on press+release without
  movement beyond the drag threshold; once a drag starts, the release
  produces no click and no double-click. (Since issue #46 the loupe
  overlay enforces this itself — an 8 logical px latch on its touch
  surface, matching the retired Flickable's grab threshold — because
  nothing steals the grab anymore. The below-threshold prefix of a drag
  is not lost: the first applied event carries the full displacement
  since the press.)
- **Loupe pan has NO inertia — decided, not omitted (issue #46).** The
  contract above always promised "pan 1:1 with pointer motion" and
  named kinetic behavior only for the GRID's drag-scroll; the shipped
  overlay nevertheless used a Flickable, whose flick physics installed
  a deceleration animation binding on the viewport offsets at release.
  That binding SURVIVES programmatic sets (the write is stored; the
  next tick overwrites it from the simulation until the decay ends), so
  an arrow pressed during the decay rendered the NEXT image at the
  still-animating offsets — crossing exactly 0,0 — and the read-back
  then folded those offsets into the pan centre as phantom drags until
  the carried position was permanently lost (M3). The fix removes the
  physics rather than fencing it: the overlay has no Flickable; drags
  pan 1:1 while the button is down and **release stops the image where
  the hand stopped**. Persona verdict (MUST-HAVE): inertia at 1:1 is
  hostile to focus judgment — a glide overshoots the feather every
  time — and long travel already has a better gesture (click
  re-centers). Flicking a grid is browsing; gliding at 1:1 is judging:
  different verbs, different physics. The grid keeps its kinetic
  scroll unchanged.
- **A double-click needs proximity, not just timing** (persona finding —
  scanning an intermediate factor by clicking eye, then beak, then wingtip
  in quick succession is two independent re-centers, not a jump to 1:1):
  the second press must land near the first. **Slint enforces this itself**
  and the app must NOT re-implement it: `check_repeat` restarts the click
  count unless the second press is within 10 logical px of the first
  (`i-slint-core`, `square_length() < 100`), so `double-clicked` cannot fire
  for distant presses at all. A bridge-level re-check is not merely
  redundant — the one shipped with #11 VETOED the gesture it guarded, see
  the deviations list below.
- **Clicks outside the image rect are ignored** (persona finding): at fit a
  landscape frame on a 16:9 screen has fat pillarbox bars, and
  `contain_click_frac` clamps them to the nearest image edge — so a
  double-click on black would slam to 1:1 on a frame edge. Clicks and
  double-clicks in the bars produce no action at all (the clamp stays for
  the drag/pan path, where it is correct). **The WHEEL is deliberately not
  bar-rejected** (recorded 2026-07-30): a wheel notch over a bar still steps
  the ladder, anchored at the nearest frame edge by the ordinary pan clamp.
  A key has no position and a wheel does, but "zoom in" is unambiguous
  wherever the pointer sits, whereas "1:1 centred HERE" is not. Note this
  became routine rather than unreachable when *One-column cell bounding*
  gave a 3:2 frame real bars (~255 px per side on a 1440-wide window).
- **The wheel only zooms over the image.** Wheel events over the IPTC
  panel, the filter bar or the overlay scrollbar are not loupe input —
  they scroll that widget or do nothing (persona finding: the pointer
  parks over the panel while keywording; a photo that zooms under it is a
  nightly accident).
- Everything else in the loupe is unchanged: the 1:1 ceiling, the
  center-anchored keyboard ladder, zoom/pan persistence across images, and
  the full-res quality rule (any factor above fit renders from the top
  rung) all apply exactly as specified above.

### Implementation contract (user requirement: "managed by a state machine")

- The machine lives in **`fastcull-core`** (rule 5 — the app crate is a thin
  Slint bridge): a pure `ViewState` + `PointerInput` → `(ViewState, Action)`
  step function with no Slint types and no I/O. Geometry (viewport size,
  native size, fit scale, 1:1 ceiling, current pan center) is passed in per
  call; the machine calls the existing `zoompan` math rather than
  duplicating it.
- The app crate's job is only to normalize Slint events into `PointerInput`
  and to apply the returned `Action`s. **No zoom/pan branching in the app
  crate** — a gesture whose behavior cannot be read off the table above is
  a bug in the machine, not in the bridge.
- Every (state, input) pair is handled explicitly. Reserved combinations
  (grid drag → rubber-band, Ctrl+wheel in the loupe, right/middle/thumb
  buttons) return an explicit "no action, reserved" variant, never a silent
  fallthrough — that is what keeps the next gesture cheap and visible.
- **Known Slint risk — RESOLVED at implementation (issue #11,
  2026-07-26)**: the feared fit-state interception worked. Mechanism: a
  permanent, visibility-toggled `TouchArea` (`fit-ta`) covers the grid
  area exactly when `columns == 1` and no zoom overlay is up; its
  `scroll-event` consumes the wheel (one ladder stop per 60px
  notch-equivalent — exactly one winit wheel notch, verified in the
  backend source; remainders carry over, a direction flip resets them),
  its `clicked` only claims the cursor, its `double-clicked` goes to 1:1.
  The machine receives the fit
  view's REAL geometry (the N=1 grid cell rect, scroll-dependent) so
  anchors and letterbox rejection follow what is actually on screen. Because it swallows presses wholesale it also implements
  "click at fit does nothing" and "drag at fit does nothing" — and it
  sits BELOW the overlay scrollbar, which keeps its drag route. Above
  fit, the wheel is taken by a `scroll-event` on the zoom overlay's
  image TouchArea (children see scroll before the Flickable, so drag-pan
  stays native while the wheel zooms). The retired browse-at-fit wheel
  gesture is gone as decided; movement inside the loupe is
  keyboard-only.

  **Defect fixed 2026-07-30 (validator FAIL-1 / QE D1) — the bridge vetoed
  its own headline gesture.** `handle_loupe_double_click` re-checked
  double-click proximity by comparing the last two clicks as FRACTIONAL
  IMAGE coordinates. But Slint fires `clicked` before `double-clicked`, and
  the first click's handler re-centers the view and refreshes — moving the
  image under a stationary pointer. The second press therefore landed on
  the same screen pixel but a different image fraction, so the measured
  "distance" was really the recenter displacement — which is exactly the
  click's own offset from the view centre. QE replayed the verbatim guard:
  a double-click 13 px off-centre measured 13 px and was vetoed, 200 px
  off-centre measured 200 px and was vetoed; only within ~12 px of the
  centre did it survive. **Above fit, double-click never reached 1:1**;
  only from
  fit (where a click re-centers nothing) did it work, which is why it
  passed review and shipped. The check is now DELETED, not repaired —
  Slint's own 10 px repeat gate already implements the rule (see the
  proximity bullet above), so any bridge-level re-check can only contribute
  false negatives.

  Recorded deviations/deferrals (gate 2026-07-26, revised 2026-07-30):
  a pinned-unresolved 1:1 desire (INFINITY while full-res decodes) makes
  every pointer gesture that goes THROUGH THE MACHINE inert until the
  render clamp resolves it (no anchor math on infinite extents); a click
  in the zoom overlay is the exception — it is applied by the bridge
  directly and still re-centers, harmlessly, since its fraction comes from
  Slint rather than from anchor math. While the ceiling is unknown the
  wheel climbs optimistically but is CAPPED (`pointer::OPTIMISTIC_MAX`):
  an unbounded ladder reached ~1e38 in ~223 notches and produced a NaN pan
  centre that persisted across images (QE D4). Wheel over the overlay
  scrollbar is swallowed (not loupe input, per this contract); extreme
  coalesced wheel deltas may emit fewer stops than notches (single emit per
  event — accepted), and the two surfaces keep separate accumulators whose
  residue carries over until a direction flip resets it; two-finger
  trackpad scroll over the overlay IMAGE now walks the zoom ladder at
  every factor (issue #46: the Flickable that used to intercept it as a
  pan is gone, so the surface's scroll handler sees it like the wheel —
  the old fit/zoomed asymmetry is closed by accident; trackpads remain
  declared out of scope, revisit with gesture support); wheel in the
  zoom overlay's letterbox BARS is not loupe input — with the Flickable
  gone (issue #46) nothing on the overlay consumes it, and the
  validator's live probes (700x1100 window, factor 1.5, 180 px bars)
  found bar wheel, bar click, bar double-click and a 900 px bar drag
  ALL INERT: an earlier revision of this bullet claimed the wheel
  reaches the grid Flickable behind the overlay and scrolls the strip
  invisibly, which did not reproduce — recorded as inert until a
  geometry is found where it is not (extend the wheel surface over the
  bars if one ever shows);
  a drag STARTED in a letterbox bar is inert since issue #46 (the drag
  surface is the image, and bars only exist at moderate factors where
  one axis has no pan anyway — deep 1:1 has no bars; extend the drag
  surface over the bars if it ever shows); the scrollbar's wheel
  swallow also deadens its 18px strip in GRID view (was native scroll —
  tiny strip, accepted).

  **One table cell is implemented OUTSIDE the machine** (recorded
  2026-07-30, narrowed by issue #46): `Zoomed` × Click is applied
  directly by `on_loupe_clicked`, because Slint already delivers
  image-relative fractions there and routing them through `step()`
  would add a lossy coordinate round-trip for no behavioural gain —
  so the `Zoomed`+`Click` arm is exercised only by its unit tests, and
  a future change to it silently changes nothing in the app. `Zoomed` ×
  Drag, previously the second outside cell (the overlay Flickable's
  kinetic pan folded back by `capture_pan`), now DOES route through the
  machine: the overlay's touch surface reports `loupe-dragged(dx, dy)`,
  the bridge feeds `PointerInput::Drag` to `step()`, and the returned
  `Recenter` is applied like every other action — the machine's Drag
  row finally has its production caller. The enum still carries a
  single `Drag` input rather than the `DragStart`/`Drag`/`DragEnd`
  triple listed under *Inputs* above (the press/release edges live in
  the overlay's drag latch, which owns only the threshold and
  click-suppression bookkeeping).

  **The machine's state is the DESIRED factor, which the screen may not be
  showing yet.** `machine_ctx` derives `Fit`/`Zoomed` from the clamped
  desired factor while the overlay only rises once a texture of the cursor
  image exists. In that window (a decode gap after a fast wheel burst, and
  also the longer honest-degradation case where neither the full-res nor
  the mid rung is in hand) anchors compute against the already-zoomed
  virtual viewport while the screen still shows fit: a double-click is
  interpreted against the virtual extents rather than the visible frame, a
  click does nothing though the machine says `Recenter`, and wheel-down
  needs a few visually inert notches. Self-corrects on texture adoption.

## Virtualization (the M2 prototype risk)

Slint virtualizes ListView only, so the grid uses a **windowed model** maintained in
Rust: the app crate exposes a `VecModel<CellData>` containing only visible rows ±1
row margin; scroll/zoom recomputes the window and mutates the model in place
(reuse, don't recreate). Cell textures are `slint::Image` handles produced from
pipeline `Thumb` events. Placeholder cells render immediately (gray + filename)
before their thumb arrives.

`CellData`: image id, texture, pick state, burst count (`burst-count: int`,
>0 only on a group's first frame — the "×N" badge; 0 = no badge),
failed flag, copied flag, selected flag. (Fields arrive with their milestones:
M2 ships texture/failed/cursor; pick badge M3, copied M6, burst M7.)
The `selected` flag drives BOTH the outline and the wash; the window carries
`selection-wash` (color) and `selection-wash-opacity` (float) so the tint is
settable from outside the UI without touching the cell model.

Recorded deviations/decisions (M2, REVISED 2026-08-02 — user decision:
"no decoding should be done on the UI thread"):
- ~~Thumb JPEG→texture decode on the UI thread (~32/refresh)~~ and
  ~~full-res→mid downscale on the UI thread (2 adoptions/refresh)~~ are
  RETIRED. All pixel work — thumb JPEG decode, the full-res 149 MB
  SharedPixelBuffer fill, and full→mid downscales — moves to the
  texture-preparation worker (01-architecture.md § Threading model). The
  UI thread's only texture duty is wrapping a finished SharedPixelBuffer
  into a `slint::Image` (O(1); `Image` is not `Send`, the buffer is).
  Consequence, accepted: a texture becomes visible one pump tick after its
  pixels are ready rather than within the same refresh in the WORST case —
  the kitchen's completion nudge (`invoke_from_event_loop`) makes the
  typical added latency milliseconds, and adoption is UNBUDGETED so a
  stopped fling fills the whole viewport in one tick, never a trickle
  (persona conditions, both honored). Stale-request rules, stated
  precisely (an earlier draft overclaimed): only MID-downscale requests
  are culled to the visible set at submission waves; Thumb jobs are never
  culled because their encoded bytes were MOVED into the job, and
  Full/Wrap jobs serve the loupe, which already governs its own requests.
  A landed thumb for a scrolled-away cell is adopted into `st.images`
  (paid-for work; the pruned-and-revisited rule); a landed MID for an
  invisible cell is adopted and then dropped by the visible-set retain on
  the next refresh — the adopt is cheap, the retain is the existing
  memory policy, and re-scrolling re-requests it.
- Ctrl+scroll zoom is deferred: Slint's Flickable consumes wheel events and
  an overlay TouchArea would steal the drag/click gestures. Keyboard `+`/`-`
  covers M2; revisit during M4 polish (needs user OK to defer past v1 if it
  stays unsolved).

## Panel docking model (made explicit after issue #12)

When the IPTC panel is visible it takes its 300px from the RIGHT edge and
the grid reflows into the remaining width, pinned flush to the LEFT edge
— never centered, never partially under the panel. Everything that
belongs to the grid area (loupe/zoom overlay, empty-state message,
overlay scrollbar) sizes to the grid area, not the window. Clicks inside
the panel never reach the grid (a stray click on panel whitespace while
keywording must not move the cursor or collapse a multi-selection).
Slint trap recorded from the incident: an element with a bound width but
no `x:` (or bound height but no `y:`) is CENTERED in its parent — every
non-layout child with a computed size needs its position bound
explicitly.
The 1:1 anchor RECOMPUTES across a panel toggle (issue #18, verified
resolved 2026-07-27 by the issue #16 early-dock-publish fix): on OPEN
the crop re-centers for the docked width in the next frame ("I zoomed
on the eye; the eye stays put when chrome docks"); on CLOSE it
restores the full-width anchor with NO stale intermediate frame (the
one-frame zoom-pop sub-symptom is gone — 12/12 clean transitions in
the re-baseline). Pinned by the reanchor screenshot regression test.

## Overlay scrollbar (task #21, user request 2026-07-25, persona-reviewed)

A modern overlay scrollbar on the GRID's right edge (inside the grid area —
when the IPTC panel docks, the bar sits between grid and panel, never on
the window edge): thin (6px) and faint whenever content overflows — NEVER
fully hidden (the "where am I?" glance is the whole point) — widening to
10px and brightening on hover/drag, with an 18px grab zone (persona: a
tired mouse hand must not hunt a 6px strip). Thumb sized
viewport/content, draggable; a TRACK CLICK JUMPS TO THE SPOT (persona
IN-MY-WAY on page-jump: PgUp/PgDn already page via the cursor; the bar
teleports). While dragging, a floating hint shows "first-visible / total"
of the filtered view, with the first visible image's capture time
appended ("795 / 1450 · 15:42") when sorting by capture time — numbers
only under filename sort. Scrollbar use NEVER moves the cursor
(scrolling is browsing); hidden under the zoom overlay and on empty
views. Panel toggle reflows anchor on the cursor. Deferred polish:
brightening during wheel scrolling (needs an activity decay timer).

## Cursor (the selector) — behavior contract (added after user bug report 2026-07-25)

- Exactly one cell is the cursor at any time; it marks where keyboard actions
  (pick/reject/zoom) land.
- Visual: a 3 px accent (blue) border drawn as an overlay ON TOP of the cell's
  content — never underneath the image (Slint renders children above a
  Rectangle's own border, so the border must be a top-most child overlay).
  It must be visible on every cell state: placeholder, loaded, failed.
- After any keyboard navigation or zoom change, the cursor must be fully
  visible: the grid's virtual height is updated BEFORE the scroll offset is
  written, so the Flickable never clamps the reveal against stale bounds.
- Mouse/wheel scrolling does not move the cursor in multi-column grid views —
  scrolling is browsing, the cursor stays where the user parked it (it may
  leave the viewport; the next arrow key first brings it back into view).
- **Grid click moves the cursor (user requirement 2026-07-25, issue #7 —
  IMPLEMENTED with the panel step)**: a plain click on a cell moves the
  cursor to that image (and claims it, per the untouched-cursor rule) and
  COLLAPSES any multi-selection (the deselect gesture; Esc/G at a grid
  zoom also clears the selection). Ctrl+click toggles membership;
  Shift+click spans cursor..clicked in view order. Clicks live in per-cell
  touch areas INSIDE the Flickable, so drag remains scrolling (the
  press+release-without-movement disambiguation comes from the Flickable's
  drag grab); clicks never scroll the view as a side effect. Clicking the
  grid returns keyboard focus to it (a stranded panel-field focus must
  never turn grid keys into text).
- Exception at 1-column (loupe) zoom: the visible image IS the cursor — the
  cursor follows scrolling so full-res loading and marks always apply to what
  the user is looking at. **Scope narrowed by issue #11 (2026-07-26)**: the
  WHEEL no longer scrolls at `N = 1` (it zooms — see the Mouse & pointer
  contract), so this rule now covers only the remaining scroll route, the
  overlay scrollbar drag. Image-to-image movement inside the loupe is
  keyboard-only. **Relayout carve-out (issue #16, 2026-07-26; extended by issue #22)**: a
  GEOMETRY change — panel toggle, window resize, anything that alters
  (grid width, viewport height) between refreshes — is NEVER scrolling
  and must NEVER claim or move the cursor; the viewport re-anchors to
  the cursor instead. The same rule covers a VIEW MUTATION (issue #22):
  a cursor displaced because the view re-sorted or changed membership
  between refreshes (capture keys streaming in during folder load, live
  filter removal) is not scrolling either — during load the claim used
  to move the cursor with no input at all. FINAL FORM (after a Windows
  DPI-timing variant slipped past both guards): the claim is
  POSITIVE-GATED on actual scrollbar activity (drag move or track
  click sets a flag Rust consumes) — displacement alone NEVER claims,
  because the scrollbar is the only legitimate trigger the contract
  names and no elimination list of displacement causes stays complete (the whole point of the follow rule is that marks
  land on what the user is looking at — a relayout claim inverted it
  into marks landing on a photo the user already left). The dock state
  is published to the window BEFORE any geometry read in the toggle
  path, so reveals never compute against a stale width (that stale
  width was also issue #17's grid-under-panel state). **Grid-level
  resize anchoring (user report 2026-07-26)**: at N>1, row pitch is a
  pure function of the grid width, so keeping the raw pixel offset
  across a relayout lands on different content (shrink = "the list
  scrolls up", grow = "scrolls down"). A grid relayout anchors CONTENT,
  not pixels: the top-visible row keeps its fractional position; at the
  bottom clamp the bottom stays the bottom (growing at End must not
  strand the viewport mid-list); a cursor that was visible stays
  visible (reveal semantics, same as the panel toggle); scroll 0 stays
  0 — except that CURSOR VISIBILITY WINS: with the cursor on the last
  visible row, a pitch-growing resize may scroll away from 0 to keep it
  in view; the cursor itself NEVER moves. A reveal marks its geometry
  as consumed, so anchor corrections never stack.
- The status bar always names the cursor image (filename, position N/M).
- **Untouched-cursor rule (issue #4, 2026-07-25; NARROWED 2026-07-31 — see
  *Provisional order while loading* below: once a folder has finished
  loading, ENGINE recomputes no longer move an untouched cursor, only a
  USER-requested view change does)**: from session open until the
  user's FIRST interaction, the cursor is "the first image of the view", not a
  pinned id, and a folder must never open with the cursor stranded mid-grid
  (real case: name order vs capture order put it at position 795/1450).
  The original rationale — "capture keys stream in progressively and re-sort
  the view under it" — no longer describes the code: the view now holds a
  stable filename order until the load completes, which is what made the
  narrowing possible. The cursor
  is CLAIMED (id-pinned from then on, all rules above apply) by: any mark,
  any navigation key, loupe scroll-follow with laid-out geometry, and any
  click on an image — loupe, fit, or grid cell (issue #7). NOT claiming it: zoom keys (they don't move it), filter
  and sort changes (pre-touch these snap to the new view's first image —
  overriding the nearest-survivor rule until the claim), and engine events.
  Open Folder resets to unclaimed. Pre-layout geometry (a refresh before the
  window has a real height) must never claim or move the cursor.
- **Provisional order while loading (issue #25, 2026-07-30)**: the sentence
  above — "capture keys stream in progressively and re-sort the view under
  it" — described a real hazard, not just a quirk, and the rule that fixed
  issue #4 is what created it. **The view is now ordered by FILENAME until
  every image's metadata job has finished**, then sorted once by the user's
  sort key (`filter::view`'s `metadata_complete`). Rationale, measured:
  - EXIF is read INSIDE the per-file thumbnail job (`pipeline.rs`, its only
    production caller), so "still loading" is the WHOLE load — measured
    ~15 s for 3,000 files on the development laptop with a warm page
    cache (the 32-thread machine retired 2026-07-28 — see
    01-architecture.md — would do it in ~2 s; a card reader, much
    slower) — not a blink.
  - The capture sort puts keyed images ahead of still-keyless ones, so when
    filename order runs contrary to capture order (two bodies or two cards
    in one folder, a counter rollover mid-event) the HEAD changes identity
    over and over for that entire window.
  - Navigation rode it: one `right` at open landed 870 frames from the
    intended second image on a 3,000-file fixture.
  - **Marks rode it too, and that is the serious half**: `Y`/`N`/`U` write
    to the cursor, and an unclaimed cursor is re-pinned to that moving head
    on every refresh, so a head change inside a photographer's reaction time
    lands the mark on a frame they never looked at — silently, and invisibly
    under an Unmarked-only filter. Reproduced with NO input at all: the
    cursor moved from image 0 to image 2000 mid-load.
  Filename order comes free from the directory scan (~13 ms for 3,000
  files) and for a single card in shooting order IS capture order, so the
  eventual re-sort is invisible in the common case. Rejected alternatives
  (persona review 2026-07-30): deferring or queueing input until the load
  finishes — IN-MY-WAY, "an app that is dead for 11 seconds after opening a
  folder is an app I'd stop using", and the user types ahead by design;
  and accepting it with documentation — fine for navigation, not for a
  silent wrong mark. Consequences, accepted: the one re-sort can happen
  after culling has begun, so a claimed cursor keeps its image while the
  frames around it move once. **The viewport follows it — but only if the
  user was looking at it**: this is the only view mutation that reorders the
  WHOLE grid at once, so the scroll offset is meaningless afterwards and the
  cursor cell can be left off-screen entirely. At `N = 1` the loupe's own
  re-anchor covers it; at multi-column zoom a dedicated one-shot reveal
  does, fired on the false→true edge of completion only — the relayout
  branch cannot see it, being gated on GEOMETRY changes while this is a
  CONTENT change (validator FAIL, 2026-07-30). Not fired on every view
  mutation, which would fight live filter removal and per-mark recomputes.
  **And gated on the cursor cell having been ON SCREEN at the previous
  refresh** (`grid::scroll_after_resort` — the decision lives in core and is
  unit-tested both ways, because the app-level version shipped into review
  with this guard missing): wheel and scrollbar browsing do not claim the
  cursor, and this contract already says an off-screen cursor stays
  off-screen until the next arrow key, so restoring it under a browsing
  user's mouse would be the same "moved with no input" defect in a new place
  (validator FAIL, 2026-07-31: without the guard a user browsed to 20,000 px
  was snapped to 0). Visibility is sampled on the PREVIOUS pass on purpose —
  the flip changes the cursor's position, so asking afterwards answers a
  different question. What that browsing user keeps is their OFFSET, not
  their content: the grid beneath has re-sorted, so they see a different
  stretch of the shoot at the same scroll position. Accepted trade — a
  viewport that stays put is recoverable by looking, one that teleports is
  not. The edge is consumed only on a refresh that can act on it, so a
  pre-layout or minimized pass cannot swallow it for the session. Note this
  is looser than it sounds: `view_len > 0` is part of the condition, so a
  load that completes while an EMPTY filter is showing defers the edge until
  some later refresh with a non-empty view — carrying a visibility sample
  from before the view emptied. Benign today because a filter change reveals
  the cursor anyway. Accepted
  residual: if completion lands on the same tick as a resize or panel
  toggle, the relayout branch rescales an offset this branch already
  corrected — rare, self-heals on the next key, and the price of a third
  `vp_y` writer in one pass; one anchoring pass is the real fix.
  Burst grouping always uses the TRUE capture order, never the provisional
  one, because a burst is a fact about capture times. **Copy Picks likewise
  always uses the true sort**: `{seq}` is baked into permanent filenames —
  the one irreversible artifact this app produces — and fileops.md promises
  the session sort, so a copy started mid-load must not encode a transient
  view state forever.
  **The UNCLAIMED cursor keeps its image too** (user decision 2026-07-31,
  narrowing issue #4 — his words: "during the loading
  phase, whatever is currently selected stays selected, and stays visible
  on the screen"). Earlier drafts re-pinned it to the new head, so a folder
  opened with no input still moved the photograph under the user once.
  It no longer does — and the rule is a STATE, not an edge: once the folder
  has loaded, ENGINE recomputes (the re-sort itself, a decode landing, a
  sidecar arriving) leave the photograph alone, while the USER asking for a
  different view — a filter chip, the sort control — still snaps pre-touch
  to the new head exactly as issue #4 specifies. An earlier edge-shaped
  attempt held the cursor for a single refresh and the next background
  decode snapped it away again (validator FAIL, 2026-07-31, reproduced
  live). `filter::cursor_after_recompute` is the one place the two rules
  meet.
  Accepted cost, chosen knowingly: on a folder whose filename order runs
  contrary to capture order, an untouched cursor that started at the top
  ends up mid-grid once the real order lands — the stranding issue #4 was
  written to prevent (measured: image 1 of 3,000 becomes 2001/3000) — and
  the viewport scrolls to keep it in view. The flip is the app finishing a
  job, not the user asking to see something else, and that is the
  distinction that decides it.
- **Load progress in the status bar** (persona ask, same review): WHILE
  loading the counter reads `LOADED/TOTAL loaded · sorting by name until
  loaded` — a number with no denominator makes the user hunt for the total
  to know whether to start now, and the grid looks identical in either
  order, so the status bar is the only honest place to say which one is on
  screen. Once complete it returns to the plain `N thumbs loaded`; the
  cursor's own `(N/M)` carries the total from then on.
- **Known divergences while the order is provisional** (recorded 2026-07-31
  rather than left to a commit message, since specs/ is the source of
  truth): the SORT CHIP still reads the user's chosen key — "Capture ↑"
  over a name-ordered grid — and clicking it mid-load reverses the grid
  without changing the key, because the ascending flag is applied after the
  override; and the scrollbar's drag hint still appends capture times,
  which therefore run non-monotonically over a name-ordered view. Both read
  `query.sort` directly, which is no longer a truthful description of what
  is on screen. `[`/`]` burst jumps resolve over VIEW positions while groups
  are computed in true capture order, so they walk oddly over a name-ordered
  view for the same window. The status bar is the compensating control. Closing this
  properly means an `effective_sort(query, complete)` in core that every
  consumer reads — deferred, not accepted as correct.
- **No fallback if a job never finishes** (recorded): completion is
  all-or-nothing, so one worker wedged in uninterruptible I/O (a dying
  card, a stalled network mount — the case 01-architecture.md's shutdown
  policy already names) leaves the session in filename order permanently,
  with the status bar stuck one short and no way to force the sort. Before
  this change a wedged file cost only its own position. The fix is a
  per-file give-up or a "sort anyway" affordance; neither is in this step.
- **Copy Picks mid-load** (recorded): `{seq}` deliberately follows the TRUE
  sort, never the provisional one, because it is baked into permanent
  filenames. With keys still streaming that sort is partial — unkeyed
  images sort to the tail — so a copy started mid-load numbers files in an
  order matching neither the grid on screen nor the same button pressed ten
  seconds later. Unchanged from before this step, but newly reachable now
  that the status bar invites working during the load.

## Visual language

- **FastCull is DARK-ONLY, and the palette is PINNED to say so** (user bug
  + decision 2026-08-02, verbatim: "I don't want a light mode. I don't
  want a toggle. Keep the design as is"). Every surface in `main.slint` is
  a hand-picked dark colour, but native `std-widgets` (MenuBar, LineEdit,
  ComboBox, Button) take their colours from the style palette, which
  follows the PLATFORM colour scheme — the winit backend reads the
  xdg-desktop-portal `color-scheme` key and live-updates it. On a
  light-mode desktop the fluent MenuBar therefore drew its labels in
  90%-alpha black over the app's `#161618` bar: invisible yet clickable,
  while the OPENED menus stayed readable because a popup draws its own
  palette background (the bar is the only palette-text-over-app-surface
  in the tree; QE's inventory found the other std-widgets draw their own
  palette surfaces and merely clashed in light mode). An unreachable
  portal resolves the scheme to Unknown, and fluent's Unknown fallback is
  ALSO light — which is what every headless/CI run gets, so the suite had
  been capturing light-palette chrome on some days and dark on others,
  green either way; the uncontrolled scheme, not the untested GPU
  renderer, was the real screenshot blind spot. The fix:
  `Palette.color-scheme = ColorScheme.dark` at the root window's `init`
  — one declaration, and every palette-derived colour follows the app's
  one design regardless of desktop theme, portal reachability, or
  anything added from `std-widgets` later. A future light mode, should it
  ever be wanted, is a deliberate feature (every hardcoded surface needs
  a light twin), never an inherited default.
- Pick: small star badge (top-left; user decision — "mark the ones taken with a
  little star"). Reject: red X badge + 40% dimmed thumb.
- **Loupe state indicator (issue #20, user request 2026-07-26,
  persona-reviewed MUST-HAVE/HIGH; implemented 2026-07-27)**: the loupe
  (fit AND zoomed — one continuous view) shows the image's mark as a
  badge overlaid in the image's TOP-LEFT corner (same location as the
  grid badge): ★ for picked, ✕ for rejected, on a small dark
  semi-transparent pill (own contrast backing — white-on-blown-sky and
  red-on-red must stay readable). Constraints, all persona-validated:
  an OVERLAY, never a reserved strip (the image must not reflow or
  shrink); a left-aligned pill that can grow horizontally to hold up to
  five stars when ratings (reserved keys 1–5) land, anchor unmoved;
  badge only for picked/rejected — unmarked is absence, backstopped by
  the STATUS BAR always spelling the state in words ("★ picked /
  ✕ rejected / unmarked"); a rejected frame is NEVER dimmed in the
  loupe (deliberate divergence from the grid's 40% dim — a reject may
  be re-judged for rescue at full brightness); pointer-inert (no hit
  area — the pointer state machine owns every gesture); permanent
  element with state toggled, state swap ATOMIC with the image swap
  (the issue #6 stale-frame class: a wrong-frame badge is a confident
  lie, worse than none); scope-guarded to the glyph pill only — no
  filename/metadata creep (the status bar owns those), top-right stays
  free for a future histogram/focus indicator. All three design choices
  CONFIRMED by the user (2026-07-27): badge at the top of the image
  (overlay, not a reserved strip); badge-only rejects at full
  brightness; no explicit unmarked glyph. Also confirmed for the
  composed issue #21 cue: same behavior at all zoom factors, loading
  indicator acceptable. Implementation notes: at N=1 the app sends
  cells `pick = 0` — the badge pill owns state display in the loupe,
  which is what keeps the grid's 40% reject dim (and the cell glyph)
  out of it; the badge property is written by the same refresh pass
  that swaps the image/cells (atomicity); the #21 loading cue stacks
  BELOW the badge slot (14 px / 44 px) so the two pills never overlap
  and their visibility contracts stay independent; the status bar
  appends the cursor's mark in words (" · ★ picked / · ✕ rejected /
  · unmarked") after the position counter whenever the cursor is in
  view — in every view, not only the loupe.
- **Burst context**: see burst-grouping.md — the ×N badge and "burst
  7/23" status fragment already serve burst position; the state
  indicator composes with them, it does not replace them.
- Burst (M7, persona-redesigned): count badge "×N" on each group's first
  frame + optional thin two-tone bottom strip; NEVER a full-perimeter
  border (cursor/selection own borders). See burst-grouping.md UI
  contract.
- Selection (wash added 2026-07-28 on user request, persona-reviewed
  MUST-HAVE): a translucent **accent-blue wash over the whole cell**, plus
  the existing accent outline; multi-select via Ctrl/Shift-click and
  Shift+arrows. Rationale — the selection is what the **IPTC panel** stamps
  (`Selection::batch()`; field commit/clear, keyword add/remove, template
  apply), so it can write metadata across hundreds of images at once, and
  the 2px outline alone was unreadable at 8–12 columns, leaving that reach
  invisible. **Marks (`Y`/`N`/`U`) are deliberately NOT batch operations** —
  they act on the cursor image only, per the marking rules' "net cursor
  movement per mark is exactly one image, always" (the same incoherence of
  advancing after a multi-image action is recorded separately for keyword
  commit, decision G4); do not let the wash's presence suggest otherwise.
  A filled area is
  the only selection indicator whose legibility does not shrink with the
  cell. The wash also makes selection and cursor ORTHOGONAL channels —
  filled = selected, bright border = cursor, both compose on one cell —
  replacing the old "two blue borders differing only in width" language.
  Acceptance criteria:
  - The wash renders on **every** selected cell **including the cursor
    cell**. (The pre-wash rule was `selected && !is-cursor`, which hid the
    selection state of the one cell whose batch membership is genuinely
    ambiguous: per `batch()`, with a non-empty selection the cursor is in
    the batch only if it is itself selected.)
  - **GRID ONLY — never in the loupe**, at fit or above ("in the loupe I am
    judging pixels"). Note the loupe fit view IS the grid at one column, so
    this requires an explicit gate on `at-fit`/`one2one`, not just placement.
  - Painted above the image and above the 40% reject dim, but BELOW the
    badges, so ★ / ✕ / ×N / ✓ / ! stay legible on a selected cell.
  - Hue and strength are **properties, not literals** (`selection-wash`,
    `selection-wash-opacity`; Rust owns the defaults). User decision
    2026-07-28: strength is 25%, chosen by eye against 12% and 18% renders,
    and is destined to become a user setting — a settings pane writes the
    property and no rendering code changes. Recorded persona caveat: above
    ~15% the tint is strong enough to shift colour judgement on a final
    pre-`N` scan; the user accepted this trade knowingly, and the variable
    is what makes it revisable.
- Selection count in the status bar (persona MUST-HAVE companion to the
  wash): `· N selected` whenever the selection is non-empty, counted over
  the view so it matches `Selection::batch()` exactly — computed by
  `Selection::count_in_view()` in fastcull-core (rule 5: the semantics live
  in core, the app only renders them), with a unit test pinning the two
  together. An empty selection is silent — the batch is then just the
  cursor, and "1 selected" on every image would be noise. The wash says
  WHICH images the IPTC batch covers; the count says HOW MANY, including
  selected images scrolled off-screen, which no on-cell indicator can
  convey. Images selected but filtered OUT of the view are excluded from
  both, matching "what you see is what you stamp".
- Failed file: warning badge + tooltip with reason.

## Keyboard map (keyboard-first is a feature)

| Key | Action |
|---|---|
| Arrows / PgUp / PgDn / Home / End | navigate (grid and loupe) |
| `Y`, `P` or `Space` | pick (take) |
| `N` or `X` | reject |
| `U` | clear mark |
| `+` / `-` | zoom in/out (grid columns → loupe fit → ×1.5 ladder → 1:1, center-anchored; see Loupe zoom ladder; Ctrl+scroll stays RESERVED per the Mouse & pointer contract) |
| `Z` | from fit: jump to 1:1; from 1:1 or any intermediate factor: back to fit; from a grid zoom: jump straight to loupe 1:1 |
| wheel | grid: scroll the view; loupe: zoom one ladder stop, anchored under the pointer (down at fit does nothing; the wheel no longer steps between images) — see Mouse & pointer contract |
| click (loupe) | above fit: center on the clicked point (no factor change); at fit: nothing |
| double-click (grid) | open that image in the loupe at fit |
| double-click (loupe) | 1:1 with the clicked point centered |
| drag | grid: scroll; loupe above fit: pan the image |
| `G` or `Esc` | back to the grid at the previous grid zoom (from loupe/1:1) |
| `I` | toggle IPTC panel |
| `K` | focus the keyword field, opening the IPTC panel if needed (persona G3; implemented with the panel step — K is never a dead key) |
| Shift+arrows | extend selection (span anchor..cursor over view positions; a new span replaces the previous one — shrink/flip works) |
| `Ctrl+A` | select all (filtered set) |
| `[` / `]` | burst boundary jump (M7): `]` = next frame whose group differs (in a contiguous capture-sorted view that is the next group's first frame; with non-contiguous members it follows view order); `[` = re-anchor on the current group's first visible frame, crossing to the previous group only from there (CD-player convention); claims the cursor; carries loupe zoom/pan persistence; see burst-grouping.md |
| `Ctrl+O` | Open Folder… (persona accelerator gap, provisional) |
| `Ctrl+Q` | Quit (persona accelerator gap, provisional) |
| `Ctrl+E` (menu: Copy picks…) | open copy dialog (`Ctrl+C` stays clipboard-idle: user decision after persona review — never repurpose it) |
| `1`–`5`, `0` | reserved (star ratings, v2) — must not conflict |

There is no undo stack in v1 (user decision): a mis-marked frame during
auto-advance is fixed with arrow-back + re-mark, which costs one keystroke.

Picking (`Y`) or rejecting (`N`) auto-advances the cursor to the next image
at EVERY zoom level — grid and loupe alike (user decision 2026-07-25: "once
I select Y or N, the UI should automatically move to the next image").
Clearing (`U`) does not advance. This becomes a configuration option
(default: on) when the settings dialog lands (File menu placeholder,
post-v1); until then it is always on.

**Advance/removal composition (persona gap G1, 2026-07-25 — the rule that
keeps the inbox-zero loop honest)**: when a mark removes the image from the
active filtered view, the live-removal cursor rule IS the advance —
auto-advance must NOT apply on top of it. Auto-advance applies only when
the marked image stays in the view. Net cursor movement per mark is exactly
one image, always.

## Window chrome (menu bar — user-requested 2026-07-24, lands M5)

Slim native menu bar (Slint MenuBar); the keyboard remains the fast path —
menus are discoverability, never a required route:

- **File**: Open Folder… (native picker via `rfd`; replaces CLI-only launch),
  Copy Picks… (`Ctrl+E`, enabled from M6), Settings… (placeholder entry,
  disabled until a settings dialog exists — post-v1 candidate), Quit.
- **View**: Zoom In/Out (`+`/`-`), IPTC Panel (`I`, from M5), Filter Bar.
- **Help**: Keyboard Shortcuts (small popup listing the keyboard map — the
  map in this spec is the source of truth), About.

Acceptance: opening a folder via the menu behaves identically to the CLI
argument (same session path); the shortcuts popup lists every binding in this
spec and closes with Esc. Persona (almost-human-user) reviews this section at
M5 implementation start per the gate.

**Folderless launch (user requirement 2026-07-25, issue #5 — IMPLEMENTED
2026-07-26)**: `fastcull-app` with NO arguments must open the normal
window in the empty state with the message "No folder open — File > Open
Folder… (Ctrl+O)" and a working menu bar — never exit with a usage error
(a desktop launcher / double-clicked binary has no arguments; printing
usage to a terminal nobody sees and exiting is a broken first run). The
"No folder open" message is distinct from the "No images" message of a
folder that opened empty (`session_open` flag). The CLI usage error
remains for genuinely malformed invocations (unknown flags, nonexistent
folder). Screenshot test: `no_args_launch_opens_empty_window`. On Windows
the app is a GUI-subsystem exe (issue #40, 01-architecture.md), so those
usage errors are by design invisible on a double-click launch — which is
exactly why the no-argument path must open a window instead of printing;
launched from a terminal, the errors still print via the parent-console
attach.

Chrome staging (updated with the panel step): IPTC Panel menu item, `I`,
`K`, Shift+arrows and `Ctrl+A` all landed; the popup lists them live.

**About dialog (issue #23, implemented 2026-07-27 — replaces the
About→shortcuts placeholder)**: Help > About opens a dedicated modal
(same scrim/close pattern as the shortcuts popup: Esc or click outside;
clicks on the card never close it). Content, user-directed: "FastCull"
with the version on its own line beneath it, the two-sentence
description, "Main contributor: Danilo de Paula", the repository URL as
plain RETYPE-ABLE text (no URL-opener dependency in v1; the URL must
never wrap or ellipsize), and the
license line "GPL-3.0-or-later" — moved here from the shortcuts footer
(its intended home per the M5 deferral). The version string is composed
by the BUILD, never hand-maintained: `X.Y.Z` when HEAD sits exactly on
the release tag `vX.Y.Z`, **`X.Y.Z-devel-YYYYMMDD-<short-hash>`**
otherwise (a bug report from a dev build must pin the commit); no git
(tarball build) falls back to plain `X.Y.Z`. Traced at startup
("about version ...") for headless assertions, and asserted by
`about_dialog_renders_and_contains_the_keyboard` as a SHAPE: off a release
tag a `-devel-` suffix is MANDATORY (CI checks out shallow with no tags,
so it is always off-tag — without this the suffix could vanish entirely
and the test would still pass), the date must be 8 digits when present,
and the dateless fallback must still be a bare hex hash.
Recorded gaps, mutation-measured: the test proves a date is present and
well-shaped but not WHICH date — swapping the committer date for the
author date ships a string wrong by years and stays green; the dateless
fallback and the two-line split are likewise unpinned. Killing the first
wants a fixture repo with divergent author/committer dates, which is
heavier than the defect.

**Date in the devel suffix (issue #26, user decision 2026-07-31)**: the
hash says WHICH code is running, the date says HOW OLD it is — without
anyone having to look the hash up, which is the point of a string people
paste into bug reports. Date before hash so builds from one branch sort
chronologically, and compact `YYYYMMDD` because dashes inside the date
would stop reading as separators.
- It is the COMMIT date, not the build date: reproducible — the same
  commit always yields the same string, and two people on that commit
  report the same version. QE confirmed the reproducibility claim is
  timezone- and locale-independent (git renders the commit's own recorded
  offset), and that the date and the hash always move together, since both
  come from one build-script run against one HEAD.
  Precisely: the date does not go stale in any case the HASH would not.
  When `build.rs` does not re-run, the binary reports the previous
  commit's date AND hash — self-consistent, but describing code that is
  not running. `build.rs` therefore also watches the TAG refs, because
  `git tag vX.Y.Z && cargo build` used to leave a `-devel-` string in a
  release binary (it did, at 0.5.0). Remaining hole, recorded: a developer
  who sets a global `CARGO_TARGET_DIR` shared between two checkouts of the
  same version gets whichever build-script result cargo cached; `cargo
  clean -p fastcull-app` fixes it. Pre-existing since #23 and identical
  for the hash alone.
- Specifically the COMMITTER date (`%cd`), not the author date: a
  rebased or cherry-picked commit keeps its original author date, which
  would describe when the code was first written rather than when the
  commit being run came into existence.
- A hash with no usable date still yields `X.Y.Z-devel-<hash>`; the date
  is additive and never costs the hash.
- **The title line is split in two** ("FastCull", then "version …" at
  13 px with `wrap`). That Text had no `wrap:` while its neighbours did,
  and Slint clips an unwrapped Text from the RIGHT — precisely where the
  commit hash sits, the one part of the string a bug report cannot afford
  to lose (persona review). Measured honestly: today's string occupies
  ~365 px of the 444 px content box, so it would NOT have clipped yet —
  the split is precaution against the next thing that lengthens the
  suffix, not a fix for an observed truncation. `wrap` does not by itself
  prevent a mid-token break (Unicode line breaking allows one after a
  hyphen); the width margin is what does.
  The card grew to 348 px with tighter padding to hold the extra line. QE
  verified the version renders complete at every size down to 480x320,
  including with `core.abbrev=20`. Accepted below ~360 px window height:
  the redundant "Esc or click outside to close" hint spills outside the
  card. That is narrower than the About card's own content and far below
  any usable culling window; the version string itself never clips.

**Modal keyboard containment (issue #23, user decision "swallow
everything in that screen")**: while About OR the shortcuts popup is
up, Esc closes it and EVERY other key is swallowed — the old Esc-only
guard let N/Y/arrows act on the grid under the scrim (persona
IN-MY-WAY: a stray N while reading About must never reject a photo;
the shortcuts popup was the worse leak — the popup a new user has open
while experimentally pressing keys). Driven NAV keys (`FASTCULL_DRIVE`)
are contained identically, or the containment tests would test
nothing. Debug facilities gained `about` and `shortcuts` toggle
actions for those tests. Containment mechanics (validator findings on
the first cut): the popups are declared LAST in the element tree so
their scrims render above every layer — the old order left the IPTC
panel clickable ON TOP of an "open" modal; opening a modal steals the
keyboard back to the main key scope (a focused panel LineEdit is a
sibling of that scope and would otherwise keep eating keys, including
the closing Esc); the scrims swallow wheel events (the grid must not
scroll under a modal). The MENU BAR stays live while a modal is up
(File > Quit works) — standard desktop behavior, deliberate.
**Esc closes the TOPMOST modal only (issue #42)**: with About or the
shortcuts popup opened over the live copy dialog (the live menu makes
that reachable), keyboard focus stays in the dialog's scope, whose own
containment branch closes the popup on top first — the dialog and its
destination/plan state survive, and the next Esc closes the dialog;
marks stay contained throughout. The previously recorded "known cost"
here — that Esc would reach the copy dialog's scope so About closes by
click-outside — understated reality: Esc actively closed the HIDDEN
dialog and threw its plan state away. Both key scopes now contain
modals identically. Opening a modal over a FOCUSED panel field steals
the keyboard in a way that survives the menu's post-activation focus
restore — see Focus continuity in the Filter & sort bar section.

## Filter & sort bar (M5 decisions recorded 2026-07-25)

- Filters: SINGLE-choice chips — All / Picked / Rejected / Unmarked (user
  decision: single choice is enough; combinations dropped). The in-burst-only
  chip was CUT at M7 kickoff (persona IN-MY-WAY, user-delegated: chips
  are single-choice, so it would trade away Unmarked and break the
  inbox-zero loop; the `[`/`]` burst-jump keys serve the actual need).
- Sort: capture time (default) ↑↓, filename ↑↓.
- Implemented as pure predicates in `fastcull-core::filter` over the session;
  the grid binds to the filtered+sorted view. Counts shown per filter state.
- **Filtered-view mutation rules (blocking spec gap closed pre-M5)**: marking
  an image so it no longer matches the active filter removes it from view
  LIVE; the cursor lands on the next image in the filtered view (else the
  previous, else none); counts update immediately. When the filter itself
  changes, the cursor goes to the nearest surviving image, else the first.
  The inbox-zero loop (filter Unmarked, Y/N until empty) must work exactly.
- **Focus containment (blocking spec gap closed pre-M5)**: while ANY text
  field has focus, no single-key shortcut fires — typing "Xavier" must not
  reject a photo. Enter commits the field and returns focus to the grid.
  (The original wording here promised "Esc in a field abandons the edit;
  second Esc acts on the panel/view" — that never shipped: Slint's
  LineEdit offers no Esc hook in v1, so Esc in a field is a no-op. The
  recorded deviation lives in iptc-templates.md's panel-step ledger; the
  field exits that DO exist are listed there and in Focus continuity
  below. Corrected 2026-08-03, gate finding — the stale promise
  contradicted the ledger and docs/metadata.md.)
- **Focus continuity (issues #41/#42)**: the inverse guarantee —
  whenever the focused editor is DESTROYED (panel close via any route,
  session swap) or COVERED (About/shortcuts opening over the panel or
  over the copy dialog, and the copy dialog itself opening over a
  focused panel field via File > Copy Picks — the strand RUN14 showed
  held only by init-timing luck), keyboard focus deterministically
  returns to the topmost surface's key scope. Never a dead keyboard,
  never keys eaten by an invisible editor: pre-fix, closing the panel
  from the menu left focus on NO element (at 1:1 with no discoverable
  recovery), and a modal opened over a focused field was un-dismissable
  while every keystroke landed invisibly in the field — committable as
  metadata. Text disposition: a DESTROYED editor DISCARDS its
  un-committed text (user decision 2026-08-03: no commit-on-destroy); a
  COVERED editor exits like click-away — the text commits. The covered
  case is NOT a new decision: it preserves the G7 semantics that
  already shipped (opening a menu blurred the field and committed
  exactly this way pre-fix — RUN17), stated here so the asymmetry with
  destroy is deliberate and on the record. A session swap
  additionally invalidates every in-flight edit by generation stamp
  (editors stamp the session generation on focus gain; a blur commit
  from a stale stamp discards), so the old session's half-typed text can
  never be committed against the new session's images — the stamp, not
  timing, is the guarantee, because the swap leaves the keyword editor
  alive and the focus steal blurs it after the swap. Mechanics (the
  menu is the hard case): Slint's MenuBar restores focus to the
  previously-focused element AFTER the item activation runs, so a
  synchronous steal inside an activation is overridden — the app
  re-claims focus QUEUED behind the current event dispatch (a
  zero-length timer cannot fire until the dispatch containing the
  menu's restore has unwound), and the panel/copy editors additionally
  BOUNCE any focus gain arriving while a modal covers them (belt and
  braces, both deterministic). The 1:1 loupe click surface claims the
  keyboard like every other click surface (defense in depth; the click
  still re-centers — grid and loupe click semantics are unchanged, by
  user decision).
- Per-image keywording is a same-evening flow (user decision): a focus-jump
  key into the keyword field, comma-separated entry, Enter commits + returns
  to the grid. Batch-apply perf target: picks-scale (hundreds), not
  whole-folder (user decision).
- No Open Recent / save-template-UI / filter hotkeys in M5 (user decision:
  keep it minimal; templates.toml is hand-edited in v1).

**Persona-review defaults adopted 2026-07-25 (user AFK; provisional until
the user confirms, all cheap to change):**
- **Inbox-zero empty state (G2)**: when the filtered view empties, the grid
  shows an empty-state message with final counts ("0 unmarked — N picked,
  M rejected"), no cursor. If it happens while in loupe, the view drops
  back to the grid empty state. The cursor contract's "exactly one cell"
  rule applies only to non-empty views.
- **Keyword commit (G4) — FINAL (persona verdict adopted by the user
  2026-07-25 after PM research)**: Enter commits + returns focus to the
  grid; cursor STAYS. PM's Save-and-advance was examined and rejected:
  stacking advance sources breaks the K→type→Enter→Y loop (the Y would
  mark the wrong frame), and advance is incoherent on a multi-selection.
  The future config option is worded as commit-and-advance-AND-KEEP-FIELD-
  FOCUS (the true PM caption loop) — advance without focus retention is
  the version nobody wants.
- **Template apply UI (G5)**: the IPTC panel shell includes a template
  dropdown (reading templates.toml) + Apply button + "Revert last apply"
  button — templates that cannot be applied from the UI are dead weight
  (persona). Revert semantics per iptc-templates.md (single level).
- **Filter-bar hide (G6)**: hiding the bar (View > Filter Bar) resets the
  filter to All — a filter must never be active while invisible.
- **Field edge cases (G7)**: click-away from a half-typed field commits
  (same as Enter, without the focus return); Tab cycles panel fields;
  default sort is capture time ascending.

## Acceptance criteria

- [x] `filter.rs` unit tests: every filter/sort combination over a synthetic
      session, counts included.
- [ ] Windowed-model tests (core side): visible-range → model-window computation,
      incl. partial rows, tiny folders, and N=1.
- [x] **Menu bar readable under any desktop colour scheme** (dark-only
      palette pin): `menu_bar_labels_survive_a_light_scheme_desktop` forces
      the failing scheme-resolution branch deterministically (an
      unreachable session bus → portal unreadable → Unknown → fluent picks
      the LIGHT palette) and asserts light glyph pixels over the dark bar,
      with an anti-vacuity check that the bar itself is still the app's
      dark chrome. Mutation-verified: removing the `Palette.color-scheme`
      pin yields "only 0 bright pixels" and FAILS. NOT `dbus-run-session`
      — an isolated bus auto-starts a fresh portal that re-reads the real
      desktop setting and passes vacuously (QE 2026-08-02).
- [x] **Transit vs settled** (user requirement 2026-08-01): a held key is
      distinguished from deliberate taps and decays on release
      (`transit_tracks_held_keys_and_decays_on_release`); the request while
      moving is a rung the mid actually serves, and the old 2048 value still
      fails (`transit_request_is_served_by_the_mid_rung`); the ring leans the
      way the user travels and clamps at both folder edges
      (`transit_ring_leans_in_the_direction_of_travel`); a settled frame
      climbs even though transit only asked for the mid, without duplicating
      an in-flight job and without spinning
      (`a_settled_frame_climbs_even_though_transit_only_asked_for_the_mid`);
      the settle poll does not disturb LRU order
      (`the_settle_guarantee_does_not_disturb_the_lru_order`). Through the
      PUBLIC api, so that disabling transit at the call site fails:
      `a_held_key_reaches_transit_through_the_public_api` and
      `a_backward_hold_keeps_leaning_backward_across_refocus`, the latter
      simulating the app's same-index re-focus storm.
      NOT covered by a test: the measured performance figures themselves —
      nothing turns red if the frames-on-screen or stop-to-sharp numbers
      regress (see issue #27 on the perf-budget rule).
- [x] **No fit-drop, no fling, no phantom fold (issue #46)**. Core: the
      ring maps view positions to ids and back
      (`the_prefetch_ring_walks_view_order_not_id_order`), the direction
      latch compares positions
      (`travel_direction_is_latched_in_view_positions`), deferred
      revival uses the same ring
      (`deferred_revival_ring_follows_view_order`), and the public api
      proves the ring decodes view neighbors and NOT id neighbors
      (`prefetch_follows_the_view_order_through_the_public_api`).
      App level, driven through real dispatched pointer/key events and
      dump/trace state (pixels are useless here — a far-panned 1:1
      snapshots black, and a wrong-position frame is a state nothing
      re-renders): a cook-widened cold jump keeps `one2one` and the
      carried pan centre and renders the thumb rung
      (`transit_to_a_cold_frame_keeps_the_overlay_at_the_carried_center`);
      drag pans 1:1 and folds into the centre, release stops the image
      dead, and navigation after a flick keeps the drag-carried centre
      with zero `pan fold` traces
      (`loupe_drag_pans_one_to_one_and_a_fling_never_survives_navigation`);
      paced taps over an interleaved-id session land warm — no overlay
      drop, no thumb-rung rescue needed
      (`paced_taps_over_an_interleaved_session_land_warm`). Every
      bug-shaped assertion red-run-verified against the pre-fix build
      (6d15ed1 + the drive-harness commit, release profile — the
      profile the 5/5 and 3/3 reproductions were proven in); the slow-
      drag half of the M3 test guards the surviving 1:1-pan contract,
      and its fold-at-drag-time assertion is ALSO red on pre-fix code,
      which deferred the fold to the next input. Recorded limitations: the F2 warm-landing pin (zero
      thumb-rung rescues at a 600 ms cadence) binds in RELEASE builds
      only — a debug build decodes a mid slower than the cadence, so
      the test skips that one assertion there (the perf_budgets
      precedent) while its no-drop and `one2one` assertions still
      bind; the M1 test and the failed-cursor gate test run in RELEASE
      only outright — in debug both ride the app's own 60 s
      screenshot-readiness cap (the cursor's 50 MP debug decode landed
      at 58.5 s on a loaded 8-core laptop; a 2-vCPU CI runner under
      the cook hold has no margin at all — validator, gate round 2),
      while the debug profile keeps its no-drop coverage through
      `paced_taps` and `transit_at_zoom_stays_soft`; the M1 test
      allows the spec'd reason-carrying drops (failure/hold-cap) while
      asserting the excuse-less `(no rung in hand)` drop away, plus
      recovery via the late "landed" dump. The failed-cursor gate is
      pinned by
      `a_decode_failed_cursor_drops_to_fit_instead_of_masking_the_badge`
      (mid-session corruption — a helper thread kills the file on disk
      after its thumb reached memory; red-run-verified against the
      pre-gate build: the thumb rendered on every visit and the
      `(decode failed)` drop never appeared). The wheel wiring the
      restructure touched is
      pinned by `overlay_wheel_still_zooms_one_stop_per_notch` (real
      dispatched scroll events via the `wheel.` token; a guard — wheel
      SEMANTICS did not change, only the surface wiring — non-vacuous
      because a dead scroll path leaves the factor at 1.0; covers both
      accumulators and the fit→overlay handoff). Still without a
      deterministic release-profile exercise (recorded, QE gate): the
      `(hold cap)` drop-and-re-raise fires routinely in debug runs and
      the M1 test asserts the recovery whenever it fires, but forcing
      it deterministically in release needs a decode-wedge knob —
      deferred alongside the wedge affordances already recorded in
      this spec.
- [x] **Provisional order while loading** (issue #25):
      `filter::provisional_order_is_stable_while_metadata_streams` feeds the
      capture keys in one at a time and asserts the view is IDENTICAL at
      every step, then that the real sort applies once — mutation-verified
      (ignoring `metadata_complete` fails it at the first key), and
      non-vacuous by construction (it asserts the settled order actually
      differs from the provisional one).
      `filter::provisional_order_still_respects_the_filter_and_direction`
      pins that the override touches the SORT only, never membership or
      direction. The RE-ANCHOR's rule is pinned by
      `grid::resort_reveals_a_watched_cursor_and_spares_a_browsing_one`,
      also mutation-verified, and the user's cursor rule by
      `filter::engine_events_stop_moving_an_untouched_cursor_once_loaded`.
      End-to-end, the two-file `cursor-order` fixture pins it through the
      flip AND through later engine events
      (`engine_events_after_loading_never_move_an_untouched_cursor`) — that
      fixture used to assert the OPPOSITE (issue #4's capture-first open)
      and was inverted by the user's decision. Recorded gap: the completion
      predicate and the mark path still have no automated test — an
      end-to-end one must catch the app mid-load, and a screenshot test that
      tries dies in the profile CI actually runs it in: a 400-file attempt
      passed in DEBUG having never finished loading, and `place_fixture`
      COPIES on Windows, so it would have written ~33 GB per run (validator,
      2026-07-31). Verified manually against 3,000-file fixtures instead;
      making the load's completion point injectable is the way in.
      QE enumerated the surviving mutations (2026-07-31); two were closed
      structurally rather than merely tested — burst grouping and Copy Picks
      now call `filter::view_true_sort`, a named function whose own test
      pins that it ignores the provisional order, instead of passing a bare
      `true` that reads as "metadata is complete" at the two places where it
      emphatically is not. **Four survive and are accepted, not fixed**:
      - `AppState::metadata_complete()` forced to `true` — i.e. the whole
        feature off — leaves the suite green. The end-to-end test cannot see
        it: with the flag true from the first refresh the cursor never
        follows the head at all, so it reaches the same end state by a
        different route. Catching it needs an assertion on the LOADING
        status string, which needs a fixture still loading at shutter —
        several hundred real RAWs, which `place_fixture` COPIES on Windows.
      - counting only `MetadataReady` instead of finished jobs (a file whose
        EXIF read fails would then strand the session forever).
      - `user_changed_query` forced to `false` at both user call sites: the
        issue #4 exception has no end-to-end coverage, because the filter
        chips and sort control are click-only in Slint with no drive action.
      - `last_cursor_visible` forced either way: the app-side sampling that
        decides reveal-vs-spare is untested, which is exactly where the
        guard was missing on the first cut.
      The status bar's LOADING form is likewise unasserted (only the
      completed form appears, as an anti-vacuity check).
      `FASTCULL_DRIVE scroll:N` exists for the manual and QE verification of
      the browsing case and is deliberately kept despite having no test
      caller — it is the one gesture the harness could not otherwise
      express. The way to close all of these at once is an injectable
      load-completion point, not more screenshot fixtures.
- [x] **The loupe fit view shows the WHOLE frame** (One-column cell bounding):
      `grid.rs` units pin that the N=1 cell never exceeds the viewport and
      that revealing it leaves nothing below the fold, while multi-column
      cells keep their 3:2 aspect; the end-to-end pin is the screenshot test
      `loupe_fit_shows_the_whole_frame_not_a_crop`, which measures the
      RENDERED photo's aspect (a 3:2 frame drawn at ~1.8 is a crop) and
      requires pillarbox bars on both sides. Both were verified BY MUTATION,
      not by passing: disabling the bound yields "aspect 1.807 (1429x791)"
      and FAILS. The 29 pre-existing screenshot tests all passed on both
      sides of this change — mean luma and centre-region variance cannot see
      a crop, which is how it shipped unnoticed since M2.
- [x] Pointer state machine (core side, issue #11): a table-driven test that
      enumerates EVERY (state, input) pair of the Mouse & pointer contract
      table and asserts the resulting state + action — including the
      reserved no-ops. Plus: wheel-up at fit anchors the pointer's image
      point (not the center), wheel notches land on the same `1.5ⁿ` stops as
      `+`/`-`, wheel-down at fit is inert, clicks outside the image rect do
      nothing, and pan offsets stay clamped to the image bounds at every
      factor (`fastcull-core/src/pointer.rs` tests). The anchor assertions
      use an OFF-CENTRE pan and pointer: with everything at `(0.5, 0.5)` the
      pointer anchor and the centre anchor coincide, and QE proved by
      mutation (2026-07-30) that three criteria were consequently vacuous —
      the zoomed wheel anchor, "wheel-down landing on fit forgets the pan",
      and the drag's vertical axis all survived being deleted. Covered
      OUTSIDE the core tests, recorded honestly: "a drag suppresses the
      click" is Slint's TouchArea click definition (press+release without
      movement); "a distant second click is two re-centers, not a
      double-click" is enforced by SLINT, whose `check_repeat` restarts the
      click count beyond 10 logical px — the app deliberately holds no
      proximity state of its own (see the deviations above for the guard
      that was deleted); "`+` after a click at fit is still center-anchored"
      holds by construction (a fit click stores nothing at all — it only
      claims the cursor — and the keyboard ladder reads no pointer state).
- [x] **Double-click reaches 1:1 from ABOVE fit**, not only from fit
      (`loupe_double_click_above_fit_reaches_one_to_one`). This is the
      gesture that shipped broken through both gates, and it broke in the
      bridge, where no core test could see it: the `FASTCULL_DRIVE`
      `dblclick:x,y` action replays Slint's real ordering (a `clicked` that
      re-centers, then `double-clicked` on the same release) so the class of
      defect is reachable from a test at all. It does NOT make the pointer
      ROUTING testable — which Slint surface receives a physical wheel or
      press is still review-verified only, and remains the case for the
      pointer-injection harness (issue #13).
- [x] Slint screenshot smoke tests (`fastcull-app --screenshot <out>` +
      `tests/screenshot.rs`): grid placeholder (synthetic), loaded thumbnails
      (texture-variance asserted), failed-badge session, loupe fit
      (`--start-loupe`) and 1:1 (`--start-11`), and the IPTC-panel-open
      docking state (issue #12 regression: left edge stays grid content,
      right strip becomes panel; reached via the `FASTCULL_DRIVE`
      `iptc` action). Recorded limitations:
      snapshots are always JPEG q92 regardless of extension; `--screenshot`
      forces the software renderer (take_snapshot yields black frames on the
      GPU renderer), so these tests do NOT exercise the shipping femtovg
      renderer — GPU-specific visual regressions need eyes or a future
      GPU-capture harness. Tests set FASTCULL_NO_CACHE for hermeticity.
      **A far-panned 1:1 view snapshots BLACK** (QE finding F1, 2026-07-30):
      Slint's software renderer stores the source offset as `Fixed<u16, 4>`
      (`i-slint-renderer-software`, `scene.rs`), so beyond ~4096 px of pan on
      either axis the frame renders empty — bracketed at −4080 px (renders)
      vs −4160 px (black). A pixel assertion on a far-panned 1:1 view would
      therefore pass vacuously on black; assert on the TRACE instead, as
      `loupe_double_click_above_fit_reaches_one_to_one` does. Believed
      renderer-local (the shipping femtovg path is unaffected), but that is
      unproven — no window capture was available to check it.
      **A `--start-11` run whose FINAL cursor is a decode-FAILED image
      while the desire is above fit trips the 60 s readiness cap and
      exits 1** (QE, issue #46 gate round 2): the 1:1 readiness gate has
      no failed-cursor escape, unlike the fit gate. Loud, arguably
      intended — but a drive script that visits a failed image at 1:1
      must END on a decodable cursor, as the failed-cursor gate test
      does.
      Also not drivable headlessly: the `✓ copied` badge, because Copy Picks
      opens a native folder dialog and `FASTCULL_DRIVE` has no copy action
      (QE G2). It is covered only by sharing the bottom-anchored band with
      the `×N` burst badge, which IS rendered on screen by a real fixture.
- [x] **Focus continuity (issues #41/#42)**: driven through REAL key and
      pointer dispatch (`key:`/`click.` — the nav tokens bypass focus and
      cannot see this class), every bug-strand test red-run-verified
      against the pre-fix build. Panel close from the menu keeps the
      keyboard at 1:1 (`panel_close_from_the_menu_at_one_to_one_keeps_
      the_keyboard` — the user's live hit) and in the grid; a modal over
      a focused field owns the keyboard and writes NOTHING — no sidecar
      on disk, revert never armed (`modal_over_a_focused_field_owns_the_
      keyboard_and_writes_nothing`); a session swap mid-edit discards
      and keeps the keyboard, both for a destroyed field editor and for
      the surviving keyword editor — the latter pins the cross-session
      write the fix's first cut produced (`session_swap_mid_keyword_
      edit_never_writes_into_the_new_session`); Esc over stacked modals
      closes the topmost first with the copy dialog's plan surviving
      verbatim (`esc_over_stacked_modals_closes_the_topmost_first`); a
      1:1 loupe click claims the keyboard (`one_to_one_click_claims_the_
      keyboard`). Clean paths guarded: menu activation with keys
      focused, the G4 Enter commit (which also pins the edit-generation
      stamping — an init-time focus gain fires no `changed has-focus`,
      and an unstamped editor once silently discarded committable text),
      the copy-dialog Esc lifecycle, the filter-bar toggle mid-edit
      (menu-open is a G7 click-away exit: the text commits), and File >
      Copy Picks over a focused field — the RUN14 luck strand, now
      backed by the same deferred claim as the modals
      (`copy_picks_from_the_menu_over_a_focused_field_owns_the_
      keyboard`; a guard, green on both sides of the hardening). Recorded
      limitation: the menu-click tests are calibrated for the Linux
      runners' font metrics and SKIP on Windows — the focus machinery is
      platform-independent Slint core, and every non-menu strand still
      runs there; each menu test asserts an intermediate state that
      fails loudly if a click misses, so font drift cannot make one pass
      vacuously.
- [ ] Manual acceptance (per release): 5,000-file A1 folder (a bad evening, per
      persona review) scrolls at 60 fps after thumbs load; pick→auto-advance→pick
      loop in loupe has no perceived latency.

## Debug facilities (env vars, app-level)

Documented because they ship in release builds (validator finding):

- `FASTCULL_TRACE=1`: eprintln any UI-thread phase (`handle_nav`, `refresh`
  stages, texture adoption) exceeding 20 ms, plus loupe-ready marks — the
  evidence channel for hang reports.
- `FASTCULL_DRIVE="6000:one2one;9000:grid;12000:quit"`: timed injection of
  nav actions (same names `handle_nav` takes, plus `quit`, `iptc` — the
  panel toggle, issue #12 — `about`/`shortcuts` — the modal toggles,
  issue #23 — and `resize:WxH` in logical pixels, issue #16: the
  wrong-photo-after-resize bug class needs real window resizes
  drivable or it ships regression-blind) for headless reproduction and
  QE runs — Wayland offers no external input automation. Driven NAV
  keys respect the modal containment exactly like real keypresses
  ("drive swallowed by modal" trace); `quit`/`iptc`/`resize` and the
  modal toggles themselves remain live harness plumbing, like the menu
  bar.
  `scroll:N` browses the grid to offset N logical px WITHOUT claiming the
  cursor — what the wheel does natively, and the one gesture the harness
  could not express, which is why a re-anchor that hauled a browsing user's
  viewport back reached review unnoticed (2026-07-31).
  `open:PATH` (issue #34) is the Open Folder menu action minus the native
  rfd dialog: it calls the same shared function the menu callback calls
  (session swap, kitchen retarget, pipeline/loupe restart, marks flush,
  fresh grid zoom), so a script can drive an app-level session swap
  mid-operation — the path the texture-kitchen gate found review-verified
  only. Like the menu bar it stays live while a modal is up (harness
  plumbing, not a nav key). The path is everything after the first colon,
  so a folder whose path contains `;` cannot be scripted (recorded
  limitation of the `;`-separated script format). Caveat for script
  authors (QE G2, 2026-08-02): the `--screenshot` readiness gates do not
  re-arm for the swapped-in session — a shutter that was already
  satisfied can fire while the new folder is still loading — so a script
  that needs the post-swap session settled must hold the shutter with a
  late trailing action (the drives-pending wait), as the issue #34 tests
  do.
  Malformed entries are skipped silently. Scripts may include mark actions
  (`pick`/`reject`), which write real sidecars — QE runs target throwaway
  copies of test data only.
  The `--screenshot` shutter WAITS for the whole drive script to have
  executed before it may fire (in addition to its readiness gates): a
  fast release build otherwise reaches readiness before late-scheduled
  actions run and captures a half-driven state — the same script must
  mean the same shot in every profile (found 2026-07-27 when
  settle-then-pin drive schedules moved past the 1.5 s floor).
  `key:<k>` / `key:ctrl+<k>` (issue #41 sweep, promoted from QE
  instrumentation) dispatches a REAL key press+release through
  `slint::Window::dispatch_event` — through the true focus system, which
  the nav tokens bypass (they call `handle_nav` directly, so they are
  blind to the whole stranded-keyboard class: only a dispatched event can
  land on no element). Named keys: `escape`, `return`, `tab`,
  `left`/`right`/`up`/`down`; anything else is sent as literal text
  (`key:k` types k). `ctrl+` synthesizes a held Control around the press.
  `click.X,Y` dispatches a real pointer move+press+release at
  window-logical coordinates, hit-tested by Slint — this makes the
  in-window menu bar drivable headlessly, including the menu's own focus
  save/restore machinery, plus panel fields and modal scrims. (Spelled
  with a dot: the visual break from the step's `MS:ACTION` colon keeps
  scripts readable.)
  `press.X,Y` / `move.X,Y` / `release.X,Y` (issue #46; promoted from QE
  instrumentation like `key:`/`click.` before them, the PR #43
  precedent) are `click.`'s three phases as separately SCHEDULABLE
  steps — real dispatched pointer events that, spread over timed script
  steps, carry real inter-event timing, which is what makes a drag a
  drag: `click.`'s single-tick sequence has zero displacement and zero
  velocity, so no drag gesture (and no drag-derived defect class — the
  issue #46 fling was exactly one) was drivable headlessly before
  these. `press.` dispatches a move first so hover state is coherent,
  like `click.`; a `move.` while pressed extends the drag; scripts are
  responsible for pairing press/release (an unpaired `press.` leaves
  the button down, exactly like a real stuck button — that fidelity is
  the point).
  `wheel.X,Y,DY` (issue #46 gate finding) dispatches a REAL scroll
  event at window-logical coordinates, `DY` in logical px (60 = one
  notch-equivalent per this contract's accumulator; positive = up),
  preceded by a move so hover targeting is coherent. Promoted because
  the overlay's scroll wiring — which #46 rewrote — was reachable by
  no test and no Wayland automation: which surface receives a wheel,
  the two separate accumulators, and the post-Flickable coordinate
  terms were all review-verified only. `delta_x` is dispatched as 0 —
  horizontal scroll is undrivable until the token grows a fourth
  field (recorded limitation; nothing in the app consumes it today).
  `dump.<label>` traces the focus/surface state for test assertions:
  `keysfocus` (the main key scope's real `has-focus`, via the
  `dbg-keys-focus` debug property), loupe/zoom state, panel and modal
  visibility, the copy dialog's visibility, plan summary and rename
  template, the revert-slot label, and the status line. Keyboard focus was otherwise INVISIBLE to
  every headless run — a stranded keyboard could not even be asserted.
  It also carries the loupe pan block (`soft`, `vx`/`vy`, the
  fractional pan centre, the desired factor — issue #46): a
  wrong-position frame is precisely a state nothing re-renders, so
  render-time traces (which fire on CHANGE) cannot see it; the dump
  makes the overlay's position observable at a scripted instant.
- `FASTCULL_NO_CONFIG=1`: makes `ui.toml` (the remembered copy
  destination/template) unreachable for both load and save — what
  `FASTCULL_NO_CACHE` does for the cache (issue #13 gap, surfaced by the
  issue #41 sweep: a driven copy dialog displayed the user's real
  remembered destination). The screenshot test harness sets both
  unconditionally.
- `FASTCULL_KITCHEN_COOK_MS=N`: hold every kitchen cook for N ms before the
  pixel work (issue #34; same family as `FASTCULL_MAX_READERS`). Pacing
  knob for the `open:PATH` session-swap test, which must catch the kitchen
  queue provably mid-flight at a scripted swap in BOTH build profiles — a
  release build otherwise drains a screenful of thumbs in tens of
  milliseconds and the test becomes timing roulette. The job still flows
  queue → cook → done → drain unchanged; with FASTCULL_TRACE the retarget
  reports how many queued jobs it dropped, which is the test's proof that
  the swap really happened mid-flight (a dropped count of zero would make
  the no-stale-adoption assertion vacuously green). Default 0 (off). When
  set, the app announces it once on stderr unconditionally
  (`fastcull: FASTCULL_KITCHEN_COOK_MS=N — every texture cook is held`):
  the knob ships in release builds, and a value leaked into some
  environment makes the whole app mysteriously slow — a bug report's
  stderr must say why (validator risk note, 2026-08-02).
