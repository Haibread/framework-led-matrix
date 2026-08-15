//! Greyscale framebuffer for a single LED matrix module.
//!
//! The Framework 16 LED Matrix is a 9x34 grid of single-colour LEDs, each with
//! an 8-bit brightness. A [`Canvas`] is one full frame for one module; scenes
//! draw into it and a [`crate::device::Matrix`] pushes it to the hardware.

/// Number of columns on one module.
pub const WIDTH: i32 = 9;
/// Number of rows on one module.
pub const HEIGHT: i32 = 34;
/// Row count expressed as an array length, mirroring [`HEIGHT`].
pub const ROWS: usize = 34;
/// Total number of LEDs on one module, mirroring [`WIDTH`] * [`HEIGHT`].
pub const PIXELS: usize = 306;

const COLUMNS: usize = 9;

// The `usize` mirrors above exist only to size arrays without a cast; keep them
// honest.
const _: () = assert!(WIDTH == 9 && HEIGHT == 34 && ROWS == 34 && COLUMNS == 9);
const _: () = assert!(PIXELS == ROWS * COLUMNS);

/// One frame of greyscale pixels, stored row by row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Canvas {
    pixels: [u8; PIXELS],
}

impl Default for Canvas {
    fn default() -> Self {
        Self::new()
    }
}

impl Canvas {
    /// Creates an all-off canvas.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            pixels: [0; PIXELS],
        }
    }

    /// Turns every pixel off.
    pub fn clear(&mut self) {
        self.pixels.fill(0);
    }

    /// Returns the brightness at `(x, y)`, or `0` outside the canvas.
    #[must_use]
    pub fn get(&self, x: i32, y: i32) -> u8 {
        index(x, y).map_or(0, |i| self.pixels[i])
    }

    /// Sets the brightness at `(x, y)`, ignoring out-of-bounds writes.
    ///
    /// Clipping rather than panicking keeps the game code free of bounds
    /// checks: a ball at the edge of the field can splat over the border.
    pub fn set(&mut self, x: i32, y: i32, value: u8) {
        if let Some(i) = index(x, y) {
            self.pixels[i] = value;
        }
    }

    /// Sets the brightness at `(x, y)` unless that pixel is already brighter.
    pub fn set_max(&mut self, x: i32, y: i32, value: u8) {
        if let Some(i) = index(x, y) {
            self.pixels[i] = self.pixels[i].max(value);
        }
    }

    /// Draws a horizontal run of pixels from `x0` to `x1`, inclusive.
    pub fn hline(&mut self, x0: i32, x1: i32, y: i32, value: u8) {
        for x in x0..=x1 {
            self.set(x, y, value);
        }
    }

    /// Extracts column `x` top to bottom, as the wire protocol wants it.
    #[must_use]
    pub fn column(&self, x: i32) -> [u8; ROWS] {
        let mut column = [0u8; ROWS];
        if let Some(start) = index(x, 0) {
            for (y, cell) in column.iter_mut().enumerate() {
                *cell = self.pixels[start + y * COLUMNS];
            }
        }
        column
    }
}

/// Maps a coordinate to a pixel offset, or `None` when it falls off the canvas.
fn index(x: i32, y: i32) -> Option<usize> {
    if x < 0 || y < 0 || x >= WIDTH || y >= HEIGHT {
        return None;
    }
    usize::try_from(y * WIDTH + x).ok()
}

/// Rounds a continuous field coordinate to the nearest pixel index.
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    reason = "field coordinates are bounded by the 9x34 grid, far from i32 limits"
)]
pub fn to_pixel(value: f32) -> i32 {
    value.round() as i32
}

/// Truncates a continuous field coordinate to the pixel containing it.
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    reason = "field coordinates are bounded by the 9x34 grid, far from i32 limits"
)]
pub fn floor_pixel(value: f32) -> i32 {
    value.floor() as i32
}

/// Converts a `0.0..=1.0` intensity to an 8-bit brightness.
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value is clamped to 0.0..=1.0 and scaled to 0..=255 before the cast"
)]
pub fn level(fraction: f32) -> u8 {
    (fraction.clamp(0.0, 1.0) * 255.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::{Canvas, HEIGHT, WIDTH, floor_pixel, level, to_pixel};

    #[test]
    fn a_new_canvas_is_dark() {
        let canvas = Canvas::new();
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                assert_eq!(canvas.get(x, y), 0);
            }
        }
    }

    #[test]
    fn pixels_round_trip() {
        let mut canvas = Canvas::new();
        canvas.set(4, 17, 200);
        assert_eq!(canvas.get(4, 17), 200);
        canvas.clear();
        assert_eq!(canvas.get(4, 17), 0);
    }

    #[test]
    fn out_of_bounds_writes_are_dropped_not_wrapped() {
        let mut canvas = Canvas::new();
        canvas.set(-1, 0, 255);
        canvas.set(WIDTH, 0, 255);
        canvas.set(0, -1, 255);
        canvas.set(0, HEIGHT, 255);
        assert_eq!(canvas, Canvas::new());
        assert_eq!(canvas.get(-1, 0), 0);
    }

    #[test]
    fn set_max_keeps_the_brighter_pixel() {
        let mut canvas = Canvas::new();
        canvas.set_max(0, 0, 100);
        canvas.set_max(0, 0, 40);
        assert_eq!(canvas.get(0, 0), 100);
        canvas.set_max(0, 0, 180);
        assert_eq!(canvas.get(0, 0), 180);
    }

    #[test]
    fn hline_spans_the_inclusive_range() {
        let mut canvas = Canvas::new();
        canvas.hline(2, 4, 9, 255);
        assert_eq!(canvas.get(1, 9), 0);
        assert_eq!(canvas.get(2, 9), 255);
        assert_eq!(canvas.get(4, 9), 255);
        assert_eq!(canvas.get(5, 9), 0);
    }

    #[test]
    fn column_reads_top_to_bottom() {
        let mut canvas = Canvas::new();
        canvas.set(3, 0, 11);
        canvas.set(3, 33, 22);
        canvas.set(4, 0, 99);

        let column = canvas.column(3);
        assert_eq!(column[0], 11);
        assert_eq!(column[33], 22);
        assert_eq!(column[1], 0);
    }

    #[test]
    fn column_outside_the_canvas_is_dark() {
        assert_eq!(Canvas::new().column(WIDTH), [0u8; super::ROWS]);
    }

    #[test]
    fn coordinate_helpers_round_and_truncate() {
        assert_eq!(to_pixel(3.6), 4);
        assert_eq!(floor_pixel(3.6), 3);
        assert_eq!(to_pixel(-0.4), 0);
        assert_eq!(floor_pixel(-0.4), -1);
    }

    #[test]
    fn level_clamps_outside_the_unit_range() {
        assert_eq!(level(0.0), 0);
        assert_eq!(level(1.0), 255);
        assert_eq!(level(-5.0), 0);
        assert_eq!(level(5.0), 255);
    }
}
