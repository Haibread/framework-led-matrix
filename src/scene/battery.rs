//! Charge level, drawn as a battery.
//!
//! An outline rather than a bare bar: on a panel with no labels, the shape is
//! what says which number you are looking at. Given five rows it lies down;
//! given the panel it stands up.

use std::time::Duration;

use crate::canvas::{self, Canvas};
use crate::device::ColorMode;
use crate::scene::{Area, Scene};
use crate::system::{self, Battery as Reading};

/// How often the charge is re-read. It moves slowly; polling faster would only
/// spin the disk cache.
const SAMPLE_INTERVAL: f32 = 5.0;

/// Rows for the battery lying down, terminal included.
pub const MIN_HEIGHT: i32 = 5;
/// Rows from which it stands up instead.
const UPRIGHT_HEIGHT: i32 = 20;

const OUTLINE_LEVEL: u8 = 90;
const FILL_LEVEL: u8 = 255;
/// Speed of the wave that runs up the fill while charging, in rows per second.
const CHARGE_WAVE_ROWS: f32 = 6.0;

/// A battery gauge.
pub struct BatteryGauge {
    mode: ColorMode,
    reading: Option<Reading>,
    elapsed: f32,
    since_sample: f32,
}

impl BatteryGauge {
    /// Starts the gauge, reading the battery straight away.
    #[must_use]
    pub fn new(mode: ColorMode) -> Self {
        Self {
            mode,
            reading: system::read_battery(),
            elapsed: 0.0,
            since_sample: 0.0,
        }
    }

    /// The battery standing up, filling from the bottom.
    fn draw_upright(&self, canvas: &mut Canvas, area: Area, reading: Option<Reading>) {
        let top = area.row(2);
        let bottom = area.bottom();
        let (left, right) = (1, 7);

        for x in 3..=5 {
            canvas.set_max(x, area.top, OUTLINE_LEVEL);
            canvas.set_max(x, area.row(1), OUTLINE_LEVEL);
        }
        canvas.hline(left, right, top, OUTLINE_LEVEL);
        canvas.hline(left, right, bottom, OUTLINE_LEVEL);
        for y in top..=bottom {
            canvas.set_max(left, y, OUTLINE_LEVEL);
            canvas.set_max(right, y, OUTLINE_LEVEL);
        }

        let Some(reading) = reading else {
            return;
        };
        let rows = bottom - top - 1;
        let filled = i32::from(reading.capacity) * rows / 100;
        for step in 0..filled {
            canvas.hline(left + 1, right - 1, bottom - 1 - step, FILL_LEVEL);
        }

        if reading.charging && rows > 0 {
            // A wave climbing the fill, so charging reads as motion rather
            // than as a number you have to remember.
            let span = f32::from(u8::try_from(rows).unwrap_or(1));
            let travel = (self.elapsed * CHARGE_WAVE_ROWS) % span;
            let wave = bottom - 1 - canvas::floor_pixel(travel);
            let level = if self.mode == ColorMode::Bw {
                0
            } else {
                OUTLINE_LEVEL
            };
            for x in (left + 1)..right {
                canvas.set(x, wave, level);
            }
        }
    }

    /// The battery lying down, filling from the left.
    fn draw_lying(canvas: &mut Canvas, area: Area, reading: Option<Reading>) {
        let (top, bottom) = (area.top, area.top + 4);
        canvas.hline(0, 7, top, OUTLINE_LEVEL);
        canvas.hline(0, 7, bottom, OUTLINE_LEVEL);
        for y in top..=bottom {
            canvas.set_max(0, y, OUTLINE_LEVEL);
            canvas.set_max(7, y, OUTLINE_LEVEL);
        }
        for y in (top + 1)..bottom {
            canvas.set_max(8, y, OUTLINE_LEVEL);
        }

        let Some(reading) = reading else {
            return;
        };
        let filled = i32::from(reading.capacity) * 6 / 100;
        for step in 0..filled {
            for y in (top + 1)..bottom {
                canvas.set_max(1 + step, y, FILL_LEVEL);
            }
        }
        if reading.charging {
            for (x, dy) in [(4, 1), (3, 2), (5, 2), (4, 3)] {
                canvas.set(x, top + dy, 0);
            }
        }
    }
}

