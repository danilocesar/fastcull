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
   - **FitPreview — folded into FullRes (M4 recorded deviation, pending
     the user's OK as an acceptance-criterion change)**: the loupe uses ONE
     asset — the fully decoded full-res RGB — GPU-scaled for fit and native
     for 1:1. zune-jpeg (pure Rust) decodes it in ~140 ms, inside the 350 ms
     budget; turbojpeg DCT scaling is not used (system-lib dependency, and
     the second decode saved no user-visible latency). Loupe assets are
     served by a dedicated 2-worker engine (`loupe.rs`) with its own event
     channel rather than through the thumbnail pipeline's queue — full-res
     decodes must never queue behind a background thumbnail sweep.
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
