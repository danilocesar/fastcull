# FastCull — Product Overview

## Vision

A Photo Mechanic-class culling tool, open source (GPL-3.0-or-later), for Linux and
Windows. The user opens a folder of thousands of ~100 MB RAW files, sees thumbnails
near-instantly, culls with the keyboard, applies IPTC metadata individually or in
groups, and copies the picks to a destination folder. The selects are then edited in
darktable, which must see every pick/reject and IPTC field FastCull wrote.

**Speed is the product.** Every design decision defers to interactive latency.

## The one architectural idea

Never decode RAW sensor data on the interactive path. Cameras embed camera-rendered
JPEG previews inside every RAW file; FastCull reads only those bytes. Measured on a
32-thread Ryzen AI MAX+ 395 (since retired; real Sony A1 files — see
`adr/0001-embedded-jpeg-strategy.md`): grid pipeline ~300 files/sec vs 0.6–1.2 s per
file for full RAW decode.

## Non-goals (v1)

- No RAW development/editing of any kind (that is darktable's job).
- No catalog/database of the user's library — a session is one folder.
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
| Preview | 1616×1080 | ~0.5 MB | grid thumbnails |
| Full-res JPEG | 8640×5760 | ~10–12 MB | loupe fit + 1:1 |

Other cameras: best-effort — TIFF-shaped RAWs (NEF/CR2/DNG…) read EXIF via the
same in-tree walker as ARW; non-TIFF containers (CR3/RAF/X3F) fall back to
rawler's parser (slower, mmap-based — acceptable for out-of-scope formats).
Decode fallback chain in `modules/raw-pipeline.md`.

## Glossary

- **Cull** — the pass of deciding picks vs rejects over a shoot.
- **Pick / Reject / Unmarked** — the three pick states of an image.
- **Sidecar** — the `<name>.<ext>.xmp` file holding all FastCull-written state.
- **Burst** — a group of frames from one continuous-drive squeeze.
- **Grid** — the multi-column thumbnail view; **Loupe** — the single-image view.
- **Session** — FastCull's in-memory state for one open folder.
