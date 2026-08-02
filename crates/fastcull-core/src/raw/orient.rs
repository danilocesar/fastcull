//! EXIF orientation applied to decoded RGB pixels — the loupe's other
//! hot loop (raw-pipeline.md § Orientation).
//!
//! Performance matters here far more than it looks: a 50 MP A1 frame is
//! 49.8 M pixels, this runs on the loupe's interactive path, and ui-grid.md
//! promises "sharpness-on-stop within ~300 ms". The original implementation
//! evaluated the orientation `match` inside a loop over every pixel, copied
//! three bytes at a time, and transposed row-major into column-major so
//! nearly every write missed cache: 236 ms measured for orientation 8 on the
//! 8-core development laptop — comparable to the ~250 ms JPEG decode it
//! follows. Only portrait frames pay it (landscape returns at the guard),
//! which is why it went unnoticed and why perf_budgets forces orientation 8.
//!
//! What this implementation does, each choice pinned by measurement
//! (research probe 2026-08-02, medians of 3 on real 8640x5760 pixels):
//!
//! - **Mirrors and 180° (2/3/4) run IN PLACE** — they keep the source
//!   dimensions, so the second 149 MB buffer the original allocated for
//!   them was pure waste. Adopted from the `perf/tiled-orientation-27`
//!   branch, which got this right.
//! - **Transposes (5-8) walk 64 px tiles** so a fetched source cache line
//!   is consumed across the whole tile instead of once. Tile sweep:
//!   16→48 ms, 32→41.7, **64→39.8**, 128→42, 256→44.7.
//! - **Scoped threads over disjoint output bands, capped at 8.** Thread
//!   sweep: 1→134 ms, 2→72.9, 4→45.9, 8→42.1 — memory-bound beyond the
//!   physical cores, so a 32-thread machine must not spawn 32 workers to
//!   fight over one bus.
//! - **Writes go through `chunks_exact_mut(3)`** so the write side carries
//!   no per-pixel bounds check, and the `y_rev` branch is hoisted out of
//!   the inner loop. This is the difference from the tiled branch's kernel:
//!   39.8 ms → **28.5-30.7 ms** for the identical result, in safe Rust.
//! - **The output buffer can be supplied pre-faulted** ([`Scratch`]):
//!   first-touching 149 MB of fresh pages costs ~40 ms single-threaded,
//!   and the caller (the loupe decode path) has seven idle cores while the
//!   strictly-serial JPEG decode runs — the perfect place to pay it.
//!
//! Measured and rejected, so nobody re-tries them (probe records in the
//! branch commit):
//! - Overlapping 4-byte moves: 58.6 ms — SLOWER than the 3-byte scalar
//!   copy; the trick defeats the vectorizer.
//! - Blocked multi-row kernels reading contiguous source pixels through an
//!   array of `&mut` row slices: 73-126 ms — the indirection defeats LLVM.
//! - An `unsafe` pointer kernel: 25.2 ms. Real, but ~4 ms over the safe
//!   kernel is not worth introducing the crate's first `unsafe` block.
//! - zune-jpeg 0.5.15 for the decode half: 267-279 ms vs 0.4.21's 247-252
//!   — the upgrade is a regression on this workload.
//!
//! Output is byte-identical to the original for all eight orientations;
//! `orientation_matches_reference` pins that against the reference
//! implementation across sizes chosen to exercise partial tiles and
//! partial thread bands.

/// Square tile for the transposing rotations, in pixels (sweep above).
const TILE: usize = 64;

/// Thread cap for the transpose (sweep above: 4 ≈ 8, both >> 2).
const MAX_THREADS: usize = 8;

/// Below this many source bytes the transpose stays single-threaded (see
/// the threads note in `transpose_rotate`): mid-rung class images gain
/// nothing from the fan-out, and portrait thumbs rotate inside already-
/// parallel pipeline workers.
const PARALLEL_THRESHOLD_BYTES: usize = 32 * 1024 * 1024;

/// Apply an EXIF orientation (1-8) to decoded RGB pixels, returning the
/// display-oriented buffer and its (possibly swapped) dimensions. Soft
/// rotation only — sources are never modified (Photo Mechanic behavior).
pub fn apply_orientation(rgb: Vec<u8>, w: u32, h: u32, orientation: u16) -> (Vec<u8>, u32, u32) {
    apply_orientation_with(rgb, w, h, orientation, None)
}

