//! A 3x5 digit font, the largest that fits legibly on a 9-pixel-wide module.

use crate::canvas::Canvas;

/// Width of one small glyph, in pixels.
pub const GLYPH_WIDTH: i32 = 3;
/// Height of one small glyph, in pixels.
#[cfg(test)]
pub const GLYPH_HEIGHT: i32 = 5;

/// Which face to draw with.
///
/// Two digits of the large face come to exactly nine pixels with their gap,
/// which is the whole panel — it is the biggest legible size this display can
/// hold, and worth having whenever there are rows to spare.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Size {
    /// 3x5, the one that fits anywhere.
    Small,
    /// 4x7, for when there is room.
    Large,
}

/// Row bitmaps for the digits `0` to `9` in the large face.
const DIGITS_LARGE: [[u8; 7]; 10] = [
    [0b1111, 0b1001, 0b1001, 0b1001, 0b1001, 0b1001, 0b1111],
    [0b0010, 0b0110, 0b0010, 0b0010, 0b0010, 0b0010, 0b0111],
    [0b1111, 0b0001, 0b0001, 0b1111, 0b1000, 0b1000, 0b1111],
    [0b1111, 0b0001, 0b0001, 0b0111, 0b0001, 0b0001, 0b1111],
    [0b1001, 0b1001, 0b1001, 0b1111, 0b0001, 0b0001, 0b0001],
    [0b1111, 0b1000, 0b1000, 0b1111, 0b0001, 0b0001, 0b1111],
    [0b1111, 0b1000, 0b1000, 0b1111, 0b1001, 0b1001, 0b1111],
    [0b1111, 0b0001, 0b0001, 0b0010, 0b0010, 0b0100, 0b0100],
    [0b1111, 0b1001, 0b1001, 0b1111, 0b1001, 0b1001, 0b1111],
    [0b1111, 0b1001, 0b1001, 0b1111, 0b0001, 0b0001, 0b1111],
];

/// Row bitmaps for the digits `0` to `9`, most significant bit leftmost.
const DIGITS: [[u8; 5]; 10] = [
    [0b111, 0b101, 0b101, 0b101, 0b111],
    [0b010, 0b110, 0b010, 0b010, 0b111],
    [0b111, 0b001, 0b111, 0b100, 0b111],
    [0b111, 0b001, 0b111, 0b001, 0b111],
    [0b101, 0b101, 0b111, 0b001, 0b001],
    [0b111, 0b100, 0b111, 0b001, 0b111],
    [0b111, 0b100, 0b111, 0b101, 0b111],
    [0b111, 0b001, 0b001, 0b001, 0b001],
    [0b111, 0b101, 0b111, 0b101, 0b111],
    [0b111, 0b101, 0b111, 0b001, 0b111],
];

/// Draws `digit` with its top-left corner at `(x, y)`.
///
/// Values above `9` draw nothing, which keeps callers free of range checks.
pub fn draw_digit(canvas: &mut Canvas, digit: u32, x: i32, y: i32, value: u8) {
    draw_digit_sized(canvas, digit, x, y, value, Size::Small);
}

/// Draws `digit` in the chosen face, top-left corner at `(x, y)`.
///
/// Values above `9` draw nothing, which keeps callers free of range checks.
pub fn draw_digit_sized(canvas: &mut Canvas, digit: u32, x: i32, y: i32, value: u8, size: Size) {
    let Some(index) = usize::try_from(digit).ok() else {
        return;
    };

    let (rows, width): (&[u8], i32) = match size {
        Size::Small => match DIGITS.get(index) {
            Some(glyph) => (glyph, GLYPH_WIDTH),
            None => return,
        },
        Size::Large => match DIGITS_LARGE.get(index) {
            Some(glyph) => (glyph, 4),
            None => return,
        },
    };

    for (bits, row) in rows.iter().zip(0..) {
        for column in 0..width {
            if bits & (1u8 << (width - 1 - column)) != 0 {
                canvas.set_max(x + column, y + row, value);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{GLYPH_HEIGHT, GLYPH_WIDTH, draw_digit};
    use crate::canvas::Canvas;

    #[test]
    fn every_digit_lights_something_inside_its_box() {
        for digit in 0..10 {
            let mut canvas = Canvas::new();
            draw_digit(&mut canvas, digit, 3, 10, 255);

            let mut lit = 0;
            for y in 10..10 + GLYPH_HEIGHT {
                for x in 3..3 + GLYPH_WIDTH {
                    if canvas.get(x, y) > 0 {
                        lit += 1;
                    }
                }
            }
            assert!(lit > 0, "digit {digit} drew nothing");
        }
    }

    #[test]
    fn glyphs_stay_within_their_bounding_box() {
        let mut canvas = Canvas::new();
        draw_digit(&mut canvas, 8, 3, 10, 255);

        assert_eq!(canvas.get(2, 10), 0, "leaked one column left");
        assert_eq!(canvas.get(6, 10), 0, "leaked one column right");
        assert_eq!(canvas.get(3, 9), 0, "leaked one row above");
        assert_eq!(canvas.get(3, 15), 0, "leaked one row below");
    }

    #[test]
    fn one_is_drawn_with_its_stem_and_foot() {
        let mut canvas = Canvas::new();
        draw_digit(&mut canvas, 1, 0, 0, 255);

        assert_eq!(canvas.get(1, 0), 255, "stem top");
        assert_eq!(canvas.get(0, 1), 255, "serif");
        assert_eq!(canvas.get(0, 4), 255, "foot");
        assert_eq!(canvas.get(2, 4), 255, "foot");
    }

    #[test]
    fn digits_above_nine_draw_nothing() {
        let mut canvas = Canvas::new();
        draw_digit(&mut canvas, 10, 3, 10, 255);
        assert_eq!(canvas, Canvas::new());
    }
}
