//! Scenes: self-contained animations that own one panel's picture.
//!
//! A scene is stepped with the elapsed time and asked to draw itself. It knows
//! nothing about serial ports, frame rates or the other panel.

pub mod battery;
pub mod clock;
pub mod load;
pub mod media;
pub mod pong;
pub mod snake;
pub mod spectrum;
pub mod tetris;
pub mod traffic;
pub mod volume;

use std::fmt;
use std::time::Duration;

use rand::SeedableRng;
use rand::rngs::StdRng;

use crate::canvas::{self, Canvas};
use crate::device::ColorMode;
use battery::BatteryGauge;
use clock::Clock;
use load::{Load, Source};
use media::Media;
use pong::Pong;
use snake::Snake;
use spectrum::Spectrum;
use tetris::Tetris;
use traffic::Traffic;
use volume::Volume;

/// A horizontal band of the panel, which is all a scene is allowed to touch.
///
/// Scenes draw relative to [`Area::top`] and adapt to [`Area::height`], so the
/// same widget can be a three-row strip in a stack or fill the panel on its
/// own. Nothing in a scene knows where it sits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Area {
    /// First row the scene may draw on.
    pub top: i32,
    /// Number of rows it has.
    pub height: i32,
}

impl Area {
    /// The whole panel.
    pub const FULL: Self = Self {
        top: 0,
        height: canvas::HEIGHT,
    };

    /// The row `offset` rows down from the top of the area.
    #[must_use]
    pub const fn row(self, offset: i32) -> i32 {
        self.top + offset
    }

    /// The last row of the area.
    #[must_use]
    pub const fn bottom(self) -> i32 {
        self.top + self.height - 1
    }

    /// The area centred on its own middle, keeping only `rows`.
    #[must_use]
    pub const fn centred(self, rows: i32) -> Self {
        Self {
            top: self.top + (self.height - rows) / 2,
            height: rows,
        }
    }
}

/// Something that animates and draws itself into a band of one module.
#[cfg_attr(test, mockall::automock)]
pub trait Scene {
    /// Short name for logs.
    fn name(&self) -> &'static str;

    /// Rows this scene needs to be worth showing at all.
    ///
    /// A stack refuses to be built if its parts do not fit, so this is a
    /// promise: given this many rows, the scene draws something legible.
    fn min_height(&self) -> i32;

    /// Advances the simulation by `delta`.
    fn update(&mut self, delta: Duration);

    /// Draws the current state into `area` on a cleared canvas.
    fn render(&self, canvas: &mut Canvas, area: Area);
}

/// What to show on a panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum SceneKind {
    /// Pong, played by two equally mediocre robots.
    Pong,
    /// Snake, played by a robot that plans ahead.
    Snake,
    /// Tetris, played by a robot that scores every landing.
    Tetris,
    /// The time of day, hours stacked over minutes.
    Clock,
    /// Share of processor time in use.
    Cpu,
    /// Share of memory in use.
    Ram,
    /// Bytes over the network, in and out.
    Net,
    /// Bytes to and from the disks.
    Disk,
    /// Output volume, with its muted state.
    Volume,
    /// What is playing, over MPRIS.
    Media,
    /// Nine bands of whatever is coming out of the speakers.
    Spectrum,
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
            Self::Pong | Self::Snake | Self::Tetris | Self::Spectrum => ColorMode::Bw,
            // Widgets change a few times a minute at most, and an unchanged
            // frame is never resent, so their shading is effectively free.
            Self::Clock
            | Self::Cpu
            | Self::Ram
            | Self::Net
            | Self::Disk
            | Self::Volume
            | Self::Media
            | Self::Battery
            | Self::Off => ColorMode::Greyscale,
        }
    }
}

