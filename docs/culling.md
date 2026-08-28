# Culling — the keyboard, the loupe, and bursts

The whole idea: your right hand stays on the arrow keys, your left on
`Y`/`N`, and you never wait for the tool.

The UI is **dark-only, by design** — there is no light mode and no theme
toggle, and your system theme does not change it (see the FAQ if an old
build's menu bar ever looks empty on a light desktop).

## The loop

1. Open the folder. The grid sorts by capture time — see *While the folder
   is still loading* below for the one exception.
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
| `Ctrl+Shift+E` | Export Frames as Video… |
| `Ctrl+O` | Open Folder… |
| `Ctrl+Q` | quit |
| `1`–`5`, `0` | reserved for star ratings (a future version) |

The same map lives in **Help > Keyboard Shortcuts** inside the app.

**Help > About** shows the version, license, and project link. When
filing a bug, include the version string from there. Development builds
read `X.Y.Z-devel-<date>-<commit>` — the date is when that commit was
made, not when the build was compiled, so the same code always reports
the same version. Together they say exactly which code you were running
and how old it is. A tagged release just reads `X.Y.Z`.
While About or the shortcuts popup is open, keys are swallowed (a
stray `N` will never reject the photo underneath); `Esc` or a click
outside closes them. `Esc` always closes the thing on top: if a popup
is open over the Copy Picks dialog, the first `Esc` closes the popup
and the dialog — destination, plan and all — survives underneath.
The keyboard itself never dies: closing the IPTC panel, opening one of
these popups, or switching folders while you are typing in a panel
field always hands the keys back — to the popup while it is up, to the
grid once everything is closed.

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
- **Dragging has no glide**: the image tracks your hand exactly and
  stops dead where you release — no coasting afterwards. (The grid
  keeps its kinetic scroll: flicking through a grid is browsing, but at
  1:1 you're judging a spot, and a glide would carry you past it. For
  long travel across a frame, one click re-centers.)
- Zoom and pan position **carry across images**: lock 1:1 on the eye,
  arrow through the sequence, `Y`/`N` each frame — every image shows
  the same spot at the same zoom.

> **Fixed in 0.5.0**: double-click did not actually reach 1:1 once you
> were zoomed in — it just re-centered twice. It works now, from fit and
> from any zoom step.

> **Fixed after 0.8.1**: a fast drag-flick used to set the image
> coasting, and an arrow pressed while it still coasted showed the next
> photo at the wrong spot — sometimes parked at its top-left corner with
> your carried position silently lost. Flicks no longer coast (release
> always stops the image), so nothing can drag your zoomed position away
> between photos.

## While the folder is still loading

You can start culling immediately — that's the point of the tool — but a
big folder takes a while to read every file's capture time, and until it
has, **the grid is ordered by filename**. The status bar says so:

    1847/3100 loaded · sorting by name until loaded

When the last file lands, the grid sorts by capture time once. Your cursor
stays on **your** photo, and if it was on screen the view scrolls to keep
it in front of you. If you'd scrolled off browsing, your place is left
alone — scrolling is browsing, and the next arrow key brings the cursor
back as usual. (Your scroll *position* is kept, not the photos under it:
the grid has re-sorted, so you'll be looking at a different stretch of the
shoot.)

This holds even if you haven't touched anything yet: the photo the cursor
is on when the load finishes is the photo it stays on. On a folder where
filename order and capture order disagree, that means you may find
yourself part-way down the shoot rather than at its start — the frame you
were looking at is worth more than the position number.

One thing that does **not** change during the load: the sort chip still
shows your chosen sort (say "Capture ↑") even while the grid is ordered by
name. The status bar is the one that tells you the truth.

For a single card shot in one run, filename order and capture order are
the same thing, so you'll never see the difference. You'll see it when one
folder holds two cards or two bodies, or when the camera's counter rolled
over from 9999 mid-event.

> **Fixed after 0.5.0**: the grid used to re-sort continuously while loading,
> as each file's capture time arrived. Marking during that window could
> land `Y` or `N` on a frame other than the one you were looking at,
> because the first cell kept changing identity underneath you.

## Holding the arrow: fast now, sharp when you stop

Hold `→` at 1:1 and the loupe **keeps up with your finger** — frames go by
as fast as the key repeats, like scrubbing a video. It does that by showing
a smaller version while you travel, and it reads ahead in the direction
you're going so the next frames are ready before you reach them.

**Stop, and it sharpens.** About a quarter of a second after your last
keystroke the app fetches full quality for the frame you landed on. You
don't press anything; just stop.

Two things worth knowing:

- **The framing never moves.** Locked to 1:1 on someone's eye, you stay at
  1:1 on that eye the whole way through — travelling never zooms you out
  and never re-centers you. If you land on a frame nothing has decoded
  yet, the loupe shows a rough placeholder-quality version **at your
  spot** (with the "◌ loading" pill) until the real pixels arrive
  moments later.
- **Tapping is not holding.** Step frame by frame — a tap, a look, a tap —
  and every frame goes to full quality immediately, because you're
  evaluating, not travelling. The app tells the two apart by your speed:
  faster than about four frames a second is travelling. That includes a
  `Y`/`N` chain — rattle through rejects faster than four a second and
  those frames are judged at the travelling quality, deliberately: at that
  speed the old behaviour didn't show you a softer frame, it showed you
  **no frame at all**. At four a second and below, every marked frame is
  full quality, same as ever.

**Frames you flew past are held at the smaller size**, and they sharpen
when you come back to them rather than being ready in advance. Step back a
few frames from where you stopped and they're already done — the app keeps
working outward from your resting place while you look. Go deep back into a
long run and you'll see the smaller version for a moment before it catches
up. That's the trade for keeping up with your finger in the first place.

Sharpening after you stop takes around a third of a second for a
full-resolution A1 frame, even on a modest laptop — most of that is the
JPEG decode itself, which cannot be split across cores. Travelling stays
smooth regardless.

> **Fixed after 0.8.1**: arrowing onto a frame nothing had decoded yet
> used to flash the ENTIRE next photo at fit for a split second before
> snapping back to your spot — most visible in folders where the
> capture-time order interleaves the filenames (two bodies, two cards),
> because the read-ahead used to follow file order instead of the order
> on your screen. Both halves are fixed: the loupe never leaves your
> zoom and position, and the read-ahead now warms the frames your arrow
> keys will actually reach.

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
go. It is also what
[Export Frames as Video](export-video.md) turns into a clip — and there,
with nothing selected, the burst under the cursor is the selection.

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
