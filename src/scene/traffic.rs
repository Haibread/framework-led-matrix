//! Bytes moving in and out, for the network or the disk.
//!
//! One widget, two sources, one grammar: out above the rule, in below, nine
//! samples of history. Two things to watch, one thing to learn.

use std::collections::VecDeque;
use std::time::Duration;

use crate::canvas::{self, Canvas};
use crate::device::ColorMode;
use crate::scene::{Area, Scene};
use crate::system::{self, Counters};

/// How often the counters are re-read, and so the width of one column.
const SAMPLE_INTERVAL: f32 = 1.0;

/// Rows for two three-deep histories either side of a rule.
const MIN_HEIGHT: i32 = 7;

/// Samples kept, one per column.
const HISTORY_LEN: usize = 9;
const _: () = assert!(HISTORY_LEN == 9 && canvas::WIDTH == 9);

/// Rate at which a bar is empty, in bytes a second.
///
/// Below this there is always some background chatter, and drawing it would
/// leave the widget permanently half lit.
const FLOOR: f64 = 1_024.0;
/// Rate at which a bar is full, in bytes a second.
///
/// A gigabyte a second is past what either link does in practice, which is the
/// point: the scale has to hold a burst without clipping every day.
const CEILING: f64 = 1_000_000_000.0;

const RULE_LEVEL: u8 = 45;
const BODY_LEVEL: u8 = 130;

/// Which counters a widget follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// Bytes over the network interfaces.
    Network,
    /// Bytes to and from the disks.
    Disk,
}

impl Source {
    /// Name used in logs and on the socket.
    const fn label(self) -> &'static str {
        match self {
            Self::Network => "net",
            Self::Disk => "disk",
        }
    }

    /// Reads this source's counters.
    fn read(self) -> Option<Counters> {
        match self {
            Self::Network => system::read_network(),
            Self::Disk => system::read_disk(),
        }
    }
}

/// A traffic gauge.
pub struct Traffic {
    mode: ColorMode,
    source: Source,
    previous: Option<Counters>,
    /// Outgoing, then incoming, as shares of the scale.
    out_history: VecDeque<f32>,
    in_history: VecDeque<f32>,
    since_sample: f32,
}

impl Traffic {
    /// Starts a gauge on `source`, taking a first reading straight away.
    #[must_use]
    pub fn new(source: Source, mode: ColorMode) -> Self {
        Self {
            mode,
            source,
            previous: source.read(),
            out_history: VecDeque::from(vec![0.0; HISTORY_LEN]),
            in_history: VecDeque::from(vec![0.0; HISTORY_LEN]),
            since_sample: 0.0,
        }
    }

    /// Re-reads the counters and pushes the rates onto the histories.
    fn sample(&mut self, seconds: f32) {
        let Some(now) = self.source.read() else {
            return;
        };
        let Some(previous) = self.previous else {
            self.previous = Some(now);
            return;
        };
        self.previous = Some(now);

        let (out, incoming) = now.rates_since(previous, f64::from(seconds));
        push(&mut self.out_history, scale(out));
        push(&mut self.in_history, scale(incoming));
    }

    /// Draws one history growing away from the rule.
    fn draw_history(
        &self,
        canvas: &mut Canvas,
        history: &VecDeque<f32>,
        rule: i32,
        rows: i32,
        up: bool,
    ) {
        if rows <= 0 {
            return;
        }
        for (index, value) in history.iter().enumerate() {
            let column = i32::try_from(index).unwrap_or(0);
            let filled = canvas::to_pixel(
                value.clamp(0.0, 1.0) * f32::from(u8::try_from(rows).unwrap_or(1)),
            );
            for step in 0..filled {
                let y = if up { rule - 1 - step } else { rule + 1 + step };
                let level = if self.mode == ColorMode::Bw {
                    u8::MAX
                } else if step == filled - 1 {
                    // The peak is what carries the number.
                    u8::MAX
                } else {
                    BODY_LEVEL
                };
                canvas.set_max(column, y, level);
            }
        }
    }
}

/// Pushes a sample, dropping the oldest.
fn push(history: &mut VecDeque<f32>, value: f32) {
    history.push_back(value);
    while history.len() > HISTORY_LEN {
        history.pop_front();
    }
}

/// Maps a byte rate onto `0.0..=1.0`, logarithmically.
///
/// Traffic spans six orders of magnitude between a keepalive and a download; on
/// a linear scale everything but the peak would be one row tall.
#[must_use]
pub fn scale(bytes_per_second: f64) -> f32 {
    if bytes_per_second <= FLOOR {
        return 0.0;
    }
    let span = (CEILING / FLOOR).log10();
    let value = (bytes_per_second / FLOOR).log10() / span;
    // The cast is of a value already clamped to 0..=1.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "clamped to the unit range before narrowing"
    )]
    {
        value.clamp(0.0, 1.0) as f32
    }
}

