# FastCull — getting started

FastCull is a fast, keyboard-first photo culling tool: point it at a
folder of RAW files — or JPEGs — mark keepers and rejects at full
speed, stamp metadata, and copy the verified keepers out. It never
edits or develops photos — that stays with darktable (or whatever you
develop in).

## Install

FastCull ships as plain archives on the
[Releases page](https://github.com/danilocesar/fastcull/releases) —
no installer, nothing touches your system.

**Linux**: unpack `fastcull-app-x86_64-unknown-linux-gnu.tar.xz` anywhere
and run `fastcull-app`. The binary needs a normal desktop's runtime
libraries: fontconfig, libxkbcommon, and Mesa OpenGL — already present
on any Fedora/Ubuntu/Arch desktop install. If launching from a file
manager does nothing, run it once from a terminal and read the message
(a missing library names itself there).

**Windows**: unpack `fastcull-app-x86_64-pc-windows-msvc.zip` and
double-click `fastcull-app.exe`. The first launch shows a blue
**"Windows protected your PC"** dialog — that is SmartScreen reacting to
an unsigned executable, not a virus warning. Click **More info**, then
**Run anyway**; Windows remembers the choice for that copy of the file.

## Can I trust it with my photos?

Three facts before you press a single key:

- **Your RAW files are never opened for writing.** Not once, not "just
  to embed a rating". Everything you do is written to a small text
  sidecar next to each file (`DSC01234.ARW.xmp`).
- **Existing sidecars are preserved.** If a folder already has sidecars
  from darktable, Photo Mechanic or anything else, FastCull edits only
  the fields it owns and keeps everything else intact.
- **Nothing is dropped into your photo folders** except those sidecars.
  The thumbnail cache lives in your user cache directory, not next to
  your photos.

## Quick start

1. Launch FastCull and open a folder (**File > Open Folder…** or
   `Ctrl+O`, or pass the folder on the command line).
2. Thumbnails stream in immediately; you can start culling before they
   finish loading.
3. Mark keepers with `Y`, rejects with `N` — the cursor auto-advances.
4. `Ctrl+E` copies your picks (with their sidecars) to a destination
   folder, checksum-verified.

One thing to know up front: **FastCull reads one folder, not its
subfolders.** If you open `2026-07-25/` and your files live in
`2026-07-25/card1/`, you'll see "No images" — open `card1/` itself.

**What gets imported**: every RAW format the decoder knows (Sony,
Canon, Nikon, Fuji, DNG, …) plus JPEGs. One rule for JPEGs: a JPEG
that has a RAW twin with the same name (`DSC01234.ARW` +
`DSC01234.JPG`, straight from a RAW+JPEG camera setting) stays hidden —
you cull the RAW, and the moment counts once. A JPEG on its own (phone
cards, a second body shooting JPEG, darktable exports elsewhere) is a
first-class image: cull it, tag it, copy it like any RAW. Videos and
other non-photo files are simply ignored.

## Where do the files come from?

FastCull has no card-ingest step, on purpose. Two workflows both work:

- **Ingest first**: download the card with your usual tool (Rapid Photo
  Downloader, your camera vendor's app…), then open that folder and
  cull.
- **Cull straight off the card**: open the mounted card's folder,
  cull there, and let **Copy Picks be your download** — the checksum
  report ("all checksums verified") is your green light before
  formatting the card. Rejects and unmarked files are never deleted or
  moved; copying is the only file operation FastCull performs.

---

Next: [Culling — the keyboard, the loupe, and bursts](culling.md)