/// A pre-faulted output buffer for the transposing orientations, built
/// ahead of time (ideally on spare cores while the JPEG decode runs) so
/// the rotate never pays first-touch page faults on the interactive path.
pub struct Scratch(Vec<u8>);

impl Scratch {
    /// Allocate and pre-fault a buffer for a `w`x`h` RGB image, touching
    /// pages from up to [`MAX_THREADS`] threads (~6 ms for 149 MB vs
    /// ~40 ms serial).
    pub fn prefaulted(w: u32, h: u32) -> Self {
        let n = (w as usize) * (h as usize) * 3;
        let mut buf = vec![0u8; n];
        prefault_parallel(&mut buf);
        Self(buf)
    }
}

/// Touch one byte per page from up to [`MAX_THREADS`] threads, so the
/// kernel maps the pages before a hot loop needs them (~6 ms for 149 MB
/// vs ~40 ms serially, measured). Public because the loupe decode path
/// prepares its `decode_into` buffer the same way.
pub(crate) fn prefault_parallel(buf: &mut [u8]) {
    let chunk = buf.len().div_ceil(MAX_THREADS).max(1);
    std::thread::scope(|scope| {
        for c in buf.chunks_mut(chunk) {
            scope.spawn(move || {
                let mut i = 0;
                while i < c.len() {
                    c[i] = 0;
                    i += 4096;
                }
            });
        }
    });
}

/// [`apply_orientation`], with an optional pre-faulted output buffer for
/// the transposing orientations. A `scratch` of the wrong size is ignored
/// (debug-asserted): correctness never depends on the optimization.
pub fn apply_orientation_with(
    rgb: Vec<u8>,
    w: u32,
    h: u32,
    orientation: u16,
    scratch: Option<Scratch>,
) -> (Vec<u8>, u32, u32) {
    // Zero-area images pass through like every other degenerate input.
    // Without this, `w == 0 || h == 0` with a transposing orientation
    // satisfied the length check (0 == 0) and panicked in `chunks_mut(0)`
    // downstream — a behavioral regression from the pre-rework code, which
    // returned the buffer untouched (validator finding, 2026-08-02).
    if orientation <= 1
        || orientation > 8
        || w == 0
        || h == 0
        || rgb.len() != (w as usize * h as usize * 3)
    {
        return (rgb, w, h);
    }
    let (wu, hu) = (w as usize, h as usize);
    let mut rgb = rgb;
    match orientation {
        2 => {
            // Mirror horizontal: reverse the pixels of each row, in place.
            for row in rgb.chunks_exact_mut(wu * 3) {
                reverse_pixels(row);
            }
            (rgb, w, h)
        }
        3 => {
            // Rotate 180 = reverse every pixel of the image, in place.
            reverse_pixels(&mut rgb);
            (rgb, w, h)
        }
        4 => {
            // Mirror vertical: swap row i with row (h-1-i), in place.
            let stride = wu * 3;
            for y in 0..hu / 2 {
                let (top, rest) = rgb.split_at_mut((y + 1) * stride);
                let a = &mut top[y * stride..];
                let b = &mut rest[(hu - 2 - 2 * y) * stride..][..stride];
                a.swap_with_slice(b);
            }
            (rgb, w, h)
        }
        _ => {
            let out = transpose_rotate(&rgb, wu, hu, orientation, scratch);
            (out, h, w)
        }
    }
}

