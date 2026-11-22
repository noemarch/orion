//! Block-element glyphs drawn as cell-fitting quads, the way Ghostty does.
//!
//! Font glyphs for U+2580..=U+259F rarely span their whole cell, so block art
//! gets seams. These rects fill the cell exactly; [`snap_rect`] aligns them to
//! device pixels.

/// A block element in U+2580..=U+259F, reduced to the quads that fill its cell.
///
/// `Horizontal` and `Vertical` fields are eighth indices in `0..=8`. A
/// `Quadrants` bitmask uses bit 1 for upper-left, 2 for upper-right, 4 for
/// lower-left, and 8 for lower-right. `Shade` carries 1, 2, or 3; the caller
/// applies the alpha.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockSprite {
    Full,
    UpperHalf,
    LowerHalf,
    LeftHalf,
    RightHalf,
    Horizontal { top: u8, bottom: u8 },
    Vertical { left: u8, right: u8 },
    Quadrants(u8),
    Shade(u8),
}

const QUAD_UL: u8 = 0b0001;
const QUAD_UR: u8 = 0b0010;
const QUAD_LL: u8 = 0b0100;
const QUAD_LR: u8 = 0b1000;

/// Classifies a block element, or returns `None` outside U+2580..=U+259F.
pub fn classify_block_sprite(c: char) -> Option<BlockSprite> {
    match c {
        '\u{2580}' => Some(BlockSprite::UpperHalf),
        '\u{2581}' => Some(BlockSprite::Horizontal { top: 7, bottom: 8 }),
        '\u{2582}' => Some(BlockSprite::Horizontal { top: 6, bottom: 8 }),
        '\u{2583}' => Some(BlockSprite::Horizontal { top: 5, bottom: 8 }),
        '\u{2584}' => Some(BlockSprite::LowerHalf),
        '\u{2585}' => Some(BlockSprite::Horizontal { top: 3, bottom: 8 }),
        '\u{2586}' => Some(BlockSprite::Horizontal { top: 2, bottom: 8 }),
        '\u{2587}' => Some(BlockSprite::Horizontal { top: 1, bottom: 8 }),
        '\u{2588}' => Some(BlockSprite::Full),
        '\u{2589}' => Some(BlockSprite::Vertical { left: 0, right: 7 }),
        '\u{258a}' => Some(BlockSprite::Vertical { left: 0, right: 6 }),
        '\u{258b}' => Some(BlockSprite::Vertical { left: 0, right: 5 }),
        '\u{258c}' => Some(BlockSprite::Vertical { left: 0, right: 4 }),
        '\u{258d}' => Some(BlockSprite::Vertical { left: 0, right: 3 }),
        '\u{258e}' => Some(BlockSprite::Vertical { left: 0, right: 2 }),
        '\u{258f}' => Some(BlockSprite::Vertical { left: 0, right: 1 }),
        '\u{2590}' => Some(BlockSprite::Vertical { left: 4, right: 8 }),
        '\u{2591}' => Some(BlockSprite::Shade(1)),
        '\u{2592}' => Some(BlockSprite::Shade(2)),
        '\u{2593}' => Some(BlockSprite::Shade(3)),
        '\u{2594}' => Some(BlockSprite::Horizontal { top: 0, bottom: 1 }),
        '\u{2595}' => Some(BlockSprite::Vertical { left: 7, right: 8 }),
        '\u{2596}' => Some(BlockSprite::Quadrants(QUAD_LL)),
        '\u{2597}' => Some(BlockSprite::Quadrants(QUAD_LR)),
        '\u{2598}' => Some(BlockSprite::Quadrants(QUAD_UL)),
        '\u{2599}' => Some(BlockSprite::Quadrants(QUAD_UL | QUAD_LL | QUAD_LR)),
        '\u{259a}' => Some(BlockSprite::Quadrants(QUAD_UL | QUAD_LR)),
        '\u{259b}' => Some(BlockSprite::Quadrants(QUAD_UL | QUAD_UR | QUAD_LL)),
        '\u{259c}' => Some(BlockSprite::Quadrants(QUAD_UL | QUAD_UR | QUAD_LR)),
        '\u{259d}' => Some(BlockSprite::Quadrants(QUAD_UR)),
        '\u{259e}' => Some(BlockSprite::Quadrants(QUAD_UR | QUAD_LL)),
        '\u{259f}' => Some(BlockSprite::Quadrants(QUAD_UR | QUAD_LL | QUAD_LR)),
        _ => None,
    }
}

const EIGHTH: [f32; 9] = [0.0, 0.125, 0.25, 0.375, 0.5, 0.625, 0.75, 0.875, 1.0];

const fn horizontal_rects() -> [[(f32, f32, f32, f32); 9]; 9] {
    let mut rects = [[(0.0, 0.0, 0.0, 0.0); 9]; 9];
    let mut top = 0usize;
    while top < 9 {
        let mut bottom = 0usize;
        while bottom < 9 {
            rects[top][bottom] = (0.0, EIGHTH[top], 1.0, EIGHTH[bottom]);
            bottom += 1;
        }
        top += 1;
    }
    rects
}

const fn vertical_rects() -> [[(f32, f32, f32, f32); 9]; 9] {
    let mut rects = [[(0.0, 0.0, 0.0, 0.0); 9]; 9];
    let mut left = 0usize;
    while left < 9 {
        let mut right = 0usize;
        while right < 9 {
            rects[left][right] = (EIGHTH[left], 0.0, EIGHTH[right], 1.0);
            right += 1;
        }
        left += 1;
    }
    rects
}

