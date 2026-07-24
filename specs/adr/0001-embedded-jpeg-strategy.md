# ADR 0001: Cull on embedded JPEGs, never decode RAW interactively

**Status**: accepted (2026-07-24) · **Decides**: the product's core architecture

## Decision

All interactive rendering uses the camera-written JPEG previews embedded in RAW
files. Full RAW decode exists only as a last-resort background fallback for cameras
with no usable embedded preview.

## Evidence (measured 2026-07-24, Ryzen AI MAX+ 395 / 32 threads / 58 GB, real Sony A1 ARWs from raw.pixls.us)

| Operation (median, warm cache) | Time |
|---|---|
| rawler: open ARW + EXIF | ~2 ms |
| rawler: extract+decode 1616×1080 preview | 6.5–10 ms |
| zune-jpeg ≈ libjpeg-turbo: decode 8640×5760 embedded JPEG | 130–150 ms |
| libjpeg-turbo DCT-scaled decode (fit-to-screen) | 64–95 ms |
| LibRaw full decode+demosaic | 556–1,224 ms |
| Parallel grid pipeline (open+extract+decode+resize), 32 threads | ~300 files/s |
| Parallel full-res decodes | ~87 imgs/s |

Embedded-preview culling is ~100–700× cheaper than RAW decoding, and the A1's
embedded JPEG is full-resolution — nothing of judgment-relevant detail is lost.

## Consequences

- 2,000-image folder → thumbnails in ~7 s on the reference machine.
- Loupe 1:1 costs ~150 ms once; ±2 neighbor prefetch hides it completely.
- We display what the camera rendered (like Photo Mechanic), not the RAW
  development darktable will produce. Accepted trade-off for a culling tool.
- rawler 0.7 gap: A1 full-res JpgFromRaw not exposed → in-tree extractor
  (raw-pipeline spec); no upstreaming without user approval.
