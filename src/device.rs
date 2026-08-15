//! Output backends: the real modules over USB serial, and a terminal preview.

pub mod serial;
pub mod terminal;

use std::fmt;

use anyhow::Result;

use crate::canvas::Canvas;
use serial::SerialMatrix;
use terminal::TerminalMatrix;

/// How a frame is pushed to the module.
///
/// This is a frame-rate decision more than a colour one. The module drains
/// roughly 60 commands a second: a greyscale frame costs ten of them, a
/// black-and-white frame costs one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum ColorMode {
    /// Per-LED brightness, nine staged columns per frame. Caps at about 6 fps.
    Greyscale,
    /// One bit per LED, a single command per frame. Roughly six times faster.
    Bw,
}

impl fmt::Display for ColorMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Greyscale => "greyscale",
            Self::Bw => "bw",
        })
    }
}

/// Brightness at which a pixel counts as lit in [`ColorMode::Bw`].
///
/// Low on purpose: the snake's tail fades to 45 and must stay visible, while
/// the pong midline sits at 18 and must not light up.
pub const BW_THRESHOLD: u8 = 40;

/// One LED matrix module, or something pretending to be one.
#[cfg_attr(test, mockall::automock)]
pub trait Matrix {
    /// Sets the global brightness of the module, `0` to `255`.
    fn set_brightness(&mut self, level: u8) -> Result<()>;

    /// Pushes a full frame to the module.
    fn draw(&mut self, canvas: &Canvas) -> Result<()>;

    /// Turns every LED off, so a stopped daemon does not leave the panel lit.
    fn clear(&mut self) -> Result<()>;
}

/// The backend chosen at startup.
///
/// An enum rather than a trait object: the set of backends is closed and known
/// at compile time, so this keeps dispatch static.
#[allow(
    clippy::large_enum_variant,
    reason = "exactly one of these exists per panel, and the big variant is the \
              cached frame that keeps the module from being redrawn in full"
)]
pub enum Panel {
    /// A real module on a serial port.
    Serial(SerialMatrix),
    /// A preview rendered in the terminal.
    Terminal(TerminalMatrix),
}

impl Matrix for Panel {
    fn set_brightness(&mut self, level: u8) -> Result<()> {
        match self {
            Self::Serial(matrix) => matrix.set_brightness(level),
            Self::Terminal(matrix) => matrix.set_brightness(level),
        }
    }

    fn draw(&mut self, canvas: &Canvas) -> Result<()> {
        match self {
            Self::Serial(matrix) => matrix.draw(canvas),
            Self::Terminal(matrix) => matrix.draw(canvas),
        }
    }

    fn clear(&mut self) -> Result<()> {
        match self {
            Self::Serial(matrix) => matrix.clear(),
            Self::Terminal(matrix) => matrix.clear(),
        }
    }
}
