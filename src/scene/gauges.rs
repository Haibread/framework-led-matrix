//! Processor and memory load, as two columns filling from the bottom.

use std::time::Duration;

use crate::canvas::{self, Canvas};
use crate::device::ColorMode;
use crate::scene::Scene;
use crate::system::{self, CpuSample};

/// How often the counters are re-read.
///
/// Processor load is a difference between two samples, so this doubles as the
/// window it is averaged over: too short and the bar is pure noise.
const SAMPLE_INTERVAL: f32 = 1.0;

/// Columns each bar occupies, either side of a one-pixel gutter.
const CPU_COLUMNS: [i32; 4] = [0, 1, 2, 3];
const MEMORY_COLUMNS: [i32; 4] = [5, 6, 7, 8];
const GUTTER: i32 = 4;

/// Rows the bars span, bottom inclusive.
const BOTTOM: i32 = 33;
const HEIGHT: i32 = 34;
/// The same height as a float, so filling a bar needs no cast.
const HEIGHT_F: f32 = 34.0;
const _: () = assert!(HEIGHT == 34 && crate::canvas::HEIGHT == 34);

/// Brightest at the bottom of a bar, dimmest at the top.
const FILL_BRIGHTEST: u32 = 255;
const FILL_DIMMEST: u32 = 70;
/// The quarter marks drawn down the gutter.
const TICK_LEVEL: u8 = 30;

/// Processor and memory gauges.
pub struct Gauges {
    mode: ColorMode,
    previous: Option<CpuSample>,
    cpu: f32,
    memory: f32,
    since_sample: f32,
}

impl Gauges {
    /// Starts the gauges, taking a first reading straight away.
    #[must_use]
    pub fn new(mode: ColorMode) -> Self {
        Self {
            mode,
            previous: system::read_cpu(),
            cpu: 0.0,
            memory: system::read_memory().unwrap_or(0.0),
            since_sample: 0.0,
        }
    }

    /// Re-reads the counters.
    fn sample(&mut self) {
        if let Some(now) = system::read_cpu() {
            if let Some(previous) = self.previous {
                if let Some(busy) = now.busy_since(previous) {
                    self.cpu = busy;
                }
            }
            self.previous = Some(now);
        }
        if let Some(used) = system::read_memory() {
            self.memory = used;
        }
    }

    /// Draws one bar filled to `fraction` of its height.
    fn draw_bar(&self, canvas: &mut Canvas, columns: [i32; 4], fraction: f32) {
        let filled = canvas::to_pixel(fraction.clamp(0.0, 1.0) * HEIGHT_F);

        for step in 0..filled {
            let y = BOTTOM - step;
            let level = if self.mode == ColorMode::Bw {
                u8::MAX
            } else {
                // A gradient up the bar, so the height reads at a glance even
                // when the panel is at the edge of vision.
                fade(step, filled)
            };
            for x in columns {
                canvas.set_max(x, y, level);
            }
        }
    }
}

impl Scene for Gauges {
    fn name(&self) -> &'static str {
        "gauges"
    }

    fn update(&mut self, delta: Duration) {
        self.since_sample += delta.as_secs_f32();
        if self.since_sample >= SAMPLE_INTERVAL {
            self.since_sample = 0.0;
            self.sample();
        }
    }

    fn render(&self, canvas: &mut Canvas) {
        // Quarter marks, so a half-full bar is readable without counting.
        if self.mode == ColorMode::Greyscale {
            for quarter in 1..4 {
                canvas.set_max(GUTTER, BOTTOM - quarter * HEIGHT / 4, TICK_LEVEL);
            }
        }

        self.draw_bar(canvas, CPU_COLUMNS, self.cpu);
        self.draw_bar(canvas, MEMORY_COLUMNS, self.memory);
    }
}

