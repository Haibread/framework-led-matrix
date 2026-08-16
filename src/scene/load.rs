//! Processor or memory load, at whatever detail the band allows.
//!
//! One widget, two sources. Splitting them means a stack can carry the
//! processor without the memory, and `cpu,ram` reproduces exactly what the old
//! combined gauge drew — the stack does the composing, so this does not have to.

use std::collections::VecDeque;
use std::time::Duration;

use crate::canvas::{self, Canvas};
use crate::device::ColorMode;
use crate::font;
use crate::scene::{Area, Scene};
use crate::system::{self, CpuSample};

/// How often the counters are re-read.
///
/// Processor load is a difference between two samples, so this doubles as the
/// window it is averaged over: too short and the bar is pure noise.
const SAMPLE_INTERVAL: f32 = 1.0;

/// Rows for a bar and its rule: the least that still says something.
const BAR_HEIGHT: i32 = 3;
/// Rows from which the figure itself is drawn.
const NUMBER_HEIGHT: i32 = 6;
/// Rows from which the history is drawn instead.
const HISTORY_HEIGHT: i32 = 11;

/// Samples kept, one per column.
const HISTORY_LEN: usize = 9;
const _: () = assert!(HISTORY_LEN == 9 && canvas::WIDTH == 9);

const RULE_LEVEL: u8 = 30;

/// Which counter a gauge follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// Share of processor time spent working.
    Cpu,
    /// Share of memory in use.
    Memory,
}

impl Source {
    /// Name used in logs and on the socket.
    const fn label(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Memory => "ram",
        }
    }

    /// Brightness the bars are drawn at.
    ///
    /// The two are told apart by weight when they sit next to each other, which
    /// they usually do.
    const fn level(self) -> u8 {
        match self {
            Self::Cpu => 255,
            Self::Memory => 190,
        }
    }
}

/// A load gauge.
pub struct Load {
    mode: ColorMode,
    source: Source,
    previous: Option<CpuSample>,
    value: f32,
    history: VecDeque<f32>,
    since_sample: f32,
}

impl Load {
    /// Starts a gauge on `source`, taking a first reading straight away.
    #[must_use]
    pub fn new(source: Source, mode: ColorMode) -> Self {
        let mut load = Self {
            mode,
            source,
            previous: system::read_cpu(),
            value: 0.0,
            history: VecDeque::from(vec![0.0; HISTORY_LEN]),
            since_sample: 0.0,
        };
        if source == Source::Memory {
            load.value = system::read_memory().unwrap_or(0.0);
        }
        load
    }

    /// Re-reads the counter and pushes it onto the history.
    fn sample(&mut self) {
        match self.source {
            Source::Cpu => {
                if let Some(now) = system::read_cpu() {
                    if let Some(previous) = self.previous {
                        if let Some(busy) = now.busy_since(previous) {
                            self.value = busy;
                        }
                    }
                    self.previous = Some(now);
                }
            }
            Source::Memory => {
                if let Some(used) = system::read_memory() {
                    self.value = used;
                }
            }
        }

        self.history.push_back(self.value);
        while self.history.len() > HISTORY_LEN {
            self.history.pop_front();
        }
    }

    /// A bar filling from the left, with its rule underneath.
    fn draw_bar(&self, canvas: &mut Canvas, area: Area) {
        canvas.hline(0, canvas::WIDTH - 1, area.bottom(), RULE_LEVEL);
        let filled = canvas::to_pixel(self.value.clamp(0.0, 1.0) * 9.0);
        for x in 0..filled {
            for y in area.top..area.bottom() {
                canvas.set_max(x, y, self.source.level());
            }
        }
    }

    /// The percentage in figures, with a column gauge beside it.
    fn draw_number(&self, canvas: &mut Canvas, area: Area) {
        let percent = canvas::to_pixel(self.value.clamp(0.0, 1.0) * 100.0);
        let top = area.top + (area.height - font::GLYPH_HEIGHT).max(0) / 2;

        // A hundred needs three digits and there are only nine columns beside
        // the gauge, so a full load reads as "99": the bar says the rest.
        let shown = percent.min(99);
        let tens = u32::try_from(shown / 10).unwrap_or(0);
        let units = u32::try_from(shown % 10).unwrap_or(0);
        font::draw_digit(canvas, tens, 0, top, self.source.level());
        font::draw_digit(canvas, units, 4, top, self.source.level());

        let filled = canvas::to_pixel(
            self.value.clamp(0.0, 1.0) * f32::from(u8::try_from(area.height).unwrap_or(1)),
        );
        for step in 0..filled {
            canvas.set_max(8, area.bottom() - step, 200);
        }
    }

