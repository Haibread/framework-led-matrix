//! Processor and memory load: two bars when cramped, two histories when not.

use std::collections::VecDeque;
use std::time::Duration;

use crate::canvas::{self, Canvas};
use crate::device::ColorMode;
use crate::scene::{Area, Scene};
use crate::system::{self, CpuSample};

/// How often the counters are re-read.
///
/// Processor load is a difference between two samples, so this doubles as the
/// window it is averaged over: too short and the bar is pure noise.
const SAMPLE_INTERVAL: f32 = 1.0;

/// Rows for the compact form: two bars of two rows, with a rule each.
const COMPACT_HEIGHT: i32 = 7;
/// Rows from which the history is drawn instead.
const HISTORY_HEIGHT: i32 = 16;

/// Samples kept, one per column.
const HISTORY_LEN: usize = 9;
const _: () = assert!(HISTORY_LEN == 9 && canvas::WIDTH == 9);

/// The rule under a bar, and the line between the two histories.
const RULE_LEVEL: u8 = 30;

/// Processor and memory gauges.
pub struct Gauges {
    mode: ColorMode,
    previous: Option<CpuSample>,
    cpu: f32,
    memory: f32,
    cpu_history: VecDeque<f32>,
    memory_history: VecDeque<f32>,
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
            cpu_history: VecDeque::from(vec![0.0; HISTORY_LEN]),
            memory_history: VecDeque::from(vec![0.0; HISTORY_LEN]),
            since_sample: 0.0,
        }
    }

    /// Re-reads the counters and pushes them onto the histories.
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

        for (history, value) in [
            (&mut self.cpu_history, self.cpu),
            (&mut self.memory_history, self.memory),
        ] {
            history.push_back(value);
            while history.len() > HISTORY_LEN {
                history.pop_front();
            }
        }
    }

    /// A load as a bar filling from the left, with its rule underneath.
    fn draw_bar(canvas: &mut Canvas, area: Area, value: f32, level: u8) {
        canvas.hline(0, canvas::WIDTH - 1, area.bottom(), RULE_LEVEL);
        let filled = canvas::to_pixel(value.clamp(0.0, 1.0) * 9.0);
        for x in 0..filled {
            for y in area.top..area.bottom() {
                canvas.set_max(x, y, level);
            }
        }
    }

    /// A history as one column per sample, newest on the right.
    fn draw_history(&self, canvas: &mut Canvas, area: Area, history: &VecDeque<f32>) {
        for (x, value) in history.iter().enumerate() {
            let column = i32::try_from(x).unwrap_or(0);
            let filled = canvas::to_pixel(
                value.clamp(0.0, 1.0) * f32::from(u8::try_from(area.height).unwrap_or(1)),
            );
            let newest = x + 1 == history.len();
            for step in 0..filled {
                let level = if self.mode == ColorMode::Bw {
                    u8::MAX
                } else if step == filled - 1 {
                    // The tip carries the value; the body is only there to
                    // show where it came from.
                    if newest { 255 } else { 200 }
                } else if newest {
                    150
                } else {
                    90
                };
                canvas.set_max(column, area.bottom() - step, level);
            }
        }
    }
}

