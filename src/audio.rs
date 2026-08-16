//! Listening to what the machine is playing.
//!
//! `parec` feeding raw samples down a pipe, rather than an audio client
//! library: the capture side of `PipeWire` needs a C library and a build
//! dependency, and this project builds with none. The cost is one subprocess,
//! which is the same bargain the volume widget already makes.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use rustfft::FftPlanner;
use rustfft::num_complex::Complex32;
use tracing::{debug, warn};

use crate::canvas;

/// Bands published, one per column of the panel.
pub const BANDS: usize = 9;
const _: () = assert!(BANDS == 9 && canvas::WIDTH == 9);

/// Samples a second asked of the capture.
///
/// Half of CD rate: it still reaches 11 kHz, which is the top of the last band,
/// and halves the work per window.
const SAMPLE_RATE: u32 = 22_050;

/// Samples per analysis window.
///
/// At this rate that is 46 ms and a 21 Hz bin — fine enough to separate a bass
/// line from a kick, short enough that the bars follow the music.
const WINDOW: usize = 1024;

/// Edges of the nine bands, in hertz.
///
/// Logarithmic, because pitch is: an equal share of the panel per octave is
/// what makes the bars move together rather than all the action sitting in the
/// left-hand column.
const EDGES: [f32; BANDS + 1] = [
    40.0, 80.0, 160.0, 300.0, 550.0, 1_000.0, 1_900.0, 3_500.0, 6_500.0, 11_000.0,
];

/// Quietest level shown, in decibels below full scale.
const FLOOR_DB: f32 = -60.0;

/// How quickly a bar rises and falls, as a share of the gap per frame.
///
/// Rising fast and falling slowly is what a VU meter does, and what makes a
/// beat readable: the peak is still visible when the eye gets there.
const ATTACK: f32 = 0.6;
const DECAY: f32 = 0.12;

/// The latest band levels, published by the capture thread.
pub struct Listener {
    bands: Arc<Mutex<[f32; BANDS]>>,
    stop: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
}

impl Listener {
    /// Starts capturing and analysing, on its own thread.
    #[must_use]
    pub fn start() -> Self {
        let bands = Arc::new(Mutex::new([0.0; BANDS]));
        let stop = Arc::new(AtomicBool::new(false));
        let child = Arc::new(Mutex::new(None));

        let listener = Self {
            bands: Arc::clone(&bands),
            stop: Arc::clone(&stop),
            child: Arc::clone(&child),
        };

        thread::spawn(move || {
            if let Err(error) = capture(&bands, &stop, &child) {
                warn!(?error, "audio capture stopped");
            }
        });

        listener
    }

    /// The most recent band levels, `0.0..=1.0` each.
    #[must_use]
    pub fn bands(&self) -> [f32; BANDS] {
        self.bands.lock().map_or([0.0; BANDS], |bands| *bands)
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // The reader is blocked on the pipe, and silence can keep it there for
        // a long time; killing the child is what actually wakes it.
        if let Ok(mut child) = self.child.lock() {
            if let Some(mut running) = child.take() {
                let _ = running.kill();
            }
        }
    }
}

