# Module spec: grid & loupe UI (`fastcull-app` + `filter.rs`)

## Purpose

The one continuous view: a zoomable virtualized grid that morphs from many columns
to a single-image loupe with 1:1 pixel zoom. Plus the filter/sort bar, pick badges,
burst borders, and the IPTC side panel shell.

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
- **Click-to-zoom** (user: yes): a click is always "center HERE" (user
  decision 2026-07-25: "single clicks centralize the image in the clicked
  point"). At fit it jumps straight to **1:1 anchored on the clicked
  point**; at 1:1 it re-centers the view on the clicked point (no zoom
  change — `Z` is the exit, no gesture is wasted on what a key does); at an
  intermediate factor it goes to 1:1 at the point (persona default, one
  line to flip: below 1:1 a click means "show me pixels HERE"). Click fires
  only on press+release without movement — it must not fight the pan drag.
  Drag-pan keeps working at every factor above fit.
- **Persistence across images (contract, was accident)**: navigating or
  pick/reject-advancing to another image keeps BOTH the zoom factor and the
  pan center, carried as a **fractional center of the image** and clamped
  for differing dimensions/orientations (lock 1:1 on the eye, arrow through
  the burst, Y/N each frame). Returning to fit forgets the pan spot — a
  fresh zoom-in re-centers (a stale pan from three images ago is a trap).
- **Quality rule**: intermediate factors are rendered from the **full-res
  rung** once cached (GPU-downscaled): ANY factor above fit requests the
  top rung outright (`display_long = u32::MAX` — a proportional request
  could legitimately resolve to the mid rung under the 25% ladder rule,
  which the next sentence forbids). NEVER upscale the mid rung for a
  sharpness-critical view — a soft 2× makes the user reject sharp frames.
  While full-res is still decoding, the existing swap-in-place behavior
  applies (same as 1:1 today).
- `G`/Esc from an intermediate factor → grid at the previous grid zoom, the
  factor is discarded (re-entering the loupe starts at fit; persistence is
  for walking images INSIDE the loupe, not across grid round-trips).

## Virtualization (the M2 prototype risk)

Slint virtualizes ListView only, so the grid uses a **windowed model** maintained in
Rust: the app crate exposes a `VecModel<CellData>` containing only visible rows ±1
row margin; scroll/zoom recomputes the window and mutates the model in place
(reuse, don't recreate). Cell textures are `slint::Image` handles produced from
pipeline `Thumb` events. Placeholder cells render immediately (gray + filename)
before their thumb arrives.

`CellData`: image id, texture, pick state, burst color index (-1 = none),
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
- Exception at 1-column (loupe) zoom: the visible image IS the cursor — the
  cursor follows scrolling so full-res loading and marks always apply to what
  the user is looking at.
- The status bar always names the cursor image (filename, position N/M).

## Visual language

- Pick: small star badge (top-left; user decision — "mark the ones taken with a
  little star"). Reject: red X badge + 40% dimmed thumb.
- Burst: 3 px border in the group color; groups are visually contiguous because
  sort is capture-time by default.
- Selection: accent outline; multi-select via Ctrl/Shift-click and Shift+arrows.
- Failed file: warning badge + tooltip with reason.

## Keyboard map (keyboard-first is a feature)

| Key | Action |
|---|---|
| Arrows / PgUp / PgDn / Home / End | navigate (grid and loupe) |
| `Y`, `P` or `Space` | pick (take) |
| `N` or `X` | reject |
| `U` | clear mark |
| `+` / `-` / Ctrl+scroll | zoom in/out (grid columns → loupe fit → ×1.5 ladder → 1:1, center-anchored; see Loupe zoom ladder) |
| `Z` | from fit: jump to 1:1; from 1:1 or any intermediate factor: back to fit; from a grid zoom: jump straight to loupe 1:1 |
| click (loupe) | center HERE: at fit/intermediate → 1:1 anchored on the clicked point; at 1:1 → re-center on the clicked point |
| `G` or `Esc` | back to the grid at the previous grid zoom (from loupe/1:1) |
| `I` | toggle IPTC panel |
| `K` | focus the keyword field of the IPTC panel (persona G3, provisional 2026-07-25 pending user confirmation — `K` is free and mnemonic) |
| Shift+arrows | extend selection (persona G7: was only in Visual language; the map is the popup's source of truth) |
| `Ctrl+A` | select all (filtered set) |
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

Chrome staging (recorded 2026-07-25, M5 step 2): the IPTC Panel menu item
lands together with the panel itself (later M5 step), as do `I`, `K`,
Shift+arrows and `Ctrl+A` — the popup lists them as "(soon)" until then.
"About" opens the shortcuts popup (which carries the license line) until a
dedicated About dialog is worth its weight — deferred, not forgotten.

## Filter & sort bar (M5 decisions recorded 2026-07-25)

- Filters: SINGLE-choice chips — All / Picked / Rejected / Unmarked (user
  decision: single choice is enough; combinations dropped). In-burst-only
  joins in M7 with bursts themselves (persona: no dead chips).
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
he confirms, all cheap to change):**
- **Inbox-zero empty state (G2)**: when the filtered view empties, the grid
  shows an empty-state message with final counts ("0 unmarked — N picked,
  M rejected"), no cursor. If it happens while in loupe, the view drops
  back to the grid empty state. The cursor contract's "exactly one cell"
  rule applies only to non-empty views.
- **Keyword commit (G4)**: Enter commits + returns focus to the grid;
  cursor STAYS (spec letter). Commit-and-advance (Photo Mechanic style)
  recorded as a future config option alongside auto-advance.
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
- [x] Slint screenshot smoke tests (`fastcull-app --screenshot <out>` +
      `tests/screenshot.rs`): grid placeholder (synthetic), loaded thumbnails
      (texture-variance asserted), failed-badge session, loupe fit
      (`--start-loupe`) and 1:1 (`--start-11`). Recorded limitations:
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
  nav actions (same names `handle_nav` takes, plus `quit`) for headless
  reproduction and QE runs — Wayland offers no external input automation.
  Malformed entries are skipped silently. Scripts may include mark actions
  (`pick`/`reject`), which write real sidecars — QE runs target throwaway
  copies of test data only.
