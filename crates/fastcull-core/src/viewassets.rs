//! UI-held asset bookkeeping for the zoom ladder (`ui-grid.md` / user test
//! demand 2026-07-25): tracks which quality rung the UI holds a texture for,
//! and — critically — pulls from the engine's cache when no event will come
//! (an index the engine already served produces NO Ready event; relying on
//! events alone left pruned-and-revisited cells stuck on the 320 px thumb —
//! the user's "+4 then arrow 8 times, image 8 looks bad" bug).

use std::collections::HashMap;

use crate::loupe::{FullImage, LoupeEngine, UPSCALE_THRESHOLD};

/// Long edges of the textures the UI currently holds, per image index.
#[derive(Default)]
pub struct ViewAssets {
    held: HashMap<usize, u32>,
}

impl ViewAssets {
    /// Does the held texture satisfy the ladder rule for this display size?
    pub fn satisfied(&self, index: usize, display_long: u32) -> bool {
        self.held
            .get(&index)
            .is_some_and(|l| *l as f32 * UPSCALE_THRESHOLD >= display_long as f32)
    }

    /// The UI built (or received) a texture of this size.
    pub fn note_held(&mut self, index: usize, long: u32) {
        let e = self.held.entry(index).or_insert(0);
        *e = (*e).max(long);
    }

    /// Drop bookkeeping for indexes outside `keep` (UI prunes textures too).
    pub fn prune(&mut self, keep: &std::ops::Range<usize>) {
        self.held.retain(|i, _| keep.contains(i));
    }

    /// Ensure every index in `range` will reach an asset for `display_long`:
    /// schedules engine work AND returns cached images the UI must adopt now
    /// because the engine considers them served (no event will fire). The
    /// returned images may be low rungs — later Ready events upgrade them.
    pub fn ensure(
        &mut self,
        range: std::ops::Range<usize>,
        display_long: u32,
        engine: &LoupeEngine,
    ) -> Vec<(usize, FullImage)> {
        engine.want(range.clone(), display_long);
        let mut adopt = Vec::new();
        for index in range {
            if self.satisfied(index, display_long) {
                continue;
            }
            if let Some(image) = engine.peek(index) {
                let long = image.width.max(image.height);
                if self.held.get(&index).is_none_or(|h| long > *h) {
                    // Caller adopts the pixels and then reports what it
                    // actually holds via note_held (it may downscale).
                    adopt.push((index, image));
                }
            }
        }
        adopt
    }
}
