//! Command line and environment configuration.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::control::PanelName;
use crate::device::ColorMode;
use crate::scene::SceneKind;

/// Default device nodes, created by the shipped udev rule.
const DEFAULT_LEFT_DEVICE: &str = "/dev/led-matrix-left";
const DEFAULT_RIGHT_DEVICE: &str = "/dev/led-matrix-right";

/// The colour mode asked for on the command line.
///
/// Distinct from [`ColorMode`] because "whatever this scene wants" is a valid
/// answer here and not a mode the device could be handed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum ColorModeChoice {
    /// Use each scene's own preference.
    Auto,
    /// Force per-LED brightness everywhere.
    Greyscale,
    /// Force one bit per LED everywhere.
    Bw,
}

impl ColorModeChoice {
    /// Resolves the choice against what a scene would like.
    #[must_use]
    pub fn resolve(self, preferred: ColorMode) -> ColorMode {
        match self {
            Self::Auto => preferred,
            Self::Greyscale => ColorMode::Greyscale,
            Self::Bw => ColorMode::Bw,
        }
    }
}

impl std::fmt::Display for ColorModeChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Auto => "auto",
            Self::Greyscale => "greyscale",
            Self::Bw => "bw",
        })
    }
}

/// What to ask of a daemon that is already running.
///
/// With no subcommand the process becomes the daemon.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Change what a panel shows
    Set {
        /// Which module
        panel: PanelName,
        /// What it should show
        scene: SceneKind,
    },
    /// Set the brightness of every panel
    Brightness {
        /// 0 to 255
        level: u8,
    },
    /// Report what each panel is showing
    Status,
}

/// Drive the Framework 16 LED Matrix modules.
#[derive(Debug, Parser)]
#[command(version, about, long_about = None)]
pub struct Cli {
    /// Talk to a running daemon instead of becoming one
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Control socket the daemon listens on
    ///
    /// Defaults to `ledmat.sock` inside `XDG_RUNTIME_DIR`, falling back to the
    /// current directory when that is unset.
    #[arg(long, env = "SOCKET_PATH")]
    pub socket: Option<PathBuf>,

    /// Serial device of the left module
    #[arg(long, env = "LEFT_DEVICE", default_value = DEFAULT_LEFT_DEVICE)]
    pub left_device: String,

    /// Serial device of the right module
    #[arg(long, env = "RIGHT_DEVICE", default_value = DEFAULT_RIGHT_DEVICE)]
    pub right_device: String,

    /// What to show on the left module
    #[arg(long, env = "LEFT_SCENE", default_value_t = SceneKind::Pong)]
    pub left_scene: SceneKind,

    /// What to show on the right module
    #[arg(long, env = "RIGHT_SCENE", default_value_t = SceneKind::Snake)]
    pub right_scene: SceneKind,

    /// Panel brightness, 0 to 255
    ///
    /// The modules sit right under your hands: anything past ~80 is a desk lamp.
    #[arg(long, env = "BRIGHTNESS", default_value_t = 30)]
    pub brightness: u8,

    /// Frames per second
    #[arg(long, env = "FPS", default_value_t = 30, value_parser = clap::value_parser!(u32).range(1..=60))]
    pub fps: u32,

    /// How frames are pushed to the modules
    ///
    /// `auto` lets each scene choose: games take `bw`, widgets take `greyscale`.
    /// `greyscale` gives per-LED brightness but costs ten commands a frame,
    /// which the module's command rate caps at about 6 fps. `bw` is one command
    /// a frame, so roughly six times smoother, with no shading.
    #[arg(long, env = "COLOR_MODE", default_value_t = ColorModeChoice::Auto)]
    pub color_mode: ColorModeChoice,

    /// Render to the terminal instead of the modules
    ///
    /// Useful for working on scenes without hardware, or without permission on
    /// the device nodes.
    #[arg(long, env = "SIMULATE")]
    pub simulate: bool,

    /// Seed the scenes for a reproducible run
    #[arg(long, env = "SEED")]
    pub seed: Option<u64>,

    /// `tracing` filter directive (e.g. `info`, `ledmat=debug,serialport=warn`)
    ///
    /// Syntax: <https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html#directives>
    #[arg(long = "log-filter", env = "LOG_FILTER", default_value = "info")]
    pub log_filter: String,
}

impl Cli {
    /// Where the control socket lives.
    #[must_use]
    pub fn socket_path(&self) -> PathBuf {
        self.socket.clone().unwrap_or_else(|| {
            std::env::var_os("XDG_RUNTIME_DIR")
                .map_or_else(|| PathBuf::from("."), PathBuf::from)
                .join("ledmat.sock")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Cli, ColorModeChoice};
    use crate::device::ColorMode;
    use crate::scene::SceneKind;
    use clap::{CommandFactory, Parser};

    #[test]
    fn the_command_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn defaults_put_pong_on_the_left_and_snake_on_the_right() {
        let cli = Cli::parse_from(["ledmat"]);
        assert_eq!(cli.left_scene, SceneKind::Pong);
        assert_eq!(cli.right_scene, SceneKind::Snake);
        assert_eq!(cli.fps, 30);
        assert!(!cli.simulate);
        assert!(cli.seed.is_none());
    }

    #[test]
    fn scenes_are_selected_by_name() {
        let cli = Cli::parse_from(["ledmat", "--left-scene", "off", "--right-scene", "pong"]);
        assert_eq!(cli.left_scene, SceneKind::Off);
        assert_eq!(cli.right_scene, SceneKind::Pong);
    }

    #[test]
    fn an_unknown_scene_is_rejected() {
        assert!(Cli::try_parse_from(["ledmat", "--left-scene", "tetris"]).is_err());
    }

    #[test]
    fn auto_defers_to_the_scene_while_the_others_override_it() {
        assert_eq!(
            ColorModeChoice::Auto.resolve(ColorMode::Bw),
            ColorMode::Bw,
            "auto ignored the scene"
        );
        assert_eq!(
            ColorModeChoice::Greyscale.resolve(ColorMode::Bw),
            ColorMode::Greyscale,
            "the flag failed to override the scene"
        );
        assert_eq!(
            ColorModeChoice::Bw.resolve(ColorMode::Greyscale),
            ColorMode::Bw
        );
    }

    #[test]
    fn the_colour_mode_defaults_to_auto() {
        assert_eq!(
            Cli::parse_from(["ledmat"]).color_mode,
            ColorModeChoice::Auto
        );
    }

    #[test]
    fn the_frame_rate_is_bounded() {
        assert!(Cli::try_parse_from(["ledmat", "--fps", "0"]).is_err());
        assert!(Cli::try_parse_from(["ledmat", "--fps", "120"]).is_err());
        assert_eq!(Cli::parse_from(["ledmat", "--fps", "60"]).fps, 60);
    }

    #[test]
    fn every_option_can_be_set_from_the_environment() {
        let command = Cli::command();
        let without_env: Vec<_> = command
            .get_arguments()
            .filter(|argument| {
                !matches!(argument.get_id().as_str(), "help" | "version")
                    && argument.get_env().is_none()
            })
            .map(|argument| argument.get_id().to_string())
            .collect();
        assert!(without_env.is_empty(), "no env var for: {without_env:?}");
    }
}
