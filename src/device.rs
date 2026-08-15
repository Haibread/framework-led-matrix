//! Output backends: the real modules over USB serial, and a terminal preview.

pub mod serial;
pub mod terminal;

use anyhow::Result;

use crate::canvas::Canvas;
use serial::SerialMatrix;
use terminal::TerminalMatrix;

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