/// Runs the capture loop until told to stop.
fn capture(
    bands: &Arc<Mutex<[f32; BANDS]>>,
    stop: &Arc<AtomicBool>,
    holder: &Arc<Mutex<Option<Child>>>,
) -> std::io::Result<()> {
    let mut child = Command::new("parec")
        .args([
            "--device=@DEFAULT_MONITOR@",
            "--format=s16le",
            &format!("--rate={SAMPLE_RATE}"),
            "--channels=1",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;

    let Some(mut output) = child.stdout.take() else {
        return Ok(());
    };
    if let Ok(mut slot) = holder.lock() {
        *slot = Some(child);
    }
    debug!("audio capture started");

    let mut planner = FftPlanner::new();
    let fft = planner.plan_fft_forward(WINDOW);

    let mut raw = vec![0u8; WINDOW * 2];
    let mut smoothed = [0.0f32; BANDS];

    while !stop.load(Ordering::Relaxed) {
        if output.read_exact(&mut raw).is_err() {
            break;
        }

        let mut buffer: Vec<Complex32> = samples(&raw)
            .into_iter()
            .zip(0..)
            .map(|(sample, index)| Complex32::new(sample * hann(index, WINDOW), 0.0))
            .collect();
        fft.process(&mut buffer);

        // Only the first half carries anything: the rest mirrors it.
        let magnitudes: Vec<f32> = buffer[..WINDOW / 2]
            .iter()
            .map(|value| value.norm())
            .collect();
        let target = bands_of(&magnitudes, SAMPLE_RATE);

        for (level, wanted) in smoothed.iter_mut().zip(target) {
            *level = smooth(*level, wanted);
        }
        if let Ok(mut published) = bands.lock() {
            *published = smoothed;
        }
    }

    if let Ok(mut slot) = holder.lock() {
        if let Some(mut running) = slot.take() {
            let _ = running.kill();
        }
    }
    Ok(())
}

/// Decodes little-endian signed 16-bit samples into `-1.0..=1.0`.
#[must_use]
pub fn samples(raw: &[u8]) -> Vec<f32> {
    raw.chunks_exact(2)
        .map(|pair| f32::from(i16::from_le_bytes([pair[0], pair[1]])) / 32_768.0)
        .collect()
}

/// The Hann window, which stops a tone leaking across neighbouring bins.
#[must_use]
pub fn hann(index: usize, width: usize) -> f32 {
    if width <= 1 {
        return 1.0;
    }
    // A window is a thousand samples, not a billion: these all convert
    // exactly, and the alternative is a try_from dance for no gain.
    #[allow(
        clippy::cast_precision_loss,
        reason = "window indices are far below the mantissa's limit"
    )]
    let position = index as f32 / (width - 1) as f32;
    0.5 - 0.5 * (std::f32::consts::TAU * position).cos()
}

/// Folds a magnitude spectrum into the nine band levels.
#[must_use]
pub fn bands_of(magnitudes: &[f32], sample_rate: u32) -> [f32; BANDS] {
    if magnitudes.is_empty() {
        return [0.0; BANDS];
    }

    #[allow(
        clippy::cast_precision_loss,
        reason = "a sample rate and a window length both convert exactly"
    )]
    let bin_width = sample_rate as f32 / (magnitudes.len() * 2) as f32;

    // A transform sums over the whole window, so its magnitudes grow with the
    // window length: a full-scale tone through 1024 points peaks near 256, or
    // +48 dB, which would peg every band that leakage reached. Scaling by a
    // quarter of the window puts full scale back at 1.0.
    #[allow(
        clippy::cast_precision_loss,
        reason = "a window length converts exactly"
    )]
    let scale = magnitudes.len() as f32 / 2.0;
    let mut levels = [0.0f32; BANDS];

    for (band, level) in levels.iter_mut().enumerate() {
        let low = EDGES[band];
        let high = EDGES[band + 1];
        // Half-open, so neighbouring bands never share a bin: rounding both
        // edges down made the boundary bin belong to both, and a tone sitting
        // on it lit two columns identically.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "both edges are positive frequencies well inside usize"
        )]
        let (first, last) = (
            ((low / bin_width).ceil() as usize).max(1),
            ((high / bin_width).ceil() as usize)
                .saturating_sub(1)
                .min(magnitudes.len() - 1),
        );

        // The loudest bin rather than the average: a band is "is there
        // anything here", and averaging drowns a sharp note in its neighbours.
        let peak = magnitudes
            .get(first..=last)
            .unwrap_or_default()
            .iter()
            .copied()
            .fold(0.0f32, f32::max);
        *level = to_level(peak / scale);
    }
    levels
}

