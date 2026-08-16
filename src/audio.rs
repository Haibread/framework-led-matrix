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

/// How much audio the capture should hand over at a time.
const LATENCY_MS: u32 = 40;

/// Range shown, in decibels below the loudest band heard lately.
///
/// Fixed against full scale it cannot work for both a quiet room and a hot
/// microphone: this machine's input sits at -1.7 dBFS, which would peg every
/// band, while a quiet track would never leave the floor. Scaling against what
/// is actually being heard is what makes the bars mean something.
const RANGE_DB: f32 = 34.0;

/// Below this, a band counts as silent whatever the reference is.
const SILENCE_DB: f32 = -70.0;

/// How fast the reference falls when things go quiet, in decibels a second.
///
/// Slow enough that a pause between two notes does not wind the gain up and
/// turn the room noise into a light show.
const REFERENCE_FALL: f32 = 9.0;

/// Quietest reference the gain will scale against.
const REFERENCE_FLOOR: f32 = -45.0;

/// How quickly a bar rises and falls, as a share of the gap per frame.
///
/// Rising fast and falling slowly is what a VU meter does, and what makes a
/// beat readable: the peak is still visible when the eye gets there.
const ATTACK: f32 = 0.6;
const DECAY: f32 = 0.12;

/// Where the sound is taken from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// What the machine is playing, through the default sink's monitor.
    Output,
    /// What the microphone hears.
    Input,
}

impl Source {
    /// The device `parec` should be pointed at.
    ///
    /// Both are resolved by the sound server rather than named outright, so
    /// changing the default output or input takes effect on the next capture.
    const fn device(self) -> &'static str {
        match self {
            Self::Output => "--device=@DEFAULT_MONITOR@",
            Self::Input => "--device=@DEFAULT_SOURCE@",
        }
    }
}

/// The latest band levels, published by the capture thread.
pub struct Listener {
    bands: Arc<Mutex<[f32; BANDS]>>,
    stop: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
}

