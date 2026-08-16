//! Scenes: self-contained animations that own one panel's picture.
//!
//! A scene is stepped with the elapsed time and asked to draw itself. It knows
//! nothing about serial ports, frame rates or the other panel.

pub mod battery;
pub mod clock;
pub mod gauges;
pub mod pong;
pub mod snake;

use std::fmt;
use std::time::Duration;

use rand::SeedableRng;
use rand::rngs::StdRng;

use crate::canvas::Canvas;
use crate::device::ColorMode;
use battery::BatteryGauge;
use clock::Clock;
use gauges::Gauges;
use pong::Pong;
use snake::Snake;

/// Something that animates and draws itself on one module.
#[cfg_attr(test, mockall::automock)]
pub trait Scene {
    /// Short name for logs.
    fn name(&self) -> &'static str;

    /// Advances the simulation by `delta`.
    fn update(&mut self, delta: Duration);

    /// Draws the current state onto a cleared canvas.
    fn render(&self, canvas: &mut Canvas);
}

/// What to show on a panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum SceneKind {
    /// Pong, played by two equally mediocre robots.
    Pong,
    /// Snake, played by a robot that plans ahead.
    Snake,
    /// The time of day, hours stacked over minutes.
    Clock,
    /// Processor and memory load, as two bars.
    Gauges,
    /// Charge level, drawn as a battery.
    Battery,
    /// Nothing; the panel stays dark and is left alone.
    Off,
}

impl SceneKind {
    /// The colour mode this scene is built for.
    ///
    /// Games pick motion over shading: greyscale costs ten commands a frame and
    /// the module only drains about sixty a second, so shading buys a 6 fps
    /// picture where black and white gives thirty. A widget that changes once a
    /// second has no such problem and should keep its shading.
    #[must_use]
    pub fn preferred_color_mode(self) -> ColorMode {
        match self {
            Self::Pong | Self::Snake => ColorMode::Bw,
            // Widgets change a few times a minute at most, and an unchanged
            // frame is never resent, so their shading is effectively free.
            Self::Clock | Self::Gauges | Self::Battery | Self::Off => ColorMode::Greyscale,
        }
    }
}

impl fmt::Display for SceneKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Pong => "pong",
            Self::Snake => "snake",
            Self::Clock => "clock",
            Self::Gauges => "gauges",
            Self::Battery => "battery",
            Self::Off => "off",
        };
        f.write_str(name)
    }
}

/// The scene chosen at startup.
///
/// An enum rather than a trait object: the set of scenes is closed and known at
/// compile time, so this keeps dispatch static.
#[allow(
    clippy::large_enum_variant,
    reason = "exactly one of these exists per panel, so boxing would trade a heap \
              allocation and an indirection for roughly 300 bytes of stack"
)]
pub enum AnyScene {
    /// See [`Pong`].
    Pong(Pong),
    /// See [`Snake`].
    Snake(Snake),
    /// See [`Clock`].
    Clock(Clock),
    /// See [`Gauges`].
    Gauges(Gauges),
    /// See [`BatteryGauge`].
    Battery(BatteryGauge),
    /// Nothing at all, for a panel switched off while the daemon runs.
    Blank,
}

impl AnyScene {
    /// Builds the scene for `kind`, or `None` for [`SceneKind::Off`].
    ///
    /// `seed` makes a run reproducible; `None` seeds from the OS. `mode` is the
    /// mode the panel will actually run in, which the scenes draw differently
    /// for: shading that a black-and-white panel would flatten is wasted, and
    /// worse, misleading.
    #[must_use]
    pub fn new(kind: SceneKind, seed: Option<u64>, mode: ColorMode) -> Option<Self> {
        match kind {
            SceneKind::Pong => Some(Self::Pong(Pong::new(seed, mode))),
            SceneKind::Snake => Some(Self::Snake(Snake::new(seed, mode))),
            SceneKind::Clock => Some(Self::Clock(Clock::new(mode))),
            SceneKind::Gauges => Some(Self::Gauges(Gauges::new(mode))),
            SceneKind::Battery => Some(Self::Battery(BatteryGauge::new(mode))),
            SceneKind::Off => None,
        }
    }

