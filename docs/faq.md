# FAQ & troubleshooting

**The menu bar looks empty / the app ignores my light system theme.**
FastCull is dark-only, by design — there is no light mode and no theme
toggle, and releases after 0.6.0 pin the palette so your system theme
cannot half-apply. (Older builds let the menu bar's text follow a
light-mode desktop, which made the labels invisible against the dark
bar — the menus still worked when clicked. If you see that, update.)

**My picks don't show up in darktable.**
Check the sidecar exists next to the RAW (`DSC01234.ARW.xmp`). If the
folder was already imported in darktable before you culled, darktable
may be showing its database copy — select the images and use
*load sidecar file* / re-import the folder. Picks arrive as ratings
(★ = pick, reject flag = reject); this round-trip is exercised against
a real darktable in FastCull's test suite.

**Does Lightroom read the sidecars?**
Honest answer: the *contents* are standard XMP that Lightroom
understands, but the *file name* follows darktable's convention
(`NAME.ARW.xmp`), while classic Lightroom looks for `NAME.xmp`.
Verified today: darktable (automatically, in CI). digiKam and Lightroom
read the same properties but haven't been round-trip tested — for
Lightroom you may need to rename sidecars to `NAME.xmp` on import. If
this matters to your workflow, say so in an issue.

**I shoot RAW+JPEG — where are my JPEGs?**
Hidden on purpose: a JPEG with a same-name RAW twin doesn't appear as a
second grid entry (that would double your cull and split your picks).
You cull the RAW; the in-camera JPEG stays untouched in the folder.
Making the pair travel together through Copy Picks — and a setting to
show pairs — is planned alongside a Settings dialog. JPEGs *without* a
RAW twin are always imported.

**I opened my shoot folder and it says "No images".**
FastCull reads one folder, not subfolders. Open the folder that
actually contains the RAW files (e.g. `.../2026-07-25/card1/`).
"No folder open" is different — that just means no folder was chosen
yet (`Ctrl+O`).

**What's this cache folder?**
Decoded previews are cached (Linux: `~/.cache/fastcull/`; Windows:
`%LOCALAPPDATA%\fastcull\fastcull\cache`) so the second open of a
folder is instant. It's capped around 2 GiB with
least-recently-used eviction, and deleting it is always safe — it just
rebuilds thumbnails on the next open. After some upgrades the app
rebuilds it once by itself; the only cost is a slower first open.

**Thumbnails load slowly from my NAS / slow card.**
Set `FASTCULL_MAX_READERS=4` (or 2) in the environment. It caps how
many files are read at once — slow media thrashes when too many reads
compete.

**A frame shows a warning ("Failed") badge instead of the photo.**
The file's embedded preview couldn't be decoded — typically a file cut
off mid-write: a dying card, an interrupted copy, a full disk. FastCull
checks that the image data is actually complete before decoding, so a
truncated file is flagged honestly instead of being shown as a
half-blank frame (and a corrupt file claiming absurd dimensions is
rejected outright instead of eating gigabytes of memory). Your original
file is never touched — try re-copying it from the card; if the badge
persists, the file really is damaged.

**Something misbehaves — what should I attach to a bug report?**
Run with `FASTCULL_TRACE=1` from a terminal and attach the output: it
timestamps every slow UI phase and loupe state change. This works on
Windows too (cmd or PowerShell): the app attaches to the terminal it was
started from, though the prompt returns immediately and the trace lines
interleave with it — that is normal for a windowed app. One consequence:
a FastCull started from a terminal is tied to that terminal — closing
the terminal window (or pressing Ctrl+C in it) also closes FastCull, so
keep the terminal open while you reproduce the problem. Started by
double-click, the app has no terminal and nothing else can close it. To capture the
trace to a file instead, redirect stderr — from cmd:
`fastcull-app.exe 2> trace.txt` (PowerShell's `2>` reformats the
lines; cmd captures them as-is). If the issue
looks cache-related, try once with `FASTCULL_NO_CACHE=1` to rule it in
or out. (You may also see `FASTCULL_DRIVE` mentioned in the source —
it's a test-automation hook that can mark real files; not for everyday
use.)

**Where do I read about how it works inside?**
The developer specs in [`specs/`](../specs/) are the source of truth —
[architecture](../specs/01-architecture.md), per-module contracts in
[`specs/modules/`](../specs/modules/). This guide is deliberately the
short version.


**The status bar says "sorting by name until loaded" and never stops.**
While a folder loads, FastCull orders the grid by filename and switches to
capture time once every file has been read. If it never switches, one file
is not coming back — a dying card, a disconnected network share, a drive
that stopped responding mid-read — and the counter sits a file or two short
of the total. Nothing is lost, your marks are already written, but the grid
stays in filename order for that session. Close the folder and reopen it;
if it happens again, the file the counter is stuck on is the one to look
at.

---

Back to: [Getting started](index.md) ·
[Culling](culling.md) ·
[Metadata](metadata.md) ·
[Copy Picks](copy-picks.md)
