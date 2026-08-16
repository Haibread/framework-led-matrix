//! A 3x5 digit font, the largest that fits legibly on a 9-pixel-wide module.

use crate::canvas::Canvas;

/// Width of one small glyph, in pixels.
pub const GLYPH_WIDTH: i32 = 3;
/// Height of one small glyph, in pixels.
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

/// Row bitmaps for `A` to `Z`, in the small face.
///
/// Three pixels wide is tight for letters — `M` and `N` lean on the same trick
/// of filling the middle column — but nine columns hold two characters and a
/// gap, and a title has to scroll either way.
const LETTERS: [[u8; 5]; 26] = [
    [0b111, 0b101, 0b111, 0b101, 0b101], // A
    [0b110, 0b101, 0b110, 0b101, 0b110], // B
    [0b111, 0b100, 0b100, 0b100, 0b111], // C
    [0b110, 0b101, 0b101, 0b101, 0b110], // D
    [0b111, 0b100, 0b111, 0b100, 0b111], // E
    [0b111, 0b100, 0b111, 0b100, 0b100], // F
    [0b111, 0b100, 0b101, 0b101, 0b111], // G
    [0b101, 0b101, 0b111, 0b101, 0b101], // H
    [0b111, 0b010, 0b010, 0b010, 0b111], // I
    [0b001, 0b001, 0b001, 0b101, 0b111], // J
    [0b101, 0b101, 0b110, 0b101, 0b101], // K
    [0b100, 0b100, 0b100, 0b100, 0b111], // L
    [0b101, 0b111, 0b111, 0b101, 0b101], // M
    [0b101, 0b111, 0b111, 0b111, 0b101], // N
    [0b111, 0b101, 0b101, 0b101, 0b111], // O
    [0b111, 0b101, 0b111, 0b100, 0b100], // P
    [0b111, 0b101, 0b101, 0b111, 0b001], // Q
    [0b111, 0b101, 0b110, 0b101, 0b101], // R
    [0b111, 0b100, 0b111, 0b001, 0b111], // S
    [0b111, 0b010, 0b010, 0b010, 0b010], // T
    [0b101, 0b101, 0b101, 0b101, 0b111], // U
    [0b101, 0b101, 0b101, 0b101, 0b010], // V
    [0b101, 0b101, 0b111, 0b111, 0b101], // W
    [0b101, 0b101, 0b010, 0b101, 0b101], // X
    [0b101, 0b101, 0b010, 0b010, 0b010], // Y
    [0b111, 0b001, 0b010, 0b100, 0b111], // Z
];

/// Distance between the left edges of two neighbouring characters.
pub const PITCH: i32 = 4;

/// Draws `text` with its top-left corner at `(x, y)`, in the small face.
///
/// Only letters and digits are drawn; anything else advances the cursor without
/// marking the panel, which is what makes a space a space. Drawing is clipped,
/// so a negative `x` is simply a line mid-scroll.
pub fn draw_text(canvas: &mut Canvas, text: &str, x: i32, y: i32, value: u8) {
    for (index, character) in text.chars().enumerate() {
        let column = x + i32::try_from(index).unwrap_or(0) * PITCH;
        // Nothing to the left of the panel can show, and once past the right
        // edge neither can anything after it.
        if column >= crate::canvas::WIDTH {
            return;
        }
        if column <= -GLYPH_WIDTH {
            continue;
        }
        draw_char(canvas, character, column, y, value);
    }
}

/// Draws one character, or nothing if there is no glyph for it.
fn draw_char(canvas: &mut Canvas, character: char, x: i32, y: i32, value: u8) {
    let glyph = match character.to_ascii_uppercase() {
        letter @ 'A'..='Z' => {
            let index = usize::from(letter as u8 - b'A');
            LETTERS.get(index).copied()
        }
        digit @ '0'..='9' => {
            let index = usize::from(digit as u8 - b'0');
            DIGITS.get(index).copied()
        }
        _ => None,
    };
    let Some(rows) = glyph else {
        return;
    };

    for (bits, row) in rows.iter().zip(0..GLYPH_HEIGHT) {
        for column in 0..GLYPH_WIDTH {
            if bits & (1u8 << (GLYPH_WIDTH - 1 - column)) != 0 {
                canvas.set_max(x + column, y + row, value);
            }
        }
    }
}

/// The width `text` would take, in pixels.
#[must_use]
pub fn text_width(text: &str) -> i32 {
    let characters = i32::try_from(text.chars().count()).unwrap_or(0);
    (characters * PITCH - 1).max(0)
}

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
