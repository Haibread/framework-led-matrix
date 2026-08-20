//! Nine bands of whatever is coming out of the speakers.
//!
//! The one scene that earns the frame rate: everything else here changes a few
//! times a minute, and this changes with the music.

use std::time::Duration;

use crate::audio::{BANDS, Listener, Source};
use crate::canvas::{self, Canvas};
use crate::device::ColorMode;
use crate::scene::{Area, Scene};

/// Rows for bars worth looking at.
pub const MIN_HEIGHT: i32 = 8;

/// Brightness of the body of a bar, under its peak.
const BODY_LEVEL: u8 = 110;
/// The always-lit floor the bars stand on.
const FLOOR_LEVEL: u8 = 60;
/// How far a peak marker falls per second, as a share of the panel.
const PEAK_FALL: f32 = 0.55;

/// An audio spectrum.
pub struct Spectrum {
    mode: ColorMode,
    source: Source,
    listener: Listener,
    /// The highest each band has reached lately, so a peak lingers.
    peaks: [f32; BANDS],
}

impl Spectrum {
    /// Starts the widget and its capture.
    #[must_use]
    pub fn new(source: Source, mode: ColorMode) -> Self {
        Self {
            mode,
            source,
            listener: Listener::start(source),
            peaks: [0.0; BANDS],
        }
    }
}

impl Scene for Spectrum {
    fn name(&self) -> &'static str {
        match self.source {
            Source::Output => "speakers-spectrum",
            Source::Input => "mic-spectrum",
        }
    }

    fn min_height(&self) -> i32 {
        MIN_HEIGHT
    }

    fn update(&mut self, delta: Duration) {
        let bands = self.listener.bands();
        let fall = PEAK_FALL * delta.as_secs_f32();

        for (peak, level) in self.peaks.iter_mut().zip(bands) {
            // A peak that only ever rose would stick to the ceiling; one that
            // fell with the bar would not be a peak at all.
            *peak = if level >= *peak {
                level
            } else {
                (*peak - fall).max(level)
            };
        }
    }

    fn render(&self, canvas: &mut Canvas, area: Area) {
        let bands = self.listener.bands();
        let rows = f32::from(u8::try_from(area.height).unwrap_or(1));

        for (index, level) in bands.iter().enumerate() {
            let column = i32::try_from(index).unwrap_or(0);
            let filled = canvas::to_pixel(level.clamp(0.0, 1.0) * rows);

            // The bars stand on a floor that is always lit. In silence every
            // band is zero and the panel went completely dark, which looks
            // exactly like a scene that is broken rather than one with nothing
            // to say — and silence is the normal state of a speaker.
            canvas.set_max(column, area.bottom(), FLOOR_LEVEL);

            for step in 0..filled {
                let level = if self.mode == ColorMode::Bw || step + 1 == filled {
                    // The top pixel is the value; the rest is where it came
                    // from, which matters less.
                    u8::MAX
                } else {
                    BODY_LEVEL
                };
                canvas.set_max(column, area.bottom() - step, level);
            }

            // The peak marker rides above the bar and sinks back onto it.
            let peak = canvas::to_pixel(self.peaks[index].clamp(0.0, 1.0) * rows);
            if peak > filled {
                canvas.set_max(column, area.bottom() - (peak - 1).max(0), u8::MAX);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MIN_HEIGHT, Spectrum};
    use crate::audio::BANDS;
    use crate::canvas::Canvas;
    use crate::device::ColorMode;
    use crate::scene::{Area, Scene};
    use std::time::Duration;

    /// A spectrum whose capture is ignored, so the tests set the levels.
    fn showing(peaks: [f32; BANDS]) -> Spectrum {
        let mut spectrum = Spectrum::new(super::Source::Output, ColorMode::Greyscale);
        spectrum.peaks = peaks;
        spectrum
    }

    #[test]
    fn a_peak_sinks_back_towards_the_bar() {
        let mut spectrum = showing([1.0; BANDS]);
        spectrum.update(Duration::from_millis(500));
        assert!(spectrum.peaks[0] < 1.0, "the peak stuck to the ceiling");
        assert!(spectrum.peaks[0] > 0.0, "the peak fell straight down");
    }

    #[test]
    fn a_peak_never_falls_below_the_bar_under_it() {
        let mut spectrum = showing([0.0; BANDS]);
        for _ in 0..100 {
            spectrum.update(Duration::from_millis(100));
        }
        for peak in spectrum.peaks {
            assert!(peak >= 0.0, "a peak went negative");
        }
    }

    #[test]
    fn nothing_is_drawn_outside_the_area() {
        let spectrum = showing([1.0; BANDS]);
        for height in MIN_HEIGHT..=34 {
            for top in [0, (34 - height) / 2, 34 - height] {
                let mut canvas = Canvas::new();
                spectrum.render(&mut canvas, Area { top, height });
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
    fn a_peak_shows_even_when_its_band_has_gone_quiet() {
        // The capture is silent in a test, so the bars are empty; the peak
        // marker is the only thing that can be lit.
        let spectrum = showing([1.0; BANDS]);
        let mut canvas = Canvas::new();
        spectrum.render(&mut canvas, Area { top: 0, height: 20 });

        let lit = (0..9)
            .filter(|x| (0..20).any(|y| canvas.get(*x, y) > 0))
            .count();
        assert_eq!(lit, 9, "the peak markers did not show");
    }

    #[test]
    fn silence_still_looks_like_an_instrument() {
        // Nothing playing is the normal state of a speaker. Drawing nothing at
        // all made it indistinguishable from a scene that had failed.
        let spectrum = showing([0.0; BANDS]);
        let mut canvas = Canvas::new();
        let area = Area { top: 4, height: 20 };
        spectrum.render(&mut canvas, area);

        for x in 0..9 {
            assert!(canvas.get(x, area.bottom()) > 0, "column {x} went dark");
        }
        // A floor, and nothing more: silence must not read as sound.
        let lit = (0..34)
            .flat_map(|y| (0..9).map(move |x| (x, y)))
            .filter(|(x, y)| canvas.get(*x, *y) > 0)
            .count();
        assert_eq!(lit, 9, "silence lit more than the floor");
    }

    #[test]
    fn the_two_sources_are_named_apart() {
        // They are the same widget pointed at different devices, and the name
        // is the only thing that says which. "spectrum" said neither, and got
        // read as the microphone by the person whose microphone was on.
        assert_eq!(
            Spectrum::new(super::Source::Output, ColorMode::Bw).name(),
            "speakers-spectrum"
        );
        assert_eq!(
            Spectrum::new(super::Source::Input, ColorMode::Bw).name(),
            "mic-spectrum"
        );
    }

    #[test]
    fn one_column_per_band() {
        assert_eq!(BANDS, 9, "the panel has nine columns to fill");
    }
}
