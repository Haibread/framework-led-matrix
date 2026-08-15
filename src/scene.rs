//! Scenes: self-contained animations that own one panel's picture.
//!
//! A scene is stepped with the elapsed time and asked to draw itself. It knows
//! nothing about serial ports, frame rates or the other panel.

pub mod pong;
pub mod snake;

use std::fmt;
use std::time::Duration;

use rand::SeedableRng;
use rand::rngs::StdRng;

use crate::canvas::Canvas;
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
    /// Nothing; the panel stays dark and is left alone.
    Off,
}

impl fmt::Display for SceneKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Pong => "pong",
            Self::Snake => "snake",
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
}

impl AnyScene {
    /// Builds the scene for `kind`, or `None` for [`SceneKind::Off`].
    ///
    /// `seed` makes a run reproducible; `None` seeds from the OS.
    #[must_use]
    pub fn new(kind: SceneKind, seed: Option<u64>) -> Option<Self> {
        match kind {
            SceneKind::Pong => Some(Self::Pong(Pong::new(seed))),
            SceneKind::Snake => Some(Self::Snake(Snake::new(seed))),
            SceneKind::Off => None,
        }
    }
}

impl Scene for AnyScene {
    fn name(&self) -> &'static str {
        match self {
            Self::Pong(scene) => scene.name(),
            Self::Snake(scene) => scene.name(),
        }
    }

    fn update(&mut self, delta: Duration) {
        match self {
            Self::Pong(scene) => scene.update(delta),
            Self::Snake(scene) => scene.update(delta),
        }
    }

    fn render(&self, canvas: &mut Canvas) {
        match self {
            Self::Pong(scene) => scene.render(canvas),
            Self::Snake(scene) => scene.render(canvas),
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
    use super::{AnyScene, Scene, SceneKind};
    use crate::canvas::Canvas;
    use std::time::Duration;

    #[test]
    fn off_builds_no_scene() {
        assert!(AnyScene::new(SceneKind::Off, Some(1)).is_none());
        assert!(AnyScene::new(SceneKind::Pong, Some(1)).is_some());
        assert!(AnyScene::new(SceneKind::Snake, Some(1)).is_some());
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
            let mut scene = AnyScene::new(kind, Some(7)).expect("scene");
            let mut canvas = Canvas::new();

            for _ in 0..120 {
                scene.update(Duration::from_millis(33));
            }
            scene.render(&mut canvas);

            assert_ne!(canvas, Canvas::new(), "{} drew nothing", scene.name());
        }
    }
}