/// Brightness of a bar pixel `step` rows above the bottom of a `filled` bar.
fn fade(step: i32, filled: i32) -> u8 {
    let filled = u32::try_from(filled.max(1)).unwrap_or(1);
    let step = u32::try_from(step).unwrap_or(0).min(filled);
    let span = FILL_BRIGHTEST - FILL_DIMMEST;
    let level = FILL_BRIGHTEST - span * step / filled;
    u8::try_from(level).unwrap_or(u8::MAX)
}

#[cfg(test)]
mod tests {
    use super::{BOTTOM, CPU_COLUMNS, Gauges, MEMORY_COLUMNS, fade};
    use crate::canvas::Canvas;
    use crate::device::ColorMode;
    use crate::scene::Scene;

    fn gauges(cpu: f32, memory: f32, mode: ColorMode) -> Gauges {
        let mut gauges = Gauges::new(mode);
        gauges.cpu = cpu;
        gauges.memory = memory;
        gauges
    }

    fn height(canvas: &Canvas, column: i32) -> usize {
        (0..34).filter(|y| canvas.get(column, *y) > 0).count()
    }

    #[test]
    fn the_bars_fill_from_the_bottom() {
        let mut canvas = Canvas::new();
        gauges(0.5, 0.5, ColorMode::Greyscale).render(&mut canvas);

        assert!(
            canvas.get(CPU_COLUMNS[0], BOTTOM) > 0,
            "empty at the bottom"
        );
        assert_eq!(canvas.get(CPU_COLUMNS[0], 0), 0, "full to the top at 50%");
    }

    #[test]
    fn each_bar_tracks_its_own_number() {
        let mut canvas = Canvas::new();
        gauges(0.25, 0.75, ColorMode::Greyscale).render(&mut canvas);

        let cpu = height(&canvas, CPU_COLUMNS[0]);
        let memory = height(&canvas, MEMORY_COLUMNS[0]);
        assert!(memory > cpu, "cpu {cpu}, memory {memory}");
        assert!((7..=11).contains(&cpu), "25% of 34 rows gave {cpu}");
        assert!((24..=27).contains(&memory), "75% of 34 rows gave {memory}");
    }

    #[test]
    fn an_idle_machine_draws_no_bar_and_a_busy_one_fills_it() {
        let mut idle = Canvas::new();
        gauges(0.0, 0.0, ColorMode::Bw).render(&mut idle);
        assert_eq!(height(&idle, CPU_COLUMNS[0]), 0);

        let mut busy = Canvas::new();
        gauges(1.0, 1.0, ColorMode::Bw).render(&mut busy);
        assert_eq!(height(&busy, CPU_COLUMNS[0]), 34, "a full bar fell short");
    }

    #[test]
    fn out_of_range_readings_cannot_overflow_the_panel() {
        let mut canvas = Canvas::new();
        gauges(5.0, -1.0, ColorMode::Greyscale).render(&mut canvas);
        assert_eq!(height(&canvas, CPU_COLUMNS[0]), 34);
        assert_eq!(height(&canvas, MEMORY_COLUMNS[0]), 0);
    }

    #[test]
    fn the_gutter_stays_clear_of_the_bars() {
        let mut canvas = Canvas::new();
        gauges(1.0, 1.0, ColorMode::Bw).render(&mut canvas);
        assert_eq!(height(&canvas, super::GUTTER), 0, "the bars ran together");
    }

    #[test]
    fn the_gradient_runs_bright_at_the_bottom_to_dim_at_the_top() {
        assert!(fade(0, 20) > fade(19, 20));
        assert_eq!(fade(0, 20), 255);
        assert!(fade(20, 20) >= 70);
        assert_eq!(fade(0, 0), 255, "an empty bar must not divide by zero");
    }

    #[test]
    fn black_and_white_drops_the_gradient_rather_than_thresholding_it_away() {
        let mut canvas = Canvas::new();
        gauges(1.0, 1.0, ColorMode::Bw).render(&mut canvas);
        for y in 0..34 {
            assert_eq!(canvas.get(CPU_COLUMNS[0], y), 255, "row {y} was dimmed");
        }
    }
}
