//! Output volume, read from `WirePlumber`.
//!
//! `wpctl` rather than a `PipeWire` binding: the answer is one line of text, and
//! a subprocess every half second is cheaper than carrying a client library
//! for a single number. It runs off the render thread either way.

use std::process::Command;
use std::time::Duration;

use crate::canvas::{self, Canvas};
use crate::device::ColorMode;
use crate::poller::Poller;
use crate::scene::{Area, Scene};

/// How often the volume is re-read.
///
/// Fast enough that turning the knob shows up while your hand is still on it.
const POLL_INTERVAL: Duration = Duration::from_millis(400);

/// Rows for the speaker and a bar beside it.
pub const MIN_HEIGHT: i32 = 5;
/// Rows from which the bar becomes a column of its own.
const TALL_HEIGHT: i32 = 12;
/// Past this the cone would be wider than the panel.
const MAX_SCALE: i32 = 3;

const SPEAKER_LEVEL: u8 = 200;
const MUTED_LEVEL: u8 = 60;
const RULE_LEVEL: u8 = 22;

/// What the sink is doing.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Reading {
    /// Level, 0.0 to 1.0. Volumes above 100% are clamped.
    pub level: f32,
    /// Whether output is muted.
    pub muted: bool,
}

/// Parses what `wpctl get-volume` prints.
///
/// The line is `Volume: 0.43` or `Volume: 0.43 [MUTED]`.
#[must_use]
pub fn parse(output: &str) -> Option<Reading> {
    let rest = output.split_once("Volume:")?.1;
    let level: f32 = rest.split_whitespace().next()?.parse().ok()?;
    Some(Reading {
        level: level.clamp(0.0, 1.0),
        muted: output.contains("[MUTED]"),
    })
}

/// Asks `WirePlumber` for the default sink's volume.
fn read() -> Option<Reading> {
    let output = Command::new("wpctl")
        .args(["get-volume", "@DEFAULT_AUDIO_SINK@"])
        .output()
        .ok()?;
    parse(&String::from_utf8_lossy(&output.stdout))
}

/// A volume gauge.
/// Nothing here is shaded — a speaker, a bar and a cross, all full brightness —
/// so unlike the other widgets this one draws the same in either colour mode.
pub struct Volume {
    poller: Poller<Reading>,
}

impl Volume {
    /// Starts the gauge and its reader.
    #[must_use]
    pub fn new(_mode: ColorMode) -> Self {
        Self {
            // Read on the poller's thread rather than here: building a scene
            // happens inside the async runtime, and this one is called from
            // there too.
            poller: Poller::spawn("volume", Reading::default(), POLL_INTERVAL, read),
        }
    }

    /// The speaker cone, three pixels wide before scaling, growing to the right.
    ///
    /// Each pixel of the shape becomes a `scale` by `scale` block, so the cone
    /// grows with the room it is given like every other widget here. Drawn at
    /// one size on a whole panel it was a pictogram lost in the dark.
    fn draw_speaker(canvas: &mut Canvas, x: i32, top: i32, level: u8, scale: i32) {
        for (row, bits) in [0b001, 0b011, 0b111, 0b011, 0b001].into_iter().enumerate() {
            for column in 0..3 {
                if bits & (1 << column) != 0 {
                    let row = i32::try_from(row).unwrap_or(0);
                    fill(canvas, x + column * scale, top + row * scale, scale, level);
                }
            }
        }
    }
}

impl Scene for Volume {
    fn name(&self) -> &'static str {
        "volume"
    }

    fn min_height(&self) -> i32 {
        MIN_HEIGHT
    }

    fn update(&mut self, _delta: Duration) {}

    fn render(&self, canvas: &mut Canvas, area: Area) {
        let reading = self.poller.latest();
        let speaker_level = if reading.muted {
            MUTED_LEVEL
        } else {
            SPEAKER_LEVEL
        };

        if area.height >= TALL_HEIGHT {
            // The speaker sits at the top, as large as the band allows, over a
            // full-width bar taking whatever is left.
            let scale = (area.height / TALL_HEIGHT).clamp(1, MAX_SCALE);
            let width = 3 * scale;
            let x = (canvas::WIDTH - width) / 2;
            let top = area.top + 1;
            Self::draw_speaker(canvas, x, top, speaker_level, scale);

            if reading.muted {
                draw_cross(canvas, x, top, scale);
                return;
            }

            let bar_top = top + MIN_HEIGHT * scale + 1;
            let rows = (area.bottom() - bar_top + 1).max(0);
            let filled =
                canvas::to_pixel(reading.level * f32::from(u8::try_from(rows).unwrap_or(0)));
            for step in 0..filled {
                canvas.hline(0, canvas::WIDTH - 1, area.bottom() - step, u8::MAX);
            }
            return;
        }

        let top = area.top + (area.height - MIN_HEIGHT).max(0) / 2;
        Self::draw_speaker(canvas, 0, top, speaker_level, 1);

        if reading.muted {
            // A cross beside the cone, because a bar at zero and a muted sink
            // look identical otherwise.
            draw_cross(canvas, 4, top, 1);
            return;
        }

        canvas.hline(4, canvas::WIDTH - 1, top + 4, RULE_LEVEL);
        let filled = canvas::to_pixel(reading.level * 5.0);
        for step in 0..filled {
            canvas.set_max(4 + step, top + 1, u8::MAX);
            canvas.set_max(4 + step, top + 2, u8::MAX);
        }
    }
}

