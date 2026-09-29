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

**I edited the copies in darktable, then copied more picks into the same
folder — will FastCull touch my edits?**
Only if you answer **Overwrite** to the clash question. darktable stores
its history in `DSC01234.ARW.xmp`, which is exactly the sidecar name
FastCull writes, so Overwrite replaces it along with the RAW. Answer
**New only** (`N`) instead: it copies the picks that are not there yet
and leaves every file already in the folder alone — not replaced, not
even read. Keep both (`DSC01234_1.ARW` twins) and a fresh folder still
work, but they duplicate what is there. One caveat: with a `{seq}` rename
template the numbers shift when you add picks, so add under original
names or export the whole set again. See
[Copy Picks](copy-picks.md#when-the-names-are-already-taken).

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

**How much memory does FastCull use, and can I change it?**
About a quarter of your computer's memory for decoded photos — never
less than 2 GB, never more than 10 GB — plus the frames on screen and
the decoders' working space. That memory is what keeps the frames ahead
of you and the ones you've recently passed ready — at full quality when
you're at 1:1 — so stepping forward and back is usually instant. In the
worst case — a long session at 1:1 — the whole app can reach, as your
system monitor counts it, about 5 GB on an 8 GB machine, 8 to 9 GB on a
16 GB one, 12 to 16 GB on a 32 GB one and 14 to 18 GB on a 64 GB one:
the more processor cores, the higher, because every core decodes in
parallel and needs working space. On a machine with 12 GB or more it
keeps that worst case under 60 % of the memory — it runs no more
decoders than one per 2 GB of memory, and on a machine under about 20 GB
it reads fewer frames ahead at full quality (three on an 8 GB machine,
about ten on a 16 GB one, fifteen above that). On an 8 GB machine the
worst case is about 60 % of the memory, and more the less of it your
system reports as usable (a processor with built-in graphics can keep a
gigabyte or more for itself). On a machine with less than 8 GB a long
session at 1:1 can need more memory than the machine has — the worst case
stays near 5 GB however small the machine — so there, close other
programs or stay at fit. At fit it uses a good deal less, and
thumbnails add about 200 MB per thousand photos. (This is memory, not
the cache folder on disk described above.) FastCull decides it once,
when it starts, from your machine's total memory and processor cores,
and prints what it chose in the terminal if you start it from one. There
is no setting for it. It does not shrink while it runs yet, so on a
machine with 16 GB or less, close other big programs before a long
session at 1:1.

**Thumbnails load slowly from my NAS / slow card.**
Set `FASTCULL_MAX_READERS=4` (or 2) in the environment. It caps how
many files are read at once — slow media thrashes when too many reads
compete.

**A frame shows a warning ("Failed") badge instead of the photo.**
The file's embedded preview couldn't be decoded — typically a file cut
off mid-write: a dying card, an interrupted copy, a full disk. FastCull
checks that the image data is actually complete before decoding, so a
truncated file is flagged honestly instead of being shown as a half-blank
frame (and a corrupt file claiming absurd dimensions is rejected outright
instead of eating gigabytes of memory). That check is not a full
corruption test: a file damaged in the middle can still open, showing the
damage. Your original file is never touched — try re-copying it from the
card; if the badge persists, the file really is damaged. If the grid
thumbnail shows the photo but the loupe says Failed, the loupe found
something in the image data it treats as damage — usually a stretch the
thumbnail's decoder quietly fills in (look for a blank or smeared band),
occasionally a flaw too small to see. A camera's harmless quirks — a few
stray bytes in the file, an unusual version number, a header field some
encoders leave at zero — don't cause the badge: FastCull shows the photo
anyway and, if you started it from a terminal, prints one line naming the
file. If a photo from another camera shows the badge while it looks whole
in other programs, the project would like to hear about it — please open
an issue with a sample file. The same goes for one whose 1:1 view stops at
the size of its small preview: FastCull ranks the previews inside a RAW by
the sizes the file states for them, so a file that understates its
full-size preview shows the small one as if it were the whole photo — no
camera on record does this. A RAW whose full-size preview is damaged — or
cut short, because an interrupted copy ended the file partway through it —
shows no badge at all: the loupe keeps the smaller preview, so at 1:1 — and
at fit on a screen at least 1600 pixels tall, such as 2560×1600 or 4K — that
one frame stays soft under the "◌ loading" pill however long you wait (on a
1440p screen or smaller, fit shows the smaller preview for every frame, so
that frame looks like any other there), and FastCull prints a line
naming the file in the terminal, if you started it from one. The one
exception is a copy that stopped just where the full-size preview begins:
nothing of that preview is left for FastCull to find, so the frame shows
the smaller preview as if it were the whole photo — no pill, no line — as
FastCull 0.14.0 did. If the copy on the card is whole, copying the file
again and reopening the folder brings the sharp frame back. One rarer case:
a file damaged or cut on disk while its folder is open, after FastCull has
already read its full-size preview — a copy still being written over the
folder you are culling, say — shows the smaller preview as above, but while
you rest on that frame where the pill shows, FastCull keeps retrying the
full-size preview it once read, which keeps one processor core busy (your
fan may spin up). Moving to another frame stops it; reopening the folder
ends it.

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

**The video I exported is enormous. Did something go wrong?**
No — that is what it is. The frames in it are the camera's own full-size
JPEGs, copied without being touched, so a Sony A1 frame is about 11 MB
and a 30-frame burst is around 330 MB. Making it smaller would mean
re-compressing your photographs, which is the one thing this export
refuses to do; your video editor will do it once, at the end, when it
knows what the clip is actually going to be. See
[Export Frames as Video](export-video.md).

**My phone editor won't open the exported video.**
The file is a standard QuickTime `.mov` holding Motion JPEG, which is
about as widely readable as video gets — but the frames inside it are
50-megapixel stills, which is unusual video material. That is the likely
sticking point, not the format. A file of exactly this shape (thirty
8640×5760 frames, 328 MB) imported and played in InShot on Android, which
is why the format was chosen — though that particular file was muxed by
ffmpeg rather than by FastCull, and nobody has yet put a FastCull-made
one on a phone. Other editors, iOS, and whether a portrait burst comes
out upright are all unverified. The project would like to hear about it
either way.

---

Back to: [Getting started](index.md) ·
[Culling](culling.md) ·
[Metadata](metadata.md) ·
[Copy Picks](copy-picks.md) ·
[Export Frames as Video](export-video.md)
