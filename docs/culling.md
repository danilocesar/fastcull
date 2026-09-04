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

In the order the in-app card lists them.

| Key | Action |
|---|---|
| `←` / `→` | previous / next frame |
| `↑` / `↓` | one row up / down |
| PgUp / PgDn | one screen back / forward |
| Home / End | first / last frame |
| `[` / `]` | previous / next burst (see below) |
| `G` | back to the grid at your previous grid zoom, selection kept — *at a grid zoom it clears the selection instead* |
| `Esc` | back to the grid, **and the selection is cleared** — from anywhere but a text field |
| `Y`, `P` or `Space` | pick (auto-advances) |
| `N` or `X` | reject (auto-advances) |
| `U` | clear mark (stays put) |
| Shift+arrows | extend the selection |
| Shift+`[` / Shift+`]` | extend it by whole bursts (see below) |
| `Ctrl+Shift+B` | add the burst under the cursor to the selection (see below) |
| `Ctrl+A` | select all (of the filtered view) |
| `+` / `-` | zoom in / out, one stop: grid columns → loupe fit → ×1.5 steps → 1:1 |
| `Z` | fit → 1:1; from 1:1 *or any zoom* → back to fit (from the grid: straight to 1:1) |
| `I` | IPTC panel |
| `K` | jump to the keyword field (opens the panel if needed) |
| `Ctrl+O` | Open Folder… |
| `Ctrl+E` | Copy Picks… |
| `Ctrl+Shift+E` | Export Frames as Video… |
| `Ctrl+Q` | quit |
| `?` or `F1` | the shortcuts card — press either again to close it |
| `1`–`5`, `0` | reserved for star ratings (a future version) |

The same map lives in **Help > Keyboard Shortcuts** inside the app — or
press **`?`** (or `F1`), which is quicker and does not need the mouse. The
card groups these under MOVE, MARK and SELECT down the left and ZOOM,
MOUSE, PANELS and FILE MENU down the right, with every key in one aligned
column so you can run your eye down them; `Esc`, `?`, `F1` or a click
anywhere closes it. On Windows those menus are the system menu bar Windows
draws for the window, not a bar inside it — the same menus either way.

> **Changed after 0.13.1**: the card used to be one 23-line block in
> which the key and its description ran together at the same size, weight
> and colour, in no useful order, inside a box that was the same height
> whatever was in it. It is now a two-column sheet with seven headed
> groups and the keys in their own aligned column, only as tall as what is
> on it — and it says several things it never said: that `Y` and `N`
> advance while `U` stays put, that `G` keeps your selection from the
> loupe and clears it at a grid zoom, and that marks only ever touch the
> frame under the cursor. Dragging is listed at last. And `?` or `F1`
> opens it: before, the keyboard help of a keyboard-first app could only
> be opened with the mouse.

**Help > About** shows the version, license, and project link. When
filing a bug, include the version string from there. Development builds
read `X.Y.Z-devel-<date>-<commit>` — the date is when that commit was
made, not when the build was compiled, so the same code always reports
the same version. Together they say exactly which code you were running
and how old it is. A tagged release just reads `X.Y.Z`.
While About or the shortcuts card is open, keys are swallowed (a
stray `N` will never reject the photo underneath); `Esc` closes either
one, a click outside closes either one, and a click *on* the shortcuts
card closes it too — About is the one that stays put under a click,
because there is a URL on it you may be trying to read. `Esc` always closes the thing on top: if a popup
is open over the Copy Picks dialog, the first `Esc` closes the popup
and the dialog — destination, plan and all — survives underneath.
The keyboard itself never dies: closing the IPTC panel, opening one of
these popups, switching folders, or opening a menu and changing your
mind about it — all while you are typing in a panel field — always hand
the keys back. To the popup while it is up, to the field you were typing
in when the panel merely refreshed under you, to the grid once
everything is closed.

> **Fixed in 0.13.0**: **switching folders** used to hand the keys
> back a moment late — a fifth of a second on a busy machine, sometimes
> more — and a key pressed inside that gap did nothing at all. That one
> is immediate now. If you ever hit "the first keystroke after switching
> folders is ignored", that was this.
>
> Closing the panel from the **menu** still hands them back on the next
> turn of the event loop (tens of milliseconds), and has to: the menu
> puts focus back where it was *after* it runs your click, so the app
> has to wait for that and then take the keys. You will not out-type it,
> but it is not the same "immediate" as the folder swap.

