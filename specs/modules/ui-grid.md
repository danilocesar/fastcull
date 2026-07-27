# Module spec: grid & loupe UI (`fastcull-app` + `filter.rs`)

## Purpose

The one continuous view: a zoomable virtualized grid that morphs from many columns
to a single-image loupe with 1:1 pixel zoom. Plus the filter/sort bar, pick badges,
burst badges, and the IPTC side panel shell.

## Zoom model (one axis, seamless)

Zoom levels: column count `N ∈ {12, 8, 6, 4, 3, 2, 1}` (Ctrl+scroll / `+`/`-`
step through; pinch later). At `N = 1` the view is the **loupe**:
- First stop: fit-to-screen (full-res asset GPU-scaled — see the recorded
  FitPreview fold in raw-pipeline.md).
- Further zoom-in: the ×1.5 ladder below, capped at 1:1 (FullRes asset as GPU
  texture, panning with drag; arrows NAVIGATE at every zoom level — they are
  never repurposed for panning, the burst focus-check loop depends on it).
- Zooming out from loupe returns to the grid **centered on the current image**.

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
  navigation, and a fresh Flickable initializes/clamps its viewport
  before the offset write lands: one 0,0 frame per transition, a visible
  top-left stream under key repeat. Mid-navigation offset read-backs are
  never folded into the pan center (`capture_pan` folds only when the
  overlay still belongs to the cursor's image — the hand is on the arrow
  key, not the mouse).
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
  texture swaps in place. Identity is sacred: the soft pixels are
  always the CURRENT image's own mid rung; if even that is missing the
  view drops to fit (honest degradation) — never the previous frame.
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
| Drag | scroll the view (Flickable kinetic drag, today's behavior — **kept**); rubber-band multi-select is the reserved future gesture | **nothing** — nothing is off-screen, so there is no pan axis | **pan the image**, 1:1 with pointer motion, clamped so the image never detaches from the viewport edges |

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
  waiting for a possible second one.
- **Drag beats click.** A click fires only on press+release without
  movement beyond the drag threshold; once a drag starts, the release
  produces no click and no double-click.
- **A double-click needs proximity, not just timing** (persona finding —
  scanning an intermediate factor by clicking eye, then beak, then wingtip
  in quick succession is two independent re-centers, not a jump to 1:1):
  the second press must land within the same small movement threshold the
  drag/click disambiguation already uses. Farther apart = two clicks.
- **Clicks outside the image rect are ignored** (persona finding): at fit a
  landscape frame on a 16:9 screen has fat letterbox bars, and
  `contain_click_frac` clamps them to the nearest image edge — so a
  double-click on black would slam to 1:1 on a frame edge. Clicks and
  double-clicks in the bars produce no action at all (the clamp stays for
  the drag/pan path, where it is correct).
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
  its `clicked` claims the cursor and feeds the double-click proximity
  trace, its `double-clicked` goes to 1:1. The machine receives the fit
  view's REAL geometry (the N=1 grid cell rect, scroll-dependent) so
  anchors and letterbox rejection follow what is actually on screen. Because it swallows presses wholesale it also implements
  "click at fit does nothing" and "drag at fit does nothing" — and it
  sits BELOW the overlay scrollbar, which keeps its drag route. Above
  fit, the wheel is taken by a `scroll-event` on the zoom overlay's
  image TouchArea (children see scroll before the Flickable, so drag-pan
  stays native while the wheel zooms). The retired browse-at-fit wheel
  gesture is gone as decided; movement inside the loupe is
  keyboard-only. Recorded deviations/deferrals (gate 2026-07-26):
  a pinned-unresolved 1:1 desire (INFINITY while full-res decodes) makes
  every pointer gesture inert until the render clamp resolves it (no
  anchor math on infinite extents); wheel over the overlay scrollbar is
  swallowed (not loupe input, per this contract); extreme coalesced
  wheel deltas may emit fewer stops than notches (single emit per event
  — accepted); two-finger trackpad scroll reaches the overlay Flickable
  as a drag (pans above fit, zooms at fit — asymmetric; trackpads are
  declared out of scope in this contract, revisit with gesture support);
  double-click proximity threshold is 12px vs Slint's 8px click/drag
  threshold (immaterial, recorded); wheel in the zoom overlay's
  letterbox BARS pans natively instead of stepping the ladder (the
  wheel surface covers the image only — the bars exist exactly when an
  axis has no pan range, so the miswheel is near-inert; extend the
  surface if it ever annoys); the scrollbar's wheel swallow also
  deadens its 18px strip in GRID view (was native scroll — tiny strip,
  accepted); during a sub-second decode gap after a fast wheel burst,
  anchors compute against the already-zoomed virtual viewport while the
  screen still shows fit (optimistic-climb consequence, self-corrects
  on texture adoption).

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

Recorded deviations/decisions (M2):
- Thumb JPEG→texture decode happens on the UI thread, bounded to ~32 decodes
  per refresh with leftovers deferred to a follow-up tick (≈16 ms worst-case
  stall on a page jump). `slint::Image` is not `Send`, so some UI-side
  conversion is unavoidable; moving the JPEG decode itself off-thread is an
  M4 follow-up together with the asset LRU.
- Grid cells above 320×1.25 physical px display the mid rung (raw-pipeline.md
  ladder); full-res images adopted for grid cells are downscaled to mid size
  on the UI thread, bounded to 2 adoptions per refresh with follow-up ticks
  (same budget philosophy as thumb decodes above).
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
- **Untouched-cursor rule (issue #4, 2026-07-25)**: from session open until the
  user's FIRST interaction, the cursor is "the first image of the view", not a
  pinned id — capture keys stream in progressively and re-sort the view under
  it, and a folder must never open with the cursor stranded mid-grid (real
  case: name order vs capture order put it at position 795/1450). The cursor
  is CLAIMED (id-pinned from then on, all rules above apply) by: any mark,
  any navigation key, loupe scroll-follow with laid-out geometry, and any
  click on an image — loupe, fit, or grid cell (issue #7). NOT claiming it: zoom keys (they don't move it), filter
  and sort changes (pre-touch these snap to the new view's first image —
  overriding the nearest-survivor rule until the claim), and engine events.
  Open Folder resets to unclaimed. Pre-layout geometry (a refresh before the
  window has a real height) must never claim or move the cursor.

## Visual language

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
- Selection: accent outline; multi-select via Ctrl/Shift-click and Shift+arrows.
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
folder). Screenshot test: `no_args_launch_opens_empty_window`.

Chrome staging (updated with the panel step): IPTC Panel menu item, `I`,
`K`, Shift+arrows and `Ctrl+A` all landed; the popup lists them live.

**About dialog (issue #23, implemented 2026-07-27 — replaces the
About→shortcuts placeholder)**: Help > About opens a dedicated modal
(same scrim/close pattern as the shortcuts popup: Esc or click outside;
clicks on the card never close it). Content, user-directed: "FastCull —
version X.Y.Z", the two-sentence description, "Main contributor: Danilo
de Paula", the repository URL as plain RETYPE-ABLE text (no URL-opener
dependency in v1; the URL must never wrap or ellipsize), and the
license line "GPL-3.0-or-later" — moved here from the shortcuts footer
(its intended home per the M5 deferral). The version string is composed
by the BUILD, never hand-maintained: `X.Y.Z` when HEAD sits exactly on
the release tag `vX.Y.Z`, `X.Y.Z-devel-<short-hash>` otherwise (user
decision — a bug report from a dev build must pin the commit); no git
(tarball build) falls back to plain `X.Y.Z`. Traced at startup
("about version ...") for headless assertions.

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
  reject a photo. Enter commits the field and returns focus to the grid;
  Esc in a field abandons the edit (second Esc acts on the panel/view).
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
- [x] Pointer state machine (core side, issue #11): a table-driven test that
      enumerates EVERY (state, input) pair of the Mouse & pointer contract
      table and asserts the resulting state + action — including the
      reserved no-ops. Plus: wheel-up at fit anchors the pointer's image
      point (not the center), wheel notches land on the same `1.5ⁿ` stops as
      `+`/`-`, wheel-down at fit is inert, clicks outside the image rect do
      nothing, and pan offsets stay clamped to the image bounds at every
      factor (`fastcull-core/src/pointer.rs` tests). Covered OUTSIDE the
      core tests, recorded honestly: "a drag suppresses the click" is
      Slint's TouchArea click definition (press+release without movement);
      "a distant second click is two re-centers, not a double-click" is the
      bridge's proximity trace (`handle_loupe_double_click`, review-verified
      — no pointer-injection harness exists); "`+` after a click at fit is
      still center-anchored" holds by construction (the fit click stores
      nothing but the proximity trace; the keyboard ladder never reads it).
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
  Malformed entries are skipped silently. Scripts may include mark actions
  (`pick`/`reject`), which write real sidecars — QE runs target throwaway
  copies of test data only.
  The `--screenshot` shutter WAITS for the whole drive script to have
  executed before it may fire (in addition to its readiness gates): a
  fast release build otherwise reaches readiness before late-scheduled
  actions run and captures a half-driven state — the same script must
  mean the same shot in every profile (found 2026-07-27 when
  settle-then-pin drive schedules moved past the 1.5 s floor).
