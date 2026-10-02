# Settings — File › Settings… or `Ctrl+,`

A small dialog with three tabs — **General**, **UI**, **Performance** —
one row per setting. Every row shows the value that is in force and a
grey line under it saying what the setting does, what its default is, and
when a change takes effect. There is no OK and no Cancel: a checkbox
applies when you click it, a number applies when you press `Enter`, `Tab`
or click elsewhere, and every change is saved to disk the moment you
make it. `Tab` into a number field selects it, so you just type the new
value. `Esc` or **Close** closes the dialog; if you were halfway
through typing a number, `Esc` throws that half-typed number away rather
than applying it — clicking **Close** (or anywhere else) applies it.
**Reset to defaults** in the footer resets the tab you are looking at,
nothing else.

While the dialog is open the grid is deaf: a stray `N` cannot reject the
photo behind it, and `Esc` always closes the thing on top first. The
dialog cannot be opened while Copy Picks or Export Frames as Video is up,
and those two cannot be opened over it; **File › Open Folder…** and
**Quit** still work from the menu.

## General

**Auto-advance after Y/N** (on). With it on, `Y` or `N` marks the frame
and moves you to the next one, the way the whole culling loop assumes,
and like any move it ends a selection. Turn it off and `Y`/`N` mark the
frame and leave you on it — handy on a second pass when you are
re-judging two frames against each other — and leave your selection
alone, exactly as `U` already does. One exception, the same one `U` has:
if the mark takes the frame out of the view you are looking at (you are
filtered to *Unmarked* and you just marked it), the cursor moves on to
the next survivor.

## UI

**Selection highlight** (25 %). How strongly selected frames are tinted
blue in the grid, 0–50 %. It is grid-only — the loupe never tints a photo
you are judging. Above about 15 % the tint can shift your colour
judgement on a final scan (green foliage reads cold), so many people set
it once to something lower; 0 is allowed, and the bright cursor outline
still tells you what is selected.

## Performance

**Loupe memory** (2 GB). How much memory the loupe may keep full-size
decoded frames in, so that stepping back through a burst at 1:1 does not
re-decode. Type a number in GB (`8`, `0.5 GB`) or a share of this
machine's RAM (`40 %`). Beside the field the dialog shows what that
means on this machine — `= 12.4 GB of 31.1 GB ≈ 89 A1 frames` — and the
app's real footprint runs 1–2 GB above the number (textures live outside
it). It takes effect when a folder is next opened: **File › Open
Folder…**, and the same folder is fine. Below 200 MB it is held at 200 MB
(one frame); above your machine's RAM it is held at your RAM. A small
figure is fine, with one cost: below about 0.7 GB (on an A1) the loupe
cannot keep all five frames around the one you are looking at, so a step
at 1:1 decodes again the frames it had to let go — it never keeps
decoding while you sit still.

**Thumbnail cache cap** (2 GB). The most the thumbnail cache (see below)
may hold in thumbnails; past it the oldest are dropped when a folder is
next opened. Never below 0.25 GB — one big shoot's worth. Dropping
thumbnails does not shrink the cache's file — the database keeps the room
for the thumbnails that come next — so the size the Thumbnail cache row
reads can stay above the cap until you **Clear** the cache.

**Read workers** (Adaptive). How many files are read at once. *Adaptive*
starts at 4 and grows while your storage keeps up — the right answer on a
local disk. A **Limit** helps on a NAS or a slow card that thrashes when
too many reads compete: `4` or less reads exactly that many files at a
time; more than 4 is a ceiling the adaptive pool may grow up to. Clearing
*Adaptive* starts the limit at 4; type another number and press `Enter`.
Takes effect at the next folder open. If `FASTCULL_MAX_READERS` is set in your
environment it wins, and this row shows its value greyed out with a note
saying so — unset the variable to change it here.

**Thumbnail cache.** The row reads how big the cache is and where it
lives (`Thumbnail cache: 22.3 MB in ~/.cache/fastcull/previews.db` —
the size includes SQLite's `-wal` and `-shm` files, so it is what `du`
would tell you), with a **Clear** button. Clearing needs no confirmation:
the cache is only ever rebuilt from your files, and the next open of any
folder simply re-reads its files once. The frames already on screen stay
on screen. Clearing compacts the database in place rather than deleting
the file, which is why the row afterwards reads a few tens of KB and not
"0 B". If you started FastCull with `FASTCULL_NO_CACHE=1` the row says
the cache is off and the button is disabled.