If the fields refresh while you are typing in one — the panel rebuilds
itself as a folder's metadata arrives — the keyboard stays **in that
field**, not on the grid. It has to: on the grid your next letter would
be a cull command.

> **Fixed in 0.13.1**: on some machines that hand-back could lose a
> race and simply not happen — the panel refreshed, and from then on the
> keyboard was dead: no typing, no `Y`/`N`, nothing but the mouse. It
> depended on how fast the machine drew the panel, so it could hit one
> computer every time and another never. Clicking back into a field (or
> anywhere in the grid) was the way out. It no longer occurs on either
> kind of machine.

## The mouse

> **Changed in 0.3.0**: the wheel used to step between images in the
> loupe. It now ZOOMS — moving between images in the loupe is
> keyboard-only (arrows, `Y`/`N` auto-advance, `[`/`]`). If your mouse
> "stopped working", it didn't — the gesture changed.

- **Grid**: wheel scrolls, click selects (and moves the cursor),
  double-click opens the image in the loupe, drag scrolls.
- **While a dialog or popup is open**: the wheel does nothing. About,
  Keyboard Shortcuts, Copy Picks and Export Frames as Video all block
  it, so an absent-minded scroll cannot move the grid behind them —
  close the dialog and you are still where you left off.
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

> **Fixed in 0.13.0**: a wheel over the Copy Picks or Export Frames
> as Video dialog scrolled the grid behind it. You closed the dialog to
> find yourself somewhere else in the folder, with no way back to the
> spot you were culling. Both dialogs now swallow the wheel, like About
> and Keyboard Shortcuts always did.

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

Shift+arrows, Shift+click, Ctrl+click, `Ctrl+A` and the burst keys
(Shift+`[`/`]`, `Ctrl+Shift+B` — see [Bursts](#bursts)) build a
**selection** of several photos. The selection is what the **IPTC panel** writes to: commit a
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
clicked. **`Esc` clears it too, from anywhere** — inside the loupe as
well, where nothing is tinted and it is easy to forget that a selection
is still live; one `Esc` and the next caption lands only on the photo
you're looking at. (The one place `Esc` does nothing is while you are
typing in an IPTC field — click the grid or press `Tab` out first.) `G` from the loupe keeps the selection (so you can go
back to the grid and look at it); at a grid zoom `G` clears it like `Esc`.
Note that plain arrow navigation does *not* clear a selection: it stays
live, and stays lit, until you clear it.

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

## Badges for what you already did

Two small badges sit in the **bottom-left** corner of a thumbnail, and
they show in the loupe as well — the pill at the top is your *judgement*,
these are the *jobs already done*:

- **✓** (green) — this photo has been copied to your destination folder
  this session. See [Copy Picks](copy-picks.md).
- **▶** — this frame is in a video you exported this session. See
  [Export Frames as Video](export-video.md). It is per frame: export half
  a burst and only that half is badged.

Both are **memory for this session only** — open another folder or quit
and they are gone — and both **follow the disk**: delete the copy or the
video and the badge goes with it the next time the matching dialog opens.
Neither of them ever decides anything for you; the dialogs ask about what
is really on the disk when it matters.

(The bottom-**right** corner is the burst counter, `×23` — see below.)

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
- **Shift+`]`** is `]` that also selects: it lands where `]` would and
  selects every *whole* burst between where you started and where you
  landed. From a burst's first frame, one press selects that burst plus
  the next one — the heron taking off in burst 40 and landing in burst
  41 is one Shift+`]` and then [Export Frames as Video](export-video.md).
  Press again to add the next burst; **Shift+`[`** drops one again
  (from the middle of a burst it selects just that burst, landing on its
  first frame, like `[`). A burst is never selected by half — with one
  exception worth knowing: with two cameras shooting at once, so that
  their bursts interleave, the selection is the *range* between the
  two bursts you spanned, and the other body's burst can be cut at the
  range's edge; use `Ctrl+Shift+B` when a burst must be exact. On a US
  keyboard these are the `}` and `{` keys — both spellings work.
- **`Ctrl+Shift+B`** selects the whole burst under the cursor **without
  moving the cursor** — the move for captioning a burst from whichever
  frame you happen to be judging. It *adds* to what is already selected
  (so "burst 40 plus burst 47" is `Ctrl+Shift+B`, `]` a few times,
  `Ctrl+Shift+B` again) and pressing it twice changes nothing. Only the
  frames the current filter shows are selected: filter to Picked first
  and the rejects stay out.
- `Esc` clears the selection, in the loupe too — see
  [the selection](#working-on-several-photos-at-once-the-selection).

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