impl Listener {
    /// Starts capturing `source` and analysing it, on its own thread.
    #[must_use]
    pub fn start(source: Source) -> Self {
        let bands = Arc::new(Mutex::new([0.0; BANDS]));
        let stop = Arc::new(AtomicBool::new(false));
        let child = Arc::new(Mutex::new(None));

        let listener = Self {
            bands: Arc::clone(&bands),
            stop: Arc::clone(&stop),
            child: Arc::clone(&child),
        };

        thread::spawn(move || {
            if let Err(error) = capture(source, &bands, &stop, &child) {
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
        debug!("listener dropped");
        self.stop.store(true, Ordering::Relaxed);
        // The reader is blocked on the pipe, and silence can keep it there for
        // a long time; killing the child is what actually wakes it.
        if let Ok(mut child) = self.child.lock() {
            if let Some(running) = child.take() {
                reap(running);
            }
        }
    }
}

/// Runs the capture loop until told to stop.
fn capture(
    source: Source,
    bands: &Arc<Mutex<[f32; BANDS]>>,
    stop: &Arc<AtomicBool>,
    holder: &Arc<Mutex<Option<Child>>>,
) -> std::io::Result<()> {
    let mut child = Command::new("parec")
        .args([
            source.device(),
            "--format=s16le",
            &format!("--rate={SAMPLE_RATE}"),
            "--channels=1",
            // Without this the capture buffers about two seconds and then
            // hands them over at once: the analysis runs a thousand times in a
            // few milliseconds, every window smears into the next, and what
            // reaches the panel is two seconds stale.
            &format!("--latency-msec={LATENCY_MS}"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let Some(mut output) = child.stdout.take() else {
        return Ok(());
    };
    if let Some(mut errors) = child.stderr.take() {
        std::thread::spawn(move || {
            let mut text = String::new();
            let _ = errors.read_to_string(&mut text);
            if !text.trim().is_empty() {
                warn!(message = text.trim(), "the capture complained");
            }
        });
    }
    if let Ok(mut slot) = holder.lock() {
        *slot = Some(child);
    }
    debug!(?source, "audio capture started");

    let mut planner = FftPlanner::new();
    let fft = planner.plan_fft_forward(WINDOW);

    let mut raw = vec![0u8; WINDOW * 2];
    let mut smoothed = [0.0f32; BANDS];
    let mut gain = Gain::default();
    // One window's worth of audio, which is what each pass represents however
    // fast the pipe hands it over.
    #[allow(
        clippy::cast_precision_loss,
        reason = "a window length and a sample rate both convert exactly"
    )]
    let seconds = WINDOW as f32 / SAMPLE_RATE as f32;

    while !stop.load(Ordering::Relaxed) {
        if let Err(error) = output.read_exact(&mut raw) {
            // Losing the capture silently is what made this look like a widget
            // that simply never lit up.
            warn!(?error, "the capture stopped sending audio");
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
        let target = gain.apply(bands_of(&magnitudes, SAMPLE_RATE), seconds);

        for (level, wanted) in smoothed.iter_mut().zip(target) {
            *level = smooth(*level, wanted);
        }
        if let Ok(mut published) = bands.lock() {
            *published = smoothed;
        }
    }

    if let Ok(mut slot) = holder.lock() {
        if let Some(running) = slot.take() {
            reap(running);
        }
    }
    Ok(())
}

/// Kills the capture and collects it.
///
/// Killing alone leaves a zombie: the process is gone but its entry stays in
/// the table until someone reads its status, and switching to this scene and
/// away again would pile them up one per visit.
fn reap(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
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

/// Folds a magnitude spectrum into the nine band loudnesses, in decibels.
#[must_use]
pub fn bands_of(magnitudes: &[f32], sample_rate: u32) -> [f32; BANDS] {
    if magnitudes.is_empty() {
        return [SILENCE_DB; BANDS];
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
    let mut levels = [SILENCE_DB; BANDS];

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
        *level = to_decibels(peak / scale);
    }
    levels
}

/// A magnitude in decibels relative to full scale.
///
/// Loudness is logarithmic; on a linear scale a quiet passage would not move
/// the bars at all and a loud one would peg them.
#[must_use]
pub fn to_decibels(magnitude: f32) -> f32 {
    if magnitude <= 0.0 {
        return SILENCE_DB;
    }
    (20.0 * magnitude.log10()).max(SILENCE_DB)
}

/// Scales loudness against what is actually being heard.
///
/// A fixed scale cannot serve a saturated microphone and a quiet track at once,
/// so the top of the display follows the loudest band heard lately and falls
/// back slowly when things go quiet.
#[derive(Clone, Copy, Debug)]
pub struct Gain {
    reference: f32,
}

impl Default for Gain {
    fn default() -> Self {
        Self {
            reference: REFERENCE_FLOOR,
        }
    }
}

impl Gain {
    /// Follows the loudest of `bands`, then reports each as `0.0..=1.0`.
    pub fn apply(&mut self, bands: [f32; BANDS], seconds: f32) -> [f32; BANDS] {
        let loudest = bands.iter().copied().fold(SILENCE_DB, f32::max);
        self.reference = if loudest >= self.reference {
            loudest
        } else {
            (self.reference - REFERENCE_FALL * seconds).max(loudest.max(REFERENCE_FLOOR))
        };

        let floor = self.reference - RANGE_DB;
        bands.map(|decibels| {
            if decibels <= SILENCE_DB {
                return 0.0;
            }
            ((decibels - floor) / RANGE_DB).clamp(0.0, 1.0)
        })
    }
}

/// Moves a bar towards its target, quickly up and slowly down.
#[must_use]
pub fn smooth(current: f32, target: f32) -> f32 {
    let rate = if target > current { ATTACK } else { DECAY };
    rate.mul_add(target - current, current)
}

#[cfg(test)]
mod tests {
    use super::{BANDS, EDGES, Gain, SAMPLE_RATE, WINDOW, bands_of, hann, samples, smooth};
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
        let mut gain = Gain::default();
        let levels = gain.apply(bands_of(&[0.0; WINDOW / 2], SAMPLE_RATE), 0.05);
        assert!(levels.iter().all(|level| *level < f32::EPSILON));
    }

    #[test]
    fn a_saturated_source_does_not_light_every_band() {
        // This machine's microphone sits at -1.7 dBFS, which against a fixed
        // scale pegged all nine columns; the display has to mean something
        // whatever the input level is.
        let mut gain = Gain::default();
        let bands = [-2.0, -30.0, -40.0, -6.0, -50.0, -3.0, -45.0, -60.0, -55.0];
        let levels = gain.apply(bands, 0.05);

        let lit = levels.iter().filter(|level| **level > 0.8).count();
        assert!(
            lit <= 3,
            "a hot source lit {lit} columns near the top: {levels:?}"
        );
        assert!(levels[0] > levels[4], "the loud band is not the tall one");
    }

    #[test]
    fn a_quiet_source_still_fills_the_panel() {
        // The same test the other way round: a quiet track must not sit flat
        // along the bottom.
        let mut gain = Gain::default();
        let bands = [
            -50.0, -62.0, -66.0, -55.0, -68.0, -52.0, -64.0, -69.0, -67.0,
        ];
        // Give the reference time to settle on what it is hearing.
        let mut levels = [0.0; BANDS];
        for _ in 0..40 {
            levels = gain.apply(bands, 0.05);
        }
        assert!(levels[0] > 0.5, "the loudest band stayed low: {levels:?}");
    }

    #[test]
    fn the_reference_falls_slowly_rather_than_chasing_every_pause() {
        // Winding the gain up during a rest would turn room noise into a light
        // show between two notes.
        let mut gain = Gain::default();
        let loud = [-2.0; BANDS];
        gain.apply(loud, 0.05);

        let quiet = [-60.0; BANDS];
        let after_a_moment = gain.apply(quiet, 0.05);
        assert!(
            after_a_moment.iter().all(|level| *level < 0.2),
            "the gain jumped straight to the quiet passage: {after_a_moment:?}"
        );
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