    /// Builds the scene for `kind`, blanking the panel for [`SceneKind::Off`].
    ///
    /// Off means two different things depending on when it is asked for: at
    /// startup there is no panel to blank, so none is opened at all, whereas a
    /// running panel switched off keeps its thread and simply goes dark.
    #[must_use]
    pub fn or_blank(kind: SceneKind, seed: Option<u64>, mode: ColorMode) -> Self {
        Self::new(kind, seed, mode).unwrap_or(Self::Blank)
    }
}

impl Scene for AnyScene {
    fn name(&self) -> &'static str {
        match self {
            Self::Pong(scene) => scene.name(),
            Self::Snake(scene) => scene.name(),
            Self::Clock(scene) => scene.name(),
            Self::Gauges(scene) => scene.name(),
            Self::Battery(scene) => scene.name(),
            Self::Blank => "off",
        }
    }

    fn update(&mut self, delta: Duration) {
        match self {
            Self::Pong(scene) => scene.update(delta),
            Self::Snake(scene) => scene.update(delta),
            Self::Clock(scene) => scene.update(delta),
            Self::Gauges(scene) => scene.update(delta),
            Self::Battery(scene) => scene.update(delta),
            Self::Blank => {}
        }
    }

    fn render(&self, canvas: &mut Canvas) {
        match self {
            Self::Pong(scene) => scene.render(canvas),
            Self::Snake(scene) => scene.render(canvas),
            Self::Clock(scene) => scene.render(canvas),
            Self::Gauges(scene) => scene.render(canvas),
            Self::Battery(scene) => scene.render(canvas),
            Self::Blank => {}
        }
    }
}

/// Builds a scene's random number generator, seeded for reproducible runs.
pub(crate) fn rng_from(seed: Option<u64>) -> StdRng {
    match seed {
        Some(seed) => StdRng::seed_from_u64(seed),
        None => StdRng::from_rng(&mut rand::rng()),
    }
}

#[cfg(test)]
mod tests {
    use super::{AnyScene, ColorMode, Scene, SceneKind};
    use crate::canvas::Canvas;
    use std::time::Duration;

    #[test]
    fn off_builds_no_scene() {
        assert!(AnyScene::new(SceneKind::Off, Some(1), ColorMode::Bw).is_none());
        assert!(AnyScene::new(SceneKind::Pong, Some(1), ColorMode::Bw).is_some());
        assert!(AnyScene::new(SceneKind::Snake, Some(1), ColorMode::Bw).is_some());
    }

    #[test]
    fn the_games_ask_for_black_and_white() {
        // Not a colour preference: greyscale costs ten commands a frame against
        // a module that drains sixty a second, so shading would buy 6 fps.
        assert_eq!(SceneKind::Pong.preferred_color_mode(), ColorMode::Bw);
        assert_eq!(SceneKind::Snake.preferred_color_mode(), ColorMode::Bw);
    }

    #[test]
    fn scene_kinds_display_as_their_flag_value() {
        assert_eq!(SceneKind::Pong.to_string(), "pong");
        assert_eq!(SceneKind::Snake.to_string(), "snake");
        assert_eq!(SceneKind::Off.to_string(), "off");
    }

    #[test]
    fn every_scene_eventually_lights_a_pixel() {
        for kind in [SceneKind::Pong, SceneKind::Snake] {
            let mut scene = AnyScene::new(kind, Some(7), ColorMode::Bw).expect("scene");
            let mut canvas = Canvas::new();

            for _ in 0..120 {
                scene.update(Duration::from_millis(33));
            }
            scene.render(&mut canvas);

            assert_ne!(canvas, Canvas::new(), "{} drew nothing", scene.name());
        }
    }
}