/// Maps a magnitude onto `0.0..=1.0` through decibels.
///
/// Loudness is logarithmic; on a linear scale a quiet passage would not move
/// the bars at all and a loud one would peg them.
#[must_use]
pub fn to_level(magnitude: f32) -> f32 {
    if magnitude <= 0.0 {
        return 0.0;
    }
    let decibels = 20.0 * magnitude.log10();
    ((decibels - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0)
}

/// Moves a bar towards its target, quickly up and slowly down.
#[must_use]
pub fn smooth(current: f32, target: f32) -> f32 {
    let rate = if target > current { ATTACK } else { DECAY };
    rate.mul_add(target - current, current)
}

#[cfg(test)]
mod tests {
    use super::{BANDS, EDGES, SAMPLE_RATE, WINDOW, bands_of, hann, samples, smooth, to_level};
    use rustfft::FftPlanner;
    use rustfft::num_complex::Complex32;

    /// Runs a tone through the same analysis the capture thread does.
    #[allow(
        clippy::cast_precision_loss,
        reason = "a sample index and a sample rate both convert exactly"
    )]
    fn analyse(frequency: f32) -> [f32; BANDS] {
        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(WINDOW);

        let mut buffer: Vec<Complex32> = (0..WINDOW)
            .map(|index| {
                let time = index as f32 / SAMPLE_RATE as f32;
                let sample = (std::f32::consts::TAU * frequency * time).sin();
                Complex32::new(sample * hann(index, WINDOW), 0.0)
            })
            .collect();
        fft.process(&mut buffer);

        let magnitudes: Vec<f32> = buffer[..WINDOW / 2]
            .iter()
            .map(|value| value.norm())
            .collect();
        bands_of(&magnitudes, SAMPLE_RATE)
    }

    #[test]
    fn a_tone_lights_the_band_it_belongs_to() {
        // The end-to-end check of the analysis: put a pure tone in the middle
        // of each band and the loudest column must be that band.
        for band in 0..BANDS {
            let middle = (EDGES[band] * EDGES[band + 1]).sqrt();
            let levels = analyse(middle);

            let loudest = levels
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
                .map_or(0, |(index, _)| index);
            assert_eq!(
                loudest, band,
                "a {middle:.0} Hz tone lit band {loudest} instead of {band}: {levels:?}"
            );
        }
    }

    #[test]
    fn silence_lights_nothing() {
        let levels = bands_of(&[0.0; WINDOW / 2], SAMPLE_RATE);
        assert!(levels.iter().all(|level| *level < f32::EPSILON));
    }

    #[test]
    fn samples_are_decoded_as_signed_little_endian() {
        assert_eq!(samples(&[0x00, 0x00]), vec![0.0]);
        assert_eq!(
            samples(&[0x00, 0x80]),
            vec![-1.0],
            "the most negative value"
        );
        // A trailing odd byte is a torn read, not a sample.
        assert_eq!(samples(&[0x00, 0x00, 0x11]).len(), 1);
    }

    #[test]
    fn the_window_tapers_to_nothing_at_both_ends() {
        assert!(hann(0, 64) < 1e-6);
        assert!(hann(63, 64) < 1e-6);
        assert!((hann(32, 64) - 1.0).abs() < 0.01, "and peaks in the middle");
        assert!(
            (hann(0, 1) - 1.0).abs() < f32::EPSILON,
            "no division by zero"
        );
    }

    #[test]
    fn levels_are_measured_in_decibels() {
        assert!(to_level(0.0) < f32::EPSILON, "silence is not a level");
        assert!(
            (to_level(1.0) - 1.0).abs() < f32::EPSILON,
            "full scale is full"
        );
        // Halving the amplitude is about 6 dB, a tenth of the sixty on show.
        let full = to_level(1.0);
        let half = to_level(0.5);
        assert!((full - half - 0.1).abs() < 0.02, "{full} then {half}");
    }

    #[test]
    fn bars_rise_faster_than_they_fall() {
        // A beat has to be visible by the time the eye arrives, and gone slowly
        // enough to have been seen at all.
        let rising = smooth(0.0, 1.0);
        let falling = 1.0 - smooth(1.0, 0.0);
        assert!(rising > falling, "rise {rising}, fall {falling}");
        assert!(
            rising < 1.0,
            "a bar that jumps straight to the target flickers"
        );
    }

    #[test]
    fn an_empty_spectrum_is_not_a_panic() {
        assert!(bands_of(&[], SAMPLE_RATE).iter().all(|l| *l < f32::EPSILON));
        assert!(
            bands_of(&[1.0], SAMPLE_RATE)
                .iter()
                .all(|l| *l < f32::EPSILON)
        );
    }
}
