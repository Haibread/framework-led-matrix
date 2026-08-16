//! The time of day, stacked to fit nine pixels across.
//!
//! Hours over minutes rather than side by side: two 3-pixel digits and a gap
//! come to seven, which fits, whereas `HH:MM` never would.

use std::time::Duration;

use chrono::{Local, Timelike};

use crate::canvas::Canvas;
use crate::device::ColorMode;
use crate::font;
use crate::scene::Scene;

/// Rows the two digit pairs start on.
const HOURS_ROW: i32 = 8;
const MINUTES_ROW: i32 = 18;
/// Rows of the blinking separator, between the two.
const COLON_ROWS: [i32; 2] = [14, 16];
/// Row of the seconds bar.
const SECONDS_ROW: i32 = 28;

/// Left edge of the first digit, centring a seven-pixel pair on nine.
const DIGITS_X: i32 = 1;
/// Distance between the two digits of a pair.
const DIGIT_PITCH: i32 = 4;

const DIGIT_LEVEL: u8 = 255;
const COLON_LEVEL: u8 = 160;
const SECONDS_LEVEL: u8 = 70;

/// A clock.
pub struct Clock {
    mode: ColorMode,
    hour: u32,
    minute: u32,
    second: u32,
}

impl Clock {
    /// Starts a clock showing the current time.
    #[must_use]
    pub fn new(mode: ColorMode) -> Self {
        let mut clock = Self {
            mode,
            hour: 0,
            minute: 0,
            second: 0,
        };
        clock.read_time();
        clock
    }

    /// Takes the wall clock reading.
    fn read_time(&mut self) {
        let now = Local::now();
        self.hour = now.hour();
        self.minute = now.minute();
        self.second = now.second();
    }

    /// Draws a two-digit number with its tens on the left.
    fn draw_pair(canvas: &mut Canvas, value: u32, row: i32) {
        font::draw_digit(canvas, (value / 10) % 10, DIGITS_X, row, DIGIT_LEVEL);
        font::draw_digit(canvas, value % 10, DIGITS_X + DIGIT_PITCH, row, DIGIT_LEVEL);
    }
}

impl Scene for Clock {
    fn name(&self) -> &'static str {
        "clock"
    }

    fn update(&mut self, _delta: Duration) {
        // Cheaper than it looks: the panel only redraws when the picture
        // changes, so a clock costs one frame a second and nothing in between.
        self.read_time();
    }

    fn render(&self, canvas: &mut Canvas) {
        Self::draw_pair(canvas, self.hour, HOURS_ROW);
        Self::draw_pair(canvas, self.minute, MINUTES_ROW);

        // The separator blinks on the second, which is what makes a digital
        // clock look like it is running rather than frozen.
        if self.second % 2 == 0 {
            for row in COLON_ROWS {
                canvas.set_max(4, row, COLON_LEVEL);
            }
        }

        // A minute's progress, one pixel per one-ninth of it.
        let filled = i32::try_from(self.second * 9 / 60).unwrap_or(0);
        for x in 0..filled {
            canvas.set_max(x, SECONDS_ROW, SECONDS_LEVEL);
        }

        if self.mode == ColorMode::Bw {
            // Thresholding would drop the dim rows; brighten them instead of
            // letting the clock lose its seconds.
            for x in 0..filled {
                canvas.set_max(x, SECONDS_ROW, u8::MAX);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Clock, HOURS_ROW, MINUTES_ROW};
    use crate::canvas::Canvas;
    use crate::device::ColorMode;
    use crate::scene::Scene;

    fn at(hour: u32, minute: u32, second: u32) -> Clock {
        let mut clock = Clock::new(ColorMode::Greyscale);
        clock.hour = hour;
        clock.minute = minute;
        clock.second = second;
        clock
    }

    #[test]
    fn the_time_is_drawn_as_two_stacked_pairs() {
        let clock = at(12, 34, 0);
        let mut canvas = Canvas::new();
        clock.render(&mut canvas);

        let lit_in = |rows: std::ops::Range<i32>| {
            rows.flat_map(|y| (0..9).map(move |x| (x, y)))
                .filter(|(x, y)| canvas.get(*x, *y) > 0)
                .count()
        };
        assert!(lit_in(HOURS_ROW..HOURS_ROW + 5) > 0, "no hours");
        assert!(lit_in(MINUTES_ROW..MINUTES_ROW + 5) > 0, "no minutes");
    }

    #[test]
    fn midnight_shows_four_zeroes_rather_than_nothing() {
        let clock = at(0, 0, 0);
        let mut canvas = Canvas::new();
        clock.render(&mut canvas);
        assert_ne!(canvas, Canvas::new(), "midnight drew an empty panel");
    }

    #[test]
    fn the_separator_blinks_on_the_second() {
        let mut lit = Canvas::new();
        at(1, 2, 0).render(&mut lit);
        let mut dark = Canvas::new();
        at(1, 2, 1).render(&mut dark);

        assert!(lit.get(4, 14) > 0, "the separator never lights");
        assert_eq!(dark.get(4, 14), 0, "the separator never goes out");
    }

    #[test]
    fn the_seconds_bar_fills_over_a_minute() {
        let count = |second| {
            let mut canvas = Canvas::new();
            at(1, 2, second).render(&mut canvas);
            (0..9)
                .filter(|x| canvas.get(*x, super::SECONDS_ROW) > 0)
                .count()
        };
        assert_eq!(count(0), 0);
        assert!(count(30) > count(5), "the bar does not grow");
        assert_eq!(count(59), 8);
    }

    #[test]
    fn every_hour_and_minute_renders_something() {
        for hour in 0..24 {
            for minute in (0..60).step_by(7) {
                let mut canvas = Canvas::new();
                at(hour, minute, 30).render(&mut canvas);
                assert_ne!(canvas, Canvas::new(), "{hour:02}:{minute:02} drew nothing");
            }
        }
    }
}