impl Scene for BatteryGauge {
    fn name(&self) -> &'static str {
        "battery"
    }

    fn min_height(&self) -> i32 {
        MIN_HEIGHT
    }

    fn update(&mut self, delta: Duration) {
        let delta = delta.as_secs_f32();
        self.elapsed += delta;
        self.since_sample += delta;
        if self.since_sample >= SAMPLE_INTERVAL {
            self.since_sample = 0.0;
            self.reading = system::read_battery();
        }
    }

    fn render(&self, canvas: &mut Canvas, area: Area) {
        // No battery, or it could not be read: the empty case says so rather
        // than showing a confident zero.
        if area.height >= UPRIGHT_HEIGHT {
            self.draw_upright(canvas, area, self.reading);
        } else {
            Self::draw_lying(canvas, area.centred(MIN_HEIGHT), self.reading);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BatteryGauge, MIN_HEIGHT, UPRIGHT_HEIGHT};
    use crate::canvas::Canvas;
    use crate::device::ColorMode;
    use crate::scene::{Area, Scene};
    use crate::system::Battery as Reading;

    fn gauge(capacity: u8, charging: bool) -> BatteryGauge {
        let mut gauge = BatteryGauge::new(ColorMode::Greyscale);
        gauge.reading = Some(Reading { capacity, charging });
        gauge
    }

    fn drawn(gauge: &BatteryGauge, area: Area) -> Canvas {
        let mut canvas = Canvas::new();
        gauge.render(&mut canvas, area);
        canvas
    }

    #[test]
    fn the_case_is_drawn_whatever_the_charge() {
        for height in [MIN_HEIGHT, UPRIGHT_HEIGHT, 34] {
            let canvas = drawn(&gauge(0, false), Area { top: 0, height });
            assert_ne!(canvas, Canvas::new(), "no case at height {height}");
        }
    }

    #[test]
    fn the_fill_tracks_the_charge() {
        let count = |capacity| {
            let canvas = drawn(&gauge(capacity, false), Area::FULL);
            (0..34)
                .flat_map(|y| (0..9).map(move |x| (x, y)))
                .filter(|(x, y)| canvas.get(*x, *y) == 255)
                .count()
        };
        assert_eq!(count(0), 0, "an empty battery drew a fill");
        assert!(
            count(50) > 0 && count(50) < count(100),
            "the fill does not grow"
        );
    }

    #[test]
    fn it_lies_down_when_cramped_and_stands_up_when_not() {
        // Lying down, the terminal is a notch on the right edge; standing up it
        // is a cap on the top row.
        let compact = drawn(
            &gauge(100, false),
            Area {
                top: 0,
                height: MIN_HEIGHT,
            },
        );
        let upright = drawn(
            &gauge(100, false),
            Area {
                top: 0,
                height: UPRIGHT_HEIGHT,
            },
        );

        assert!(compact.get(8, 2) > 0, "no terminal on the lying battery");
        assert!(upright.get(4, 0) > 0, "no cap on the upright battery");
    }

    #[test]
    fn nothing_is_drawn_outside_the_area() {
        let full = gauge(100, true);
        for height in MIN_HEIGHT..=34 {
            for top in [0, (34 - height) / 2, 34 - height] {
                let canvas = drawn(&full, Area { top, height });
                for y in 0..34 {
                    if y < top || y >= top + height {
                        for x in 0..9 {
                            assert_eq!(canvas.get(x, y), 0, "row {y} for {top}+{height}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn a_battery_that_cannot_be_read_shows_an_empty_case() {
        let mut gauge = BatteryGauge::new(ColorMode::Greyscale);
        gauge.reading = None;
        let canvas = drawn(&gauge, Area::FULL);

        assert_ne!(canvas, Canvas::new(), "no case at all");
        let filled = (0..34)
            .flat_map(|y| (0..9).map(move |x| (x, y)))
            .filter(|(x, y)| canvas.get(*x, *y) == 255)
            .count();
        assert_eq!(filled, 0, "an unreadable battery showed a level");
    }
}