static HORIZONTAL_RECTS: [[(f32, f32, f32, f32); 9]; 9] = horizontal_rects();
static VERTICAL_RECTS: [[(f32, f32, f32, f32); 9]; 9] = vertical_rects();

const QUAD_RECT_UL: (f32, f32, f32, f32) = (0.0, 0.0, 0.5, 0.5);
const QUAD_RECT_UR: (f32, f32, f32, f32) = (0.5, 0.0, 1.0, 0.5);
const QUAD_RECT_LL: (f32, f32, f32, f32) = (0.0, 0.5, 0.5, 1.0);
const QUAD_RECT_LR: (f32, f32, f32, f32) = (0.5, 0.5, 1.0, 1.0);

static QUADRANT_RECTS: [&[(f32, f32, f32, f32)]; 16] = [
    &[],
    &[QUAD_RECT_UL],
    &[QUAD_RECT_UR],
    &[QUAD_RECT_UL, QUAD_RECT_UR],
    &[QUAD_RECT_LL],
    &[QUAD_RECT_UL, QUAD_RECT_LL],
    &[QUAD_RECT_UR, QUAD_RECT_LL],
    &[QUAD_RECT_UL, QUAD_RECT_UR, QUAD_RECT_LL],
    &[QUAD_RECT_LR],
    &[QUAD_RECT_UL, QUAD_RECT_LR],
    &[QUAD_RECT_UR, QUAD_RECT_LR],
    &[QUAD_RECT_UL, QUAD_RECT_UR, QUAD_RECT_LR],
    &[QUAD_RECT_LL, QUAD_RECT_LR],
    &[QUAD_RECT_UL, QUAD_RECT_LL, QUAD_RECT_LR],
    &[QUAD_RECT_UR, QUAD_RECT_LL, QUAD_RECT_LR],
    &[QUAD_RECT_UL, QUAD_RECT_UR, QUAD_RECT_LL, QUAD_RECT_LR],
];

/// Cell-fraction rects `(left, top, right, bottom)` for a sprite, in `0.0..=1.0`.
pub fn block_rects(sprite: BlockSprite) -> &'static [(f32, f32, f32, f32)] {
    match sprite {
        BlockSprite::Full | BlockSprite::Shade(_) => &[(0.0, 0.0, 1.0, 1.0)],
        BlockSprite::UpperHalf => &[(0.0, 0.0, 1.0, 0.5)],
        BlockSprite::LowerHalf => &[(0.0, 0.5, 1.0, 1.0)],
        BlockSprite::LeftHalf => &[(0.0, 0.0, 0.5, 1.0)],
        BlockSprite::RightHalf => &[(0.5, 0.0, 1.0, 1.0)],
        BlockSprite::Horizontal { top, bottom } => std::slice::from_ref(
            &HORIZONTAL_RECTS[usize::from(top).min(8)][usize::from(bottom).min(8)],
        ),
        BlockSprite::Vertical { left, right } => std::slice::from_ref(
            &VERTICAL_RECTS[usize::from(left).min(8)][usize::from(right).min(8)],
        ),
        BlockSprite::Quadrants(mask) => QUADRANT_RECTS[usize::from(mask) & 0x0f],
    }
}

/// Snaps each rect edge to the `scale` device-pixel grid.
pub fn snap_rect(x0: f32, y0: f32, x1: f32, y1: f32, scale: f32) -> [f32; 4] {
    let snap = |v: f32| (v * scale).round() / scale;
    [snap(x0), snap(y0), snap(x1), snap(y1)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn classifies_block_elements() {
        assert_eq!(classify_block_sprite('\u{2588}'), Some(BlockSprite::Full));
        assert_eq!(
            classify_block_sprite('\u{2580}'),
            Some(BlockSprite::UpperHalf)
        );
        assert_eq!(
            classify_block_sprite('\u{2584}'),
            Some(BlockSprite::LowerHalf)
        );
        assert_eq!(
            classify_block_sprite('\u{2581}'),
            Some(BlockSprite::Horizontal { top: 7, bottom: 8 })
        );
        assert_eq!(
            classify_block_sprite('\u{2587}'),
            Some(BlockSprite::Horizontal { top: 1, bottom: 8 })
        );
        assert_eq!(
            block_rects(BlockSprite::Horizontal { top: 7, bottom: 8 }),
            &[(0.0, 0.875, 1.0, 1.0)]
        );
        assert_eq!(
            classify_block_sprite('\u{2591}'),
            Some(BlockSprite::Shade(1))
        );
        assert_eq!(
            classify_block_sprite('\u{2598}'),
            Some(BlockSprite::Quadrants(QUAD_UL))
        );
    }

    #[::core::prelude::v1::test]
    fn rejects_non_block_characters() {
        assert_eq!(classify_block_sprite('A'), None);
        assert_eq!(classify_block_sprite('\u{2500}'), None);
    }

    #[::core::prelude::v1::test]
    fn full_rect_is_the_whole_cell() {
        assert_eq!(block_rects(BlockSprite::Full), &[(0.0, 0.0, 1.0, 1.0)]);
    }

    #[::core::prelude::v1::test]
    fn half_rects() {
        assert_eq!(block_rects(BlockSprite::UpperHalf), &[(0.0, 0.0, 1.0, 0.5)]);
        assert_eq!(block_rects(BlockSprite::LowerHalf), &[(0.0, 0.5, 1.0, 1.0)]);
    }

    #[::core::prelude::v1::test]
    fn snaps_rect_edges_to_device_pixels() {
        assert_eq!(
            snap_rect(10.3, 20.7, 18.3, 36.7, 2.0),
            [10.5, 20.5, 18.5, 36.5]
        );
    }
}
