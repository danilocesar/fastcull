# Module spec: grid & loupe UI (`fastcull-app` + `filter.rs`, `grid.rs`, `zoompan.rs`, `pointer.rs`, `selection.rs`, `transit.rs`)

## Purpose

The one continuous view: a zoomable virtualized grid that morphs from many
columns to a single-image loupe with 1:1 pixel zoom — plus the filter/sort
bar, the badges, the window chrome and the IPTC panel shell. Every policy
is a pure function in core; the app crate bridges Slint and applies what
core decides.

## Behaviour

### The zoom model (one axis, seamless)

- Column count `N ∈ {12, 8, 6, 4, 3, 2, 1}`; `+`/`-` step through
  (Ctrl+scroll is the M2 deferral, reserved in the pointer contract; pinch
  later). At `N = 1` the view is the **loupe**: first stop fit — **the
  WHOLE frame on screen**, the requirement — rendered from the best rung in
  hand (the mid on displays up to ~2K, the full-res above; raw-pipeline.md's
  ladder); then the ×1.5 ladder, capped at 1:1. Arrows NAVIGATE at every
  zoom, never pan. Zooming out of the loupe returns to the grid centred on
  the current image.
- **One-column cell bounding** (2026-07-30, user-approved): cells are 3:2
  (`CELL_ASPECT`) and span the grid width, which made the one-column cell
  taller than the viewport on every normal window (16.6 % of the frame
  hidden at 1440×900, 23.4 % fullscreen on 1080p, unreachable by any input).
  At one column the cell is bounded by the grid viewport (`cell_height =
  min(cell_width / CELL_ASPECT, viewport_height − 2·CELL_GAP)`,
  `GridLayout::new`), so the image contain-fits with pillarbox bars. The
  photo renders ~17-23 % smaller than the old fill-width crop (persona: pay
  it happily — completeness is what fit is for; sharpness is the ladder's
  job). Multi-column grids are NOT bounded. The bars stay pure black — no
  filmstrip, no histogram, no info panel. Pre-layout refreshes skip the
  bound. Residual: the zoom overlay covers the filter bar while the fit view
  does not, so the overlay's factor-1.0 extent is ~6 % larger than the fit
  cell and the first rung magnifies ~1.59× rather than 1.5× — size only.
- **The badge policy in the loupe**: the `✓ copied`, `▶ exported` and `×N
  burst` cell badges, anchored to the cell bottom, are visible at one
  column, while the MARK is suppressed there (cells get `pick = 0`): the
  state pill owns the mark, and the grid's 40 % reject dim stays out of the
  loupe; "already copied", "already in a video" and "burst of N" have no
  pill and are what a last pass before bed wants to see. One channel per
  fact.
- **The ladder** (user decisions 2026-07-25): each `+` multiplies the factor
  by 1.5 from fit, computed as `fit × 1.5ⁿ` so `-` retraces the stops with
  no drift (a stop within rounding of 1:1 folds into it); a step that would
  exceed 1:1 lands exactly at 1:1; zoom never passes 1:1 (beyond it you
  judge the embedded JPEG, not focus); when 1:1 ≤ fit, `+` at fit does
  nothing. **Anchor**: every keyboard step keeps the centre of the visible
  region fixed — at fit the image centre, after a pan the subject the user
  panned to. **`Z`**: fit → 1:1; from 1:1 or any factor → back to fit. A
  single click above fit is "centre HERE" and at fit does nothing;
  double-click reaches 1:1 (the pointer contract).
- **Persistence across images**: navigating or Y/N-advancing keeps BOTH the
  factor and the pan centre, carried as a fractional centre of the image
  and clamped for differing dimensions and orientations (lock 1:1 on the
  eye, arrow through the burst, Y/N each frame). Returning to fit forgets
  the pan; `G`/Esc from a factor return to the grid at the previous grid
  zoom and discard the factor (persistence is for walking images inside
  the loupe, not across grid round-trips). The persistence holds visually
  across EVERY frame, decoded or not (the render ladder).