impl fmt::Display for SceneKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Pong => "pong",
            Self::Snake => "snake",
            Self::Tetris => "tetris",
            Self::Clock => "clock",
            Self::Cpu => "cpu",
            Self::Ram => "ram",
            Self::Net => "net",
            Self::Disk => "disk",
            Self::Volume => "volume",
            Self::Media => "media",
            Self::Spectrum => "spectrum",
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
    /// See [`Tetris`].
    Tetris(Tetris),
    /// See [`Clock`].
    Clock(Clock),
    /// See [`Load`].
    Load(Load),
    /// See [`Traffic`].
    Traffic(Traffic),
    /// See [`Volume`].
    Volume(Volume),
    /// See [`Media`].
    Media(Media),
    /// See [`Spectrum`].
    Spectrum(Spectrum),
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
            SceneKind::Tetris => Some(Self::Tetris(Tetris::new(seed, mode))),
            SceneKind::Clock => Some(Self::Clock(Clock::new(mode))),
            SceneKind::Cpu => Some(Self::Load(Load::new(Source::Cpu, mode))),
            SceneKind::Ram => Some(Self::Load(Load::new(Source::Memory, mode))),
            SceneKind::Net => Some(Self::Traffic(Traffic::new(traffic::Source::Network, mode))),
            SceneKind::Disk => Some(Self::Traffic(Traffic::new(traffic::Source::Disk, mode))),
            SceneKind::Volume => Some(Self::Volume(Volume::new(mode))),
            SceneKind::Media => Some(Self::Media(Media::new(mode))),
            SceneKind::Spectrum => Some(Self::Spectrum(Spectrum::new(mode))),
            SceneKind::Battery => Some(Self::Battery(BatteryGauge::new(mode))),
            SceneKind::Off => None,
        }
    }

    /// Builds the stack a panel should run for `spec`.
    ///
    /// # Errors
    ///
    /// Fails when the named scenes cannot fit the panel together.
    pub fn stack(
        spec: &crate::control::SceneSpec,
        seed: Option<u64>,
        mode: ColorMode,
    ) -> anyhow::Result<Stack> {
        let scenes = spec
            .scenes()
            .iter()
            .map(|kind| Self::or_blank(*kind, seed, mode))
            .collect();
        Stack::new(scenes)
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
    fn min_height(&self) -> i32 {
        match self {
            Self::Pong(scene) => scene.min_height(),
            Self::Snake(scene) => scene.min_height(),
            Self::Tetris(scene) => scene.min_height(),
            Self::Clock(scene) => scene.min_height(),
            Self::Load(scene) => scene.min_height(),
            Self::Traffic(scene) => scene.min_height(),
            Self::Volume(scene) => scene.min_height(),
            Self::Media(scene) => scene.min_height(),
            Self::Spectrum(scene) => scene.min_height(),
            Self::Battery(scene) => scene.min_height(),
            Self::Blank => 1,
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Self::Pong(scene) => scene.name(),
            Self::Snake(scene) => scene.name(),
            Self::Tetris(scene) => scene.name(),
            Self::Clock(scene) => scene.name(),
            Self::Load(scene) => scene.name(),
            Self::Traffic(scene) => scene.name(),
            Self::Volume(scene) => scene.name(),
            Self::Media(scene) => scene.name(),
            Self::Spectrum(scene) => scene.name(),
            Self::Battery(scene) => scene.name(),
            Self::Blank => "off",
        }
    }

    fn update(&mut self, delta: Duration) {
        match self {
            Self::Pong(scene) => scene.update(delta),
            Self::Snake(scene) => scene.update(delta),
            Self::Tetris(scene) => scene.update(delta),
            Self::Clock(scene) => scene.update(delta),
            Self::Load(scene) => scene.update(delta),
            Self::Traffic(scene) => scene.update(delta),
            Self::Volume(scene) => scene.update(delta),
            Self::Media(scene) => scene.update(delta),
            Self::Spectrum(scene) => scene.update(delta),
            Self::Battery(scene) => scene.update(delta),
            Self::Blank => {}
        }
    }

    fn render(&self, canvas: &mut Canvas, area: Area) {
        match self {
            Self::Pong(scene) => scene.render(canvas, area),
            Self::Snake(scene) => scene.render(canvas, area),
            Self::Tetris(scene) => scene.render(canvas, area),
            Self::Clock(scene) => scene.render(canvas, area),
            Self::Load(scene) => scene.render(canvas, area),
            Self::Traffic(scene) => scene.render(canvas, area),
            Self::Volume(scene) => scene.render(canvas, area),
            Self::Media(scene) => scene.render(canvas, area),
            Self::Spectrum(scene) => scene.render(canvas, area),
            Self::Battery(scene) => scene.render(canvas, area),
            Self::Blank => {}
        }
    }
}

/// Brightness of the dotted rule drawn between two stacked scenes.
const DIVIDER_LEVEL: u8 = 26;

/// One or more scenes sharing a panel, top to bottom.
///
/// A panel always runs one of these, even for a single scene: a stack of one
/// hands over the whole panel, which is how a widget gets its full-screen form
/// without a second implementation.
pub struct Stack {
    scenes: Vec<AnyScene>,
}

impl Stack {
    /// Builds a stack, or explains why the parts do not fit.
    ///
    /// # Errors
    ///
    /// Fails when the scenes together need more rows than the panel has. It is
    /// better to refuse the whole thing than to silently drop the last one.
    pub fn new(scenes: Vec<AnyScene>) -> anyhow::Result<Self> {
        anyhow::ensure!(!scenes.is_empty(), "a panel needs at least one scene");

        let needed = required_height(&scenes);
        anyhow::ensure!(
            needed <= canvas::HEIGHT,
            "these scenes need {needed} rows and the panel has {}",
            canvas::HEIGHT
        );
        Ok(Self { scenes })
    }

    /// Splits the panel between the scenes, tallest appetite first.
    ///
    /// Spare rows are shared out evenly rather than given to one scene: every
    /// widget here reads better with room, and an even split is predictable
    /// enough to reason about when composing a stack.
    fn layout(&self) -> Vec<Area> {
        let count = i32::try_from(self.scenes.len()).unwrap_or(1);
        let dividers = count - 1;
        let spare = canvas::HEIGHT - required_height(&self.scenes);
        let (each, mut leftover) = if count > 0 {
            (spare / count, spare % count)
        } else {
            (0, 0)
        };

        let mut areas = Vec::with_capacity(self.scenes.len());
        let mut top = 0;
        for scene in &self.scenes {
            let mut height = scene.min_height() + each;
            if leftover > 0 {
                height += 1;
                leftover -= 1;
            }
            areas.push(Area { top, height });
            top += height + 1;
        }
        let _ = dividers;
        areas
    }
}

