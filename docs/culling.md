# Culling — the keyboard, the loupe, and bursts

The whole idea: your right hand stays on the arrow keys, your left on
`Y`/`N`, and you never wait for the tool.

## The loop

1. Open the folder. The grid sorts by capture time.
2. Click the **Unmarked** filter chip (or leave **All** on).
3. Walk with the arrows. `Y` picks, `N` rejects — both auto-advance to
   the next frame. `U` clears a mark (and doesn't advance).
4. When the Unmarked view is empty, you're done — the empty-state
   message shows your final counts.

There is no undo stack: a mis-marked frame costs one `←` and a re-mark,
which is faster than any undo dialog.

## Keyboard map

| Key | Action |
|---|---|
| Arrows / PgUp / PgDn / Home / End | navigate (grid and loupe) |
| `Y`, `P` or `Space` | pick (auto-advances) |
| `N` or `X` | reject (auto-advances) |
| `U` | clear mark (stays put) |
| `+` / `-` | zoom: grid columns → loupe fit → ×1.5 steps → 1:1 |
| `Z` | fit → 1:1; from 1:1 *or any zoom* → back to fit (from the grid: straight to 1:1) |
| `G` or `Esc` | back to the grid at your previous grid zoom |
| `[` / `]` | previous / next burst (see below) |
| `I` | IPTC panel |
| `K` | jump to the keyword field (opens the panel if needed) |
| Shift+arrows | extend a selection |
| `Ctrl+A` | select all (of the filtered view) |
| `Ctrl+E` | Copy Picks… |
| `Ctrl+O` | Open Folder… |
| `Ctrl+Q` | quit |
| `1`–`5`, `0` | reserved for star ratings (a future version) |

The same map lives in **Help > Keyboard Shortcuts** inside the app.

## The mouse

> **Changed in 0.3.0**: the wheel used to step between images in the
> loupe. It now ZOOMS — moving between images in the loupe is
> keyboard-only (arrows, `Y`/`N` auto-advance, `[`/`]`). If your mouse
> "stopped working", it didn't — the gesture changed.

- **Grid**: wheel scrolls, click selects (and moves the cursor),
  double-click opens the image in the loupe, drag scrolls.
- **Loupe, at fit**: wheel-up zooms in one step, anchored under the
  pointer — aim the wheel at an eye and it stays put. Wheel-down does
  nothing (use `G`/`Esc` to leave). A single click does nothing;
  **double-click jumps to 1:1** on the clicked point.
- **Loupe, zoomed**: the wheel walks the zoom steps under the pointer,
  capping exactly at 1:1. A click re-centers on the clicked point,
  double-click jumps to 1:1 there, and dragging pans the image.
- Zoom and pan position **carry across images**: lock 1:1 on the eye,
  arrow through the sequence, `Y`/`N` each frame — every image shows
  the same spot at the same zoom.

## Bursts

Continuous-drive squeezes are detected automatically and marked where
it helps:

- A **×23 badge** on the first frame of each burst tells you the depth
  before you dive in.
- **`]`** jumps to the next burst (or single frame); **`[`** first
  returns to the *current* burst's opening frame — press it again to
  cross into the previous burst. Same double-tap habit as a CD player's
  back button.
- The status bar shows **burst 7/23** while you're inside one.

Bursts never touch your files or marks — the grouping is a display and
navigation aid only, and every frame is still culled individually.

**Camera brands differ** — this is expected, not a bug:

| | Sony bodies | Other brands |
|---|---|---|
| Detection | exact (the camera's own burst counter) | by capture-time clustering |
| Minimum burst size | 2 frames | 3 frames |
| Back-to-back squeezes | split correctly | may merge into one group |

No brand does worse than time-based clustering — a Sony file with an
unreadable burst counter falls back to it — and a file with no usable
timestamp at all simply stays ungrouped.

---

Next: [Metadata — picks, sidecars, and templates](metadata.md)