- Two implementation rules: the overlay is a PERMANENT element whose
  visibility toggles, never a conditional — a re-created element
  initializes its viewport before the offset write lands, one 0,0 frame per
  transition (issue #6); and Rust is the ONLY writer of the overlay's
  viewport offsets (issue #46) — a drag is reported by the overlay's touch
  surface as an explicit `loupe-dragged` event, folded through the pointer
  machine into the pan centre, and the offsets are rewritten synchronously
  from that centre; no Flickable, no read-back. Intent is only ever claimed
  from a POSITIVE input signal, never inferred from displacement — no
  elimination list of displacement causes stays complete.

### Transit and settled (user requirement 2026-08-01)

*"While I'm holding a key and rapidly moving between shots I don't need the
image to be as good as possible, I need it to move fast, feeling almost
like a video. But when I release the key, then I want quality to be high."*
Three request states govern what is ASKED of the decoder, never what is
DISPLAYED — the renderer always shows the best rung in cache:

| state | trigger | request |
|---|---|---|
| TRANSIT | frame changes < `TRANSIT_GAP` (250 ms) apart | the mid rung ONLY, over a wide ring leaning the way of travel |
| SETTLED | the user stops (~250 ms, the reserved lane's `FOCUS_DEBOUNCE`) | the app's real target for the focused frame |
| SETTLED-AND-IDLE | after that lands | full-res look-ahead on the ±`PREFETCH` neighbours |

- **Every ring is a VIEW-ORDER ring** (issue #46): transit, settled,
  look-ahead and the deferred-revival gate are planned in view positions
  and mapped to ids at request time (`LoupeEngine::set_view`, re-keyed by
  the app on every view recompute). An id-space ring on a capture-sorted
  multi-body folder warmed frames no arrow could reach while every real
  neighbour stayed cold — the deterministic per-step fit-flash of #46. The
  travel-direction latch compares view positions for the same reason.
- **The geometry never changes in transit**: the carried factor and pan
  centre, never a drop to fit, in every reachable path — jumps (`[`/`]`,
  PgUp/PgDn, Home/End) included; the render ladder below covers them. The
  renderer traces `loupe overlay dropped` if any path re-opens it, and the
  regression tests assert the excuse-less form away.
- SETTLED-AND-IDLE is not optional: requesting only the focused frame on
  settle would make tap-stepping through a burst at 1:1 pay a full decode
  on every frame, forever.
- The same rule at every factor, fit included: on displays up to ~2K, fit
  asks for less than the mid, so `transit_request` is a no-op; on QHD and
  4K, transit DOES engage at fit and a hold shows mids upscaled ~1.6–2.4×
  until release — the designed trade applied consistently, not yet
  eyeballed by the user on a 4K monitor (issue #60 is parked).
- Direction is latched at the index change, never re-derived per call: the
  app re-focuses the SAME index on every refresh, and per-call derivation
  flipped the ring forward within milliseconds of every backward step.
- The transit request is a rung the mid actually SERVES: `serves` allows a
  1.25× upscale, so a 1616 mid covers 2020 px; requesting 2048 sent every
  transit frame to full-res and measured as no improvement at all.
- The settle guarantee lives in the engine's reserved lane, not in the app
  (the app's refresh loop goes quiet exactly when nothing is decoding);
  the settle the user feels is ~250 ms — `SETTLE_DEBOUNCE` (150 ms) only
  decays `in_transit`, and both run from the same origin, so they do not
  add. The "◌ loading" pill is up throughout a hold — a steady pill, not a
  flicker (784 of 787 rendered frames, one state change).
- Measured on the 8-core laptop, cold cache, 1:1: a 150-key hold at 40 ms
  puts 139 of 150 frames on screen (was 12 of 150), key→pixels median 2 ms
  (was 119 ms), p90 3 ms (was 9.3 s); an 800-key hold makes 2 full-res
  decodes (was 182); 20 keys at 120 ms show 18 (was 9). Two limits: a short
  burst barely benefits (the first ~340 ms of a hold from a cold loupe
  stall either way), and stop-to-sharp is ~40 ms slower at 120 ms repeats —
  accepted, motion-first. An adaptive settle was measured and rejected: it
  sharpened 200 ms sooner but a 60 ms floor was fragile to repeat jitter.
- Known and deferred (none a spec acceptance criterion): a `Y`/`N` chain
  faster than 4 marks/s is classified as travelling and judged from the mid
  — DOCUMENTED AS INTENDED (user decision 2026-08-01: at 4/s nothing
  changes; above ~4.2/s the old code showed BLANK frames where this shows
  soft ones; the recorded fix if a rating workflow ever bites is an
  exclusion keyed on the mark keys); a wraparound cursor would lean the ring
  wrong for one refocus; stop-to-sharp at the ENGINE is 371–408 ms ±20 ms
  while the APP-level 721–1047 ms is compositor overhead — measure at the
  right layer before tuning; no hysteresis on `moving` (a stretched gap
  mid-hold fires a full-res ring); entering a hold commits up to two
  uninterruptible full-res decodes; transit queues with `focus_origin =
  true`, so leaving the loupe mid-hold leaves up to 11 stale entries ahead
  of grid cells; the settle guarantee's `Slot::Wait` is untimed for a
  core-only consumer.

### The render ladder (issues #21, #46; core `transit`)

Any factor above fit requests the top rung outright (`display_long =
u32::MAX`). **Never show upscaled pixels UNFLAGGED, and never leave a frame
at rest unsharp without the cue**: an above-fit view rendered from below the
top rung shows the top-left "◌ loading" pill, removed atomically when the
sharp texture swaps in. The ladder: full-res (sharp) → the mid rung (soft)
→ the cursor's own 320 px THUMB (soft — ~25× mush at 1:1, and right during
transit, where position and identity continuity is what the eye tracks;
persona MUST-HAVE) → the residual HOLD. When not even the thumb exists (a
cold-start edge), the overlay keeps the PREVIOUS image's pixels at the
carried geometry, pill on — the video-player dropped-frame convention; the
alternatives were the fit strobe (the bug) or a black frame. That is a
knowing, bounded breach of "never the previous frame": the mark badge and
the status bar name the NEW image over the old pixels (addressing is
correct; only the judged pixels lag). The bound is double: a decode FAILURE
of the cursor image drops to fit immediately (the strip owns the failed
badge), and `OVERLAY_HOLD_CAP` (250 ms, one settle window, PER CURSOR IMAGE)
caps a wedged decode; a capped drop traces `loupe overlay dropped … (hold
cap)` and the overlay re-raises the moment any rung of the cursor image
lands. A cold ENTRY with no pixels of the image keeps the overlay down until
the first rung. A decode-FAILED cursor skips the thumb rescue — a live thumb
texture would otherwise sit at 1:1 behind a pill that can never complete,
hiding the failed badge; fit plus the badge is the honest floor. One
causally unavoidable transient is accepted: the first focus of a freshly
dead file MAY render the thumb until its decode attempt fails, and nothing
may be asserted on that order (issue #50). A VIRGIN pin (nothing resolved
yet this session) renders the mid at its native resolution, floored at fit;
an INFINITY-pinned desire (`Z` during transit) renders at the last resolved
factor. Same behaviour at all factors.

The whole block is core (`fastcull_core::transit`, 2026-08-11):
`render_rung(&RungInputs) -> RenderDecision` — which rungs are in hand,
whether the cursor's decode failed, whether the overlay is wanted and was
up, the hold's pair — is TOTAL and swept over all 320 input combinations
against the pre-move app ladder, so the extraction is pinned as an
equivalence; `evict_fullres(held, cursor, view)` — the cursor's texture is
never the victim, an out-of-view entry goes first, a tie goes to the LATER
slot; `FULLRES_RING = 2·PREFETCH + 1`. The cap duration is passed in as a UI
tuning value. The app keeps what only the app can do: texture lookup, the
clock, the extent math, the property writes. The landing frame's full-res
preempts the transit backlog via the focus/want-culling priority.

### The pointer contract (state machine; user request 2026-07-26, issue #11)

The mouse means different things in the grid and in the loupe, and the
difference is an explicit state machine whose state is the zoom level —
the source of truth for every gesture. The user's intent: *in the
multi-image view the wheel scrolls; once a single image is shown the wheel
zooms; a click centres the clicked point; a double-click goes to 1:1 with
the clicked point centred; drag moves the image in the single-image view;
dragging in the multi-image view is reserved.*

| State | Meaning |
|---|---|
| `Grid { columns: N }`, `N ∈ {12, 8, 6, 4, 3, 2}` | multi-image view |
| `Fit` | single image, zoom factor `1.0` (the whole image is on screen) |
| `Zoomed { factor }`, `1.0 < factor ≤ max` | single image, above fit; `factor == max` is 1:1 |

`N = 1` is not a grid state — one column IS the loupe, i.e. `Fit` or
`Zoomed`. Marks, cursor, filter and selection are untouched by the machine.
Inputs, normalized from Slint: `Wheel { notches, pos }`, `Click { pos }`,
`DoubleClick { pos }`, `Drag { dx, dy }` (the press/release edges live in
the overlay's drag latch); `pos` converts to a fractional image coordinate
through `zoompan::contain_click_frac`. NOT inputs (persona review, user
decision): **Ctrl+wheel** (grid zoom stays the M2 deferral; in the loupe
the modifier is ignored — reserved), **right/middle/thumb buttons**
(explicit reserved no-ops, so nobody grows a context menu into the grid by
accident), pinch and trackpad gestures (out of scope; two-finger scroll
over the overlay image walks the ladder like the wheel since #46).

| Input | `Grid { N }` | `Fit` | `Zoomed { factor }` |
|---|---|---|---|
| Wheel up | scroll the view up; cursor unmoved (browsing) | **zoom in** one ladder stop → `Zoomed { 1.5 }`, anchored under the pointer | one ladder stop up, anchored under the pointer; caps exactly at 1:1 |
| Wheel down | scroll the view down; cursor unmoved | **nothing** (clamped — user decision 2026-07-26: the wheel never falls out of the loupe; `-`/`G`/`Esc` are the exits) | one ladder stop down, anchored under the pointer; a step landing on `1.0` → `Fit` |
| Ctrl+Wheel | grid zoom in/out — still the M2 deferral | **reserved**: the modifier is ignored, the plain-wheel row applies | **reserved**: the modifier is ignored, the plain-wheel row applies |
| Click | move the cursor to that cell + collapse the multi-selection (issue #7 — and since 2026-09-06 the same collapse every plain keyboard move performs, brief 002); Ctrl/Shift variants per the cursor contract | **nothing** — the whole image is on screen, and the keyboard ladder stays center-anchored (user decision 2026-07-26, Q5) | re-center the view on the clicked point; factor unchanged |
| Double-click | **open that image in the loupe at fit** (user decision 2026-07-26 — the first click has already moved the cursor there, so this is purely "enter the loupe"); the previous grid zoom is remembered for `G`/`Esc` | → **1:1 with the clicked point centered** | → **1:1 with the clicked point centered** (already at 1:1: re-center only) |
| Drag | scroll the view (Flickable kinetic drag, today's behavior — **kept**); rubber-band multi-select is the reserved future gesture | **nothing** — nothing is off-screen, so there is no pan axis | **pan the image**, 1:1 with pointer motion, clamped so the image never detaches from the viewport edges; **release stops the image dead — no fling, no inertia** (issue #46, see below) |

Rules the table does not carry:

- **The wheel no longer browses images in the loupe** (user decision
  2026-07-26): movement inside the loupe is keyboard-only — arrows, PgUp/
  PgDn, Home/End, `Y`/`N`, `[`/`]`; the scrollbar drag is the one scroll
  route left.
- **A click at fit does not arm the next zoom** (Q5): `+`/`-`/`Z` stay
  centre-anchored, including right after a click at fit, which stores
  nothing and only claims the cursor.
- **The wheel anchor is the pointer, not the centre**: you wheel toward an
  eye without clicking first. A key has no position and the wheel does. When
  the pan clamp makes the anchor impossible (an image edge), the clamp wins.
- **One notch = one ladder stop** — the identical `1.5ⁿ` stops as the keys
  (`zoompan::ladder_up`/`ladder_down`); high-resolution wheels accumulate
  delta and emit one stop per notch-equivalent (60 logical px — one winit
  notch), remainders carry over, a direction flip resets them; the fit
  surface and the overlay keep separate accumulators.
- **Click and double-click need no timer**: Slint fires `clicked` before
  `double-clicked`, and a click's action (centre on P) is a strict prefix of
  the double-click's. The target point survives because the two `clicked`
  calls rewrite the offsets SYNCHRONOUSLY, so the frozen `mouse-x` and the
  new offset cancel exactly — if the pan write is ever deferred or animated,
  the 1:1 landing point silently moves by most of the viewport.
- **Drag beats click**: an 8 logical px latch on the overlay's touch
  surface; once a drag starts, the release produces no click.
- **Loupe pan has NO inertia — decided, not omitted** (issue #46): release
  stops the image where the hand stopped. The shipped Flickable's fling
  physics survived programmatic sets, so an arrow pressed during the decay
  rendered the next image at the still-animating offsets and the read-back
  folded them into the pan centre until the carried position was lost. The
  grid keeps its kinetic scroll: flicking a grid is browsing; gliding at
  1:1 is judging (persona MUST-HAVE).
- **A double-click needs proximity** and Slint enforces it (`check_repeat`
  restarts the click count beyond 10 logical px); the app holds NO
  proximity state of its own — the bridge re-check shipped with #11 vetoed
  the gesture above fit (2026-07-30) and was deleted, not repaired.
- **Clicks in the letterbox bars do nothing**; at fit, where the `fit-ta`
  surface covers the bars, the wheel over a bar still steps the ladder,
  anchored at the nearest frame edge ("zoom in" is
  unambiguous wherever the pointer sits; "1:1 centred HERE" is not). The
  wheel only zooms over the image: over the IPTC panel, the filter bar or
  the scrollbar it scrolls that widget or nothing.

Implementation: the machine is `fastcull-core::pointer` — a pure
`step(state, input, geometry) -> (state, action)` with no Slint types and
an explicit `Reserved` variant for every reserved pair; the app normalizes
events and applies actions, with NO zoom/pan branching of its own. The
fit-state interception is a permanent, visibility-toggled `TouchArea`
(`fit-ta`) covering the grid area exactly when `columns == 1` and no
overlay is up; above fit the overlay's image `TouchArea` takes the wheel.
Recorded deviations: `Zoomed` × Click is applied directly by
`on_loupe_clicked` (Slint delivers image fractions there), so that arm is
exercised only by its unit tests; a pinned-unresolved 1:1 desire (INFINITY
while the full-res decodes) makes gestures through the machine inert until
the render clamp resolves it (a click in the overlay still re-centres);
while the ceiling is unknown the wheel climbs optimistically but CAPPED
(`pointer::OPTIMISTIC_MAX` — an unbounded ladder reached ~1e38 and a NaN pan
centre); extreme coalesced wheel deltas may emit fewer stops than notches;
a drag started in a bar is inert, and so is the wheel in the zoom overlay's
letterbox bars — nothing on the overlay consumes it since #46; probed inert
at 700x1100, factor 1.5, 180 px bars (2026-08-09), and recorded as inert
until a geometry is found where it is not (extend the wheel surface over the
bars if one ever shows); the scrollbar's wheel swallow deadens its
18 px strip in grid view; and the machine's state is the DESIRED factor,
which the screen may not show yet — during a decode gap anchors compute
against the virtual viewport and self-correct on adoption.

### Virtualization (the M2 risk)

Slint virtualizes ListView only, so the grid is a windowed model
maintained in Rust: a `VecModel<CellData>` holding only the visible rows
±1, mutated in place on scroll and zoom (reuse, never recreate); textures
are `slint::Image` handles; placeholder cells render at once (gray +
filename). `CellData`: image id, texture, pick state, burst count (> 0 only
on a group's first frame), failed, copied, exported, selected — `selected`
drives both the outline and the wash, and the window carries
`selection-wash` and `selection-wash-opacity`. All pixel work happens in
the kitchen (01-architecture.md; user decision 2026-08-02): the UI thread
only wraps a finished `SharedPixelBuffer` into a `slint::Image`; a texture
becomes visible one pump tick after its pixels are ready at worst, and
adoption is UNBUDGETED so a stopped fling fills the viewport in one tick.
Only MID requests are culled to the visible set; thumb jobs are never
culled (their bytes were moved into them); a landed thumb for a
scrolled-away cell is adopted, a landed MID for an invisible cell is
adopted then dropped by the visible-set retain. Ctrl+scroll zoom stays
deferred: Slint's Flickable consumes wheel events and an overlay TouchArea
would steal the drag and click gestures; `+`/`-` cover it.

### Panel docking (issue #12)

The IPTC panel takes its 300 px from the RIGHT edge; the grid reflows into
the remaining width, pinned flush LEFT — never centred, never partly under
the panel; everything in the grid area (overlay, empty-state message,
scrollbar) sizes to the grid area, not the window; clicks inside the panel
never reach the grid. The 1:1 anchor recomputes across a toggle (issue
#18): on open the crop re-centres for the docked width in the next frame
("I zoomed on the eye; the eye stays put when chrome docks"); on close it
restores the full-width anchor with no stale intermediate frame.

### The overlay scrollbar (task #21)

On the grid's right edge, inside the grid area (between grid and panel when
docked): 6 px and faint whenever content overflows — NEVER fully hidden;
the "where am I?" glance is the point — widening to 10 px and brightening
on hover or drag, with an 18 px grab zone. The thumb is sized
viewport/content and draggable; a TRACK CLICK JUMPS to the spot (PgUp/PgDn
already page via the cursor). While dragging, a floating hint shows
"first-visible / total" of the filtered view, with the first visible
capture time under a capture sort (`795 / 1450 · 15:42`). Scrollbar use
never moves the cursor except through the loupe's follow rule; hidden under
the overlay and on empty views. Deferred polish: brightening during wheel
scrolling.

### The cursor

- Exactly one cell of a non-empty view is the cursor; keyboard actions
  (mark, zoom) land on it. Visual: a 3 px accent border drawn as a top-most
  overlay, visible on every cell state.
- After any keyboard navigation or zoom change the cursor is fully visible:
  the grid's virtual height is updated BEFORE the scroll offset is written.
- Mouse and wheel scrolling never move the cursor in multi-column views —
  scrolling is browsing; the cursor may leave the viewport and the next
  arrow key first brings it back.
- A plain click on a cell moves the cursor there (and claims it) and
  COLLAPSES the selection; Ctrl+click toggles membership; Shift+click spans
  cursor..clicked in view order, FRESH after a plain click. Clicks live in
  per-cell touch areas inside the Flickable, so a drag remains scrolling;
  clicking the grid returns keyboard focus to it.
- **At one column the visible image IS the cursor**, and the cursor follows
  the ONE scroll route left — the overlay scrollbar drag — POSITIVE-GATED
  on scrollbar activity (`sb-activity`, a flag Rust consumes). A GEOMETRY
  change (panel toggle, resize) or a VIEW MUTATION (a re-sort, a filter
  removal, capture keys streaming in) is never scrolling and never claims
  or moves the cursor; the viewport re-anchors to it instead (issues #16,
  #22 — a displacement-based claim once inverted the rule into marks
  landing on a photo the user had left). The dock state is published to
  the window BEFORE any geometry read in the toggle path.
- **Grid-level resize anchoring** (user report 2026-07-26): a relayout
  anchors CONTENT, not pixels — the top-visible row keeps its fractional
  position; at the bottom clamp the bottom stays the bottom; a cursor that
  was visible stays visible; scroll 0 stays 0 — except that CURSOR
  VISIBILITY WINS; the cursor itself never moves; a reveal marks its
  geometry consumed so corrections never stack.
- The status bar always names the cursor image (filename, position N/M)
  and its mark in words (`· ★ picked / · ✕ rejected / · unmarked`).
- **The untouched-cursor rule** (issue #4; narrowed 2026-07-31): from
  session open until the user's first interaction the cursor is "the first
  image of the view", not a pinned id, and a folder never opens with the
  cursor stranded mid-grid. It is CLAIMED — id-pinned from then on — by a
  mark, a navigation key, a loupe scroll-follow with laid-out geometry, or
  a click on an image; NOT by zoom keys, filter and sort changes (pre-touch
  these snap to the new view's first image) or engine events. Open Folder
  resets it to unclaimed; pre-layout geometry never claims or moves it.
  Once the folder has loaded, ENGINE recomputes — the re-sort, a decode
  landing, a sidecar arriving — leave even an untouched cursor's image
  alone (user decision 2026-07-31: "whatever is currently selected stays
  selected, and stays visible on the screen"); only the USER asking for a
  different view — a chip, the sort control — snaps pre-touch to the new
  head. `filter::cursor_after_recompute` is the one place the two rules
  meet. Accepted cost: on a folder whose filename order runs contrary to
  capture order, an untouched cursor that started at the top ends up
  mid-grid once the real order lands (image 1 of 3,000 becomes 2001/3000),
  and the viewport scrolls to keep it in view — the app finishing a job,
  not the user asking to see something else.
- **Provisional order while loading** (issue #25): the view is in FILENAME
  order until every image's metadata job has finished, then sorted once by
  the user's key (`filter::view`'s `metadata_complete`). EXIF is read inside
  the per-file thumbnail job, so "loading" is the whole load (~15 s for
  3,000 files on the laptop with a warm cache; a card reader much slower),
  and a capture sort with keys streaming in changed the head identity over
  and over: one `right` at open landed 870 frames away, and a `Y` typed 4 s
  after opening wrote the sidecar of a file the user never saw — with NO
  input the cursor moved from image 0 to image 2000. Filename order comes
  free from the scan and for a single card in shooting order IS capture
  order. Rejected (persona 2026-07-30): deferring input until the load
  finishes (IN-MY-WAY — "an app that is dead for 11 seconds after opening a
  folder is an app I'd stop using"); accepting it with documentation (fine
  for navigation, not for a silent wrong mark). The one re-sort can happen
  after culling has begun; at one column the loupe re-anchors, and at
  multi-column zoom a one-shot reveal fires on the false→true edge of
  completion — only if the cursor cell was ON SCREEN at the previous
  refresh (`grid::scroll_after_resort`; sampled on the previous pass on
  purpose, because the flip moves the cursor): a browsing user keeps their
  OFFSET, not their content. The edge is consumed only on a refresh that
  can act on it. The status bar reads `LOADED/TOTAL loaded · sorting by name
  until loaded` while loading, then `N thumbs loaded`. Known divergences
  while provisional — deferred, not accepted as correct: the sort chip
  reads the chosen key ("Capture ↑" over a name-ordered grid, and clicking
  it mid-load reverses the grid without changing the key); the scrollbar
  hint appends capture times that run non-monotonically; `[`/`]` walk view
  positions over a name-ordered view — an `effective_sort(query, complete)`
  in core that every consumer reads is the fix. No fallback if a job never
  finishes: a wedged worker leaves the session in filename order with the
  status bar stuck one short (a per-file give-up or "sort anyway" is the
  fix). Burst grouping and Copy Picks always use the TRUE sort
  (`filter::view_true_sort`): a burst is a fact about capture times, and
  `{seq}` is baked into permanent file names.

### Visual language

- **DARK-ONLY, and the palette is PINNED** (user decision 2026-08-02: "I
  don't want a light mode. I don't want a toggle. Keep the design as is"):
  `Palette.color-scheme = ColorScheme.dark` at the root window's `init`.
  Native `std-widgets` take their colours from the platform scheme
  otherwise, and on a light-mode desktop the fluent MenuBar drew its labels
  in 90 %-alpha black over the app's `#161618` bar — invisible yet
  clickable; an unreachable portal resolves the scheme to Unknown, whose
  fluent fallback is ALSO light, which is what every headless run got. A
  light mode, should it ever be wanted, is a deliberate feature, never an
  inherited default.
- Pick: a small star badge top-left. Reject: a red ✕ badge and a 40 %
  dimmed thumb.
- **The loupe state pill** (issue #20; user-confirmed 2026-07-27): the
  image's mark as ★ or ✕ on a small dark semi-transparent pill in the
  image's TOP-LEFT (its own contrast backing), an OVERLAY that never
  reflows the image, left-aligned so it can grow to hold up to five stars
  when ratings land; unmarked is absence, backstopped by the status bar's
  words; a rejected frame is NEVER dimmed in the loupe (a reject may be
  re-judged for rescue at full brightness); pointer-inert; state swap
  ATOMIC with the image swap (a wrong-frame badge is a confident lie); no
  filename or metadata creep; the top-right stays free. The #21 loading cue
  stacks BELOW the badge slot (14 px / 44 px), so the two pills never
  overlap.
- **Exported** (`▶`, issue #56): video-export.md owns the contract; the
  memory is session-only and reads, never decides. **Burst** (`×N` badge,
  the optional strip): burst-grouping.md. **Failed**: a warning badge and a
  tooltip with the reason.
- **The selection wash** (2026-07-28, persona MUST-HAVE): a translucent
  accent-blue wash over the whole cell, 25 % (chosen by eye against 12 %
  and 18 %; a property, `selection-wash`/`selection-wash-opacity`, destined
  to become a setting — above ~15 % the tint can shift colour judgement on
  a final scan, accepted knowingly), plus the accent outline. The wash
  renders on EVERY selected cell, the cursor cell included; GRID ONLY,
  never in the loupe at fit or above (gated on `at-fit`/`one2one`);
  painted above the image and the reject dim, BELOW the badges. Filled =
  selected, bright border = cursor: two channels for DRAWING, not for
  moving. Marks are NOT batch operations — `Y`/`N`/`U` act on the cursor
  image only; net cursor movement per mark is exactly one image. The
  selection is what the IPTC panel stamps (`Selection::batch()`) and what
  the exports take.
- **The selection count**: `· N selected` in the status bar whenever the
  selection is non-empty, counted over the view (`Selection::count_in_view()`,
  matching `Selection::batch()` exactly — images selected but filtered out
  are excluded from both: what you see is what you stamp); an empty
  selection is silent. Drawn in the selection accent (`#4da3ff`, 6.2:1 on
  the bar's `#202024`; brief 002), because in the loupe, where no wash
  shows, this fragment is the ONLY sign a selection is live; it is its own
  `Text` and reports its rectangle (`status selected laid out …`).
- **The selection rule** (user decision 2026-09-06, brief 002: "file
  manager style"; until then a plain arrow kept the selection lit and a
  fresh Shift-span was UNIONED with it — a rule no surveyed product has,
  which the user met as a second video holding the first video's frames):
  1. **Plain navigation collapses the selection.** Every unmodified cursor
     move — Left, Right, Up, Down, PgUp, PgDn, Home, End, `[`, `]` — and the
     `Y`/`N` mark auto-advance leave the selection EMPTY: the cursor is the
     batch, the count is silent, no wash. Grid and loupe alike, at every
     zoom; whether or not the move changed the cursor (a Right at the last
     frame still clears; an arrow under a filter that matches nothing still
     clears). `U` stays put and leaves the selection alone unless its mark
     removed the frame from the view and the cursor moved on. Not
     navigation, never touching the selection: the zoom keys, the wheel and
     the scrollbar, a modal's Esc, the IPTC panel, the export, and every
     cursor move the ENGINE makes (the load-settled re-sort, a filter or a
     landing sidecar moving the cursor to a survivor). Esc, a plain click
     and `G` at a grid zoom clear as before; `G` from the loupe keeps.
  2. **Ctrl-navigation keeps it.** Ctrl+Left/Right/Up/Down, Ctrl+PgUp/PgDn/
     Home/End and Ctrl+`[`/`]` move the cursor exactly as the plain key
     would, reset the Shift anchor, and leave the selection alone — the
     file-manager companion (Explorer, GTK, Qt, Thunderbird). They claim
     the cursor.
  3. **A fresh Shift-span REPLACES the whole selection**, Ctrl-added frames
     included: Shift+arrows, Shift+page keys and Shift+`[`/`]` whose anchor
     arms on this press — after a plain click, after Ctrl-navigation, on an
     empty selection — ARE the selection. A span that continues a live
     anchor (Shift held on, or the anchor armed by Ctrl+click, Ctrl+Space or
     Ctrl+Shift+B) replaces only the live span, so shrink and flip work.
     Ctrl+click, Ctrl+Space and Ctrl+Shift+B add; Ctrl+A is exactly the view
     and arms no anchor.
  4. **Ctrl+Space toggles the cursor frame's membership** — additive like
     Ctrl+click, cursor unmoved, anchor armed on the cursor: the keyboard's
     way to build a discontiguous selection.
  What it buys: "export, `]`, export" and "caption, `]`, Ctrl+Shift+B,
  caption" act on the new burst only; a fresh span can never carry an old
  one along; no stale selection can beat the burst under the cursor at the
  next Ctrl+Shift+E. What it costs (persona IN-MY-WAY, recorded with the
  user's decision): a stray arrow after a 40-frame chord loses the
  selection silently and there is no undo (one Ctrl+Shift+B rebuilds a
  burst; a hand-built span costs its keys again); Ctrl+arrow is
  "essentially undiscoverable", so the shortcuts card and docs/culling.md
  name it beside the plain arrows; the walk-and-mark of a captioned run
  ends at the first `Y`. The persona's alternative — drop the selection
  only when a key lands OUTSIDE it — was put to the user, who chose the
  file-manager rule and rejected a finished export consuming its selection.

## Keyboard map

An H2 rather than a subsection because `the_shortcuts_card_lists_every_binding_in_the_spec`
locates this table by the heading and reads its first column until the
next `##`; a row may be paired with NO card row only if the card teaches
the binding in prose, in that test's own short list.

| Key | Action |
|---|---|
| Arrows / PgUp / PgDn / Home / End | navigate (grid and loupe). A plain move COLLAPSES the selection — the cursor is then the batch, the count silent (user decision 2026-09-06, brief 002: the file-manager rule, in full under "Selection" in Visual language); with Ctrl held the same key moves without touching it |
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
| `G` | back to the grid at the previous grid zoom (from loupe/1:1); at a grid zoom it is also the deselect gesture (clears the selection); from the loupe it KEEPS the selection — the "go and look at what I selected" exit; the first plain move after it drops the selection like any other (brief 002, 2026-09-06) |
| `Esc` | back to the grid at the previous grid zoom AND the selection cleared — from anywhere, the loupe included (user decision 2026-08-28, issue #55: the burst chords build a 40-frame selection in the loupe with one press, where no wash shows it, and a stale one would silently take the next IPTC commit; the cancel key must work where the selection was made). Modal popups still take Esc first (they close; the grid never sees it), and with keyboard focus in an IPTC field Esc stays the recorded no-op (Slint LineEdit has no Esc hook — see the panel section; QE 2026-08-28). Like every nav key it ends in the cursor reveal, so an Esc taken by the grid also scrolls the cursor back into view — that is the reveal rule, not a lost scroll position: only keys that never reach the grid (a modal's Esc) leave a browsing viewport alone. Since 2026-09-06 (brief 002) any plain move ends a selection too; Esc remains the clear that leaves the cursor where it is, and the only one that reaches a selection made before a dialog opened — the dialog takes the first Esc, the grid the second |
| `I` | toggle IPTC panel |
| `K` | focus the keyword field, opening the IPTC panel if needed (persona G3; implemented with the panel step — K is never a dead key) |
| Shift+arrows / PgUp / PgDn / Home / End | extend selection (span anchor..cursor over view positions; a new span replaces the previous one — shrink/flip works). A span whose anchor arms on THIS press — after a plain click, after Ctrl-navigation, on an empty selection — replaces the WHOLE selection, Ctrl-added frames included (user decision 2026-09-06, brief 002 answer 3; until then a fresh span was unioned with the folded old one, `selection.rs` 7ed0949 — a rule no surveyed product has, which this row never stated and which produced the two-sets video of the user's report); a span continuing a live anchor (Shift held on, or the anchor armed by Ctrl+click, Ctrl+Space or Ctrl+Shift+B) replaces only the live span. The PAGE keys extend by the same rule, a span from the anchor to wherever the plain key lands (QE 2026-09-06, D1; Manager ruling: the file-manager convention the collapse rule comes from). They were UNBOUND when that rule landed and an unbound Shift chord falls through to its plain form, so for one commit Shift+Home/End/PgUp/PgDn moved the cursor and destroyed the selection — a Shift-modified NAVIGATION or MARK key is never silently its plain form (the letters, `Esc` and `F1` ignore Shift by design and always have: Shift+Esc clears like Esc, Shift+F1 opens the card, and every letter arm matches both cases — measured 2026-09-06, senior-developer re-review N-3). Shift+Space is inert for the same reason: the map gives it no job, so it is swallowed rather than picking the frame and advancing. Shift+Ctrl+arrows stay reserved |
| `Ctrl+A` | select all (filtered set); arms no anchor, so a Shift+arrow after it starts fresh from the cursor and replaces it — Explorer's and GTK's behaviour, kept (brief 002 OQ2, 2026-09-06) |
| `[` / `]` | burst boundary jump (M7): `]` = next frame whose group differs (in a contiguous capture-sorted view that is the next group's first frame; with non-contiguous members it follows view order); `[` = re-anchor on the current group's first visible frame, crossing to the previous group only from there (CD-player convention); claims the cursor; carries loupe zoom/pan persistence; a plain `[`/`]` collapses the selection like the arrows, and Ctrl+`[`/`]` jumps the same way with the selection kept (2026-09-06, brief 002); see burst-grouping.md |
| Shift+`[` / Shift+`]` (also `{` / `}`, the shifted characters a US keyboard sends) | extend the selection by WHOLE bursts (issue #55): the cursor lands where `[`/`]` would, and every whole burst between the anchor's burst and the cursor's is selected; the opposite key drops a burst; a following Shift+arrow is frame-precise from the burst's edge; from a FRESH anchor the burst span is the whole selection, the same rule as Shift+arrows (brief 002, 2026-09-06); see burst-grouping.md |
| `Ctrl+Shift+B` | select this burst (issue #55, user proposal): the burst under the cursor joins the selection, cursor unmoved, additive, idempotent, arms the anchor; a plain `]` between two presses now empties the selection, so two non-adjacent bursts are Ctrl+Shift+B, Ctrl+`]`×n, Ctrl+Shift+B (2026-09-06, brief 002); see burst-grouping.md |
| Ctrl+arrows / PgUp / PgDn / Home / End | move the cursor exactly as the plain key would and leave the selection alone — the file-manager companion of the collapse rule (user decision 2026-09-06, brief 002 R3; Explorer, GTK, Qt, Thunderbird). Resets the Shift anchor, so a Shift+arrow that follows starts fresh from the cursor — deliberately not Explorer's sticky anchor: one rule for where a span starts. Ctrl+Shift+arrows stay unbound (reserved). Grid and loupe alike; claims the cursor |
| Ctrl+`[` / Ctrl+`]` | burst jump with the selection kept — `]`'s landing, `[`'s re-anchor convention, and the anchor reset of Ctrl+arrows; the seven hops in "Ctrl+Shift+B on 40, Ctrl+`]`×7, Ctrl+Shift+B on 47" (brief 002 R3; burst-grouping.md) |
| Ctrl+Space | toggle the cursor frame's membership — additive like Ctrl+click, anchor armed on the cursor, cursor unmoved; the keyboard's way to build a discontiguous selection (brief 002 R4; Explorer, GTK, WAI-ARIA). Inert while a field or a dialog holds the keyboard, like every grid key. A desktop whose input-method switcher owns Ctrl+Space (older IBus setups) never delivers it; Ctrl+click is the same toggle there |
| `Ctrl+O` | Open Folder… (persona accelerator gap, provisional) |
| `Ctrl+Q` | Quit (persona accelerator gap, provisional) |
| `Ctrl+E` (menu: Copy picks…) | open copy dialog (`Ctrl+C` stays clipboard-idle: user decision after persona review — never repurpose it) |
| `Ctrl+Shift+E` (menu: Export Frames as Video…) | open the video export dialog (M9, video-export.md). A CHORD, not a bare letter, so it cannot fire from a fat finger mid `]`/`N` (persona 2026-08-27); it is matched BEFORE `Ctrl+E` because with Shift held the event still arrives as the letter plus modifiers. Disabled — with its reason in the status line, never silently — when there is neither a selection nor a burst under the cursor |
| `?` / `F1` | open the keyboard-shortcuts card — and, while it is up, close it again (2026-09-04; the persona's finding was that the keyboard help of a keyboard-first app could be opened only with the mouse). `?` reaches the app as the shifted character on most layouts, so it is matched both with and without a reported Shift modifier, and `/`-with-Shift is matched too for layouts that report the unshifted key. The opener lives in the MAIN key scope beside the other bare letters, which is what keeps it from firing while an IPTC field, the keyword field or a dialog's own field holds the keyboard; the close arm is mirrored in the copy and export scopes (issue #42's topmost-first rule). About keeps `Esc` as its only key. **Both keys are therefore inert while a field or a dialog holds the keyboard, and for `F1` that is a decision, not a consequence** (2026-09-04): for `?` it is forced — the key is a typed character, and a help card that opened instead of typing a question mark into a keyword would be a defect — while `F1` is not a character and could have been given a scope of its own. It was not, because the help it opens is the GRID's help: none of its 29 rows applies while a text field has the keyboard, and a modal that appeared over a half-typed keyword would have to decide what happens to the edit. Esc leaves the field first; F1 works there |
| `1`–`5`, `0` | reserved (star ratings, v2) — must not conflict |

### Marks and auto-advance

Picking (`Y`) or rejecting (`N`) auto-advances the cursor to the next image
at EVERY zoom level, grid and loupe alike (user decision 2026-07-25);
clearing (`U`) does not advance. The advance is a cursor move and collapses
the selection like an arrow (Lightroom's auto-advance does; an exemption
would make `Y` and Right disagree). It becomes a configuration option
(default on) with the settings dialog; until then it is always on. When a
mark removes the image from the active filtered view, the live-removal
cursor rule IS the advance — auto-advance never applies on top of it — so
net cursor movement per mark is exactly one image, always (persona gap G1:
the rule that keeps the inbox-zero loop honest). There is no undo stack in
v1: a mis-marked frame costs one arrow back and a re-mark.

### Window chrome

- A slim menu bar; the keyboard remains the fast path — menus are
  discoverability, never a required route. **Where the bar is drawn is the
  platform's**: on Windows the winit backend supports a NATIVE menu bar
  (`muda`), outside the client area; on Linux Slint draws it in-window, 40
  px tall in the `fluent` style — so everything below sits exactly 40 px
  higher on Windows (measured between the two CI runners, 2026-09-02). No
  driven test clicks an in-window element at a coordinate measured on the
  other platform (test-harness.md), and the menu-click strands are
  Linux-only: a dispatched pointer event cannot reach an OS menu.
- **File**: Open Folder… (native picker via `rfd`), Copy Picks… (`Ctrl+E`),
  Export Frames as Video… (`Ctrl+Shift+E`, greyed when there is nothing to
  export while the keystroke explains itself in the status line), Settings…
  (placeholder, disabled until a settings dialog exists — post-v1), Quit.
  **View**: Zoom In/Out (`+`/`-`), IPTC Panel (`I`), Filter Bar. **Help**:
  Keyboard Shortcuts (the card below), About. Opening a folder via the menu
  behaves identically to the CLI argument.
- **The keyboard-shortcuts card** (rebuilt 2026-09-04 after the user's
  verdict on the old one: "awful and cramped"): every row is a `KeyRow` — a
  **104 px** right-aligned key cell (13 px, weight 600, `#e8e8f0`), a 14 px
  gutter, a stretching action cell (13 px, `#c8c8d0`); the 104 px is a
  CONSTANT, never a content measurement, which is what makes the action
  column start at the same x on every row in any font, and right-alignment
  gives a second hard edge; both cells wrap and neither elides. A `dim`
  variant carries the reserved star-rating row (`#8a8a96`, 4.74:1 — a row
  dimmed below AA is a missing statement, not a softer one). Seven one-word
  sections in two columns — MOVE, MARK, MOUSE down the left; ZOOM, SELECT,
  PANELS, FILE MENU down the right — 11 px `#8a8a96` headings with 1 px
  `#6a6a76` rules (3.03:1, WCAG's non-text minimum, deliberately below the
  heading), a 1 px hairline between the columns; MOUSE is its own section
  because a card headed "Keyboard shortcuts" that files `wheel` among the
  keys is lying; FILE MENU echoes the File menu character for character.
  An action text fits ONE line of the 240 px action cell (38 characters at
  13 px fit, 40 do not). The card GROUPS AND PARAPHRASES the map and lists
  every binding in it (one map row may become four, two may share one;
  `?`/F1 is named in the title-row hint). SELECT lists the collapse rule's
  companions as TWO rows — `Ctrl+arrows`, whose action text says "any MOVE
  key", which is what pairs the map's Ctrl+`[`/`]` row to that cell in the
  parity test, and `Ctrl+Space`; the MOVE `← / →` row reads "previous / next
  frame (ends selection)", which is where the collapse rule reaches the
  card; SELECT's `Shift+arrows` row reads "extend the selection (page keys
  too)", which is how the Shift+page-keys map row is listed without a row of
  its own. **780 px wide, content-driven
  tall**: `min(780px, window − 48px)` by `ModalScrim`'s `card-fits-content`
  clamped to `window − 40px`; it fits whole at 1000x700, the smallest
  supported window (about 25 px of room there — the next binding replaces
  a row or moves a section). Its height, 568 px on the development seat,
  is a MEASUREMENT no test may pin: it is the sum of ~29 text line boxes
  and ranges from 491 (Liberation Sans) to 627 (Noto Sans Mono) across
  faces; the test pins only what is geometric — the width, inside the
  layer, fits whole at 1000x700 (measured as slack, not as a ceiling), the
  footer inside it, the same height at both window sizes. Every length is a
  LOGICAL pixel (a 200 % seat at 1920x1080 is smaller than 1000x700).
  Exactly three children of `ModalScrim`'s layout — the title row (the
  closing hint flush right), the body, the footer (the zoom ladder, a
  diagram, not a binding) — because its 8 px spacing is hard-coded; the
  title row carries 18 px under it. The card CLIPS, in `ModalScrim`, for
  every card it draws, and the title texts and the footer elide, so nothing
  paints outside the card at any window size (measured at 400x320). Nothing
  in the card takes the pointer or the keyboard — click anywhere closes,
  no hover highlight, no search field. The body is a `Flickable {
  interactive: false }`, deliberately not a `ScrollView`: where the card
  fits both are transparent to the pointer, but once clamped a
  `ScrollView`'s bar eats every click on the 14 px strip down the right
  edge, and "click anywhere" would quietly stop being true there — a SAFETY
  VALVE for windows under ~640 px tall, where the list clips and scrolls on
  the wheel while the title and footer stay inside the rounded rect.
  Recorded, not fixed: the export dialog's `?`/F1 close arm has never been
  driven (the copy dialog's was; the export one cannot be reached on
  synthetic data, and the two arms are character-identical).
- **Folderless launch** (issue #5): `fastcull-app` with no arguments opens
  the normal window in the empty state — "No folder open — File > Open
  Folder… (Ctrl+O)" — with a working menu bar, never a usage error (a
  desktop launcher has no arguments); distinct from the "No images" state
  of a folder that opened empty. CLI usage errors remain for malformed
  invocations, invisible on a Windows double-click by design
  (01-architecture.md).
- **About** (issue #23): a modal (Esc or click outside; clicks on the card
  never close it): "FastCull", the version on its own line, the
  two-sentence description, "Main contributor: Danilo de Paula" (the
  user-directed exception to CLAUDE.md's M7), the repository URL as plain
  retype-able text that never wraps or ellipsizes, and "GPL-3.0-or-later".
  The version is composed by the BUILD, never hand-maintained: `X.Y.Z` when
  HEAD sits exactly on the release tag, `X.Y.Z-devel-YYYYMMDD-<short-hash>`
  otherwise (a bug report from a dev build must pin the commit; the hash
  says WHICH code, the date HOW OLD — the COMMITTER date, so a rebased
  commit dates when it came into existence, compact and before the hash so
  builds sort; a hash with no usable date still yields
  `X.Y.Z-devel-<hash>`), plain `X.Y.Z` without git. `build.rs` watches HEAD,
  the branch ref and the TAG refs — `git tag && cargo build` once left a
  `-devel-` string in a release binary (v0.5.0); a shared global
  `CARGO_TARGET_DIR` across two checkouts of one version can still serve a
  cached result (`cargo clean -p fastcull-app` fixes it). Traced at startup
  (`about version …`). The title is split in two so the hash never clips.
  Recorded gap: the test proves a date is present and well-shaped, not
  WHICH date.
- **Modal keyboard containment** (issue #23, user decision "swallow
  everything in that screen"): while About or the shortcuts card is up, Esc
  closes it and EVERY other key is swallowed — driven NAV keys identically.
  The popups are declared last in the tree so their scrims render above
  every layer; opening a modal steals the keyboard back to the main key
  scope; ALL FOUR scrims swallow the wheel (issue #49 — Copy Picks and the
  export dialog are hand-rolled copies of `ModalScrim`, because their focus
  scope must WRAP the card, and a hand-rolled scrim must carry the
  `scroll-event` arm); the menu bar stays live under a modal (File > Quit
  works). **Esc closes the TOPMOST modal only** (issue #42): About over the
  live copy dialog takes two Esc presses, the dialog's plan and destination
  survive the first, and both key scopes contain modals identically.

### The filter and sort bar (M5)

- SINGLE-choice chips — All / Picked / Rejected / Unmarked — with counts
  (combinations dropped; the in-burst-only chip was cut at M7). Sort:
  capture time (default) ↑↓, filename ↑↓. Pure predicates in
  `fastcull-core::filter`; the grid binds to the filtered, sorted view.
- **View mutation rules**: marking an image so it no longer matches the
  filter removes it from the view LIVE; the cursor lands on the next image
  in the filtered view (else the previous, else none); counts update at
  once. When the filter itself changes, the cursor goes to the nearest
  surviving image, else the first. The inbox-zero loop — filter Unmarked,
  `Y`/`N` until empty — must work exactly; when the view empties the grid
  shows the final counts ("0 unmarked — N picked, M rejected") with no
  cursor, dropping out of the loupe if it was there.
- **Focus containment**: while ANY text field has focus, no single-key
  shortcut fires — typing "Xavier" must not reject a photo; Enter commits
  the field and returns focus to the grid. Esc in a field is a no-op (the
  LineEdit offers no hook); the field exits are iptc-templates.md's.
- Persona defaults adopted 2026-07-25: a keyword commit returns to the grid
  and the cursor STAYS (Save-and-advance rejected: it breaks the
  K→type→Enter→Y loop and is incoherent on a multi-selection; the future
  option is commit-and-advance-AND-keep-field-focus); the panel shell has a
  template dropdown, Apply and "Revert last apply"; hiding the filter bar
  resets the filter to All (a filter is never active while invisible);
  click-away commits (G7); Tab cycles the panel fields; per-image
  keywording is a same-evening flow (`K` into the keyword field,
  comma-separated entry, Enter); no Open Recent, template UI or filter
  hotkeys in M5.

### Focus continuity (issues #41, #42, #63, #64)

- **The guarantee**: whenever the focused editor is DESTROYED (the panel
  closed by any route, a session swap, the field rows rebuilt) or COVERED
  (About or the shortcuts card over the panel or the copy dialog; the copy
  dialog over a focused field), keyboard focus deterministically returns
  to the topmost surface's key scope — never a dead keyboard, never keys
  eaten by an invisible editor (pre-fix, closing the panel from the menu
  left focus on NO element, and a modal over a focused field was
  un-dismissable while every keystroke landed in the hidden field,
  committable as metadata). A destroyed editor DISCARDS its uncommitted
  text (user decision 2026-08-03); a covered one commits like click-away,
  the shipped G7 semantics. A session swap invalidates in-flight edits by
  generation stamp, so the old session's text can never be committed
  against the new session's images.
- **The owner invariant** (2026-08-30): Slint's window holds a WEAK
  reference to its focus item, so an editor destroyed by a model
  replacement delivers no `FocusOut`, nothing reassigns focus, and every
  key afterwards dies on a failed upgrade. The app carries the token
  itself — a root `focus-owner`: `0` the main key scope, `1..=N` panel
  field row i, `N+1` the keyword field, `-1` a dialog's own scope — written
  SYNCHRONOUSLY at every claim site and ONLY by a gain, never by a loss
  (the gainer's `changed has-focus` runs first, and a row destroyed by a
  rebuild shares its id with the row recreated in its place). Staleness —
  focus leaving for a menu popup or a deactivated window — is the safe
  direction: the reclaim hands the keyboard back to the field the user was
  in.
- **Reclaim points**: the field-rows rebuild → back to the SAME ROW, never
  the grid (a claim on `keys` there made the next caption character a cull
  command): Rust arms `iptc-refocus-row` — only when the token names a
  FIELD ROW, so a rebuild never pulls the keyboard out of a `K`-parked
  keyword editor — with the rebuild GENERATION, one event-loop iteration
  late; the flag is cleared only by an actual claim, never by an arm firing,
  so an EARLY arm survives until its row exists; and the recreated row claims from a `changed`
  handler or from a 1 ms per-row `Timer` — never from `init`, where
  `focus()` silently does nothing, and never the doomed instance, which
  the generation excludes; both belts are load-bearing under the EARLY arm
  ordering the headless CI seat produces. The residual gap is 5–6 ms
  release-idle and 11–35 ms loaded (95–230 ms in debug); a keystroke inside
  it is delivered in order when the claim runs first and dropped otherwise,
  so scripts wait on `row 0 (gen K)`. A session SWAP → synchronous, to the
  topmost scope (0 ms; the field's meaning went with the folder); the
  deferred re-assert captures the session generation when QUEUED and falls
  back to `focus-keys()` if a swap landed before it fired — reachable only as
  menu activation then File > Open Folder, which the harness cannot drive,
  so review-verified only. Panel
  CLOSE → synchronous AND deferred, because the MenuBar restores focus to
  the destroyed editor after the activation returns (21–53 ms, by design;
  docs/culling.md says so). Any MENU ITEM → deferred, re-asserting the
  TOKEN (a `menu-activated` callback fires first; blanket-claiming `keys`
  took the keyboard off a live keyword editor). Modals and panel OPEN →
  deferred belts. NEVER after the keyword-chip or template model
  replacements, which hold no editor. A menu DISMISSED without activating
  anything — the nastiest shape — is closed by the same late arm.
- **The discard rule is deterministic**: Rust bumps `iptc-rebuild-gen`
  before every rows replacement, each editor stamps it on focus gain, and
  the blur commits only if the stamp still matches — ANY rows rebuild
  discards (the cursor image's own IPTC landing included; docs and this
  spec say "any rows rebuild"), while a click-away or Tab commits.
- **Open, issue #68**: deactivating the WINDOW mid-edit delivers a real
  `FocusOut` and the blur COMMITS, exactly as a click-away would — the
  likely root of the 1-in-4 keyword-swap intermittent (#54; unchanged by
  the owner invariant, the blur arriving from outside before any rebuild).
  Telling it from a click-away needs the window's activation state, which
  Slint 1.17 exposes only through `i-slint-core`'s internal
  `WindowInner::active()` — its own step. Until then docs/metadata.md tells
  users a window switch mid-word commits, and points at Revert. Its
  signature in a trace: a lone `focus: … lost` with no `gained` after it
  and no `focus-keys (…)` before it; a stranded reclaim is a rebuild with
  no `row N (gen …)` claim after it — and a standing `revert=…` in a dump
  is NOT this defect (a committed field and a dead keyboard leave the same
  dumps).
- Recorded follow-up: updating changed rows IN PLACE (`set_row_data`)
  instead of replacing the model would keep the editors alive and remove
  the hazard at its root; not a drop-in (a binding a handler has assigned
  once is dropped for good, and the swap test's proof would rest on the
  stamp alone).

### Slint facts this module depends on (1.17)

A `changed` tracker is installed AFTER a row's `init` runs, so anything
written in `init` is the baseline, never a change; `focus()` from a
repeater row's `init` does not take effect; a repeater does not tear its
children down when the model is replaced — they die at its next update;
the MenuBar restores focus to the previously focused element AFTER an
item's activation runs; `WindowInner::focus_item` is weak; `keys.has-focus`
reads false when the WINDOW is deactivated while keys still arrive; an
element with a bound width but no `x:` (or height but no `y:`) is CENTRED
in its parent; a conditional element is re-created; `check_repeat`
restarts the click count beyond 10 logical px; a `Text` without `wrap`
clips from the RIGHT; a layout does not clip its children; `Flickable {
interactive: false }` forwards non-wheel pointer events and handles the
wheel; a repeated timer re-arms BEFORE its callback runs; `quit_event_loop`
is a user event that Wayland's loop delivers one dispatch later; a
Flickable's fling binding survives programmatic sets; the software
renderer's source offsets are `Fixed<u16, 4>`.

## Contracts

- Pure functions in core, the app only bridges: `pointer::step`; `zoompan`
  (`ladder_up`, `ladder_down`, `contain_click_frac`); `grid::visible_range`,
  `grid::scroll_after_resort`, `GridLayout::new`; `filter::view`,
  `filter::view_true_sort`, `filter::cursor_after_recompute`; `selection`
  (`batch`, `count_in_view`, `extend_to`, `extend_bursts`, `select_group`);
  `transit::render_rung`, `transit::evict_fullres`, `FULLRES_RING`.
- Constants: `TRANSIT_GAP` 250 ms, `SETTLE_DEBOUNCE` 150 ms,
  `FOCUS_DEBOUNCE` 250 ms, `OVERLAY_HOLD_CAP` 250 ms, `PREFETCH` 2,
  `TRANSIT_BEHIND`/`TRANSIT_AHEAD` 2/8, `MID_RUNG_MAX_LONG` 2048,
  `UPSCALE_THRESHOLD` 1.25, 60 logical px per wheel notch,
  `pointer::OPTIMISTIC_MAX`, `CELL_ASPECT` 3:2, the 300 px panel, the 25 %
  wash, `#4da3ff`.
- The marks and dump fields this module emits are test-harness.md's.
- The keyboard-map table above is parsed by
  `the_shortcuts_card_lists_every_binding_in_the_spec`.
- The About dialog's credit line is the recorded exception to M7.

## Acceptance criteria

`core:` a `fastcull-core` unit or integration test; `app:` a driven
`tests/screenshot.rs` test (real dispatched events, dumps and traces).

- [x] `filter.rs`: every filter/sort combination over a synthetic session,
      counts included — the `filter::tests`.
- [x] The windowed model: visible-range → model-window computation, partial
      rows, tiny folders, `N = 1` — `grid.rs`
      `visible_range_windows_with_margin`, `visible_range_edges`,
      `visible_range_at_single_column`.
- [x] The menu bar is readable under any desktop colour scheme —
      `menu_bar_labels_survive_a_light_scheme_desktop` forces the failing
      scheme-resolution branch (an unreachable session bus, NOT
      `dbus-run-session`, which passes vacuously) and asserts light glyphs
      over the dark bar; removing the pin yields 0 bright pixels and fails.
- [x] Transit vs settled: a held key is distinguished from taps and decays
      on release; the request while moving is a rung the mid serves (2048
      still fails); the ring leans the way of travel and clamps at both
      edges; a settled frame climbs without duplicating an in-flight job or
      spinning; the settle poll leaves LRU order alone; through the public
      api, so disabling transit at the call site fails —
      `transit_tracks_held_keys_and_decays_on_release`,
      `transit_request_is_served_by_the_mid_rung`,
      `transit_ring_leans_in_the_direction_of_travel`,
      `a_settled_frame_climbs_even_though_transit_only_asked_for_the_mid`,
      `the_settle_guarantee_does_not_disturb_the_lru_order`,
      `a_held_key_reaches_transit_through_the_public_api`,
      `a_backward_hold_keeps_leaning_backward_across_refocus` (the app's
      same-index re-focus storm). Not covered: the measured performance
      figures themselves.
- [x] No fit-drop, no fling, no phantom fold (issue #46). Core: the ring
      maps view positions to ids and back, the direction latch compares
      positions, deferred revival uses the same ring, the public api decodes
      view neighbours not id neighbours —
      `the_prefetch_ring_walks_view_order_not_id_order`,
      `travel_direction_is_latched_in_view_positions`,
      `deferred_revival_ring_follows_view_order`,
      `prefetch_follows_the_view_order_through_the_public_api`. App: a
      cook-widened cold jump keeps `one2one` and the carried centre and
      renders the thumb rung; drag pans 1:1, release stops dead, navigation
      after a flick keeps the drag-carried centre with zero `pan fold`
      traces; paced taps over an interleaved session land warm —
      `transit_to_a_cold_frame_keeps_the_overlay_at_the_carried_center`
      (both profiles since 2026-09-05; its landing dump gated on the sharp
      rung's mark; the thumb-rung render-order pin release-only, since a
      congested debug kitchen can collapse the order),
      `loupe_drag_pans_one_to_one_and_a_fling_never_survives_navigation`
      (both profiles; its pointer work gated on `wait:loupe idx 0 factor`),
      `paced_taps_over_an_interleaved_session_land_warm` (its warm-landing
      pin binds in release, a timing pin like the perf budgets; its no-drop
      assertion in both), `transit_at_zoom_stays_soft` (the soft render and
      the sharp landing). Every bug-shaped assertion was red on the pre-fix
      build. The `(hold cap)` drop-and-re-raise fires under the #76 load
      recipe in debug (14 of 14) and never on CI; a deterministic
      release-profile exercise still wants a decode-wedge knob (deferred;
      the policy itself is unit-covered as `render_rung` rows).
- [x] A decode-FAILED cursor drops to fit instead of masking the badge —
      `a_decode_failed_cursor_drops_to_fit_instead_of_masking_the_badge`
      (a helper thread zeroes the file after `thumb bytes idx 11`; the
      second End-jump — failure known, texture in hand — must not render
      the rescue; both landing orders are correct product behaviour and
      neither is asserted; its preconditions `cursor=11`, `zf=inf` and the
      `(decode failed)` drop are asserted, not reasoned).
- [x] The wheel: one stop per notch through the restructured wiring, the
      notch size pinned (59 px nothing, 60 px one stop), residue carried, a
      full notch down at fit inert —
      `overlay_wheel_still_zooms_one_stop_per_notch`.
- [x] Provisional order while loading (issue #25): the view is identical at
      every step as keys stream in, then the real sort applies once
      (mutation-verified; non-vacuous by construction); the override touches
      the sort only; the re-anchor reveals a watched cursor and spares a
      browsing one; engine events after loading never move an untouched
      cursor, end to end through the flip —
      `filter::provisional_order_is_stable_while_metadata_streams`,
      `filter::provisional_order_still_respects_the_filter_and_direction`,
      `grid::resort_reveals_a_watched_cursor_and_spares_a_browsing_one`,
      `filter::engine_events_stop_moving_an_untouched_cursor_once_loaded`,
      `app: engine_events_after_loading_never_move_an_untouched_cursor`
      (gated on `load settled gen 0` since 2026-09-05: under load the settle
      landed after the first drive step 10 of 10 times and the test was
      green having measured nothing). Recorded gaps: the completion
      predicate and the mark path have no automated test (an end-to-end one
      must catch the app mid-load; an injectable load-completion point is
      the way in); four surviving mutants are accepted — `metadata_complete`
      forced true, counting only `MetadataReady`, `user_changed_query`
      forced false (the chips are click-only), `last_cursor_visible` forced
      either way; the LOADING status form is unasserted.
- [x] The loupe fit view shows the WHOLE frame — `grid.rs` units pin the
      bound and the reveal; `loupe_fit_shows_the_whole_frame_not_a_crop`
      measures the rendered aspect and requires bars on both sides
      (disabling the bound reads "aspect 1.807" and fails; the 29
      pre-existing screenshot tests could not see a crop).
- [x] The pointer state machine: a table-driven test over EVERY (state,
      input) pair of the transition table, reserved no-ops included; the
      wheel anchors the pointer's image point (asserted OFF-CENTRE — at
      (0.5, 0.5) the pointer and centre anchors coincide, which made three
      criteria vacuous once), wheel notches land on the keys' stops,
      wheel-down at fit is inert, bar clicks do nothing, pan offsets stay
      clamped at every factor — `pointer.rs` tests. Slint's own semantics —
      a drag suppresses the click, a distant second click is two clicks —
      are pinned as dependencies: `a_grid_drag_scrolls_without_clicking_the_cell_under_it`,
      `two_distant_clicks_are_two_clicks_not_a_double_click`.
- [x] Double-click reaches 1:1 from ABOVE fit —
      `loupe_double_click_above_fit_reaches_one_to_one` (the `dblclick:`
      token replays Slint's real ordering).
- [x] Pointer ROUTING (issue #13): which surface receives a physical click,
      drag or wheel, through real hit-testing, each test pairing every
      "nothing happened" claim with a control that proves the same token
      acts — `a_click_inside_the_iptc_panel_never_reaches_the_grid` (a
      field click proven by the COMMIT it produces),
      `the_wheel_routing_table_holds_over_every_surface`,
      `a_grid_drag_scrolls_without_clicking_the_cell_under_it`,
      `two_distant_clicks_are_two_clicks_not_a_double_click`,
      `a_scrollbar_drag_in_the_loupe_claims_the_cursor` (the positive half
      of the `sb-activity` claim). Coordinates calibrated against traced
      geometry; every one mutation-verified; load-verified in debug under
      six busy cores. `i-slint-backend-testing` is NOT adopted (an internal,
      unstable crate that would hide exactly the class where an element is
      somewhere unexpected). Still out of reach: the native folder dialog,
      OS-level focus, Tab-cycling — manual-acceptance items.
- [x] Containment through the real path: `about_dialog_renders_and_contains_the_keyboard`
      and `shortcuts_popup_contains_the_keyboard` open through the real Help
      menu items, send real keys, assert `keysfocus=true`, and end with
      Esc then the same key, which must mark; with the containment arms cut
      back to Esc-only they fail and the old token-driven tests passed;
      `a_wheel_over_the_help_popups_never_scrolls_the_grid_behind_them`
      keeps the nav-token mirror's coverage.
- [x] Screenshot smoke tests: grid placeholder (synthetic), loaded
      thumbnails (texture variance), a failed-badge session, loupe fit and
      1:1, bursts in a synthetic session, the panel-open docking state —
      and `no_args_launch_opens_empty_window`. The version string's SHAPE:
      off a tag a `-devel-` suffix is MANDATORY (CI checks out shallow, so
      it is always off-tag), an 8-digit date when present, a bare hex hash
      otherwise — `about_dialog_renders_and_contains_the_keyboard`.
- [x] No modal scrolls the grid behind it (issue #49) — the two hand-rolled
      scrims and `ModalScrim`, each test wheeling the grid before, under and
      after the modal, and over a CHILD of the card —
      `a_wheel_over_the_copy_dialog_never_scrolls_the_grid_behind_it`,
      `a_wheel_over_the_export_dialog_never_scrolls_the_grid_behind_it`,
      `a_wheel_over_the_help_popups_never_scrolls_the_grid_behind_them`
      (red with the `scroll-event` arm removed: `vpy=-360` against `-180`).
- [x] Focus continuity, red-run-verified against the pre-fix build: panel
      close from the menu keeps the keyboard at 1:1 and in the grid; a
      modal over a focused field owns the keyboard and writes nothing; a
      session swap mid-edit discards and keeps the keyboard, for a destroyed
      field editor and for the surviving keyword editor; Esc over stacked
      modals closes the topmost first with the copy dialog's plan intact; a
      1:1 loupe click claims the keyboard; the guards on the clean paths
      (menu activation with keys focused, the G4 Enter commit, the
      copy-dialog Esc lifecycle, the filter-bar toggle mid-edit, File > Copy
      Picks over a focused field) —
      `panel_close_from_the_menu_at_one_to_one_keeps_the_keyboard`,
      `modal_over_a_focused_field_owns_the_keyboard_and_writes_nothing`,
      `session_swap_mid_field_edit_discards_and_keeps_the_keyboard` (asserts
      by acting: `key:+` 50 ms after the swap must zoom, and the ORDER of the
      marks — the reclaim is the first claim after the rebuild — which is
      what kills the mutant 20/20),
      `session_swap_mid_keyword_edit_never_writes_into_the_new_session`
      (**KNOWN INTERMITTENT**, ~1 run in 4 measured 2026-08-22, 0 in 40 on
      2026-08-30 — the recorded rate stands unrefuted; it fails on the
      DISCARD assertion with a lone `focus: … lost`, the issue #68 shape,
      and must not be quieted),
      `esc_over_stacked_modals_closes_the_topmost_first`,
      `one_to_one_click_claims_the_keyboard`,
      `copy_picks_from_the_menu_over_a_focused_field_owns_the_keyboard`; the
      owner-invariant strands `a_cursor_move_rebuild_keeps_the_keyboard_in_the_field`
      (waits on `row 0 (gen K)`; asserts the stranded-reclaim and the
      deactivation signatures separately; inherits the #68 intermittent —
      ~2 in 35 runs, a lone `focus: … lost` before the rebuild — and must
      not be quieted),
      `a_menu_item_over_a_focused_field_row_keeps_the_keyboard`,
      `a_dismissed_menu_over_a_focused_field_row_keeps_the_keyboard`. The
      menu-click strands skip on Windows (no in-window menu bar). Under the
      forced EARLY arm ordering the whole suite is 71 passed, 3 failed
      without the `Timer` and all green with it. Issue #64 (a real click
      then a real `I`) does not reproduce on this tree (0 in 90).
- [x] Geometry: the relayout path re-anchors on the cursor across a resize
      and a panel toggle at 1:1 and in the grid; the loupe survives a
      vertical resize with one whole frame; grid resizes keep content
      anchored, the bottom at the bottom, the top at the top — every resize
      gated on `wait:window geometry WxH` (6/6 red with the token
      neutered): `window_resize_keeps_the_photo` (its anti-vacuity guard
      reads the `relayout re-anchor` AFTER the resize echo),
      `panel_toggle_at_one_to_one_keeps_the_photo`,
      `panel_toggle_at_one_to_one_reanchors_the_crop` (release-strength
      timing behind `wait:loupe idx 0 factor` in release; the clock kept in
      debug), `loupe_survives_a_vertical_resize_with_one_whole_frame`
      (gated on the settle in front of its `home`),
      `grid_resize_shrink_keeps_content_anchored`,
      `grid_resize_grow_at_bottom_stays_at_bottom`,
      `grid_resize_at_top_stays_at_top` (on a seat that reverts `1200x800`
      these run their post-resize steps at 1440x900 — the reaction is
      genuine, the geometry the compositor's). Two stay on the clock
      deliberately: `panel_toggle_at_one_to_one_keeps_the_photo` and
      `window_resize_keeps_the_photo` place six copies of one file — one
      capture key, so a settle gate protects nothing and would cost +3.2 to
      +4.9 s of Windows tail out of the one budget those tests are known to
      lose.
- [x] The selection wash never reaches the loupe, and the count is drawn in
      the accent — `selection_wash_never_reaches_the_loupe` (rendered pixels
      across two processes, pinned by the shutter's 1.5 s floor; if ever
      gated, on the textures it reads), `the_selection_count_is_drawn_in_the_accent`
      (18.0 of blue bias against 4.1; the two mutants at 4.1 and 7.4 against
      a threshold of 8.0).
- [x] Brief 002, the selection rule: the user's scenario (4 selected →
      export → Esc → Right, Right → Shift+Right → exactly 2, no earlier-video
      hint) — `the_second_video_holds_only_the_new_span`; caption, hop,
      caption lands on the second burst only —
      `caption_then_hop_then_caption_lands_on_the_second_burst_only`; two
      bursts by Ctrl-hops and Ctrl+Space's toggle —
      `ctrl_navigation_keeps_the_selection_and_ctrl_space_toggles` (with
      its `hopfresh` strand for the anchor reset), core
      `a_fresh_burst_span_replaces_ctrl_added_frames`,
      `ctrl_space_toggles_additively_across_ctrl_navigation`; the mark
      advance collapses and `U` does not, in the grid and the loupe, and an
      empty filtered view still collapses —
      `a_plain_move_collapses_the_selection_in_the_grid`,
      `a_plain_move_collapses_the_selection_in_the_loupe`; a dialog's Esc
      leaves the selection intact and a second Esc clears it (in
      `the_second_video_holds_only_the_new_span`); the Shift page keys
      extend by the same rule and Shift+Space is inert —
      `shift_page_keys_extend_the_selection`; the Ctrl chords claim the
      cursor — `ctrl_navigation_claims_the_cursor` (through a filter change;
      the load-settled half and Ctrl+Space's claim are review-verified);
      the core rule — `a_fresh_span_replaces_the_whole_selection_a_continued_one_replaces_its_span`
      (red against the pre-fix `extend_to`); the suite green on both
      runners, the card carrying the new pairings, the checksums unchanged
      (PR #84).
- [x] The shortcuts card lists every binding in this spec —
      `the_shortcuts_card_lists_every_binding_in_the_spec` parses the
      Keyboard map above and the card's own rows and fails if either grows
      a row the other lacks; the card is a two-column sheet that fits its
      window and closes with Esc, `?`, F1 and a click anywhere, including on
      the card's centre and on the body's right edge where it is clamped —
      `shortcuts_card_is_a_two_column_sheet_that_fits_its_window`.
- [x] The shutter fires exactly once per run; the 60 s readiness cap is
      margin again in a debug build — test-harness.md and
      01-architecture.md ("Build profiles").
- RETIRED 2026-09-17 (user decision): the per-release manual acceptance
  (a 5,000-file A1 folder at 60 fps; no perceived latency in the
  pick→auto-advance loop) — never recorded as run; the perf budgets, the
  driven suite and daily use guard the claims.

## History

- 2026-09-17 — Rewritten (brief 007); the harness section moved to
  test-harness.md; the manual acceptance retired (the user). One stale
  sentence — that the dialog answer rows report no rectangle — was dropped
  rather than moved: the rows report themselves since brief 005. The
  pre-rewrite text — every campaign, count and seat measurement — is
  `specs/history/ui-grid.md`.
- 2026-09-06 — The selection rule: plain navigation collapses, Ctrl keeps,
  a fresh span replaces, Ctrl+Space toggles; the count in the accent; two
  SELECT rows on the card (brief 002, user decision).
- 2026-09-05 — The shutter fires once (issue #77); the settle mark at every
  zoom (issue #73); dependencies optimised in debug lift two release-only
  gates (issue #76).
- 2026-09-04 — The keyboard-shortcuts card rebuilt (PR #75); the CI audit.
- 2026-09-02/03 — Windows CI restored; clicks by name (issue #70), geometry
  waits (issue #65), the `run N` and `gen N` marks; v0.13.1.
- 2026-08-30/09-01 — The owner invariant (issues #63, #64): the token, the
  gain-only rule, the generation-stamped rebuild reclaim and its `Timer`;
  the discard rule deterministic; issue #68 found.
- 2026-08-29 — Pointer routing through real events (issue #13); `wait:`
  (issue #61); all four scrims swallow the wheel (issue #49); the exported
  badge (issue #56); Esc clears from the loupe (issue #55).
- 2026-08-11 — The render ladder and the eviction moved into core
  (`transit`), the deferral of 2026-08-09 honoured.
- 2026-08-09 — No fit-drop, no fling, no phantom fold (issue #46; v0.9.0):
  view-order rings, the single-writer rule, the thumb rung and the residual
  hold, the pan without inertia.
- 2026-08-03 — Focus continuity (issues #41, #42); the Windows GUI
  subsystem (issue #40); v0.8.1.
- 2026-08-02 — The dark-only palette pin; the texture kitchen (issue #30);
  v0.8.0.
- 2026-08-01 — Transit vs settled (user requirement; v0.7.0), motion-first:
  the quality rule's earlier contract, "sharpness-on-stop within ~300 ms",
  gave way to the settle (371–408 ms at the engine), an accepted cost.
- 2026-07-30/31 — One-column cell bounding; the double-click defect fixed;
  the provisional order while loading and the narrowed untouched-cursor
  rule (issues #25, #4; user decisions).
- 2026-07-27 — The loupe state pill (issue #20); soft transit (issue #21);
  the About dialog (issue #23); the anchor across a panel toggle (#18).
- 2026-07-26 — The pointer contract (issue #11, user decisions); panel
  docking (issue #12); the relayout carve-out (#16, #22); folderless launch
  (#5); the grid click (#7).
- 2026-07-25 — The ladder, `Z`, persistence, the overlay scrollbar, the
  cursor contract, the M5 filter-bar decisions and the persona defaults,
  auto-advance (user decisions).
- 2026-07-24 — M2: the windowed model.
