# Module spec: RAW preview pipeline (`raw/` + `pipeline.rs`)

## Purpose

Turn a folder of RAW files into displayable images at interactive speed without ever
decoding RAW sensor data on the hot path.

## Inputs / outputs

- In: file paths from `catalog`; priority hints from the UI (visible range, loupe pos).
- Out: per image, up to three assets delivered as `SessionEvent`s:
  `Thumb` (320 px, grid), `FitPreview` (screen-sized), `FullRes` (1:1 pixels).

## Extraction strategy (per file)

1. Open with `rawler::RawSource` + `get_decoder` — never read the whole file; only
   IFD/metadata and the byte ranges of the chosen embedded JPEG.
2. Asset sources, in order of preference:
   - **Grid thumb**: largest embedded preview ≤ ~2 MP (A1: the 1616×1080), decoded
     with zune-jpeg, SIMD-resized (`fast_image_resize`) to 320 px.
   - **FullRes**: largest embedded JPEG (A1: 8640×5760 `JpgFromRaw`) decoded with
     turbojpeg. **Known gap**: rawler 0.7 does not expose the A1 full-res JPEG
     (`full_image()` returns 1616×1080). Discovery therefore uses an in-tree,
     from-scratch minimal TIFF/IFD walker (`raw/tiff.rs`) rather than rawler's
     TIFF parser: it operates on any `Read + Seek` (enabling the counting-reader
     budget tests), reads only IFD tables and JPEG headers, and is hardened
     against hostile files (offset cycles, entry-count bombs, out-of-range
     offsets) — properties rawler's path-based API doesn't offer. rawler remains
     the EXIF/metadata and RAW-decode-fallback dependency. BigTIFF (magic 43)
     containers are rejected as not-TIFF. Do NOT upstream anything without
     explicit user approval.
   - **Loupe asset ladder (user decision 2026-07-25, replaces the separate
     DCT FitPreview)**: display the best already-loaded asset immediately,
     and cook a higher-resolution one ONLY when the display size exceeds the
     loaded asset by more than 25% (the `UPSCALE_THRESHOLD = 1.25` rule).
     Rungs for the A1: 320 px thumb → 1616×1080 mid preview (~5 ms decode —
     covers fit view on ≲1.9k-wide viewports instantly) → 8640×5760 full
     (~140 ms, cooked in background for 1:1 and large displays; the shown
     image swaps in place when ready, never blocks). Loupe assets are served
     by a dedicated 2-worker engine (`loupe.rs`) with its own event channel —
     full-res decodes must never queue behind a background thumbnail sweep.
     turbojpeg DCT scaling is a recorded FUTURE optimization only (saves
     ~35–45% on the cook; the ladder already hides that latency).
3. Fallback chain when a source is missing (non-A1 cameras): full-res JPEG → mid
   preview upscaled → half-size RAW decode via rawler (background priority only,
   with a "rendered from RAW" badge event) → `Failed(reason)`.

## Priority queue contract

- Three levels: `Visible` > `Prefetch` (loupe ±2) > `Background` (sequential file
  order — cold-cache/card-reader friendly).
- Scroll/zoom calls `set_visible(range)`; already-queued jobs are reprioritized, not
  re-enqueued. In-flight jobs are never cancelled mid-decode (they're ≤150 ms).
- Duplicate requests for the same (image, asset) coalesce.

## Memory budget

- Thumbs: unbounded (≈200 KB each; 5,000 images ≈ 1 GB worst case — acceptable; the
  SQLite cache lets us evict and re-load cheaply if this ever pinches).
- FullRes decodes: LRU capped at 2 GiB (configurable). FitPreviews count toward it.

## Acceptance criteria (tests)

- [ ] For each of the 3 A1 test files: grid thumb is produced from the 1616×1080
      preview (assert source dimensions), FullRes is 8640×5760.
- [ ] No test may observe a read of more than 20 MB from a 100 MB A1 file for the
      grid path (instrument with a counting reader).
- [ ] A file with a truncated/garbage preview yields `Failed` and does not poison
      the pipeline (subsequent jobs complete).
- [ ] `set_visible` promotion: with a saturated queue, a newly visible image's thumb
      arrives before ≥90% of background items (deterministic test with a fake
      2-thread pool and instrumented job order).
- [ ] The budgets in `01-architecture.md` are enforced by release-mode tests
      (`tests/perf_budgets.rs`, dedicated CI step); criterion benches
      (`benches/hot_path.rs`) provide the numbers for humans.