/// The transposing orientations (5-8): tiled, banded, bounds-check-free on
/// the write side.
///
/// Inverting the forward mapping gives a source coordinate whose `x`
/// depends only on the output ROW and whose `y` depends only on the output
/// COLUMN, which is what makes a clean tiled walk possible:
///   5: x = oy,        y = dx
///   6: x = oy,        y = hu-1-dx
///   7: x = wu-1-oy,   y = hu-1-dx
///   8: x = wu-1-oy,   y = dx
fn transpose_rotate(
    src: &[u8],
    wu: usize,
    hu: usize,
    orientation: u16,
    scratch: Option<Scratch>,
) -> Vec<u8> {
    let x_rev = matches!(orientation, 7 | 8);
    let y_rev = matches!(orientation, 6 | 7);
    let (ow, oh) = (hu, wu);
    let mut out = match scratch {
        Some(Scratch(buf)) if buf.len() == src.len() => buf,
        other => {
            debug_assert!(
                other.is_none(),
                "Scratch of wrong size: {} for image of {}",
                other.map_or(0, |Scratch(b)| b.len()),
                src.len(),
            );
            vec![0u8; src.len()]
        }
    };
    // Small images run single-threaded. Honest trade, measured on the mid
    // rung (1080x1616 portrait): on an IDLE machine threads do win there —
    // ~4.4 ms single vs ~1.5 ms with 8 — but this path also serves every
    // PORTRAIT GRID THUMB via the pipeline, whose workers are already
    // parallel one-per-core, and 8 workers each spawning 8 scoped threads
    // is 64-way oversubscription during exactly the import bursts the
    // pipeline exists to keep fast. ~3 ms of idle-case mid latency is
    // beneath notice; multiplying threads under full pipeline load is not
    // (validator risk item, 2026-08-02). Full-res frames (149 MB) stay
    // well above the threshold and keep the fan-out.
    let threads = if src.len() < PARALLEL_THRESHOLD_BYTES {
        1
    } else {
        std::thread::available_parallelism()
            .map_or(1, |n| n.get())
            .clamp(1, MAX_THREADS)
    };
    let rows_per = oh.div_ceil(threads);
    std::thread::scope(|scope| {
        for (t, band) in out.chunks_mut(rows_per * ow * 3).enumerate() {
            scope.spawn(move || {
                let base = t * rows_per;
                let nrows = band.len() / (ow * 3);
                for dy0 in (0..nrows).step_by(TILE) {
                    let dy1 = (dy0 + TILE).min(nrows);
                    for dx0 in (0..ow).step_by(TILE) {
                        let dx1 = (dx0 + TILE).min(ow);
                        for dy in dy0..dy1 {
                            let oy = base + dy;
                            let x = if x_rev { wu - 1 - oy } else { oy };
                            let row = &mut band[dy * ow * 3..(dy + 1) * ow * 3];
                            // Write side: chunked, so no per-pixel bounds
                            // check; `y_rev` hoisted out of the pixel loop.
                            let tile_px = row[dx0 * 3..dx1 * 3].chunks_exact_mut(3);
                            if y_rev {
                                for (i, px) in tile_px.enumerate() {
                                    let y = hu - 1 - (dx0 + i);
                                    let s = (y * wu + x) * 3;
                                    px.copy_from_slice(&src[s..s + 3]);
                                }
                            } else {
                                for (i, px) in tile_px.enumerate() {
                                    let s = ((dx0 + i) * wu + x) * 3;
                                    px.copy_from_slice(&src[s..s + 3]);
                                }
                            }
                        }
                    }
                }
            });
        }
    });
    out
}

