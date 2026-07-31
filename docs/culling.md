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

**Help > About** shows the version, license, and project link. When
filing a bug, paste the version string from there — development builds
read `X.Y.Z-devel-<commit>`, which pins the exact code you're running.
While About or the shortcuts popup is open, keys are swallowed (a
stray `N` will never reject the photo underneath); `Esc` or a click
outside closes them.

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

> **Fixed in 0.5.0**: double-click did not actually reach 1:1 once you
> were zoomed in — it just re-centered twice. It works now, from fit and
> from any zoom step.

## Fit shows the whole frame

At the loupe's first stop the **entire photograph is on screen**. On a
16:9 or 16:10 screen a 3:2 frame therefore sits between black bars at
the left and right — that's the frame fitting, not something missing.

> **Fixed in 0.5.0**: the loupe used to fill the width instead, which
> cut off the bottom of every frame — about 17% of the image height in a
> window, 23% fullscreen on a 1080p screen — with nothing on screen to
> say so. If you culled with an earlier version, frames you judged at
> fit had an edge you never saw. Sharpness and expression are still the
> job of `+`/`Z` and 1:1; fit is the view that has to be complete.

## Working on several photos at once: the selection

Shift+arrows, Shift+click, Ctrl+click and `Ctrl+A` build a **selection** of
several photos. The selection is what the **IPTC panel** writes to: commit a
field, add or remove a keyword, or apply a template, and it lands on every
selected photo at once. That's how you caption a whole run of frames in one
go.

- Selected photos are **tinted blue** in the grid, so a selection reads
  at a glance across the whole page even at 12 columns.
- The status bar shows **`· N selected`** whenever a selection exists —
  including selected photos that have scrolled off-screen, which the
  tint alone can't show you.
- The **cursor keeps its bright outline** on top of the tint, so you can
  always see where the keyboard is pointing inside a selection.
- The tint is **grid-only**. In the loupe your photo is never recolored:
  there you're judging pixels, so what you see is the real image.
- A photo you selected but then **filtered out of view** is not stamped,
  and is not counted — what you see is what you stamp.

**Marks are not batched.** `Y`, `N` and `U` always act on the single photo
under the cursor, even with fifty photos selected — one keystroke, one
photo (`Y`/`N` then advance; `U` stays put, as everywhere else). That's
deliberate: marking is a one-at-a-time rhythm, and there is no sensible
place to advance to after marking fifty frames at once.

A plain click **clears** the selection outright — the tint disappears and
the `· N selected` counter goes with it, leaving you on the photo you
clicked. `Esc` or `G` clears it too. Note that plain arrow navigation does
*not* clear a selection: it stays live, and stays lit, until you clear it.

## Knowing where you are: marks in the loupe

You never have to zoom out to check a frame's state:

- A small **badge pill in the top-left corner** of the loupe (fit and
  zoomed alike) shows **★** on a picked frame and **✕** on a rejected
  one. An unmarked frame shows no badge — and the **status bar always
  spells it out** (`· ★ picked / · ✕ rejected / · unmarked`), so
  "no badge" is never ambiguous.
- The badge always belongs to the frame on screen. Auto-advance means
  the frame you just marked is one step behind you; when you arrow
  back to compare candidates in a burst, the badge tells you instantly
  which one you already took.
- A rejected frame is **never dimmed in the loupe** (unlike its grid
  thumbnail): if you're reconsidering a reject, you get full
  brightness to judge it.

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
