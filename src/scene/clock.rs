//! The time of day, stacked to fit nine pixels across.
//!
//! Hours over minutes rather than side by side: two 3-pixel digits and a gap
//! come to seven, which fits, whereas `HH:MM` never would. Given more rows it
//! switches to a 4x7 face, where two digits come to exactly nine.

use std::time::Duration;

use chrono::{Local, Timelike};

use crate::canvas::Canvas;
use crate::device::ColorMode;
use crate::font;
use crate::scene::{Area, Scene};

/// Rows needed for the small face: two rows of digits and a gap.
const SMALL_HEIGHT: i32 = 11;
/// Rows from which the large face is used instead.
const LARGE_HEIGHT: i32 = 15;
/// Rows from which there is room for the seconds bar as well.
const SECONDS_HEIGHT: i32 = 17;

const DIGIT_LEVEL: u8 = 255;
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

    /// Draws a two-digit number, centred on the panel.
    fn pair(canvas: &mut Canvas, value: u32, row: i32, large: bool) {
        let (glyph, pitch, left) = if large {
            (font::Size::Large, 5, 0)
        } else {
            (font::Size::Small, 4, 1)
        };
        font::draw_digit_sized(canvas, (value / 10) % 10, left, row, DIGIT_LEVEL, glyph);
        font::draw_digit_sized(canvas, value % 10, left + pitch, row, DIGIT_LEVEL, glyph);
    }
}

impl Scene for Clock {
    fn name(&self) -> &'static str {
        "clock"
    }

    fn min_height(&self) -> i32 {
        SMALL_HEIGHT
    }

    fn update(&mut self, _delta: Duration) {
        // Cheaper than it looks: the panel only redraws when the picture
        // changes, so a clock costs one frame a second and nothing in between.
        self.read_time();
    }

    fn render(&self, canvas: &mut Canvas, area: Area) {
        let large = area.height >= LARGE_HEIGHT;
        let digits = if large { 7 } else { 5 };
        // The two rows of digits, with a gap, centred in whatever we were given.
        let block = digits * 2 + 1;
        let top = area.top + (area.height - block).max(0) / 2;

        Self::pair(canvas, self.hour, top, large);
        Self::pair(canvas, self.minute, top + digits + 1, large);

        if area.height < SECONDS_HEIGHT {
            // No room for seconds without crowding the digits; the time itself
            // is what this is for.
            return;
        }

        // A minute's progress along the bottom row of the area.
        let level = if self.mode == ColorMode::Bw {
            u8::MAX
        } else {
            SECONDS_LEVEL
        };
        let filled = i32::try_from(self.second * 9 / 60).unwrap_or(0);
        for x in 0..filled {
            canvas.set_max(x, area.bottom(), level);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Clock, LARGE_HEIGHT, SECONDS_HEIGHT, SMALL_HEIGHT};
    use crate::canvas::Canvas;
    use crate::device::ColorMode;
    use crate::scene::{Area, Scene};

    fn at(hour: u32, minute: u32, second: u32) -> Clock {
        let mut clock = Clock::new(ColorMode::Greyscale);
        clock.hour = hour;
        clock.minute = minute;
        clock.second = second;
        clock
    }

    fn drawn(clock: &Clock, area: Area) -> Canvas {
        let mut canvas = Canvas::new();
        clock.render(&mut canvas, area);
        canvas
    }

    fn lit(canvas: &Canvas) -> usize {
        (0..9)
            .flat_map(|x| (0..34).map(move |y| (x, y)))
            .filter(|(x, y)| canvas.get(*x, *y) > 0)
            .count()
    }

    #[test]
    fn the_time_shows_at_the_smallest_size_it_asks_for() {
        let clock = at(12, 34, 0);
        let canvas = drawn(
            &clock,
            Area {
                top: 0,
                height: SMALL_HEIGHT,
            },
        );
        assert!(lit(&canvas) > 0, "nothing drawn in the height it asked for");
    }

    #[test]
    fn a_taller_area_gets_the_larger_face() {
        // The large face spans all nine columns; the small one is inset by a
        // pixel either side, so the outer columns tell them apart.
        let clock = at(8, 8, 0);
        let small = drawn(
            &clock,
            Area {
                top: 0,
                height: SMALL_HEIGHT,
            },
        );
        let large = drawn(
            &clock,
            Area {
                top: 0,
                height: LARGE_HEIGHT,
            },
        );

        let outer = |canvas: &Canvas| (0..34).any(|y| canvas.get(0, y) > 0 || canvas.get(8, y) > 0);
        assert!(!outer(&small), "the small face reached the outer columns");
        assert!(
            outer(&large),
            "the large face was not used when there was room"
        );
    }

    #[test]
    fn nothing_is_drawn_outside_the_area() {
        // The invariant the whole stack rests on: a widget that overflows would
        // silently scribble on its neighbour.
        let clock = at(23, 59, 59);
        for height in SMALL_HEIGHT..=34 {
            for top in 0..=(34 - height) {
                let canvas = drawn(&clock, Area { top, height });
                for y in 0..34 {
                    if y < top || y >= top + height {
                        for x in 0..9 {
                            assert_eq!(
                                canvas.get(x, y),
                                0,
                                "row {y} lit for an area at {top} of {height}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn the_seconds_bar_waits_until_there_is_room_for_it() {
        let clock = at(1, 2, 59);
        let cramped = drawn(
            &clock,
            Area {
                top: 0,
                height: SECONDS_HEIGHT - 1,
            },
        );
        let roomy = drawn(
            &clock,
            Area {
                top: 0,
                height: SECONDS_HEIGHT,
            },
        );

        let bottom_row = |canvas: &Canvas, height: i32| {
            (0..9).filter(|x| canvas.get(*x, height - 1) > 0).count()
        };
        assert_eq!(
            bottom_row(&cramped, SECONDS_HEIGHT - 1),
            0,
            "crowded the digits"
        );
        assert!(
            bottom_row(&roomy, SECONDS_HEIGHT) > 0,
            "no seconds when there was room"
        );
    }

    #[test]
    fn every_hour_and_minute_renders_something() {
        for hour in 0..24 {
            for minute in (0..60).step_by(7) {
                let canvas = drawn(&at(hour, minute, 30), Area::FULL);
                assert_ne!(canvas, Canvas::new(), "{hour:02}:{minute:02} drew nothing");
            }
        }
    }
}