impl Scene for Traffic {
    fn name(&self) -> &'static str {
        self.source.label()
    }

    fn min_height(&self) -> i32 {
        MIN_HEIGHT
    }

    fn update(&mut self, delta: Duration) {
        self.since_sample += delta.as_secs_f32();
        if self.since_sample >= SAMPLE_INTERVAL {
            let elapsed = self.since_sample;
            self.since_sample = 0.0;
            self.sample(elapsed);
        }
    }

    fn render(&self, canvas: &mut Canvas, area: Area) {
        // The rule sits in the middle, and each history gets what is left on
        // its side: an odd band gives the extra row to the incoming half,
        // which is the one that usually has something to show.
        let rule = area.top + (area.height - 1) / 2;
        canvas.hline(0, canvas::WIDTH - 1, rule, RULE_LEVEL);

        self.draw_history(canvas, &self.out_history, rule, rule - area.top, true);
        self.draw_history(canvas, &self.in_history, rule, area.bottom() - rule, false);
    }
}

#[cfg(test)]
mod tests {
    use super::{MIN_HEIGHT, Source, Traffic, scale};
    use crate::canvas::Canvas;
    use crate::device::ColorMode;
    use crate::scene::{Area, Scene};

    fn traffic(out: f32, incoming: f32) -> Traffic {
        let mut traffic = Traffic::new(Source::Network, ColorMode::Greyscale);
        for (history, value) in [
            (&mut traffic.out_history, out),
            (&mut traffic.in_history, incoming),
        ] {
            history.clear();
            for _ in 0..super::HISTORY_LEN {
                history.push_back(value);
            }
        }
        traffic
    }

    fn drawn(traffic: &Traffic, area: Area) -> Canvas {
        let mut canvas = Canvas::new();
        traffic.render(&mut canvas, area);
        canvas
    }

    #[test]
    fn the_two_sources_are_named_apart() {
        assert_eq!(Traffic::new(Source::Network, ColorMode::Bw).name(), "net");
        assert_eq!(Traffic::new(Source::Disk, ColorMode::Bw).name(), "disk");
    }

    #[test]
    fn outgoing_goes_above_the_rule_and_incoming_below() {
        let area = Area { top: 0, height: 15 };
        let up_only = drawn(&traffic(1.0, 0.0), area);
        let down_only = drawn(&traffic(0.0, 1.0), area);

        assert!(up_only.get(0, 0) > 0, "nothing at the top for outgoing");
        assert_eq!(up_only.get(0, 14), 0, "outgoing leaked below the rule");
        assert!(
            down_only.get(0, 14) > 0,
            "nothing at the bottom for incoming"
        );
        assert_eq!(down_only.get(0, 0), 0, "incoming leaked above the rule");
    }

    #[test]
    fn a_silent_link_draws_only_the_rule() {
        let canvas = drawn(&traffic(0.0, 0.0), Area { top: 0, height: 15 });
        let lit = (0..9)
            .flat_map(|x| (0..34).map(move |y| (x, y)))
            .filter(|(x, y)| canvas.get(*x, *y) > 0)
            .count();
        // The rule spans the width, and nothing else is drawn.
        assert_eq!(lit, 9, "a quiet link drew {lit} pixels");
    }

    #[test]
    fn nothing_is_drawn_outside_the_area() {
        let busy = traffic(1.0, 1.0);
        for height in MIN_HEIGHT..=34 {
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
    fn the_scale_is_logarithmic_and_bounded() {
        // Six orders of magnitude have to fit in a handful of rows, so equal
        // ratios must take equal space.
        assert!(scale(0.0) < f32::EPSILON, "silence is not empty");
        assert!(
            scale(500.0) < f32::EPSILON,
            "background chatter lit the bar"
        );
        assert!(
            (scale(1e12) - 1.0).abs() < f32::EPSILON,
            "the scale did not clamp"
        );

        // Three consecutive decades must be three equal steps.
        let low = scale(100_000.0);
        let middle = scale(1_000_000.0);
        let high = scale(10_000_000.0);
        assert!(
            ((middle - low) - (high - middle)).abs() < 0.01,
            "a factor of ten is not a constant step: {low} {middle} {high}"
        );
    }

    #[test]
    fn a_burst_reads_taller_than_a_trickle() {
        let area = Area { top: 0, height: 15 };
        let height = |value| {
            let canvas = drawn(&traffic(value, 0.0), area);
            (0..15).filter(|y| canvas.get(0, *y) > 0).count()
        };
        assert!(height(scale(50_000_000.0)) > height(scale(50_000.0)));
    }
}