/// A three-by-three cross, scaled to match the cone it is drawn over.
fn draw_cross(canvas: &mut Canvas, x: i32, top: i32, scale: i32) {
    for step in 0..3 {
        let row = top + (1 + step) * scale;
        fill(canvas, x + step * scale, row, scale, u8::MAX);
        fill(canvas, x + (2 - step) * scale, row, scale, u8::MAX);
    }
}

/// One pixel of a shape, as a `scale` by `scale` block.
fn fill(canvas: &mut Canvas, x: i32, y: i32, scale: i32, level: u8) {
    for row in 0..scale {
        for column in 0..scale {
            canvas.set_max(x + column, y + row, level);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MIN_HEIGHT, Reading, TALL_HEIGHT, Volume, parse};
    use crate::canvas::Canvas;
    use crate::scene::{Area, Scene};

    /// A gauge showing a fixed reading, rather than whatever this machine is
    /// doing while the tests run.
    fn showing(reading: Reading) -> Volume {
        Volume {
            poller: crate::poller::Poller::spawn(
                "test",
                reading,
                std::time::Duration::from_secs(3600),
                || None,
            ),
        }
    }

    fn drawn(reading: Reading, height: i32) -> Canvas {
        let mut canvas = Canvas::new();
        showing(reading).render(&mut canvas, Area { top: 0, height });
        canvas
    }

    #[test]
    fn the_speaker_grows_with_the_panel_it_is_on() {
        // Muted on a whole panel drew a three-by-five pictogram and nothing
        // else: a small square adrift in the dark, which is exactly what it
        // was reported as.
        let muted = Reading {
            level: 0.0,
            muted: true,
        };
        let lit = |height| {
            let canvas = drawn(muted, height);
            (0..9)
                .flat_map(|x| (0..34).map(move |y| (x, y)))
                .filter(|(x, y)| canvas.get(*x, *y) > 0)
                .count()
        };
        assert!(
            lit(34) > lit(TALL_HEIGHT) * 3,
            "a whole panel drew barely more than a band: {} against {}",
            lit(34),
            lit(TALL_HEIGHT)
        );
    }

    #[test]
    fn the_line_wpctl_prints_is_understood() {
        assert_eq!(
            parse("Volume: 0.43\n"),
            Some(Reading {
                level: 0.43,
                muted: false
            })
        );
        assert_eq!(
            parse("Volume: 0.00 [MUTED]\n"),
            Some(Reading {
                level: 0.0,
                muted: true
            })
        );
    }

    #[test]
    fn a_volume_above_a_hundred_percent_is_clamped() {
        // wpctl happily reports 1.4 when something has boosted the sink.
        assert!((parse("Volume: 1.40\n").expect("parse").level - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn nonsense_is_refused_rather_than_guessed() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("Volume:\n"), None);
        assert_eq!(parse("Volume: loud\n"), None);
    }

    #[test]
    fn muting_is_not_the_same_as_turning_it_down() {
        // Both leave the bar empty, so the cross is the only thing that says
        // which of the two happened.
        let quiet = drawn(
            Reading {
                level: 0.0,
                muted: false,
            },
            MIN_HEIGHT,
        );
        let muted = drawn(
            Reading {
                level: 0.0,
                muted: true,
            },
            MIN_HEIGHT,
        );
        assert_ne!(quiet, muted, "a muted sink looks like a quiet one");
    }

    #[test]
    fn the_bar_tracks_the_level() {
        let width = |level| {
            let canvas = drawn(
                Reading {
                    level,
                    muted: false,
                },
                MIN_HEIGHT,
            );
            (4..9).filter(|x| canvas.get(*x, 1) > 0).count()
        };
        assert_eq!(width(0.0), 0);
        assert_eq!(width(1.0), 5);
        assert!(width(0.5) > 0 && width(0.5) < 5);
    }

    #[test]
    fn nothing_is_drawn_outside_the_area() {
        for height in MIN_HEIGHT..=34 {
            for top in [0, (34 - height) / 2, 34 - height] {
                let scene = showing(Reading {
                    level: 1.0,
                    muted: false,
                });
                let mut canvas = Canvas::new();
                scene.render(&mut canvas, Area { top, height });
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
    fn a_taller_band_gets_the_wide_bar() {
        let short = drawn(
            Reading {
                level: 1.0,
                muted: false,
            },
            MIN_HEIGHT,
        );
        let tall = drawn(
            Reading {
                level: 1.0,
                muted: false,
            },
            TALL_HEIGHT,
        );
        let lit = |canvas: &Canvas| {
            (0..9)
                .flat_map(|x| (0..34).map(move |y| (x, y)))
                .filter(|(x, y)| canvas.get(*x, *y) > 0)
                .count()
        };
        assert!(lit(&tall) > lit(&short), "the tall form drew no more");
    }
}
