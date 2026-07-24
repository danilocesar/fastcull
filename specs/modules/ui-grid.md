# Module spec: grid & loupe UI (`fastcull-app` + `filter.rs`)

## Purpose

The one continuous view: a zoomable virtualized grid that morphs from many columns
to a single-image loupe with 1:1 pixel zoom. Plus the filter/sort bar, pick badges,
burst borders, and the IPTC side panel shell.

## Zoom model (one axis, seamless)

Zoom levels: column count `N ∈ {12, 8, 6, 4, 3, 2, 1}` (Ctrl+scroll / `+`/`-`
step through; pinch later). At `N = 1` the view is the **loupe**:
- First stop: fit-to-screen (FitPreview asset).
- Further zoom-in: 1:1 (FullRes asset as GPU texture, panning with drag/arrows).
- Zooming out from loupe returns to the grid **centered on the current image**.

## Virtualization (the M2 prototype risk)

Slint virtualizes ListView only, so the grid uses a **windowed model** maintained in
Rust: the app crate exposes a `VecModel<CellData>` containing only visible rows ±1
row margin; scroll/zoom recomputes the window and mutates the model in place
(reuse, don't recreate). Cell textures are `slint::Image` handles produced from
pipeline `Thumb` events. Placeholder cells render immediately (gray + filename)
before their thumb arrives.

`CellData`: image id, texture, pick state, burst color index (-1 = none),
failed flag, copied flag, selected flag.

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
| `+` / `-` / Ctrl+scroll | zoom in/out (grid columns → loupe → 1:1) |
| `Z` | toggle fit ↔ 1:1 in loupe |
| `G` | jump back to grid (from loupe) |
| `I` | toggle IPTC panel |
| `Ctrl+A` | select all (filtered set) |
| `Ctrl+E` (menu: Copy picks…) | open copy dialog (`Ctrl+C` stays clipboard-idle: user decision after persona review — never repurpose it) |
| `1`–`5`, `0` | reserved (star ratings, v2) — must not conflict |

There is no undo stack in v1 (user decision): a mis-marked frame during
auto-advance is fixed with arrow-back + re-mark, which costs one keystroke.

Pick/reject in loupe auto-advances to the next image (config: on by default).

## Filter & sort bar

- Filters (combinable): All / Picked / Rejected / Unmarked / In-burst-only.
- Sort: capture time (default) ↑↓, filename ↑↓.
- Implemented as pure predicates in `fastcull-core::filter` over the session;
  the grid binds to the filtered+sorted view. Counts shown per filter state.

## Acceptance criteria

- [ ] `filter.rs` unit tests: every filter/sort combination over a synthetic
      session, counts included.
- [ ] Windowed-model tests (core side): visible-range → model-window computation,
      incl. partial rows, tiny folders, and N=1.
- [ ] Slint screenshot smoke tests: grid with placeholder + loaded + badge states;
      loupe fit and 1:1.
- [ ] Manual acceptance (per release): 5,000-file A1 folder (a bad evening, per
      persona review) scrolls at 60 fps after thumbs load; pick→auto-advance→pick
      loop in loupe has no perceived latency.