/// Rows a set of scenes needs, dividers included.
fn required_height(scenes: &[AnyScene]) -> i32 {
    let dividers = i32::try_from(scenes.len().saturating_sub(1)).unwrap_or(0);
    scenes.iter().map(Scene::min_height).sum::<i32>() + dividers
}

impl Scene for Stack {
    fn name(&self) -> &'static str {
        self.scenes.first().map_or("off", Scene::name)
    }

    fn min_height(&self) -> i32 {
        required_height(&self.scenes)
    }

    fn update(&mut self, delta: Duration) {
        for scene in &mut self.scenes {
            scene.update(delta);
        }
    }

    fn render(&self, canvas: &mut Canvas, _area: Area) {
        let areas = self.layout();
        for (index, (scene, area)) in self.scenes.iter().zip(&areas).enumerate() {
            scene.render(canvas, *area);
            // A dotted rule between neighbours, so two stacked widgets do not
            // read as one.
            if index + 1 < self.scenes.len() {
                for x in (0..canvas::WIDTH).step_by(2) {
                    canvas.set_max(x, area.bottom() + 1, DIVIDER_LEVEL);
                }
            }
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
    use super::{AnyScene, Area, ColorMode, Scene, SceneKind, Stack};
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

    fn stack_of(kinds: &[SceneKind]) -> anyhow::Result<Stack> {
        let spec: crate::control::SceneSpec = kinds
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",")
            .parse()
            .expect("spec");
        AnyScene::stack(&spec, Some(1), ColorMode::Greyscale)
    }

    #[test]
    fn a_stack_of_one_hands_over_the_whole_panel() {
        // This is what gives a widget its full-screen form without a second
        // implementation: alone, it simply gets every row.
        let stack = stack_of(&[SceneKind::Clock]).expect("stack");
        let areas = stack.layout();

        assert_eq!(areas.len(), 1);
        assert_eq!(areas[0].top, 0);
        assert_eq!(areas[0].height, crate::canvas::HEIGHT);
    }

    #[test]
    fn a_stack_splits_the_panel_without_gaps_or_overlaps() {
        let kinds = [SceneKind::Clock, SceneKind::Cpu, SceneKind::Battery];
        let stack = stack_of(&kinds).expect("stack");
        let areas = stack.layout();

        assert_eq!(areas.len(), 3);
        for (index, area) in areas.iter().enumerate() {
            assert!(area.top >= 0, "area {index} starts above the panel");
            assert!(
                area.bottom() < crate::canvas::HEIGHT,
                "area {index} runs past the bottom"
            );
            // One row between neighbours, for the rule.
            if let Some(next) = areas.get(index + 1) {
                assert_eq!(
                    next.top,
                    area.bottom() + 2,
                    "areas {index} and {} collide",
                    index + 1
                );
            }
        }
    }

    #[test]
    fn every_scene_in_a_stack_gets_at_least_what_it_asked_for() {
        let stack = stack_of(&[SceneKind::Clock, SceneKind::Battery]).expect("stack");
        let areas = stack.layout();
        for (scene, area) in stack.scenes.iter().zip(&areas) {
            assert!(
                area.height >= scene.min_height(),
                "{} got {} rows and needs {}",
                scene.name(),
                area.height,
                scene.min_height()
            );
        }
    }

    #[test]
    fn a_stack_that_cannot_fit_is_refused_rather_than_truncated() {
        // Two games need 34 rows each. Dropping the second silently would be
        // worse than saying so: the panel would just look wrong.
        let Err(error) = stack_of(&[SceneKind::Pong, SceneKind::Snake]) else {
            panic!("two games cannot share a panel");
        };
        let message = error.to_string();
        assert!(message.contains("rows"), "unhelpful message: {message}");
    }

    #[test]
    fn a_rule_is_drawn_between_neighbours_but_not_below_the_last() {
        let stack = stack_of(&[SceneKind::Clock, SceneKind::Battery]).expect("stack");
        let areas = stack.layout();

        let mut canvas = Canvas::new();
        stack.render(&mut canvas, Area::FULL);

        let rule_row = areas[0].bottom() + 1;
        assert!(canvas.get(0, rule_row) > 0, "no rule between the two");
        let below_last = areas[1].bottom() + 1;
        if below_last < crate::canvas::HEIGHT {
            assert_eq!(canvas.get(0, below_last), 0, "a rule under the last scene");
        }
    }

    #[test]
    fn every_scene_eventually_lights_a_pixel() {
        for kind in [SceneKind::Pong, SceneKind::Snake] {
            let mut scene = AnyScene::new(kind, Some(7), ColorMode::Bw).expect("scene");
            let mut canvas = Canvas::new();

            for _ in 0..120 {
                scene.update(Duration::from_millis(33));
            }
            scene.render(&mut canvas, Area::FULL);

            assert_ne!(canvas, Canvas::new(), "{} drew nothing", scene.name());
        }
    }
}
