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
- The `✓ copied`, `▶ exported` and `×N burst` cell badges, anchored to the
  cell bottom,
  become visible in the loupe again; they had been rendering below the fold
  while the app deliberately populated them at `N = 1`. **This is the
  intended loupe badge policy, not an accident of the new geometry**: the
  MARK is suppressed at `N = 1` (`pick: 0`) because the issue #20 pill owns
  state display and the grid's 40% reject dim must stay out of the loupe,
  while "already copied", "already in a video" and "burst of N" have no
  pill and are exactly what
  a last pass before bed wants to see on the full-screen frame (persona).
  One channel per fact: pill for the mark, cell badges for the rest. The
  three are set unconditionally in `fill_grid_cells` — the loupe
  visibility is the POLICY, not an omission (issue #56 extended it to the
  `▶` badge on the same reasoning).
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
  freshly dead file MAY render the thumb for the milliseconds until its
  decode attempt fails, because the failure does not exist as
  knowledge yet; the gate binds from the Failed event on. Accepted, not
  required — whether that first focus renders the thumb at all depends
  on which of the two lands first (the thumb texture or the failure),
  and both orders are honest. Nothing may be asserted on the order
  (issue #50: a test that did reddened CI ~15 % of runs); what binds is
  every focus AFTER the failure is known.
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
  **Recorded deferral — CLOSED 2026-08-11 (raised as a validator
  concern, gate 2026-08-09)**: the deferral read "the hold state
  machine (cap timing, failure gating, re-raise) and the view-distance
  full-res texture eviction live in the APP crate as stateful policy
  with no core unit pins — only the timing-sensitive integration tests
  cover them. Precedented (the render ladder was already app-side) but
  each #46-class bug so far lived exactly in untestable app-side
  state; the next transit-affecting change should force this block
  into core as a pure decision function. Deferred explicitly, not
  silently." That trigger was honored: the whole block is now
  `fastcull_core::transit`, and the app-side state it named no longer
  decides anything.
  * `render_rung(&RungInputs) -> RenderDecision` is the ladder above,
    entire: which rungs are in hand, whether the cursor's decode
    failed, whether the overlay is wanted and whether it was up, and
    the hold's (is-it-this-cursor, how-long) pair go in; Sharp /
    Soft{is_thumb} / Hold{start} / Drop{reason} comes out. The
    function is TOTAL — every input combination yields a decision, and
    the table sweeps all 320 of them (2^6 booleans × 5 hold states)
    against the pre-move app ladder transcribed in its own shape, so
    the extraction is pinned as an equivalence, not as a description.
    Both residuals recorded above have their own named rows: the
    causally-unavoidable thumb transient before a failure is knowledge,
    and the cap re-timing at each cursor change.
  * `evict_fullres(held, cursor, view) -> Option<slot>` is the
    view-distance eviction, with the three rules the app had left
    implicit now written down and tested: the cursor's texture is
    never the victim, an out-of-view entry (or any entry when the
    cursor itself has left the view) goes first, and a tie goes to the
    LATER slot. `FULLRES_RING` is `2·PREFETCH+1`, derived rather than
    the bare `5` the app used to carry beside a comment saying so.
  * The cap DURATION (`OVERLAY_HOLD_CAP`, 250 ms) stays an app
    constant and is passed in as `hold_cap`. That is deliberate: it is
    a UI tuning value beside its siblings, and passing it makes the
    cap a table row instead of a hardwired duration.
  What the app kept is what only the app can do: texture lookup, the
  clock read, the zoompan extent math, and the property writes each
  decision names. The claim that this policy lives in "untestable
  app-side state" is therefore retired — the timing-sensitive
  integration tests are no longer the only cover, they are the
  integration pins over a unit-covered policy. The release-profile
  exercise of the `(hold cap)` drop-and-re-raise (recorded below with
  the acceptance log) still wants a decode-wedge knob, but now only for
  integration-level exercise: the policy itself is unit-covered.
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
failed flag, copied flag, exported flag, selected flag. (Fields arrive with
their milestones:
M2 ships texture/failed/cursor; pick badge M3, copied M6, burst M7,
exported #56.)
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
  A landed thumb for a scrolled-away cell is adopted into
  `st.textures.images`
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
  COLLAPSES any multi-selection (the deselect gesture; Esc clears the
  selection from anywhere — user decision 2026-08-28 — and G does at a
  grid zoom). Ctrl+click toggles membership;
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
- **Exported as video (issue #56, 2026-08-29)**: `▶` on every frame that
  went into a video THIS SESSION whose file is still on disk —
  bottom-left, immediately right of the `✓` (which keeps `x: 8px`), in
  the `×N` pill's palette (`#d8d8e0` on `#202028cc`) rather than ✓'s
  green, because green is the data-safety signal and this is not one, and
  because a bare glyph washes out under the 40 % reject dim these frames
  usually wear. Per FRAME, never per burst: the export's scope is an
  arbitrary set, so an opener-only badge would lie about a partial burst.
  Visible in the loupe, per the badge policy above. The memory behind it
  is session-only and reads-never-decides — video-export.md, "Exported
  badge and hint", owns the contract.
- **Burst context**: see burst-grouping.md — the ×N badge and "burst
  7/23" status fragment already serve burst position; the state
  indicator composes with them, it does not replace them.
- Burst (M7, persona-redesigned): count badge "×N" on each group's first
  frame + optional thin two-tone bottom strip; NEVER a full-perimeter
  border (cursor/selection own borders). See burst-grouping.md UI
  contract.
- Selection (wash added 2026-07-28 on user request, persona-reviewed
  MUST-HAVE): a translucent **accent-blue wash over the whole cell**, plus
  the existing accent outline; multi-select via Ctrl/Shift-click,
  Shift+arrows, and the burst chords Shift+`[`/`]` and Ctrl+Shift+B
  (burst-grouping.md, issue #55). Rationale — the selection is what the **IPTC panel** stamps
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
    badges, so ★ / ✕ / ×N / ✓ / ▶ / ! stay legible on a selected cell.
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
| `G` | back to the grid at the previous grid zoom (from loupe/1:1); at a grid zoom it is also the deselect gesture (clears the selection); from the loupe it KEEPS the selection — the "go and look at what I selected" exit |
| `Esc` | back to the grid at the previous grid zoom AND the selection cleared — from anywhere, the loupe included (user decision 2026-08-28, issue #55: the burst chords build a 40-frame selection in the loupe with one press, where no wash shows it, and a stale one would silently take the next IPTC commit; the cancel key must work where the selection was made). Modal popups still take Esc first (they close; the grid never sees it), and with keyboard focus in an IPTC field Esc stays the recorded no-op (Slint LineEdit has no Esc hook — see the panel section; QE 2026-08-28). Like every nav key it ends in the cursor reveal, so an Esc taken by the grid also scrolls the cursor back into view — that is the reveal rule, not a lost scroll position: only keys that never reach the grid (a modal's Esc) leave a browsing viewport alone |
| `I` | toggle IPTC panel |
| `K` | focus the keyword field, opening the IPTC panel if needed (persona G3; implemented with the panel step — K is never a dead key) |
| Shift+arrows | extend selection (span anchor..cursor over view positions; a new span replaces the previous one — shrink/flip works) |
| `Ctrl+A` | select all (filtered set) |
| `[` / `]` | burst boundary jump (M7): `]` = next frame whose group differs (in a contiguous capture-sorted view that is the next group's first frame; with non-contiguous members it follows view order); `[` = re-anchor on the current group's first visible frame, crossing to the previous group only from there (CD-player convention); claims the cursor; carries loupe zoom/pan persistence; see burst-grouping.md |
| Shift+`[` / Shift+`]` (also `{` / `}`, the shifted characters a US keyboard sends) | extend the selection by WHOLE bursts (issue #55): the cursor lands where `[`/`]` would, and every whole burst between the anchor's burst and the cursor's is selected; the opposite key drops a burst; a following Shift+arrow is frame-precise from the burst's edge; see burst-grouping.md |
| `Ctrl+Shift+B` | select this burst (issue #55, user proposal): the burst under the cursor joins the selection, cursor unmoved, additive, idempotent; see burst-grouping.md |
| `Ctrl+O` | Open Folder… (persona accelerator gap, provisional) |
| `Ctrl+Q` | Quit (persona accelerator gap, provisional) |
| `Ctrl+E` (menu: Copy picks…) | open copy dialog (`Ctrl+C` stays clipboard-idle: user decision after persona review — never repurpose it) |
| `Ctrl+Shift+E` (menu: Export Frames as Video…) | open the video export dialog (M9, video-export.md). A CHORD, not a bare letter, so it cannot fire from a fat finger mid `]`/`N` (persona 2026-08-27); it is matched BEFORE `Ctrl+E` because with Shift held the event still arrives as the letter plus modifiers. Disabled — with its reason in the status line, never silently — when there is neither a selection nor a burst under the cursor |
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
menus are discoverability, never a required route.

**Where that bar is drawn is the platform's choice, and it differs**
(recorded 2026-09-02, issue #70 — existing behaviour, not a change): on
Windows the winit backend supports a NATIVE menu bar (`muda`), so the
menus belong to the OS window frame and sit outside the client area; on
Linux there is none and Slint draws the bar in-window, 40 px tall in the
`fluent` style. Everything below the bar should therefore sit about
40 px higher on Windows. That mechanism is read from the backend source
(`i-slint-backend-winit` 1.17.1: `supports_native_menu_bar` is true under
`cfg(muda)`, and `muda` is active for Windows in Cargo.lock); the one
Windows measurement so far is the Title field 43 px higher (issue #70,
3 px of it font metrics), and the Windows `window geometry` mark in the
CI artifact — `grid 1440x840` against Linux's `1440x800` — is what
confirms the number (pending the first artifact, 2026-09-02). Either
way no driven test may click an in-window element at a coordinate
measured on the other platform (harness section, `click:<element>`),
and the menu-click strands are Linux-only: a dispatched pointer event
cannot reach an OS menu.

The menus:

- **File**: Open Folder… (native picker via `rfd`; replaces CLI-only launch),
  Copy Picks… (`Ctrl+E`, enabled from M6), Export Frames as Video…
  (`Ctrl+Shift+E`, from M9 — greyed when there is nothing to export, and
  the keystroke then explains itself in the status line rather than doing
  nothing), Settings… (placeholder entry, disabled until a settings dialog
  exists — post-v1 candidate), Quit.
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
**ALL FOUR scrims swallow the wheel (issue #49)**: About and the
shortcuts popup get it from the shared `ModalScrim`; Copy Picks and
Export Frames as Video are hand-rolled copies of that scaffolding (the
focus scope has to WRAP the card, which the component cannot express —
the migration blocker recorded in `main.slint`), and until 2026-08-29
their scrim `TouchArea`s had no `scroll-event` arm, so a wheel over
either dialog fell through to the grid's Flickable behind it and the
user came back to a different place in the folder (persona: IN-MY-WAY
when it bites). A hand-rolled scrim must carry the arm. All four scrims
are driven now — the two copies plus `ModalScrim` itself, whose arm was
correct but untested: each test wheels the grid once BEFORE the modal
(so a grid that cannot scroll fails loudly instead of passing
vacuously), wheels again with the modal up and requires `vpy` unmoved,
and wheels a third time after Esc as the control that the token still
reaches the grid. Each also wheels over a CHILD of the card — the copy
dialog's rename field, About's click-eating `TouchArea` — which is the
one place where "the card swallowed it" could be a child's doing rather
than the scrim's.
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
  un-committed text (user decision 2026-08-03: no commit-on-destroy —
  unchanged, and DETERMINISTIC since 2026-08-30: it used to depend on
  whether Slint happened to deliver a `FocusOut` before dropping the
  item, and a rebuild-generation stamp now decides it, see the owner
  invariant below); a
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
- **The owner invariant (issues #63/#64, 2026-08-30)**: *a destroyed
  editor never keeps the focus token, and the keyboard goes back where
  the user had it.* The mechanism the earlier fixes worked around without
  naming: Slint's window holds a WEAK reference to its focus item
  (`WindowInner::focus_item`), so an editor destroyed by a model
  replacement delivers NO `FocusOut`, nothing reassigns focus, and the
  reference simply dangles — every key event afterwards dies on an
  `upgrade()` that returns `None`. Instrumented and measured, not
  inferred: in 18 of 20 runs the dying editor never announced a loss at
  all, and a `key:y` sent inside the window is provably lost (it lands on
  a tree with the reclaim and is dropped on one without).
  The app therefore carries the token itself, a root `focus-owner`
  int — `0` the main key scope, `1..=N` panel field row i (written
  `i + 1`), `N+1` the keyword field, `-1` a dialog's own scope.
  **Only a GAIN ever writes it, never a loss**, for two measured reasons:
  `changed has-focus` handlers run on the next event-loop iteration and
  the GAINER's runs first here (click Title then Description: `iptc field
  1 gained` at [3733], `iptc field 0 lost` at [3735]), so a loss-write
  would land after the new owner's and blank it; and a row destroyed by a
  rebuild shares its id with the row RECREATED in its place, so the dying
  item's late blur cannot be told from the live one's ([3762] the new row
  claims, [3835] the old one finally announces its loss — a
  compare-and-clear was tried and zeroed a live token exactly there).
  Every claim writes the token SYNCHRONOUSLY at the site — inside
  `focus-keys()`, beside every bare `keys.focus()`, and in the rows' and
  the keyword field's `init`-time claims (an `init` focus gain fires no
  `changed has-focus` at all, the same Slint behaviour `edit-gen` already
  works around; leaving it out is what made the `K` flow read
  `focus-owner == 0` while the keyword editor held the keyboard, and made
  a dialog scope read a panel row while the dialog owned the keys).
  Leaving a claim unrecorded is not cosmetic: a rebuild in the gap reads
  the stale EDITOR id and pulls the keyboard back out of the grid, which
  measurably undid the shipped G4 "Enter returns to the grid" rule until
  the writes were made synchronous.
  The remaining staleness is focus leaving for something Slint owns — a
  menu popup, or the WINDOW being deactivated — and staleness is the SAFE
  direction: the reclaim then hands the keyboard back to the field the
  user was editing, where it was. Clearing would strand it.
  Reclaim points:
  - **the field-rows rebuild — back to the SAME ROW**, armed immediately
    after `set_iptc_fields` replaces the model and only when the token
    names a field row. Rust cannot name a repeater's child, so it arms a
    root `iptc-refocus-row` and the RECREATED row claims the keyboard and
    clears the flag — the `iptc-focus-keywords` pattern, except that the
    row cannot do it from `init` (see below), so it claims from a
    `changed` handler or, when there was no edge to see, from its
    `Timer`.
    **The flag is armed one event-loop iteration LATE** (QE 2026-08-30;
    that text read "and must be", which was true of the tree it
    described and is no longer the whole story — see the early-arm
    paragraph below). What stands: `self.focus()` called from a repeater
    row's `init` DOES NOT TAKE EFFECT in Slint 1.17. It silently does
    nothing — proven both ways, 10/10 dead with the claim in `init`,
    10/10 alive with only the flag write deferred — so a row can take
    the keyboard only from an event-loop callback that runs once it is
    alive to the window: a `changed` handler, or its `Timer` tick. The
    deferral is what makes the LATE ordering — the flag arriving after
    the repeater has rebuilt, so the recreated row sees an edge and the
    doomed one is already gone — the likely one; it is not a guarantee,
    and the ordering it cannot promise is what the generation stamp and
    the `Timer` answer. A `changed
    absolute-position` belt was tried and does not rescue it: that
    handler never fires for rows 0 and 1, whose first computed position
    is already their last (0/6). **So this path leaves a real gap, and it
    is not the 0 ms the swap path gets** — measured from the rebuild to
    the claim, per profile, because the difference is large and only the
    release figures describe what ships:

    | build / load | min | median | max |
    | --- | --- | --- | --- |
    | **release, idle** | 5 ms | 5 ms | 6 ms |
    | **release, six spinners** | 11 ms | 12 ms | 35 ms |
    | debug, idle | 95 ms | 102 ms | 117 ms |
    | debug, six spinners | 193 ms | 206 ms | 230 ms |

    (QE measured the same shape independently at 5-10 ms release-idle and
    12-31 ms release-loaded.) **A keystroke inside the gap is not lost
    when the claim runs before it in the iteration** — the common case,
    measured 12/12 by the validator and 20/20 by QE at 2026-08-30's
    loads: the key is queued in the same event-loop batch and delivered
    to the recreated editor after the claim, in order. It IS dropped when
    the key is processed before the claim callback (QE 2026-09-02, issue
    #69): under six spinners plus a build loop in a debug build the
    iteration is one frame of 200-500 ms, the claim landed 190-540 ms
    after the rebuild, and a `drive: key:w` at [5936] was lost to a
    claim at [5937]. The cursor-move test's fixed-time keys after the
    move fell in that gap once in 20 on that seat; its acting assertion
    now names both readings, and the keys no longer run on the clock: the
    script waits for the claim itself (`wait:row 0 (gen K)`, issue #69 —
    the harness section's `wait:` paragraph says why the gen has to be in
    the substring). The gap the table prices is unchanged; what closed is
    a driven run's exposure to it. The loss case the FAIL-1 family was — "no
    claim at all" — is closed on both orderings; a key inside the gap is
    the residual the table above prices.
    The arm that actually DELIVERS is the deferred RE-ASSERT
    (`restore -> row N`), not the SameRow arm queued beside it: 40/40
    observed runs across the three profiles above. The SameRow arm is
    still the one that names the row, and it is what makes the re-assert
    find a field-row token to re-assert. (Both are zero-length timers
    queued in the same refresh, so they run in the same event-loop turn —
    which is why, on the seat where that turn beats the repeater update,
    NEITHER delivered and the row's `Timer` had to. See the early-arm
    paragraph below.)
    The gap is the price of going back to the ROW rather than to the
    grid, which is the right trade — a claim on `keys` in between makes
    the next caption character a cull command — but it is a residual, not
    a clean win, and closing it needs either an in-place row update (the
    follow-up below) or a Slint that can focus from `init`.
    **An arm that fires before its row exists must SURVIVE — and the
    `settled` edge that was meant to make it survive never could**
    (CI red on the v0.13.0 commit, diagnosed 2026-09-01). A zero-length
    arm timer can beat the repeater update; the flag is then already set
    when the row is born, so `want-refocus` is true from its first
    evaluation, `changed want-refocus` never fires, and the request is
    armed, matched and silently never claimed — ownerless for good, with
    every keystroke after it dead.
    The first answer was half right: the flag is cleared ONLY by an
    actual claim (never by an arm firing), so an early arm does survive
    until its row exists. The other half — a `settled` property assigned
    in the row's `init` to manufacture a false→true EDGE — **cannot
    work**, and the generated Slint code says why: `user_init` runs the
    `init` statements FIRST and installs the row's change trackers
    AFTERWARDS, so anything written in `init` is the tracker's baseline,
    not a change it can ever see. The 2026-08-30 campaigns did not catch
    this because they never produced the early ordering; `settled` is
    removed rather than left looking load-bearing.
    **Which ordering a seat gets is not the app's to choose.** On the
    developer's GPU-composited seat the repeater recreates the rows ~3 ms
    after the model swap and the arm timer fires only ~87 ms later (LATE
    arm — the path that always worked, and the only one the campaigns
    ever measured). On the 2-core headless CI runner both arms fired
    inside the model swap's own millisecond and the rows were recreated
    16 ms afterwards (EARLY arm), and the keyboard was stranded for the
    rest of the run.
    **What answers both orderings is a per-row `Timer`** (1 ms, `running:
    want-refocus`, the claim re-checked on the tick). A timer tick is the
    one hook that runs after the change trackers are installed however
    the race went, and a row CAN focus from it — unlike from `init`. It
    costs nothing on the ordering that already worked: there the fast
    path claims in the arm's own iteration, clearing the flag, so
    `running` goes false and the timer never ticks.
    Measured, with the arm forced early to make the CI ordering
    deterministic (`arm_row_refocus` called synchronously and the
    deferred re-assert removed): 0/5 claimed before the `Timer`, 10/10
    after; the test itself 0/10 red before (the identical panic and line
    CI reported) and 10/10 green after, under `taskset -c 0,1` plus four
    spinners. On the unforced developer seat: 20/20 green before and
    after — which is exactly why only CI could find this.
    **The flag also carries a GENERATION (validator FAIL-1,
    2026-08-30).** A Slint repeater does not tear its children down when
    the model is replaced; they die at its next update. So the DOOMED row
    instance is still alive, still watching the flag, and its `changed
    want-refocus` runs FIRST: armed with the index alone and
    SYNCHRONOUSLY, it consumed the flag in the rebuild's own millisecond,
    focused itself, cleared the flag and then died — the recreated row
    saw nothing, `focus-owner` still read that row, and no element owned
    the keyboard. Measured dead 10 runs in 10 on a cursor-move rebuild.
    The blur-triggered rebuild hid it (there the commit runs inside the
    blur, so the timing differs), which is why every probe written before
    this one missed it. So `iptc-refocus-row` is armed together with
    `iptc-refocus-gen` = the current `iptc-rebuild-gen`, each row stamps
    its own `born-gen` in `init`, and a row claims only if it was born
    for the generation the flag names.
    **Which of these is load-bearing on the shipped tree, honestly**
    (re-measured 2026-09-01, because the 2026-08-30 answer was written
    from the LATE ordering only). The DEFERRAL is not the guarantee it
    was taken for — it decides which ordering is *likely*, not which one
    happens, and CI got the other one. On this developer seat the arm is
    late, so: making the arm synchronous again is 0/10 dead without the
    `Timer` and 10/10 alive with it, and the generation stamp is not
    independently demonstrable at all (the doomed instance is gone before
    a late flag arrives; removing the stamp measures 15/15 alive,
    validator 2026-08-30, and 6/6 alive here).
    Force the EARLY ordering, which is what a 2-core headless seat
    actually does, and both belts become load-bearing and measurable:
    without the `Timer` 0/10, without the generation stamp 0/6 (the
    still-alive doomed instance consumes the flag, FAIL-1's original
    shape), with both 10/10. So the honest statement is that the arm's
    timing is a race the app does not control, and the two belts —
    generation stamp against a doomed instance claiming, `Timer` against
    a live instance never getting an edge — are what make the claim
    ordering-independent.
    **Decision (validator 2026-09-01): the deferral is no longer a
    mutation-tested invariant.** Of the three 2026-08-30 mutants the gate
    kept (`.qe-scratch/dev/focus/3b/`), the synchronous-arm one is GREEN
    by design with the `Timer` — that is the fix working, not the mutant
    escaping — and the no-stamp one is red only under the forced EARLY
    ordering (6/6 alive unforced on this seat, 0/6 forced); only the
    disabled-synchronous-reclaim one is red on either ordering. The
    deferral stays because it makes the fast path the likely one and
    keeps the gap in the table above small, not because anything would
    strand without it.
    **Not to the grid**, which an earlier cut did and which is a HIGH
    defect: the panel is a captioning surface, "focus stays where
    clicked" is a shipped rule (iptc-templates.md), and the blur commit
    of clicking from Title to Description itself rebuilds the rows — so
    reclaiming to `keys` there made the very next character a cull
    command. Measured: `x` rejected the photo and wrote a sidecar.
    Synchronous is safe *here* precisely where it is not safe for the
    menu: a rebuild runs in app code (a `refresh` pass), not inside the
    MenuBar's activation dispatch, so no post-activation focus restore
    follows it to override the claim.
  - **a session SWAP — SYNCHRONOUS, to the topmost scope.** The same
    rebuild, answered the other way, because the field's MEANING went
    with the folder (#41 D3): there is no "same row" to go back to. The
    panel cache carries the session generation it was built for, which is
    how the two cases are told apart. A swap ALWAYS reaches this branch:
    `IptcPanelState::begin_session` clears the row cache, so the next
    refresh replaces the model even when the two folders' rows are
    identical (both un-captioned) — verified 6/6.
    The deferred re-assert additionally captures the session generation
    when it is QUEUED and falls back to `focus-keys()` if a swap landed
    before it fired (validator FAIL-4): the token would otherwise name a
    field of a folder that is gone. Reachable only as menu-activation
    then swap, i.e. File > Open Folder — which the harness cannot drive
    (the native rfd dialog blocks the loop), so this guard is verified by
    inspection and by the swap probes around it, not end to end.
  - **panel CLOSE — SYNCHRONOUS *and* deferred**, the range covering the
    keyword field too, because closing destroys the whole panel. The
    deferred claim is the one that matters and it stays: while an editor
    holds the keyboard the panel can only be closed from the MENU (`I`
    types an `i` into the field — focus containment), and the MenuBar
    restores focus to the destroyed editor after the activation returns,
    which nothing synchronous can undo. So the close path is a few tens
    of milliseconds (21-53 ms measured) BY DESIGN, and docs/culling.md
    says so rather than claiming it is immediate. The synchronous half
    covers the non-menu routes (the `iptc` drive token today).
  - **the rebuild's own deferred re-assert**, queued behind the flag as a
    belt: it re-reads the token when it fires, so it also routes to a
    dialog that took over in between. Traced as `restore -> …`, where the
    menu path is `menu -> …`, so a reader can tell which queued it.
  - **any MENU ITEM — DEFERRED, and it re-asserts the TOKEN** rather than
    claiming `keys` (QE finding 2026-08-30). Activating any item blurs a
    focused field, the blur COMMITS it (G7), the commit rebuilds the rows
    and destroys the editor, and then the MenuBar's restore hands focus
    to the destroyed item: measured dead 5 times in 5 through View >
    Filter Bar, which unlike View > IPTC Panel queued no claim of its
    own. Every item now fires `menu-activated` first. It re-asserts the
    token — a field row through `iptc-refocus-row`, the keyword field
    through `iptc-focus-keywords`, anything else through `focus-keys()` —
    because after a menu action the keyboard belongs where it belonged
    before; blanket-claiming `keys` took it off the live keyword editor
    and broke the shipped RUN17 behaviour on the first cut.
  - **the other menu-driven paths — DEFERRED, unchanged**: a modal
    opening, and the swap's own belt claim.
  - **panel OPEN — DEFERRED, a belt**: queued when the panel opened and
    `iptc-focus-keywords` was false on entry. Gated on that flag because
    with `K` the keyword field's `init` claims focus during instantiation
    and an unconditional claim would steal it straight back.
  - **NOT after the keyword-chip or template model replacements.** Neither
    holds an editor, and the keyword `LineEdit` is their SIBLING rather
    than their child, so a reclaim there would fire while the editor it
    "rescued" was alive and focused — taking the keyboard away from a
    user mid-word every time another image's sidecar landed a keyword.
    The rows model is the only one whose replacement destroys a focus
    holder.
  **The discard rule is unchanged and now deterministic.** "A DESTROYED
  editor DISCARDS its un-committed text" (user decision 2026-08-03)
  stands exactly as written. It used to hold by accident — the dying
  editor usually got no `FocusOut`, so its blur handler simply never ran,
  except in the 2 runs of 20 where it did and the text was committed
  instead. It now holds by construction: Rust bumps a root
  `iptc-rebuild-gen` immediately before every rows replacement, each
  editor stamps that generation on focus gain, and the blur commits only
  if the stamp still matches. A rebuild between gain and blur therefore
  discards, every time; a real click-away or Tab (no rebuild in between)
  commits, exactly as before. Measured on the same-session probe — type
  into Title, then grow the batch so the row goes mixed — 9 of 9 clean
  runs discard. Note how easily that rebuild is reached: it is ANY change
  to what a row should show, the CURSOR image's own IPTC landing in a
  single-image folder with nothing selected included (QE saw a title
  committed with no Enter twice in ten runs on the pre-fix tree). Both
  the docs and this spec therefore say "any rows rebuild", never "another
  image of the selection". A boolean "suppress the next blur commit" could not do
  this: `changed has-focus` is deferred, so a flag set and cleared around
  the reclaim is already false when the handler reads it, and one held
  for a whole refresh tick would swallow real click-away commits.
  **Not done here, recorded as a follow-up**: updating changed rows IN
  PLACE (`VecModel::set_row_data`) instead of replacing the model would
  keep the editors alive and remove the hazard at its root. It is not a
  drop-in. The row's `text` is a BINDING on `row.value`, and Slint drops
  a binding permanently the first time a handler assigns `self.text` —
  which every exit path here does — so today only the model replacement,
  by re-creating the item, restores it; in place, an edited-then-blurred
  field would sit on a stale value for the rest of the session. It would
  also change what `session_swap_mid_field_edit_discards_and_keeps_the_
  keyboard` proves: the editor would survive the swap, leaving the D3
  discard resting entirely on the generation stamp and `changed seen-gen`
  rather than on destruction. The owner-token reclaim is the fix; this is
  an optimisation, and it needs its own step with those two questions
  answered.
  **A menu DISMISSED without activating anything** (validator FAIL-3,
  2026-08-30) — the nastiest shape in the family, and closed by the same
  deferral. Opening a menu over a focused field blurs it, the blur
  commits (G7), the commit rebuilds the rows and destroys the editor;
  then the menu is dismissed — a click elsewhere, Esc, a missed item —
  and the MenuBar restores focus to the destroyed instance. Nothing
  announces it: no `activated` fires, so the `menu-activated` claim never
  runs, and Slint 1.17 exposes no menu open/dismiss callback to hang one
  on. Measured dead 10/10 while the reclaim's flag was armed
  synchronously. It needed no new claim in the end: armed one event-loop
  iteration late (see the rebuild bullet), the flag lands on a row that
  is alive and can actually take focus, and that claim survives the
  restore. 15/15 alive on the Esc route and 5/5 on the click-elsewhere
  route, against 0/10 on a tree with the arming made synchronous again.
  A third "route" measured at the time — clicking the menu bar again —
  was WITHDRAWN: it RE-OPENS the menu rather than dismissing it, so those
  runs measured containment while a menu is up, not recovery after a
  dismissal (validator, from the screenshots). Pinned by
  `a_dismissed_menu_over_a_focused_field_row_keeps_the_keyboard`, which
  drives the unambiguous Esc route.
  **Open, found while measuring this (NOT fixed here)**: deactivating the
  WINDOW mid-edit — alt-tab away with a half-typed caption — delivers a
  real `FocusOut` to the live editor, whose blur handler then COMMITS,
  exactly as a click-away would. It is pre-existing, it is invisible to
  `keysfocus` reasoning, and it is the best current explanation for the
  intermittent leak recorded against
  `session_swap_mid_keyword_edit_never_writes_into_the_new_session`
  (#54): measured here at 1 leak in 10 probe runs, correlating 1:1 with a
  lone `focus: … lost` that no gain follows, and reproduced on the
  unmodified tree at 1 failing group run in 6. QE's independent A/B puts
  it at 3/10 on this tree against 4/10 with the reclaim removed, trace
  shape identical — i.e. unchanged by this step, as it must be: the blur
  arrives from OUTSIDE and commits BEFORE any rebuild, so the
  rebuild-generation stamp that enforces the discard rule never gets to
  see it. Telling a deactivation
  blur from a click-away blur needs the window's activation state, which
  Slint 1.17 exposes only through `i-slint-core`'s internal
  `WindowInner::active()` — so it needs its own step and a decision about
  that dependency. Until then `docs/metadata.md` tells users plainly that
  a window switch mid-word commits, and points at Revert.
  **It makes every "no sidecar was written" assertion seat-sensitive**,
  not just the two tests that name it: any script that leaves a field
  focused with text and later reads the folder can be hit. Observed once
  in a full suite run on `copy_picks_from_the_menu_over_a_focused_field_
  owns_the_keyboard`, which is 0/8 in isolation and 0/6 when its script
  is driven by hand. Those assertions now print the child's trace, so the
  next occurrence can be read rather than guessed at: the signature is a
  lone `focus: … lost` with no `gained` after it and no `focus-keys (…)`
  before it.
  **Do not read a standing `revert=…` in a dump as this defect** (learned
  the expensive way on the 2026-09-01 CI red): a committed field and a
  dead keyboard leave the same dumps from there on, and a script that
  commits something earlier carries the line into every later dump
  anyway. Only the TRACE separates them — this defect is a `lost` BEFORE
  the rebuild with no claim preceding it; a stranded reclaim is a rebuild
  with no `row N (gen …)` claim AFTER it. The cursor-move test now
  asserts the two separately so a red run names the right one.
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
      (mid-session corruption — a helper thread zeroes the file on disk
      at the later of two moments: the app tracing that its embedded
      thumb JPEG is in memory (`thumb bytes idx 11`, ~0.3 s) and a T+9 s
      floor, which is the historical schedule and the one that decides on
      any normal machine; the anchor engages only when a loaded runner
      slows the scan past it. Either way, well before the End-jump that
      focuses the file; red-run-verified against the pre-gate build: the
      thumb rendered on every visit and the `(decode failed)` drop never
      appeared). Its
      non-vacuity guard is the thumb TEXTURE's arrival — `thumb landed
      idx 11` before the second End-jump — not a thumb RENDER: the
      texture and the failed full decode land ~17 ms apart inside that
      first End-jump (the kitchen only decodes a thumb when its cell
      comes near the view, which for the last image of a 1-column loupe
      IS the End-jump itself), so
      demanding a render asks a scheduling coin flip to come up heads,
      and it reddened CI ~15 % of runs (issue #50, 2026-08-29). Both
      orders are correct product behaviour. What binds is the SECOND
      End-jump — failure known, texture in hand — where the rescue must
      not render at all; the test counts renders only after the `t1`
      dump, and bounds the whole run at the one unavoidable transient.
      The zero count's own preconditions are asserted, not reasoned:
      `cursor=11` and `zf=inf` at both dumps, plus the second End's own
      `(decode failed)` drop — which `render_rung` emits only when the
      overlay was wanted AND up, so it IS the proof that the rung was
      attempted there. Without them a swallowed key, or a session simply
      sitting at fit, would buy the zero. The residual that was recorded
      here — the texture had to land inside the ~2 s between the two
      End-jumps — is closed (issue #13, 2026-08-29): the second End-jump
      is held by `wait:thumb landed idx 11`, so a runner slow enough to
      take longer moves the End with it instead of losing the arming, and
      the `thumb landed` assertion stays as the reading of that ordering
      off the log. The wheel wiring the
      restructure touched is
      pinned by `overlay_wheel_still_zooms_one_stop_per_notch` (real
      dispatched scroll events via the `wheel.` token; a guard — wheel
      SEMANTICS did not change, only the surface wiring — non-vacuous
      because a dead scroll path leaves the factor at 1.0; covers both
      accumulators and the fit→overlay handoff). Since 2026-08-29 that
      test also pins the NOTCH SIZE (issue #13): 59 logical px fire
      nothing and the 60th fires exactly one stop, so winit's 60 px per
      line — a number the accumulator is written against and that was
      comment-only — fails a test if a backend upgrade changes it,
      instead of quietly turning every notch into a fraction of a stop.
      The same pair pins the residue carry (the accumulator subtracts 60
      rather than zeroing), and a full notch DOWN at fit is asserted
      inert, the reserved no-op's end-to-end half. Still without a
      deterministic release-profile exercise (recorded, QE gate): the
      `(hold cap)` drop-and-re-raise fires routinely in debug runs and
      the M1 test asserts the recovery whenever it fires, but forcing
      it deterministically in release needs a decode-wedge knob —
      deferred alongside the wedge affordances already recorded in
      this spec. Narrowed 2026-08-11 (A3): the cap timing, the failure
      gate and the re-raise are now unit-covered as
      `transit::render_rung` table rows, so the missing knob costs an
      integration-level exercise of a policy that is otherwise pinned,
      not the only coverage of it.
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
      The first two of those are no longer taken on trust: issue #13's
      `a_grid_drag_scrolls_without_clicking_the_cell_under_it` drives a
      real four-event drag over the grid (it scrolls, the cursor does not
      move) and `two_distant_clicks_are_two_clicks_not_a_double_click` two
      real clicks 600 px apart (two cursor moves, no loupe), with the
      same-point double-click as its control — two tests, because a click
      after a fling lands at an offset no script can predict (QE finding
      2026-08-29: the split retired a debug-only load flake). They
      are tests of a DEPENDENCY on Slint's semantics rather than of the
      app's own code, which is exactly why they are worth having: if a
      Slint upgrade changes either rule, the app has no guard of its own.
- [x] **Double-click reaches 1:1 from ABOVE fit**, not only from fit
      (`loupe_double_click_above_fit_reaches_one_to_one`). This is the
      gesture that shipped broken through both gates, and it broke in the
      bridge, where no core test could see it: the `FASTCULL_DRIVE`
      `dblclick:x,y` action replays Slint's real ordering (a `clicked` that
      re-centers, then `double-clicked` on the same release) so the class of
      defect is reachable from a test at all. That token does not
      hit-test — it invokes the callbacks directly — but the ROUTING it
      could not reach now has its own tests (issue #13, below).
- [x] **Pointer ROUTING** (issue #13, closed 2026-08-29): which Slint
      surface receives a physical click, drag or wheel, driven through real
      dispatched events and Slint's own hit-testing. Five tests, each with
      an intermediate assertion that fails loudly and specifically when a
      click misses, and each pairing every "nothing happened" claim with a
      control in the same run that proves the same token DOES act when it
      should — a dead pointer path must never buy a green:
      `a_click_inside_the_iptc_panel_never_reaches_the_grid` (issue #12's
      deferral: neither panel chrome nor a panel field fires `cell-clicked`
      — the cursor and a 300-cell selection survive both — while the field
      click provably lands ON the field, proven the way a user would: the
      `t` and the Enter after it COMMIT a Title across the selection and
      arm the revert slot, which a click that missed cannot do (`t` is not
      a binding on the main scope). The landing is still proven through
      the COMMIT and not through `keysfocus` — they are different
      questions, and only the commit says the pointer hit THIS field —
      but the `keysfocus=true` assertion at the `before` dump is back
      since the owner-invariant fix (2026-08-30). It was left out while
      opening the panel with a real `I` after a real click stranded the
      keyboard about one run in eight — **issue #64**, found here,
      pre-existing, in the issue #41 family, and never reproducible
      through the `iptc` nav token, i.e. only when the item tree changes
      inside a key dispatch — because a pointer-routing test must not go
      red for a focus bug it is not about. What the restored assertion
      pins is bounded and stated in the test: the cell click before that
      dump claims the keyboard itself, so it says "nothing between the
      `I` and here left the keyboard ownerless", not "the `I` alone kept
      it". A control click on the grid then moves the cursor and
      collapses the selection);
      `the_wheel_routing_table_holds_over_every_surface` (the grid scrolls;
      the overlay scrollbar, the docked IPTC panel and the fit surface each
      leave the grid where it was — the panel row also guards issue #12's
      docking bug, where the Flickable really did extend under the panel);
      `a_grid_drag_scrolls_without_clicking_the_cell_under_it` and
      `two_distant_clicks_are_two_clicks_not_a_double_click` (above, and
      deliberately two runs: sharing one script made every click after the
      drag land wherever the flick's scroll had stopped, which under load
      put one in a gutter); and `a_scrollbar_drag_in_the_loupe_claims_the_cursor` — the
      POSITIVE half of the `sb-activity` claim, which until now had only
      negative tests ("the claim does not fire") because nothing headless
      could raise the flag; a `press./move./release.` on the overlay
      scrollbar can, and the claim fires with the cursor following.
      Coordinates are calibrated against measured geometry, not guessed:
      the panel tests read the row rectangles the app traces
      (`iptc field N laid out at X,Y size WxH`) and assert the point they
      aimed at was inside, and the grid clicks sit in cell INTERIORS (at
      8 columns x=900 lands in the 6 px gutter between two columns and hits
      nothing — measured).
      Every one is mutation-verified (2026-08-29, in a scratch worktree):
      the scrollbar's `scroll-event` arm removed reddens the scrollbar row
      (`vpy=-360` against `-180`); the fit surface's arm made to `reject`
      reddens the fit row; `grid-width` ignoring `panel-w` — issue #12's
      docking bug — reddens the panel WHEEL row on its own, and the panel
      CLICK test together with the containment `TouchArea`'s removal (each
      alone leaves the click test green: the two are defence in depth, and
      the test binds on their conjunction — recorded in its comment);
      `sb_activity` forced to `false` reddens the scrollbar-drag claim;
      the wheel accumulator's 60 px changed to 50 reddens the notch pin.
      The two Slint-dependency tests pin a DEPENDENCY rather than app
      code: no app-side mutation can make a drag click (claiming the
      cursor from the cell's raw pointer release does nothing, because the
      Flickable takes the grab and the cell stops receiving events), so the
      drag test is reddened from the other side — a non-interactive
      Flickable, or a drag shortened below Slint's 8 px threshold, both
      fail its "the drag scrolled" precondition — and the double-click rule
      carries its control in the run (the same cadence on one point DOES
      open the loupe).
      All five are also load-verified in DEBUG, which on the Windows
      runner is a profile CI really runs the screenshot suite in
      (corrected 2026-09-02, issue #70): `has_display()` is
      `cfg!(windows)` there, so `cargo test --workspace` runs the suite in
      debug and the release step runs it a second time, while on Linux the
      debug step has no display and only the xvfb release step runs it.
      In debug: 20 runs each under six busy cores, after the
      gesture spans were compressed to a third of Slint's `DURATION_THRESHOLD`
      and `click_interval` (a 600 ms drag and a 150 ms click pair fit on an
      idle machine and lost the race about one run in ten — those windows
      are measured against a frame clock, which lags under load).
      **Containment through the real path** — the fidelity trap this issue
      names, closed in the two tests it bit:
      `about_dialog_renders_and_contains_the_keyboard` and
      `shortcuts_popup_contains_the_keyboard` used to open the popup with
      the `about`/`shortcuts` drive token and press N/P as NAV tokens —
      and the nav tokens never reach the `keys` FocusScope at all (the
      harness mirrors the containment with an `if` of its own), so both
      tests asserted the mirror and the shipped guard could have been
      deleted with the suite green. They now open through the real Help
      menu items where the geometry is calibrated, send real key events,
      assert `keysfocus=true` while the popup is up (a stranded keyboard
      swallows keys just as thoroughly and means the opposite), and end
      with the control the mirror never needed: Esc, then the SAME key,
      which must mark. The tokens themselves do not force focus (they have
      run the menu item's own `activated` body — set visible +
      `modal-opened` — since issue #41; unchanged here), so what they skip
      is only the MenuBar's focus-restore strand, which the click-driven
      tests cover. The nav-token mirror keeps its own coverage in
      `a_wheel_over_the_help_popups_never_scrolls_the_grid_behind_them`,
      which drives two nav actions under a token-opened About and asserts
      both the `about toggled to true` line and the two
      "drive swallowed by modal" ones — the assertions the migration
      moved out of the About test rather than dropped.
      Verified by mutation, both directions (2026-08-29): with the
      FocusScope's containment arms cut back to Esc-only — the exact
      pre-#23 bug the persona reported — the NEW tests fail ("a mark
      leaked through the modal", `✕1` with the popup up) and the OLD
      token-driven ones PASS. Demonstrated rather than argued.
      Decision, recorded: `i-slint-backend-testing` is NOT adopted. Real
      dispatched events already go through Slint's hit-testing, so its
      `ElementHandle` would buy element lookup at the price of a dependency
      on an internal, unstable crate — and would replace calibrated
      coordinates (which are the same thing a user's pointer has) with
      element identity, hiding exactly the class of bug where an element is
      somewhere unexpected. Still out of reach and NOT covered by any of
      this: the native rfd folder dialog's focus behaviour, OS- or
      compositor-level focus (native menus on X11/Windows), and Tab-cycling
      within panel fields (spec G7) — all three need input the app never
      sees, and remain manual-acceptance items.
- [x] Slint screenshot smoke tests (`fastcull-app --screenshot <out>` +
      `tests/screenshot.rs`): grid placeholder (synthetic), loaded thumbnails
      (texture-variance asserted), failed-badge session, loupe fit
      (`--start-loupe`) and 1:1 (`--start-11`), bursts in a synthetic
      session (`--synthetic N --bursts`: a fixed Sony-style pattern of
      singles and bursts, since the real test RAWs are three single shots
      — the burst keys of issue #55 are driven over it), and the IPTC-panel-open
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
      The `✓ copied` badge was not drivable headlessly either, because Copy
      Picks opens a native folder dialog and `FASTCULL_DRIVE` had no copy
      action (QE G2) — it was covered only by sharing the bottom-anchored
      band with the `×N` burst badge. Since 2026-08-21 `copydest:PATH`
      (below) drives a REAL copy: the re-run regression test copies,
      deletes by hand, copies again, and asserts on disk and on the
      dialog's note/report, and the clash-question test drives all three
      answers (`key:b` / `key:o` / `key:escape`) plus an inert Enter,
      asserting `copystate`, the question's text and what each answer left
      on disk; the badge itself is still asserted only at the state level
      (`SessionCopies::is_copied`), not by pixels.
      The `▶` exported badge (issue #56) does NOT stop at that limit, and
      the difference is worth stating because it is the first cell badge
      that does not. `exported=` (how many frames of the VIEW carry it),
      `curexported=` (the cursor's own flag) and `cliphint=` went into the
      QEDUMP line, but those three prove the LEDGER, not the grid: with
      them alone, sending `exported: false` from the presenter or deleting
      the badge block from `main.slint` left the whole suite green
      (validator finding, 2026-08-29). So
      `an_exported_frame_wears_a_badge_until_its_video_is_gone` also
      asserts the CELLS, in its own final screenshot: it copies one frame
      first so all three badge layouts are on screen at once — no badge,
      ✓ + stepped `▶`, and `▶` alone in the ✓'s slot — locates the first
      cell row in the picture (the menu bar's height is a font metric, so
      it is measured, not assumed) and reads each badge slot's DARK
      FRACTION, plus the ✓'s greenness against the same rectangle of a
      cell that has none. The `▶` glyph's MONOCHROME rendering is
      mechanized in the same place: its strokes must be bright and neutral,
      which is what proves the font gave us text in the UI's own colour
      rather than a colour-emoji bitmap. Both mutations above were
      confirmed RED against it, as was removing the badge's 28 px step.
      **Positional navigation in a drive script must be gated on the
      settled sort.** The view is in provisional FILENAME order until the
      last frame's metadata lands and then re-sorts to the user's sort
      (issue #25), so `home`/`right`/`shift-right` fired during that
      window silently address different images — which is how the #56
      badge test selected the wrong pair and died in a clash question one
      run in two under full-suite load (validator, 2026-08-29). The idiom:
      dump before the first positional key and assert BOTH that every
      thumb has loaded (metadata precedes each thumb on the same ordered
      channel, so all-thumbs implies all-keys) and that the status line
      names the image the script expects at that position; then leave
      seconds of slack before the navigation itself. Renaming fixtures
      does not help — a partially keyed view sorts the keyed ones first.
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
      edit_never_writes_into_the_new_session` — **INTERMITTENTLY RED,
      measured 2026-08-22: it fails roughly one run in four, and the
      leaked sidecar really does contain the abandoned `wip` keyword, in
      the OLD session's folder. So the discard rule has a race, not just a
      slow test: the editor's commit sometimes wins against the
      session-generation bump. Reproduced on `c060e7c`, i.e. it predates
      the clash-question work; interleaved A/B runs of 6 gave 2/6 failures
      before that change and 1/6 after, so nothing in that change caused
      or worsened it. Needs a real fix — this is the one guard standing
      between a half-typed keyword and someone else's photograph.
      RE-MEASURED 2026-08-30 with the issue #63/#64 owner invariant, 20
      runs of its script under six spinners on the fixed tree and 20 on
      a tree with the reclaim disabled: 0 leaked sidecars on both sides,
      so the race did not reproduce at all this time and the change
      neither fixed nor worsened it. The recorded rate stands unrefuted,
      not confirmed — do not quiet this test on the strength of 40 green
      runs**); Esc
      over stacked modals
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
      **The owner-invariant campaigns (issues #63/#64, 2026-08-30)**, all
      driving the real binary with the focus marks on, every log kept:
      the session-swap script 20x under six spinners plus a full-core
      build loop, asserting the post-swap keystroke ACTS (20/20); the
      ownerless window on the SWAP path measured from the marks at
      **0 ms** (min 0, median 0, max 1) against **199/254/479 ms** on the
      same tree with only the reclaim removed — the same-session ROW path
      keeps a smaller residual of its own (5-6 ms release-idle, 11-35 ms
      release-loaded), tabulated in the owner-invariant section; the 1:1 loupe-click script 30x under 24 spinners; the
      issue #64 repro 30x under six spinners (0 stranded, 0/90 with phase
      3a's 60); the issue #13 panel-click script 10x under six spinners;
      issue #54's script 20x on each side; and three A/B probes that are
      the real behavioural evidence — the F3 probe (click Title, type,
      click Description, press `x`: `x` must TYPE; 10/10 under six
      spinners with focus staying in Description, where the earlier cut
      REJECTED the photo and wrote a sidecar); the cursor-move probe
      (validator FAIL-1 and QE's shape both: type, then rebuild the rows
      without the editor's own blur causing it — once by moving the
      cursor onto a titled image, once by growing the batch until the row
      goes mixed — then type + Enter + `y`. 10/10 alive on each shape,
      0/10 with the generation stamp removed, 0/10 with the flag armed
      synchronously, 0/6 with the claim made from the row's `init`); and the
      discard probe (type into Title, grow the batch so the row goes
      mixed), which now asserts the KEYBOARD as well as the disk: the
      text vanishes 10/10, the row holds the keyboard 10/10, and three
      further keystrokes commit and mark 10/10. Asserting only the disk
      was the hole FAIL-1 slipped through — a dead keyboard writes no
      sidecar either.
      The D3 test carries a KNOWN INTERMITTENT banner for the
      deactivation-commit defect (below); it fails on the DISCARD
      assertion only, with a lone `focus: … lost` in the trace, and must
      not be quieted. `a_cursor_move_rebuild_keeps_the_keyboard_in_the_
      field` INHERITS that intermittent: the same fingerprint appeared
      ~2 times in 35 select-all probe runs (`Revert: … on 1 image(s)`,
      ★0, a stale owner token), and QE caught a release-IDLE instance
      that settles the attribution — a partial `n` committed with the
      `lost` arriving 28 ms after the keystroke and the rebuild only
      AFTER that, i.e. the blur came from outside and beat the rebuild
      entirely.
      **That banner is not what turned CI red on the v0.13.0 commit**
      (run 33578204067: 2026-09-01 on the local clock, 2026-09-02T01:08Z
      on GitHub's; ubuntu-latest, first run after the merge), and the
      difference is worth keeping because the two look alike in a dump.
      The deactivation fingerprint is a lone `focus: … lost` with no
      claim before it. The CI trace has NO such `lost` between the click
      that focused Title and the rebuild; its `revert="Revert: Title on
      1 image(s)"` is the test's own seeding Enter three steps earlier
      and is present in every GREEN run too. What the trace does show is
      both arms firing in the rebuild's own millisecond and the rows
      recreated 16 ms later with no claim after them — the EARLY-arm
      ordering above, i.e. a live residual of FAIL-1's class, not #68.
      Reading `revert` as a deactivation commit would have quieted a real
      strand: on a dump, a committed field and a dead keyboard differ
      only in whether a claim mark follows the rebuild.
      **The exposure sweep for that ordering (2026-09-01)**, because one
      red test is never the whole class: the WHOLE screenshot suite run
      under `taskset -c 0,1` with the arm forced early and the row
      `Timer` removed — the runner's ordering, made deterministic —
      is **71 passed, 3 failed**, and the three are exactly the tests
      that need a field ROW to reclaim after a rebuild:
      `a_cursor_move_rebuild_keeps_the_keyboard_in_the_field`,
      `a_menu_item_over_a_focused_field_row_keeps_the_keyboard`,
      `a_dismissed_menu_over_a_focused_field_row_keeps_the_keyboard`.
      Nothing else in the suite is exposed, and in particular the tests
      that carry focus banners of their own are NOT: a session swap and a
      panel close reclaim to the topmost SCOPE synchronously and never
      arm the row flag, so `session_swap_mid_field_edit_discards_and_
      keeps_the_keyboard`, `session_swap_mid_keyword_edit_never_writes_
      into_the_new_session`, `modal_over_a_focused_field_owns_the_
      keyboard_and_writes_nothing` and both `panel_close_from_the_menu_*`
      all pass on that tree. Only the cursor-move test went red on the
      real runner because the two menu tests get a THIRD arm from
      `reassert_owner_deferred("menu")`, queued in a LATER dispatch than
      the rebuild and therefore landing after the repeater update; the
      sweep removes that arm, so it overstates their real-world exposure
      while still naming them as the class. All three are green with the
      `Timer` on the same forced ordering: 10/10 for the cursor-move
      shape, 5/5 for each menu shape, under `taskset -c 0,1` plus four
      spinners.
      The dismiss and cursor-move shapes are pinned by their own acting
      tests, and every one of these numbers was re-measured with a FRESH
      fixture per run after an earlier round reported false reds from a
      fixture whose pick counts accumulated (the `★1` check silently
      stopped matching). A campaign that reuses a fixture across runs
      must reset it or assert a delta.
      **What kills the mutant, and what does not.** The post-swap
      keystroke is the user's contract but a WEAK mutant-killer, and the
      measurement says so: with the reclaim removed it still passes 19
      runs in 20 idle and 19 in 20 under load. Both the claim it races
      and the drive step that sends it are zero-length timers on the same
      event loop, so the claim usually wins the 50 ms anyway — and under
      load the drive timer itself slips 300-700 ms, which moves the
      keystroke clean out of the window. The assertion that fails 20/20
      on the mutant, and passes 40/40 on the fixed tree idle and loaded,
      is on the ORDER of the marks: the reclaim must be the FIRST claim
      after the rows rebuild that destroyed the editor — 20/0 on the
      fixed tree, 0/20 on the mutant, idle and loaded. Both are in the
      test; a scripted keystroke alone would have let this regress.
      **And an assertion on the DISK alone proves nothing about the
      keyboard** — the hole FAIL-1 hid in for a whole round. "No sidecar
      was written" is equally true of a dead keyboard, so the discard
      probes now assert the keyboard by ACTING as well: after the
      rebuild the row holds the token, typing reaches it, Enter commits
      and the key after that marks. Re-verified deliberately once the
      refocus worked, because the earlier "9/9 discarded" was measured in
      a state where the keyboard was dead and therefore said nothing
      about the discard's interaction with a LIVE refocused editor:
      10/10 idle and 10/10 under six spinners, all four properties at
      once (row holds the keyboard, revert not armed, no sidecar, the
      keyboard acts).
      **Campaign pass rates are SEAT-SENSITIVE — read them with that in
      mind.** Any campaign counted on `keysfocus` measures the desktop
      seat as much as the app: on a seat where something else takes the
      window focus, QE recorded 18/20 and 14/20 for scripts that ran
      10/10 and 12/12 on a quiet one, with the keyboard alive in every
      "failing" run. The rates above are from a quiet seat. The
      action probe (`key:+` must zoom) and the mark-order assertion are
      the contract, and neither can be moved by a deactivation; a
      `keysfocus` count is context, never a verdict.
      The range gate — "the keyword field is not in the destroyed range,
      so a rows rebuild must not pull the keyboard out of it" — was NOT
      evidenced by the first `K` campaign, and an earlier draft of this
      section wrongly claimed it was: `K` takes the keyboard during
      `init`, which at the time wrote no token, so the campaign passed
      because the token read `0` and no reclaim was ever considered. It
      is evidenced now, by the same campaign re-run after the `init`
      claims were fixed: `focusowner=12` at the post-`K` dump in all 30
      runs under 24 spinners — the token really does reach `N+1` — and
      the rebuilds in those runs leave that editor alone.
      **Gated on state, not on the clock (issue #61, 2026-08-29):** two of
      these strands clicked at a script timestamp and failed under load —
      `session_swap_mid_field_edit_discards_and_keeps_the_keyboard` 17 runs
      in 20 under six busy cores, `one_to_one_click_claims_the_keyboard` 14
      in 20 under 24. Both now `wait:` for what the click needs. The first
      was not a layout race at all: it asked for a 1200x800 window and the
      compositor did not answer for the life of the run, so the click at
      x=1050 fell 90 px short of a panel still docked at the 1440 px
      window's edge, onto the grid. That measurement is what issue #65
      later generalised: `resize:` is a request, and every script that
      CHANGES the geometry now gates on `wait:window geometry WxH` (see
      the harness section for what a satisfied one does and does not
      promise). The 16 `PIN_WINDOW` scripts are not in that set by
      design: they ask for the size the window already has and gate on
      their own layout waits.
      Measured with the old script under six
      spinners, 9 runs in 10: no `iptc field 0 laid out at 910` at all,
      `geometry at shutter: grid 1140x800`, and a 1440 px-wide snapshot
      12 s after the request — the row sat at 1150 instead of 910, a
      240 px shift. It now pins the window at the size it already has and
      waits on the Title row's layout report INCLUDING its x (`iptc field
      0 laid out at 1150`), which is that row's place at that width and no
      other — so the click happens in the state its coordinates were
      measured in, or the run ends saying so. The second waits for the
      zoom overlay to be UP at all (any
      rung — `idx 0 factor`), because before the first rung that point
      belongs to the fit surface, whose click also claims the keyboard: the
      test went green having exercised the wrong element. Both keep the
      preconditions they used to fail on, and both gained one: the field
      click is RESOLVED against the rectangle the app reported (issue #70
      — the script names the element and the test asserts the resolved
      point is inside the rect; it used to name a coordinate and check
      that), and the
      loupe click is off-centre so the re-centre it produces proves it
      reached the overlay's own surface rather than the cell behind it.
      That sharper aim also made the 1:1 test able to fail for the right
      reason: in one full debug suite it went red with the re-centre
      assertion PASSING and `keysfocus=false` — the click reached the
      overlay and the shipped `keys.focus()` did not stick, which is
      **issue #64**'s family (a focus claim made while the panel's field
      rows are rebuilt under the same dispatch), not a timing miss. It is
      telling the truth when it does that; do not quiet it.
      **Issue #64 does not reproduce on this tree** (measured 2026-08-30,
      instrumented): its own repro — a real click, then a real `I` — ran
      0 stranded in 90 runs (30 idle and 30 under six spinners in phase
      3a, 30 more under six spinners after the fix), and the traces say
      why rather than leaving it to luck: with `I` (not `K`)
      `iptc-focus-keywords` stays false, so no editor ever takes focus,
      `keys` holds the keyboard from startup and never emits a `lost`,
      and the rebuild destroys eleven `LineEdit`s that were holding
      nothing. The 1-in-8 was measured on 2026-08-29 and the tree has
      moved through issues #13 and #62 since, both of which changed the
      harness's key and focus paths. The second signature — the 1:1 test
      above, where `K` really does park the keyboard in an editor before
      the click — is the one the family fix covers by construction: the
      owner token names that editor, and any rebuild that destroys a
      field row reclaims in the same pass. Recorded, not closed by
      assertion: the campaigns are in the issue. And the same caveat as
      #63 applies to its evidence — the reported `keysfocus=false`
      readings cannot distinguish a stranded keyboard from a deactivated
      window, so the 1-in-8 was never established as a strand in the
      first place.
      **Issue #63, 2026-08-30: the ownerless window is closed; the
      reported symptom was a different thing.** Two findings, and they
      must not be conflated.
      (1) The REPORTED reds — `keysfocus=false` at a dump 1.2 s after the
      swap — are window-DEACTIVATION artifacts of the assertion, not
      stranded keyboards: every one of those runs went on to zoom with a
      `+`, and the harness section above records why `keysfocus` cannot
      answer this question at all. The test now asserts by acting, so it
      can no longer go red for that reason.
      (2) The ownerless window it led us to is separately real and
      separately measured. Instrumented, the rebuild destroyed the
      focused editor with no `FocusOut` and nothing owned the keyboard
      until the deferred claim's zero-length timer ran: **199 ms minimum,
      254 ms median, 479 ms maximum** over 20 runs under six spinners
      plus a full-core build loop, on 100 % of runs (A/B against this
      tree with only the reclaim removed; an earlier run of the same
      campaign, recomputed by the validator, gave 178/215/269). What that costs the
      user is a lost keystroke, and it is provable in one A/B: a `key:+`
      50 ms after the swap zooms on the fixed tree and is dropped on the
      unmodified one. With the synchronous reclaim the window is **0 ms
      in every run** (min 0, median 0, max 1) and the post-swap keystroke
      acts 20/20. That 0 ms is the SWAP path only, where the reclaim goes
      straight to the topmost scope; the same-session row refocus cannot
      be synchronous (Slint cannot focus a row from `init`) and keeps the
      residual tabulated in the owner-invariant section. The window, not the keystroke, is the measurement that
      discriminates — see "what kills the mutant" in the test ledger.
      The test also gained the gate this issue asked for — `wait:load
      settled gen 1`, which the session generation in that mark finally
      made expressible (`load settled` read identically for both
      sessions, the #13 "next occurrence" limitation) — but it gates the
      DISCARD dumps only. Putting the keyboard assertion behind it was a
      mistake: it moved that dump seconds later and widened the
      deactivation exposure, which is how it was caught.
- [x] **No modal scrolls the grid behind it (issue #49)**: a wheel over
      any of the four scrims leaves the grid's `vpy` where it was, and all
      four are now driven. The two hand-rolled scrims (Copy Picks, Export
      Frames as Video) are pinned by
      `a_wheel_over_the_copy_dialog_never_scrolls_the_grid_behind_it` and
      `a_wheel_over_the_export_dialog_never_scrolls_the_grid_behind_it`;
      the shared `ModalScrim` behind About and the shortcuts popup — never
      broken, but until now resting on a reading of the component — by
      `a_wheel_over_the_help_popups_never_scrolls_the_grid_behind_them`.
      All three were red-run-verified against the same tree with the
      relevant `scroll-event` arm removed (the `wheeled` dump read
      `vpy=-360.0` against the required `-180.0`). Each also wheels over a
      CHILD of the card rather than bare scrim — the rename field (the one
      `TextInput` in either dialog, after a click and a keystroke prove the
      pointer is really on it) and About's `card-eats-clicks` `TouchArea` —
      so "the scrim swallowed it" is measured, not reasoned. Every script
      pins the window with `resize:1440x900` first: the card coordinates
      are that geometry and no other (at 1024x768 the card sits higher and the field click lands ~66 px below the field, on the summary line), and the over-the-field strand is not driven off the
      Linux runners, where the font metrics above the field could drift the
      click onto the destination picker. The export test runs over a folder
      of tiny synthetic RAWs rather than a `--synthetic` session, because
      the export offer is off for a session with no paths, and it needs a
      grid DEEP enough to scroll — a three-frame folder does not scroll at
      any zoom, which would make the control vacuous; it asserts the
      load-settled edge landed before the first wheel, because that edge
      writes `vp_y` itself.
- [ ] Manual acceptance (per release): 5,000-file A1 folder (a bad evening, per
      persona review) scrolls at 60 fps after thumbs load; pick→auto-advance→pick
      loop in loupe has no perceived latency.

## Debug facilities (env vars, app-level)

Documented because they ship in release builds (validator finding):

- `FASTCULL_TRACE=1`: eprintln any UI-thread phase (`handle_nav`, `refresh`
  stages, texture adoption) exceeding 20 ms, plus loupe-ready marks — the
  evidence channel for hang reports. The thumb path is traced at BOTH of
  its stages, because they are seconds apart and only the first touches
  the file: `thumb bytes idx N` (the pipeline read the embedded JPEG, at
  scan time) and `thumb landed idx N` (the kitchen decoded it into a
  texture — only for cells near the view, and nothing evicts it within a
  session, so the line is also "the loupe's thumb rescue is armed for N"
  for the rest of that session). A test that manufactures a mid-session decode failure
  needs both: the first says the corruption is safe to apply, the second
  that the rescue rung had a texture to skip (issue #50).
  The IPTC panel's field rows report their own geometry the same way:
  `iptc field N laid out at X,Y size WxH`, in window-logical px, emitted
  whenever the layout moves row N — and once per row when the conditional
  panel's items are instantiated, which is the moment the row becomes
  hit-testable at all (issues #13/#61). A driven click on a panel field is
  a point chosen before the app existed, and whether the field is THERE
  yet is a layout outcome a loaded machine can be seconds late with — so a
  script clicks it BY NAME (`click:iptc field 0`) and the test asserts
  afterwards that the point the harness resolved was inside the rectangle
  — the calibration guard, which since issue #70 reads that resolution
  instead of a coordinate the test repeats. Both hooks are
  needed: the instantiation report is the only one rows 0 and 1 ever emit
  (their first computed position is already their last), and the
  move report is what tells a script that a `resize:` has landed.
  The two dialog cards report the same way (issue #62): `clip card laid
  out at X,Y size WxH`, `clip buttons laid out …`, and the `copy` pair,
  from `changed absolute-position` and `changed height`. Their heights
  follow their content now, so a card's rectangle is a layout outcome
  rather than a number in the .slint file, and the property that matters
  — the button row is inside the card — is a relation between the two:
  `buttons.y + buttons.h <= card.y + card.h`. No screenshot can stand in
  for it: neither card clips, so a row laid out below its card is drawn
  over the scrim looking almost right and stays clickable. A card's mark
  is also the landing witness for a `resize:` while a dialog is up — the
  card is centred, so its x moves with the window's width.
  A dialog body that scrolls reports its offset the same way —
  `clip body scrolled to Y` / `copy body scrolled to Y`, 0 at the top and
  negative going down, emitted on change — because a body holds text and
  no cursor, so nothing else in a dump moves when PgDn does. `key:` also
  understands `pgdn`, `pgup`, `home` and `end` now; before issue #62 the
  grid's own PgUp/PgDn/Home/End were reachable only through the `nav`
  tokens, which bypass the key path.
  Two more marks let a driven run gate on the app instead of the clock
  (issue #62): `clip export finished run N` and `copy finished run N` fire
  when the respective report card goes up, and `load settled gen N`
  carries the session generation — `session-gen` counts from 0 for the folder the app
  opened with, so the second folder a script opens settles as `gen 1`.
  That generation is what makes the #13 "next occurrence" limitation
  survivable for a session swap: every session used to settle with the
  same sentence, so `wait:load settled` could only ever match the first.
  The `run N` on the two finish marks is the same idiom against the same
  limitation (issue #70): N counts the copies (respectively exports) this
  PROCESS has started, 1-based, incremented where the worker is launched
  and carried across a session swap like the remembered destination — so
  a script's second copy waits on `copy finished run 2` instead of being
  satisfied by the first one's mark, which is what the two clash tests
  replaced with an 800 ms and a 1.3 s guess (the copy one was a Windows
  red at v0.13.0: `copystate` read 1, the copy was still running). A bare
  `wait:copy finished` still matches, being a substring. A run CANCELLED
  by a session swap emits no mark at all — cancelled is not finished, and
  the dialog's report says which — so a wait for that run's number ends
  the script, correctly: nothing it waits for will happen.
  **Focus, as it moves (issues #63/#64)**: `keysfocus` at a dump is one
  sample of a value that changes several times inside a single input
  dispatch, which is how a stranded keyboard shipped twice — a run could
  say the keyboard was lost, never by what. Four marks make the whole
  path readable, all through `trace_mark_with` so they cost nothing when
  tracing is off:
  - `focus: <what> gained|lost` — from the `changed has-focus` handler of
    the main key scope (`keys`), each panel field row (`iptc field N`),
    the keyword field, and each dialog scope (`copy dialog`, `clip
    dialog`). A `gained` with no matching `lost` from the previous holder
    is the dangling-weak signature, printed.
  - `focus-keys (<reason>)` — a claim was MADE, tagged at every call
    site: `swap`, `panel-open`, `panel-close`, `modal`, `rebuild`,
    `deferred`, `copy-dialog`, `clip-dialog`, `cell-click`, `fit-click`,
    `overlay-click`, `template-apply`, `revert`, `field-clear`,
    `field-accepted`, `keyword-removed`, `keyword-accepted`,
    `keyword-init`, `keyword-watch`, and the two behind-a-cover bounces.
    `deferred` is the one that says a queued claim has ARRIVED, which is
    not the same event as the caller queuing it — the gap between those
    two lines is exactly what issue #63 turned out to be.
  - `iptc rows rebuilt (gen G)` — the item-tree mutation itself, emitted
    just before `set_iptc_fields` replaces the field-rows model.
  - `iptc keyword field created` — the keyword editor's `init`, the other
    moment an editor can take focus without any click.
  Read together they answer "who held the keyboard, what destroyed it,
  who asked for it back, and when the claim landed" from one log.
  **`window geometry WxH grid GWxGH` (issue #65)** — emitted from
  `presenter::detect_drift` when the geometry it compares has changed
  (and once at the first laid-out refresh), i.e. at the instant a new
  geometry reaches the layout. Narrower than "every relayout", and
  deliberately: a PANEL TOGGLE relayouts the grid but emits no mark,
  because that path consumes the geometry change before `detect_drift`
  gets to compare. The upshot is a feature — no panel toggle can satisfy
  a geometry wait, so a script waiting for a window size cannot be fooled
  by a dock opening — but it is a carve-out, not a general rule, and a
  script that wants to gate on a toggle must wait on a panel mark
  instead. It is the acknowledgement `resize:` never had: `geometry at
  shutter` is the only other geometry witness and it fires once, at the
  end. **Both terms are LOGICAL pixels** — `Window::size()` is physical,
  so a HiDPI runner at scale 2 would report `2400x1600` and never match
  the `1200x800` a script asked for; the window size is divided by the
  scale factor and the grid terms are logical already.
  The wait is an EXACT substring match on a `{:.0}`-rounded logical size,
  which bounds where that holds: a fractional-scale runner (1.25, 1.5)
  can grant a non-integer logical size that rounds to a neighbour —
  `1200x800` requested, `1199x800` announced — and the wait then hangs
  its full 30 s and ends the run. The signature is a `never satisfied`
  line on an otherwise healthy machine, with a `window geometry` mark in
  the log one pixel away from the one asked for; the fix is to ask for a
  size that survives the runner's scale, not to loosen the match.
  **What a satisfied `wait:window geometry WxH` promises, exactly:** the
  app's LAYOUT reached that geometry — the relayout path ran, columns and
  cell sizes were recomputed for it, and the re-anchor logic saw it. It
  does NOT promise the window is still that size when the run ends.
  Whether the window STAYS is seat- and size-dependent, which is exactly
  why no test may assume it. On the development seat `resize:1200x800`
  measured 10 runs of 10 where the layout reached it and the compositor
  reverted the window to 1440x900 some 31-38 ms later, so `geometry at
  shutter` read `grid 1440x800` — while `1440x700`, `1000x700` and
  `1300x750` stuck on that same seat, and the validator's seat did not
  revert `1200x800` at all. The practical consequence, worth knowing
  when reading these tests: on a reverting seat the three tests that ask
  for `1200x800` (`grid_resize_shrink_keeps_content_anchored`, whose
  control run makes four scripts, `grid_resize_grow_at_bottom_stays_at_
  bottom` and `grid_resize_at_top_stays_at_top`) run their post-resize
  steps at 1440x900, so the grow case is really 1440 -> 1500. The
  relayout path they exercise is genuine either way; the geometry they
  exercise it AT is the compositor's choice. That is the honest limit of a request-with-no-reply,
  and it is enough for what the resize tests assert (the app's REACTION
  to a geometry change); a test that needs "and it stayed" must read
  `geometry at shutter` instead. See also the issue #61 paragraph in the
  test ledger, which is the same fact seen from the other side.
  **`keysfocus` IS NOT "the keyboard is alive" (issue #63, 2026-08-30) —
  every focus test must assert by ACTING or by the token.** Slint sends a
  `FocusOut` when the WINDOW is deactivated
  (`WindowInner::set_active(false)`), but `WindowInner::focus_item` is
  untouched and keeps routing key events to the same scope. So an
  unfocused window reads `keysfocus=false` with a perfectly live
  keyboard: proven in a driven run with no clicks at all — `keysfocus`
  went false on its own, and the next `key:+` zoomed the grid. Both of
  the reds originally reported for issues #63 and #64 read
  `keysfocus=false` at a dump whose run went on to zoom with a `+`, so
  the SYMPTOM those issues reported is this artifact; the ownerless
  window they led to is separately real and separately measured. A
  keystroke that acts cannot be faked by a deactivation, so
  `session_swap_mid_field_edit_discards_and_keeps_the_keyboard` now sends
  `key:+` 50 ms after the swap and requires the zoom, and the other focus
  tests assert `focusowner=` instead.
  That field is the fourth thing a dump carries about focus (appended
  2026-08-30): the owner token itself — `0` the main key scope, `1..=N` a
  panel field row, `N+1` the keyword field, `-1` a dialog's own scope. It
  answers "WHICH element does the app believe holds the keyboard", which
  is a different and stronger question than `keysfocus`'s "is the main
  scope's `has-focus` set". **Every `keysfocus` assertion in the
  screenshot suite was converted to it** (20 of them, 2026-08-30): the
  `=true` ones were false-REDS waiting to happen — two fired in release
  suites the same afternoon, one with `zoom` proving the menu action and
  the following `+` both worked, one in a run whose EVERY dump read
  `keysfocus=false` including those taken with no dialog up at all — and
  the `=false` ones were weak besides, since "not the main scope" is
  equally true of a stranded keyboard. `keysfocus` stays in the dump: it is still the only
  reading of the real `has-focus`, and comparing it against the token is
  how a deactivation is recognised.
  `winactive=` was considered and NOT added: Slint 1.17 exposes window
  activation only through `i-slint-core`'s internal `WindowInner::active`
  (the `.slint` language has no `Window.active`), and `fastcull-app` does
  not depend on that crate. The artifact is already readable in a trace
  as a `focus: … lost` that no `gained` follows and no `focus-keys (…)`
  precedes.
- `FASTCULL_DRIVE="6000:one2one;9000:grid;12000:quit"`: timed injection of
  nav actions (same names `handle_nav` takes, plus `quit`, `iptc` — the
  panel toggle, issue #12 — `about`/`shortcuts` — the modal toggles,
  issue #23 — and `resize:WxH` in logical pixels, issue #16: the
  wrong-photo-after-resize bug class needs real window resizes
  drivable or it ships regression-blind) for headless reproduction and
  QE runs — Wayland offers no external input automation.
  **`resize:` is a REQUEST, and a script must assert its landing with
  `wait:window geometry WxH` (issue #65).** The token calls
  `Window::set_size` and returns; the compositor is free to answer late,
  to answer with a different size, or never to answer at all — the issue
  #61 investigation measured 9 loaded runs in 10 where a `resize:1200x800`
  went unanswered for the life of the run. A test that does not wait is
  testing the DEFAULT geometry, and three of the six resize tests passed
  with the token neutered because their invariants hold at 1440x900 too.
  All six gate on the mark now; with the token neutered they are 6/6 red
  at the wait, naming the geometry that never arrived.
  Note which failure each half catches. An UNSATISFIED wait ends the run
  through the app's own `exit(1)` after the 30 s cap, so a compositor
  that never answers is loud on its own and needs no assertion. The
  `stderr.contains("wait:… (satisfied")` guards in the three tests whose
  invariants hold at any geometry catch the other failure: a wait step
  that was never REGISTERED — a dropped or misspelled token, a `;` eaten
  by an edit — where the app exits 0 and the run is back on the clock
  with nothing complaining. Driven NAV
  keys respect the modal containment exactly like real keypresses
  ("drive swallowed by modal" trace); `quit`/`iptc`/`resize` and the
  modal toggles themselves remain live harness plumbing, like the menu
  bar. That mirror is CONVENIENCE, not evidence: it is the harness's own
  `if`, not the FocusScope's, so a test that asserts containment must
  press a real key (`key:n`), not a nav token — the two containment tests
  did the latter for months and would have stayed green with the shipped
  guard deleted (issue #13's fidelity note). The `about`/`shortcuts`
  toggles are the menu item's own `activated` body — the visibility flag
  plus `modal-opened` — and nothing else: they do not force focus (that
  bare `focus-keys()` went away with issue #41), and what they cannot
  exercise is the MenuBar's post-activation focus restore, which is why
  the focus-sensitive tests reach the popups by clicking the real Help
  menu items — a strand gated by `menu_clicks_are_calibrated()`, so on
  the Windows runner those tests fall back to the token path and the
  fidelity fix is, in practice, exercised on Linux only (QE 2026-08-29).
  That gate is NOT about font metrics, which is what this paragraph and
  the helper's own comment used to say (corrected 2026-09-02, issue #70):
  on Windows there is no in-window MenuBar to click at all — the winit
  backend reports `supports_native_menu_bar()` there (its `muda`
  dependency) and the menus are the OS menu bar, outside the client area,
  where no dispatched pointer event can reach them. Within the Linux
  in-window bar the item geometry does follow the platform's font
  metrics, which is what the coordinates are calibrated for.
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
  limitation of the `;`-separated script format).
  `copytemplate:TEXT` (2026-08-22) fills the Copy Picks rename field and
  replans exactly as the field's own `edited` callback does, so a driven
  run gets the preview and the plan a real keystroke would produce
  without focusing a LineEdit and sending one key event per character.
  Use it AFTER the `Ctrl+E` that opens the dialog: opening deliberately
  clears the field (the remembered template is offered, never
  pre-applied), so a template set before it is wiped.
  `copydest:PATH` (2026-08-21) is the Copy Picks destination picker minus
  the native rfd dialog: it sets the destination the dialog shows on its
  next `Ctrl+E` (the open path keeps an already-chosen destination over
  the remembered ui.toml one), so a script can drive a real copy →
  hand-delete → copy run — the exact flow the copied-this-session re-run
  bug shipped through, untestable before (fileops.md, "already copied
  means still there"). Same `;` limitation as `open:`; use it BEFORE the
  `Ctrl+E` that should see it (it does not replan an open dialog).
  `clipdest:PATH` (2026-08-27) is the same thing for the video export's
  destination (video-export.md): the export writes a NEW KIND of file, and
  without this the whole flow — plan line, clash question, the `.mov` on
  disk — is unreachable headlessly. Same `;` limitation and the same
  "use it before the `Ctrl+Shift+E` that should see it" rule.
  `key:ctrl+shift+<k>` (2026-08-27) dispatches a real two-modifier chord,
  which the video export needs: `Ctrl+Shift+E` and `Ctrl+E` are two
  different actions and differ ONLY by the Shift modifier, so a harness
  that could not hold Shift could not tell them apart. `key:shift+<k>`
  alone holds Shift only (first used 2026-08-28 for Shift+`]`; the
  modifier is what separates it from `]`, and the `}` spelling is sent as
  plain `key:}`).
  Caveat for script
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
  `click:<element>` (issue #70) is the same click at the CENTRE of the
  rectangle the app last reported for a self-reporting element — the
  names of the layout marks above: `iptc field N`, `copy card`, `copy
  buttons`, `clip card`, `clip buttons`. The harness keeps those
  rectangles in a table written by the same callbacks that emit the marks,
  UNCONDITIONALLY (a resolved click must not depend on whether the run
  also asked for a trace log), and resolves the name AT DISPATCH TIME, so
  the point is the layout this run produced rather than one measured
  elsewhere. It echoes `drive ptr click X,Y (<element>)`, unobserved like
  the other pointer echoes, and that echo is what a test reads to assert
  the click landed inside the rectangle. A name with no layout mark yet is
  never a click into nowhere: the step traces `drive: click: no layout
  mark for <element> — abandoning the run`, prints the same sentence on
  bare stderr and exits non-zero — the `wait:` cap's shape, and for its
  reason (the step holds the shutter, so the silent alternative is a
  half-driven run photographed anyway).
  What the table cannot know is whether the element is still THERE: a
  mark is never retracted (Slint has no destroy hook to retract it
  from), so a name whose element has since gone — the panel closed, the
  dialog dismissed — resolves to its LAST rectangle and clicks whatever
  is under it now, silently (validator 2026-09-02: `key:i`, `key:i`,
  `click:iptc field 0` → a click into the grid, exit 0). A script names
  only elements it has just put on screen, and its outcome assertion —
  `focus: iptc field 0 gained`, the dialog's answer — is what catches
  the stale case; `assert_click_resolved` alone does not.
  **The rule: a traced element is clicked by NAME, never by coordinate.**
  A literal point is measured on one platform's layout and lands silently
  somewhere else on another. The measurement that made this a rule: on
  Windows the menu bar is the OS menu bar, outside the client area (see
  "Window chrome"), so every in-window y should sit about 40 px higher
  than under the Linux `fluent` bar's 40 px band (source-verified; the
  number is pending the Windows artifact, see "Window chrome"), and the
  seven clicks, in five tests, that hit the Title field at `1290,177`
  were landing 43 px below its centre on Windows — three reds at v0.13.0
  (issue #70; the coordinate appeared 12 times in the file, seven script
  steps and five assertion markers). Coordinates remain right for what
  reports no rectangle: grid cells (derived from the column geometry), the
  menu bar, the panel's padding strip, the dialog answer rows.
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
  `wait:<trace substring>` (issue #13, 2026-08-29) holds the REST of the
  script until a trace mark whose label contains the substring has been
  emitted. Every other step is an absolute single-shot timer, i.e. a
  guess about how long the app will take: three tests hand-rolled the
  missing primitive with observer threads and channels, and two clicked
  at a timestamp a loaded machine did not honour (issue #61 — the panel
  field was not laid out yet; the 1:1 texture was not up yet, so the
  point belonged to a different surface). The steps after a `wait:` keep
  the GAPS the script wrote, measured from the moment it fires (their
  timestamps are rebased on the wait's own), so a wait already satisfied
  when it comes due changes the schedule not at all and a late one shifts
  the tail bodily. Matching is against the mark's LABEL, not the
  `fastcull-trace: [ms]` prefix, and includes marks emitted BEFORE the
  wait's own step: the substrings are registered when the script is
  parsed, so `wait:thumb landed idx 11` is satisfied by a thumb that
  landed ten seconds earlier ("has this happened yet?", never "happen
  next"). Four recorded limitations of that shape: a wait cannot ask for
  the NEXT occurrence of a mark already emitted once — find a substring
  unique to the state you mean, or keep that step on the clock. (Waiting
  for the second session's settle was the example, and it is now
  expressible: the mark carries the session generation,
  `wait:load settled gen 1`, issue #62. That is the pattern for the
  limitation generally — put the thing that DIFFERS into the mark. The
  second instance is the rebuild reclaim, `wait:row 0 (gen K)` from
  `focus-keys (row 0 (gen K))`, where K is `iptc-rebuild-gen` at the
  row's birth, i.e. the number of content-changing rows rebuilds so far:
  it is what lets a script hold its keys until THIS rebuild's claim has
  landed instead of trusting a timestamp against the reclaim gap the
  owner-invariant table prices — issue #69. K is a property of the
  script, so a script that gains or loses a rebuild must re-read it; the
  failure is the loud one, a wait that is never satisfied. Two
  corollaries (validator 2026-09-02): the cursor-move script opens the
  panel only after `wait:load settled gen 0`, so a slow runner whose
  metadata lands after the panel opened cannot add a rebuild and shift
  K; and where the row's `gen` does NOT differ, the re-assert's own mark
  does — `focus-keys (<why> -> row N)`, `menu -> row 0` once a menu item
  has activated, `restore -> row 0` after a rebuild — which is what the
  menu-item strand waits on before its keys. A DISMISSED menu emits no
  claim mark of its own, so that strand stays on the clock, and its test
  says so. The third
  instance is `run N` on the copy/export finish marks); "past"
  starts at `harness::install`, which runs AFTER the session dispatch and
  the first refresh, so a mark from the opening scan or the first layout
  is never observed; only the APP is observed, never the harness narrating
  its own script (the `drive: <action>` echo, the pointer/wheel echoes,
  the modal-swallow line and the wait reports are all emitted unobserved,
  because each quotes the script's own text and would otherwise let a wait
  fire on a later step's echo — `QEDUMP` lines stay observable, being app
  state); and a substring cannot contain `;`, the step separator, which
  splits it first (the same limitation `open:PATH` carries).
  Because it is a plain substring, a wait can pin the GEOMETRY a
  script's coordinates were measured in — `wait:iptc field 0 laid out at
  1150` is satisfied only in a 1440 px-wide window — which matters because
  `resize:` is a REQUEST to the compositor, and under load it can go
  unanswered for the whole run: that is the other half of issue #61 (the
  click was fine, the window was never resized, and the panel was 240 px
  from where the script thought). A script that needs a non-default size
  must therefore wait for evidence of it, and one that only needs a KNOWN
  size should ask for the default it already has. A step whose timestamp
  is EARLIER than the wait's fires immediately when the wait is satisfied
  (the rebase saturates at zero); it does not run before the wait, and
  timestamps below the wait's own carry no meaning beyond their order.
  It does NOT require
  `FASTCULL_TRACE=1` — the switch decides
  what is printed, not what the app may observe about itself — though
  every test that waits also traces, because the failure below is a trace
  line. A wait that is never satisfied never lets the rest of the script
  through silently: its own step holds the shutter, and after 30 s
  (bounded under the screenshot harness's 90 s watchdog, so the app is
  still alive to say why) it prints `drive: wait never satisfied:
  <substring>` on the trace and on bare stderr and exits non-zero. Two
  budgets bound how long a wait may reasonably take: that 30 s buys the
  diagnostic only for a wait whose step comes due before ~60 s (later, the
  harness watchdog's generic timeout wins), and the shutter's own 60 s
  readiness cap runs from `shutter::arm` and is NOT paused while a drive
  step is pending — a wait that takes 25 s leaves ~35 s for the cursor's
  texture to arrive, which in a debug build over a 50 MP frame is a real
  margin. An
  empty substring would match the next mark whatever it is, so
  `wait:` with nothing after it is dropped like any other malformed step.
  `dump.<label>` traces the focus/surface state for test assertions:
  `keysfocus` (the main key scope's real `has-focus`, via the
  `dbg-keys-focus` debug property), loupe/zoom state, panel and modal
  visibility, the copy dialog's visibility, plan summary and rename
  template, the revert-slot label, and the status line. Since 2026-08-21
  it also carries `copystate=` (0 plan / 1 running / 2 report / 3 the
  clash question) and `confirm=` (the question's text): the clash question
  is a STATE of the Copy dialog rather than a second modal (fileops.md),
  so `copy=true` alone cannot tell a plan preview from a question about
  replacing files — without those two fields the one irreversible
  operation in the app would be assertable only down to "a dialog exists".
  Since 2026-08-27 the same block exists for the video export —
  `clip=`, `clipstate=` (the same four states), `clipavail=` (is there
  anything to export), `clipsummary=` (the plan line), `clipskipped=`,
  `cliperror=`, `clipreport=` and `clipconfirm=` — for the same reason:
  it is the app's second irreversible file operation, and its plan line
  is the only place the user is told the frame rate before pressing
  Enter. Keyboard focus was otherwise INVISIBLE to
  every headless run — a stranded keyboard could not even be asserted.
  It also carries the loupe pan block (`soft`, `vx`/`vy`, the
  fractional pan centre, the desired factor — issue #46): a
  wrong-position frame is precisely a state nothing re-renders, so
  render-time traces (which fire on CHANGE) cannot see it; the dump
  makes the overlay's position observable at a scripted instant.
  Since 2026-08-29 it ends with `vpy=` — the grid Flickable's scroll
  offset, in Slint's own sign (0 at the top, negative going down).
  Whether a wheel moved the GRID was observable only at SHUTTER time
  (the `geometry at shutter` trace carries a `scroll` term), never at a
  scripted instant, which is how two modal scrims that let the wheel
  through to the Flickable behind them went unnoticed (issue #49). New
  fields are APPENDED (`dump_field` finds `name=` by prefix and does not
  care about order — appending is for the reader and for small diffs,
  not correctness).
- `FASTCULL_NO_CONFIG=1`: makes `ui.toml` (the remembered copy
  destination/template and the video export's destination) unreachable
  for both load and save — what
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
