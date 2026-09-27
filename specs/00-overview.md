# FastCull — Product Overview

## Vision

A Photo Mechanic-class culling tool, open source (GPL-3.0-or-later), for Linux and
Windows. The user opens a folder of thousands of ~100 MB RAW files, sees thumbnails
near-instantly, culls with the keyboard, applies IPTC metadata individually or in
groups, and copies the picks to a destination folder. The selects are then edited in
an external editor, which must see every pick/reject and IPTC field FastCull wrote.
darktable is the **reference editor** — the contract is written against it and
enforced in the test suite (ADR 0003); digiKam, Lightroom and Photo Mechanic read
the same sidecars, best-effort.

**Speed is the product.** Every design decision defers to interactive latency.

## The one architectural idea

Never decode RAW sensor data on the interactive path. Cameras embed camera-rendered
JPEG previews inside every RAW file; FastCull reads only those bytes. Measured on a
32-thread Ryzen AI MAX+ 395 (since retired; real Sony A1 files — see
`adr/0001-embedded-jpeg-strategy.md`): grid pipeline ~300 files/sec vs 0.6–1.2 s per
file for full RAW decode. The numbers a gate round compares against today are the
budget table in `01-architecture.md`.

## Non-goals (v1)

- No RAW development/editing of any kind (that is the editor's job).
- No catalog/database of the user's library — a session is one folder, and a
  snapshot of it at open: no folder watching (user decision 2026-09-17).
- No card ingest (v2), no star ratings/color labels (v2), no monitor ICC color
  management (v2), no macOS (v2), no video files IN the grid (video OUTPUT
  of a burst's embedded JPEGs is an export — `modules/video-export.md`,
  ADR 0004, user decision 2026-08-27).
- No reject-file handling: after copy-picks, rejects stay where they are (user
  deletes them manually later — recorded decision from the persona review).
- No undo stack (arrow-back + re-mark covers culling; IPTC has a single-level
  revert-last-apply). No paired-JPEG handling (v2 candidate). No burst
  stack/unstack (post-v1 nice-to-have).
- No cloud, no AI culling, no telemetry. Ever, for the last one.

## Reference camera

Sony A1 (ILCE-1) is 100%-supported, test-suite-enforced with real files in all three
ARW variants (compressed / lossless-compressed / uncompressed). Every A1 ARW embeds:

| Embedded image | Dimensions | Size | Used for |
|---|---|---|---|
| Thumbnail | 160×120 | ~13 KB | never (too small) |
| Preview | 1616×1080 | ~0.5 MB | grid thumbnails; loupe fit on displays up to ~2K, at rest and under a held key |
| Full-res JPEG | 8640×5760 | ~10–12 MB | 1:1 and every factor above fit; loupe fit on wider displays through the screen rung — this JPEG decoded at N/8 to the fit size (`modules/raw-pipeline.md`) |

Other cameras (the user, 2026-09-26: *"at some point, I will want more users
with different cameras"*; CLAUDE.md M11): TIFF-shaped RAWs (NEF/CR2/DNG…) are
designed for, not best-effort — they read EXIF and their embedded JPEGs
through the same in-tree walker as ARW, every general path (a decoder, a
parser, a ring, a cache) is built and tested for files from other bodies, a
decoder's harmless complaint never refuses a frame
(`modules/raw-pipeline.md`, "The decoder's complaints"), and a decision that
rests on an A1 property says so where it is recorded. Only the A1 is enforced
with real files. Non-TIFF containers (CR3/RAF/X3F) stay best-effort until a
unit takes them on: they fall back to rawler's parser (slower, mmap-based).
Decode fallback chain in `modules/raw-pipeline.md`.

## Glossary

The words the specs use as terms of art, grouped by where they live.

**The cull**
- **Cull** — the pass of deciding picks vs rejects over a shoot.
- **Pick / Reject / Unmarked** — the three pick states of an image.
- **Sidecar** — the `<name>.<ext>.xmp` file holding all FastCull-written state.
- **Burst** — a group of frames from one continuous-drive squeeze. Its
  **opener** is its first frame, where `]` lands (in a capture-sorted view)
  and `[` re-anchors; for
  `[`/`]` a burst or a single is one **territory** (burst-grouping.md).
- **Session** — FastCull's in-memory state for one open folder.

**The views**
- **Grid** — the multi-column thumbnail view; **Loupe** — the single-image
  view, which is the grid at one column.
- **Fit / 1:1** — the loupe with the whole frame on screen / with one image
  pixel per screen pixel; between them the ×1.5 **ladder** of zoom factors
  (ui-grid.md).
- **View order** — the order on screen (the sort, then the filter), as
  opposed to id order (the folder scan). Every ring and every span is in
  view order. **Provisional order** — filename order, held while a folder
  loads until every file's metadata has landed; then the real sort applies
  once, the **load-settled** edge.

**Rungs and rendering**
- **Rung** — a size an image is available at: the **thumb** (320 px, the
  grid's), the **mid** (the camera's 1616×1080 preview), the **screen
  rung** (the embedded full JPEG decoded at N/8 to the fit box, on displays
  wider than ~2K), the **full-res** (the embedded 8640×5760 JPEG). The rung
  ladder cooks the next rung only when the display needs more than 1.25×
  the one in hand (raw-pipeline.md).
- **Fit box** — the loupe's one-column cell in physical pixels: what fit
  asks the decoder for (raw-pipeline.md).
- **Transit / settled** — the loupe's request states: while a key is held
  (frame changes under 250 ms apart) motion comes first and the frame on
  screen asks for no more than the fit box; once the user stops, it asks
  for the real target (ui-grid.md; what every frame of the ring asks in
  each state, raw-pipeline.md). Never what is displayed: the screen always
  shows the best rung in hand.
- **Ring** — the frames around the cursor decoded ahead, in view order: 2
  behind and 15 ahead, leaning the way of travel, at every zoom; above fit
  its far end is clamped to what the pixel cache holds. The **ring in
  force** is that ring as the current state shapes it, the one the cull and
  the revival read (raw-pipeline.md).
- **Switch rule** — during a hold above fit, when the frames ahead stop
  asking for full-res and ask for the fit box, and when they step back up;
  its one measure is the **time-to-screen**, from a full-res decode's start
  to the app's adoption of its texture (raw-pipeline.md, "Above fit").
- **Pixel cache** — the loupe engine's RAM cache of decoded rungs, a
  quarter of total RAM between 2 and 10 GiB; the app's textures are copies
  outside it (raw-pipeline.md, "Memory").
- **Reserved lane** — the loupe worker that takes only the focused frame's
  work, after a quarter-second debounce; it asks for the real target once
  the user stops. Its **idle cook** decodes the cursor's full-res after a
  stop at fit on a wide viewport, so `Z` finds it ready or cooking
  (raw-pipeline.md).
- **Texture window** — the view positions around the cursor whose textures
  the app keeps, leaned the way of travel by the engine (ui-grid.md, "The
  render ladder").
- **The pill** — the small dark badge in the loupe's top-left: ★ or ✕ for
  the mark, and "◌ loading" while the view is rendered from a rung that
  does not serve it — above fit, anything below the top rung; at fit, a
  rung that does not serve the fit box — and, once lit while travelling,
  for at least a quarter second (ui-grid.md).
- **Kitchen** — the app's one texture-preparation thread; every
  pixels→texture step the app performs happens there, never on the UI
  thread (01-architecture.md). The GPU renderer's own upload of a finished
  texture is Slint's, at draw time, on the UI thread.

**The cursor and the selection**
- **Cursor** — the one cell keyboard actions land on. **Claimed** once the
  user touches it (a mark, a navigation key, a click on an image, a loupe
  scroll-follow); until
  then it is "the first image of the view".
- **Reveal / re-anchor** — scrolling so the cursor is on screen after a key;
  keeping the viewport on the cursor across a relayout (panel toggle,
  resize) or the load-settled re-sort.
- **Selection** — the set the IPTC panel and the exports act on, drawn as
  the accent **wash** over each selected cell. The **anchor** is where a
  Shift-span starts. A plain move collapses the selection — the
  file-manager rule (ui-grid.md, brief 002).

**Copying and exporting**
- **The clash question** — the one question Copy Picks asks when a name it
  would write is already at the destination: New only, Keep both,
  Overwrite, Cancel (fileops.md). **The pair is the unit**: a RAW and its
  sidecar clash together.
- **The green light** — "all checksums verified", printed only by a run
  that earned it.
- **Derived output** — a file built from bytes the RAW already contains;
  today the Motion JPEG `.mov` (ADR 0004, video-export.md).

**The harness** (test-harness.md)
- **Drive script** — `FASTCULL_DRIVE`, the timed list of actions a headless
  run executes. **Mark** — a trace line the app emits, which a script can
  `wait:` on and a test can assert. **Dump** — the `QEDUMP` line of app
  state a script requests. **Settle mark** — `load settled gen N`, emitted
  when every file's thumb work has finished; the gate for positional keys.
  **Generation** — the session counter (`gen N`) and the panel-rebuild
  counter that keep marks distinguishable across a swap or a rebuild.

**Roles named in older text** — the team is CLAUDE.md's; older specs and
briefs say **validator** or **architect** for the review role before
2026-09-05 (now `senior-developer`) and **qe-engineer** for `qe`. **PM** is
Photo Mechanic — the reference culling tool the July research compared
against (`PM/LR/digiKam` in ADR 0003; "the PM research" of 2026-07-25 and
"PM's ticked-but-empty case" in iptc-templates.md) — never a role.
