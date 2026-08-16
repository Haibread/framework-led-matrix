//! Charge level, drawn as a battery.
//!
//! An outline rather than a bare bar: on a panel with no labels, the shape is
//! what says which number you are looking at.

use std::time::Duration;

use crate::canvas::{self, Canvas};
use crate::device::ColorMode;
use crate::scene::Scene;
use crate::system::{self, Battery as Reading};

/// How often the charge is re-read. It moves slowly; polling faster would only
/// spin the disk cache.
const SAMPLE_INTERVAL: f32 = 5.0;

/// The terminal on top of the case.
const CAP_ROWS: [i32; 2] = [3, 4];
const CAP_COLUMNS: [i32; 3] = [3, 4, 5];

/// The case: an outline from `TOP` to `BOTTOM`, `LEFT` to `RIGHT` inclusive.
const TOP: i32 = 5;
const BOTTOM: i32 = 31;
const LEFT: i32 = 1;
const RIGHT: i32 = 7;

/// The fillable inside of the case.
const FILL_TOP: i32 = TOP + 1;
const FILL_BOTTOM: i32 = BOTTOM - 1;
const FILL_ROWS: i32 = FILL_BOTTOM - FILL_TOP + 1;
/// The same count as a float, for the charging wave.
const FILL_ROWS_F: f32 = 25.0;
const _: () = assert!(FILL_ROWS == 25);

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

    /// Draws the case, which is the same whatever the charge.
    fn draw_outline(canvas: &mut Canvas) {
        for column in CAP_COLUMNS {
            for row in CAP_ROWS {
                canvas.set_max(column, row, OUTLINE_LEVEL);
            }
        }

        canvas.hline(LEFT, RIGHT, TOP, OUTLINE_LEVEL);
        canvas.hline(LEFT, RIGHT, BOTTOM, OUTLINE_LEVEL);
        for row in TOP..=BOTTOM {
            canvas.set_max(LEFT, row, OUTLINE_LEVEL);
            canvas.set_max(RIGHT, row, OUTLINE_LEVEL);
        }
    }
}

impl Scene for BatteryGauge {
    fn name(&self) -> &'static str {
        "battery"
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

    fn render(&self, canvas: &mut Canvas) {
        Self::draw_outline(canvas);

        let Some(reading) = self.reading else {
            // No battery, or it could not be read: the empty case says so
            // rather than showing a confident zero.
            return;
        };

        let filled = i32::from(reading.capacity) * FILL_ROWS / 100;
        for step in 0..filled {
            let row = FILL_BOTTOM - step;
            canvas.hline(LEFT + 1, RIGHT - 1, row, FILL_LEVEL);
        }

        if !reading.charging {
            return;
        }

        // A wave climbing the fill, so charging reads as motion rather than as
        // a number you have to remember.
        let travel = (self.elapsed * CHARGE_WAVE_ROWS) % FILL_ROWS_F;
        let wave = FILL_BOTTOM - canvas::floor_pixel(travel);
        let level = if self.mode == ColorMode::Bw {
            0
        } else {
            OUTLINE_LEVEL
        };
        for x in (LEFT + 1)..RIGHT {
            canvas.set(x, wave, level);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BOTTOM, BatteryGauge, FILL_BOTTOM, FILL_ROWS, LEFT, RIGHT, TOP};
    use crate::canvas::Canvas;
    use crate::device::ColorMode;
    use crate::scene::Scene;
    use crate::system::Battery as Reading;

    fn gauge(capacity: u8, charging: bool) -> BatteryGauge {
        let mut gauge = BatteryGauge::new(ColorMode::Greyscale);
        gauge.reading = Some(Reading { capacity, charging });
        gauge
    }

    fn fill_height(canvas: &Canvas) -> i32 {
        (0..34)
            .filter(|y| canvas.get(LEFT + 1, *y) == 255)
            .count()
            .try_into()
            .unwrap_or(0)
    }

    #[test]
    fn the_case_is_drawn_whatever_the_charge() {
        let mut canvas = Canvas::new();
        gauge(0, false).render(&mut canvas);

        assert!(canvas.get(LEFT, TOP) > 0, "no top-left corner");
        assert!(canvas.get(RIGHT, BOTTOM) > 0, "no bottom-right corner");
        assert!(canvas.get(4, 3) > 0, "no terminal");
    }

    #[test]
    fn the_fill_tracks_the_charge() {
        let mut empty = Canvas::new();
        gauge(0, false).render(&mut empty);
        assert_eq!(fill_height(&empty), 0);

        let mut full = Canvas::new();
        gauge(100, false).render(&mut full);
        assert_eq!(fill_height(&full), FILL_ROWS);

        let mut half = Canvas::new();
        gauge(50, false).render(&mut half);
        let height = fill_height(&half);
        assert!(
            (FILL_ROWS / 2 - 1..=FILL_ROWS / 2 + 1).contains(&height),
            "50% gave {height} of {FILL_ROWS}"
        );
    }

    #[test]
    fn the_fill_grows_upwards_from_the_bottom_of_the_case() {
        let mut canvas = Canvas::new();
        gauge(20, false).render(&mut canvas);
        assert_eq!(
            canvas.get(LEFT + 1, FILL_BOTTOM),
            255,
            "not filled at the bottom"
        );
        assert_ne!(canvas.get(LEFT + 1, TOP + 1), 255, "20% reached the top");
    }

    #[test]
    fn the_fill_never_escapes_the_case() {
        let mut canvas = Canvas::new();
        gauge(100, false).render(&mut canvas);
        for y in 0..34 {
            assert_ne!(canvas.get(0, y), 255, "the fill spilled past the left wall");
            assert_ne!(
                canvas.get(8, y),
                255,
                "the fill spilled past the right wall"
            );
        }
        assert_eq!(canvas.get(LEFT + 1, BOTTOM), 90, "the fill ate the case");
    }

    #[test]
    fn a_battery_that_cannot_be_read_shows_an_empty_case_not_a_flat_zero() {
        let mut gauge = BatteryGauge::new(ColorMode::Greyscale);
        gauge.reading = None;

        let mut canvas = Canvas::new();
        gauge.render(&mut canvas);

        assert!(canvas.get(LEFT, TOP) > 0, "no case at all");
        assert_eq!(fill_height(&canvas), 0);
    }

    #[test]
    fn charging_animates_while_discharging_stays_still() {
        let mut moving = gauge(60, true);
        let mut seen = std::collections::HashSet::new();
        for _ in 0..30 {
            moving.update(std::time::Duration::from_millis(50));
            let mut canvas = Canvas::new();
            moving.render(&mut canvas);
            seen.insert((0..34).map(|y| canvas.get(4, y)).collect::<Vec<_>>());
        }
        assert!(seen.len() > 1, "the charging wave never moved");

        let mut still = gauge(60, false);
        let mut frames = std::collections::HashSet::new();
        for _ in 0..30 {
            still.update(std::time::Duration::from_millis(50));
            let mut canvas = Canvas::new();
            still.render(&mut canvas);
            frames.insert((0..34).map(|y| canvas.get(4, y)).collect::<Vec<_>>());
        }
        assert_eq!(frames.len(), 1, "a discharging battery flickered");
    }
}