    /// The history, one column per sample, newest on the right.
    fn draw_history(&self, canvas: &mut Canvas, area: Area) {
        for (index, value) in self.history.iter().enumerate() {
            let column = i32::try_from(index).unwrap_or(0);
            let rows = f32::from(u8::try_from(area.height).unwrap_or(1));
            let filled = canvas::to_pixel(value.clamp(0.0, 1.0) * rows);
            let newest = index + 1 == self.history.len();

            for step in 0..filled {
                let level = if self.mode == ColorMode::Bw {
                    u8::MAX
                } else if step == filled - 1 {
                    // The tip carries the value; the body only shows where it
                    // came from.
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

impl Scene for Load {
    fn name(&self) -> &'static str {
        self.source.label()
    }

    fn min_height(&self) -> i32 {
        BAR_HEIGHT
    }

    fn update(&mut self, delta: Duration) {
        self.since_sample += delta.as_secs_f32();
        if self.since_sample >= SAMPLE_INTERVAL {
            self.since_sample = 0.0;
            self.sample();
        }
    }

    fn render(&self, canvas: &mut Canvas, area: Area) {
        if area.height >= HISTORY_HEIGHT {
            self.draw_history(canvas, area);
        } else if area.height >= NUMBER_HEIGHT {
            self.draw_number(canvas, area);
        } else {
            self.draw_bar(canvas, area);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BAR_HEIGHT, HISTORY_HEIGHT, Load, NUMBER_HEIGHT, Source};
    use crate::canvas::Canvas;
    use crate::device::ColorMode;
    use crate::scene::{Area, Scene};

    fn load(source: Source, value: f32) -> Load {
        let mut load = Load::new(source, ColorMode::Greyscale);
        load.value = value;
        load.history.clear();
        for _ in 0..super::HISTORY_LEN {
            load.history.push_back(value);
        }
        load
    }

    fn drawn(load: &Load, height: i32) -> Canvas {
        let mut canvas = Canvas::new();
        load.render(&mut canvas, Area { top: 0, height });
        canvas
    }

    #[test]
    fn the_two_sources_are_named_apart() {
        assert_eq!(load(Source::Cpu, 0.0).name(), "cpu");
        assert_eq!(load(Source::Memory, 0.0).name(), "ram");
    }

    #[test]
    fn the_detail_grows_with_the_band() {
        // Three renderings of the same number: a bar, then figures, then a
        // history. Each is distinguishable from the others by what it lights.
        let busy = load(Source::Cpu, 0.5);

        let bar = drawn(&busy, BAR_HEIGHT);
        let number = drawn(&busy, NUMBER_HEIGHT);
        let history = drawn(&busy, HISTORY_HEIGHT);

        // The bar fills from the left and stops halfway.
        assert!(bar.get(0, 0) > 0 && bar.get(8, 0) == 0);
        // The figures light the middle columns, which a half bar never reaches.
        assert!(number.get(4, NUMBER_HEIGHT / 2 - 1) > 0 || number.get(5, 2) > 0);
        // The history fills every column from the bottom.
        assert!(history.get(8, HISTORY_HEIGHT - 1) > 0, "no newest sample");
    }

    #[test]
    fn the_bar_tracks_the_value() {
        let width = |value| {
            let canvas = drawn(&load(Source::Cpu, value), BAR_HEIGHT);
            (0..9).filter(|x| canvas.get(*x, 0) > 0).count()
        };
        assert_eq!(width(0.0), 0);
        assert_eq!(width(1.0), 9);
        assert!(width(0.5) > 0 && width(0.5) < 9);
    }

    #[test]
    fn a_full_load_shows_ninety_nine_rather_than_overflowing() {
        // Three digits do not fit beside the gauge column; the bar carries the
        // difference between 99 and 100.
        let canvas = drawn(&load(Source::Cpu, 1.0), NUMBER_HEIGHT);
        let lit_columns = (0..9).filter(|x| (0..NUMBER_HEIGHT).any(|y| canvas.get(*x, y) > 0));
        assert!(lit_columns.count() <= 9, "the figures ran off the panel");
    }

    #[test]
    fn nothing_is_drawn_outside_the_area() {
        let busy = load(Source::Memory, 1.0);
        for height in BAR_HEIGHT..=34 {
            for top in [0, (34 - height) / 2, 34 - height] {
                let mut canvas = Canvas::new();
                busy.render(&mut canvas, Area { top, height });
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
        // A counter going backwards over a suspend can produce these.
        let canvas = drawn(&load(Source::Cpu, 5.0), HISTORY_HEIGHT);
        assert!(canvas.get(0, 0) > 0, "500% did not fill the band");

        let empty = drawn(&load(Source::Cpu, -1.0), HISTORY_HEIGHT);
        assert_eq!(empty, Canvas::new(), "a negative reading drew something");
    }
}