impl Scene for Gauges {
    fn name(&self) -> &'static str {
        "gauges"
    }

    fn min_height(&self) -> i32 {
        COMPACT_HEIGHT
    }

    fn update(&mut self, delta: Duration) {
        self.since_sample += delta.as_secs_f32();
        if self.since_sample >= SAMPLE_INTERVAL {
            self.since_sample = 0.0;
            self.sample();
        }
    }

    fn render(&self, canvas: &mut Canvas, area: Area) {
        if area.height < HISTORY_HEIGHT {
            // Two bars, processor over memory, sharing the rows evenly.
            let half = area.height / 2;
            Self::draw_bar(canvas, area.take(half), self.cpu, 255);
            Self::draw_bar(
                canvas,
                area.skip(area.height - half).take(half),
                self.memory,
                190,
            );
            return;
        }

        let half = (area.height - 1) / 2;
        self.draw_history(canvas, area.take(half), &self.cpu_history);
        canvas.hline(0, canvas::WIDTH - 1, area.row(half), RULE_LEVEL);
        self.draw_history(
            canvas,
            Area {
                top: area.row(half + 1),
                height: area.height - half - 1,
            },
            &self.memory_history,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{COMPACT_HEIGHT, Gauges, HISTORY_HEIGHT};
    use crate::canvas::Canvas;
    use crate::device::ColorMode;
    use crate::scene::{Area, Scene};

    fn gauges(cpu: f32, memory: f32) -> Gauges {
        let mut gauges = Gauges::new(ColorMode::Greyscale);
        gauges.cpu = cpu;
        gauges.memory = memory;
        for (history, value) in [
            (&mut gauges.cpu_history, cpu),
            (&mut gauges.memory_history, memory),
        ] {
            history.clear();
            for _ in 0..super::HISTORY_LEN {
                history.push_back(value);
            }
        }
        gauges
    }

    fn drawn(gauges: &Gauges, area: Area) -> Canvas {
        let mut canvas = Canvas::new();
        gauges.render(&mut canvas, area);
        canvas
    }

    #[test]
    fn the_compact_form_gives_each_number_its_own_bar() {
        let canvas = drawn(
            &gauges(0.25, 0.75),
            Area {
                top: 0,
                height: COMPACT_HEIGHT,
            },
        );
        let width = |row: i32| (0..9).filter(|x| canvas.get(*x, row) > 0).count();
        // Processor on top, memory below; the wider bar is the larger number.
        assert!(
            width(0) < width(4),
            "cpu {} vs memory {}",
            width(0),
            width(4)
        );
    }

    #[test]
    fn a_taller_area_gets_the_history_instead() {
        // The compact form fills from the left, the history from the bottom;
        // a full-height left column is what tells them apart.
        let busy = gauges(1.0, 1.0);
        let compact = drawn(
            &busy,
            Area {
                top: 0,
                height: COMPACT_HEIGHT,
            },
        );
        let tall = drawn(
            &busy,
            Area {
                top: 0,
                height: HISTORY_HEIGHT,
            },
        );

        let column =
            |canvas: &Canvas, height: i32| (0..height).filter(|y| canvas.get(0, *y) > 0).count();
        assert!(column(&compact, COMPACT_HEIGHT) < COMPACT_HEIGHT as usize);
        assert!(column(&tall, HISTORY_HEIGHT) > COMPACT_HEIGHT as usize);
    }

    #[test]
    fn nothing_is_drawn_outside_the_area() {
        let busy = gauges(1.0, 1.0);
        for height in COMPACT_HEIGHT..=34 {
            for top in [0, (34 - height) / 2, 34 - height] {
                let canvas = drawn(&busy, Area { top, height });
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
    fn out_of_range_readings_are_clamped_rather_than_wrapped() {
        // A counter that goes backwards over a suspend can produce these; a
        // wrap would paint a full bar for a frame, which reads as a spike.
        let canvas = drawn(&gauges(5.0, -1.0), Area::FULL);
        assert!(canvas.get(0, 0) > 0, "500% did not fill its half");
        assert_eq!(canvas.get(0, 33), 0, "a negative reading drew something");
    }

    #[test]
    fn an_idle_machine_draws_almost_nothing() {
        let canvas = drawn(
            &gauges(0.0, 0.0),
            Area {
                top: 0,
                height: COMPACT_HEIGHT,
            },
        );
        let filled = (0..9)
            .flat_map(|x| (0..COMPACT_HEIGHT).map(move |y| (x, y)))
            .filter(|(x, y)| canvas.get(*x, *y) == 255)
            .count();
        assert_eq!(filled, 0, "an idle machine lit a full bar");
    }
}