## The settings file

Settings live in one file, `settings.toml`, in your config directory —
`~/.config/fastcull/settings.toml` on Linux,
`%APPDATA%\fastcull\fastcull\config\settings.toml` on Windows — next to
`ui.toml` (the remembered Copy Picks and video destinations) and
`templates.toml` (your IPTC templates). It does not exist until you
change something. It is plain text, and hand-editing it is fine:

```toml
[general]
# Y or N moves to the next frame and ends a selection, like an arrow (default
# on; applies at once). Off keeps the cursor on the frame you marked — unless
# the filter hides it, in which case the cursor moves to the next one.
auto_advance = true

[ui]
# How strongly selected frames are tinted in the grid, 0–50 % (default 25;
# applies at once). Grid only — the loupe never tints. Above about 15 % the
# tint can shift your colour judgement on a final scan.
selection_wash = 25

[performance]
# Memory for decoded full-size frames: a number in GB (2, 0.5 GB) or a share
# of this machine's RAM (40 %) (default 2 GB; applies at the next folder open
# — File › Open Folder…, the same folder is fine). The app's footprint runs
# 1–2 GB above this number.
loupe_memory = "2 GB"
# The most the thumbnail cache may hold in thumbnails, in GB (default 2 GB,
# never below 0.25 GB; enforced when a folder is next opened). The file itself
# shrinks only when you Clear it.
cache_cap = "2 GB"
# Adaptive (recommended): 4 readers, growing while the storage keeps up. Limit
# N: exactly N readers when N is 4 or less; above 4, at most N (default
# adaptive; applies at the next folder open).
max_readers = 0
```

(The comment lines are the same notes the dialog shows under each row;
`max_readers = 0` is Adaptive, any other number is a Limit.)

A hand edit takes effect when you next open the dialog (the Performance
rows still wait for their own moment). FastCull rewrites the file when
you change something in the dialog, and keeps your own comments, any
keys it does not know and the file's own line endings — a file saved
with Windows line endings (Notepad's) comes back with them, and a
byte-order mark stays where it was. A value out of range is held to the range (60 %
reads as 50 %); a value it cannot read at all reads as the default. One
thing it cannot keep: a table you put where one of these settings or
groups belongs (`[performance.loupe_memory]`, `[[general]]`) is replaced
by the setting at the next change — the file cannot hold both.

If the whole file will not parse — a stray bracket — FastCull does not
touch it: it runs on the defaults, the terminal and the status bar say
`settings.toml could not be read`, and the dialog's notice line shows the
error with its line number. The first change you make in the dialog
moves the unreadable file aside as `settings.toml.broken` (numbered if
one exists) and writes a fresh one; the status bar names where it went.
The same happens if you break the file by hand while FastCull is running:
it is moved aside at the next change, never written over. If that hand
edit breaks the fresh file FastCull wrote after moving one aside,
reopening the dialog says so — the notice line and the status bar say the
file could not be read, the defaults are in force again, and the earlier
`settings.toml.broken` is still named. What counts is
the file as it is when the change is saved: if you mend it by hand
first, nothing is moved aside — reopen the dialog and your mended file is
read; a change made with the dialog still open is written into it, the
values the dialog shows taking the place of the ones in the file.

If the file cannot be written (a read-only config folder, a full disk),
the change stays in force for this session and the dialog's notice line
says so. Until a change can be saved again, reopening the dialog keeps
what you set rather than re-reading the file, so nothing you chose is
quietly taken back.

## Environment variables and settings

Where an environment variable already governs a setting, the variable
wins — today that is `FASTCULL_MAX_READERS` alone. The other variables
you may see mentioned (`FASTCULL_TRACE`, `FASTCULL_NO_CACHE`,
`FASTCULL_NO_CONFIG`) are diagnostics for bug reports and tests, not
settings; see the [FAQ](faq.md). No setting gets a new environment
variable of its own: a switch that only lives in someone's `.bashrc` is a
switch nobody can see.

---

Next: [FAQ & troubleshooting](faq.md)