/// Reverse a run of 3-byte pixels in place.
fn reverse_pixels(buf: &mut [u8]) {
    let n = buf.len() / 3;
    for i in 0..n / 2 {
        let (a, b) = (i * 3, (n - 1 - i) * 3);
        for k in 0..3 {
            buf.swap(a + k, b + k);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tiled/threaded/in-place implementation must be byte-identical
    /// to the straightforward version for EVERY orientation, at sizes that
    /// exercise partial tiles and partial thread bands (the tile is 64 px,
    /// so 1x1, odd, and >64 all take different paths). Reference is
    /// deliberately the naive per-pixel mapping the original shipped.
    #[test]
    fn orientation_matches_reference() {
        fn reference(rgb: &[u8], w: u32, h: u32, o: u16) -> (Vec<u8>, u32, u32) {
            if o <= 1 || o > 8 || rgb.len() != (w as usize * h as usize * 3) {
                return (rgb.to_vec(), w, h);
            }
            let (wu, hu) = (w as usize, h as usize);
            let swap = matches!(o, 5..=8);
            let (ow, oh) = if swap { (hu, wu) } else { (wu, hu) };
            let mut out = vec![0u8; rgb.len()];
            for y in 0..hu {
                for x in 0..wu {
                    let (dx, dy) = match o {
                        2 => (wu - 1 - x, y),
                        3 => (wu - 1 - x, hu - 1 - y),
                        4 => (x, hu - 1 - y),
                        5 => (y, x),
                        6 => (hu - 1 - y, x),
                        7 => (hu - 1 - y, wu - 1 - x),
                        8 => (y, wu - 1 - x),
                        _ => (x, y),
                    };
                    let src = (y * wu + x) * 3;
                    let dst = (dy * ow + dx) * 3;
                    out[dst..dst + 3].copy_from_slice(&rgb[src..src + 3]);
                }
            }
            (out, ow as u32, oh as u32)
        }
        // A deterministic pattern where every pixel is distinguishable, so
        // a mis-mapped pixel cannot hide behind a same-coloured neighbour.
        let mk = |w: usize, h: usize| -> Vec<u8> {
            (0..w * h * 3)
                .map(|i| ((i * 37 + i / 3 * 11) % 251) as u8)
                .collect()
        };
        for (w, h) in [
            (1usize, 1usize),
            (1, 7),
            (7, 1),
            (2, 3),
            (63, 65),
            (64, 64),
            (65, 63),
            (129, 64),
            (64, 129),
            (130, 71),
            (191, 257),
            (200, 137),
        ] {
            let src = mk(w, h);
            for o in 1..=8u16 {
                let want = reference(&src, w as u32, h as u32, o);
                let got = apply_orientation(src.clone(), w as u32, h as u32, o);
                assert_eq!(got.1, want.1, "width mismatch {w}x{h} orientation {o}");
                assert_eq!(got.2, want.2, "height mismatch {w}x{h} orientation {o}");
                assert!(got.0 == want.0, "PIXELS DIFFER at {w}x{h} orientation {o}");
                // The Scratch path must be identical too — correctness may
                // never depend on who allocated the output.
                let with = apply_orientation_with(
                    src.clone(),
                    w as u32,
                    h as u32,
                    o,
                    Some(Scratch::prefaulted(w as u32, h as u32)),
                );
                assert!(with.0 == want.0, "SCRATCH PATH DIFFERS at {w}x{h} o {o}");
            }
        }
    }

    /// Degenerate inputs pass through untouched (same contract as always).
    /// Zero-area dims are part of this contract: the first cut of this
    /// module panicked in `chunks_mut(0)` for them under a transposing
    /// orientation, where the pre-rework code passed them through
    /// (validator finding, 2026-08-02) — dims come back UNCHANGED, which
    /// is why these live here and not in the reference matrix (a real
    /// transpose of a 0x5 would swap the dims; a pass-through must not).
    #[test]
    fn invalid_inputs_pass_through() {
        let rgb = vec![1u8, 2, 3];
        let (out, w, h) = apply_orientation(rgb.clone(), 1, 1, 0);
        assert_eq!((out.as_slice(), w, h), (rgb.as_slice(), 1, 1));
        let (out, ..) = apply_orientation(rgb.clone(), 1, 1, 9);
        assert_eq!(out, rgb);
        // Length mismatch: refuse to touch.
        let (out, ..) = apply_orientation(rgb.clone(), 5, 5, 6);
        assert_eq!(out, rgb);
        // Zero-area, every orientation incl. the transposes that panicked.
        for (w, h) in [(0u32, 0u32), (0, 5), (5, 0)] {
            for o in 0..=9u16 {
                let (out, ow, oh) = apply_orientation(Vec::new(), w, h, o);
                assert!(out.is_empty());
                assert_eq!((ow, oh), (w, h), "zero-area dims must pass through");
            }
        }
    }

    /// A wrong-size Scratch is ignored, never trusted (release builds fall
    /// back to a fresh allocation; correctness holds either way).
    ///
    /// Release-only because the debug build's `debug_assert` makes the
    /// same call panic by design. CI runs lib unit tests in DEBUG only, so
    /// this is exercised by the local release gate rounds, not CI — a
    /// recorded decision (validator 2026-08-02), revisit if CI ever grows
    /// a release unit-test leg.
    #[test]
    #[cfg(not(debug_assertions))]
    fn wrong_size_scratch_falls_back() {
        let src: Vec<u8> = (0..4 * 2 * 3).map(|i| i as u8).collect();
        let bad = Scratch::prefaulted(1, 1);
        let want = apply_orientation(src.clone(), 4, 2, 6);
        let got = apply_orientation_with(src, 4, 2, 6, Some(bad));
        assert_eq!(got.0, want.0);
    }
}
