//! A terminal preview of a module, so scenes can be developed without hardware.
//!
//! Each pixel pair is drawn as one half-block character, which keeps a 9x34
//! panel to 17 terminal lines and roughly square pixels.

use std::io::Write;
use std::sync::Once;

use anyhow::{Context, Result};

use crate::canvas::{self, Canvas};
use crate::device::{BW_THRESHOLD, ColorMode, Matrix};

/// Hide the cursor and clear the screen, once per process.
static SCREEN_SETUP: Once = Once::new();

/// Row the panel grid starts on, leaving room for the label.
const GRID_TOP: usize = 2;

/// A panel rendered to the terminal instead of to hardware.
pub struct TerminalMatrix {
    label: &'static str,
    column: usize,
    mode: ColorMode,
    brightness: u8,
    buffer: String,
}

impl TerminalMatrix {
    /// Creates a preview panel whose left edge sits at terminal `column`.
    #[must_use]
    pub fn new(label: &'static str, column: usize, mode: ColorMode) -> Self {
        SCREEN_SETUP.call_once(|| {
            // Hide the cursor and clear the screen. Failure here is cosmetic.
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(b"\x1b[?25l\x1b[2J");
            let _ = out.flush();
        });

        Self {
            label,
            column,
            mode,
            brightness: 255,
            buffer: String::with_capacity(8 * 1024),
        }
    }

    /// Scales a pixel by the emulated global brightness.
    ///
    /// In [`ColorMode::Bw`] the pixel is thresholded first, so the preview shows
    /// what the module would show rather than a nicer greyscale version of it.
    fn shade(&self, value: u8) -> u8 {
        let value = match self.mode {
            ColorMode::Greyscale => value,
            ColorMode::Bw if value >= BW_THRESHOLD => u8::MAX,
            ColorMode::Bw => 0,
        };
        u8::try_from((u32::from(value) * u32::from(self.brightness)) / 255).unwrap_or(u8::MAX)
    }

    /// Renders the canvas into the internal escape-sequence buffer.
    fn compose(&mut self, canvas: &Canvas) {
        use std::fmt::Write as _;

        let label = self.label;
        let column = self.column;
        self.buffer.clear();
        let _ = write!(self.buffer, "\x1b[1;{column}H\x1b[0m{label}");

        for (line, y) in (0..canvas::HEIGHT).step_by(2).enumerate() {
            let row = GRID_TOP + line;
            let _ = write!(self.buffer, "\x1b[{row};{column}H");
            for x in 0..canvas::WIDTH {
                let top = self.shade(canvas.get(x, y));
                let bottom = self.shade(canvas.get(x, y + 1));
                let _ = write!(
                    self.buffer,
                    "\x1b[38;2;{top};{top};{top}m\x1b[48;2;{bottom};{bottom};{bottom}m\u{2580}\u{2580}"
                );
            }
            self.buffer.push_str("\x1b[0m");
        }
    }

    /// Writes the composed buffer out as a single locked write.
    ///
    /// Both panels share stdout, so a partial write would interleave escape
    /// sequences and scramble the picture.
    fn flush_buffer(&self) -> Result<()> {
        let mut out = std::io::stdout().lock();
        out.write_all(self.buffer.as_bytes())
            .context("writing the terminal preview")?;
        out.flush().context("flushing the terminal preview")
    }
}

impl Matrix for TerminalMatrix {
    fn set_brightness(&mut self, level: u8) -> Result<()> {
        self.brightness = level;
        Ok(())
    }

    fn draw(&mut self, canvas: &Canvas) -> Result<()> {
        self.compose(canvas);
        self.flush_buffer()
    }

    fn clear(&mut self) -> Result<()> {
        use std::fmt::Write as _;

        self.compose(&Canvas::new());
        // Park the cursor below the panels and bring it back for the shell.
        let bottom = GRID_TOP + canvas::ROWS / 2 + 1;
        let _ = writeln!(self.buffer, "\x1b[{bottom};1H\x1b[?25h");
        self.flush_buffer()
    }
}

#[cfg(test)]
mod tests {
    use super::TerminalMatrix;
    use crate::canvas::Canvas;
    use crate::device::{ColorMode, Matrix};

    #[test]
    fn a_frame_positions_every_row_of_the_panel() {
        let mut matrix = TerminalMatrix::new("left", 3, ColorMode::Greyscale);
        matrix.compose(&Canvas::new());
        // 34 rows drawn two at a time, plus the label line.
        assert!(matrix.buffer.matches("\x1b[").count() > 17);
        assert!(matrix.buffer.contains("left"));
        assert!(matrix.buffer.contains("\u{2580}"));
    }

    #[test]
    fn brightness_dims_the_preview() {
        let mut matrix = TerminalMatrix::new("left", 3, ColorMode::Greyscale);
        let mut canvas = Canvas::new();
        canvas.set(0, 0, 255);

        matrix.set_brightness(255).unwrap();
        matrix.compose(&canvas);
        assert!(matrix.buffer.contains("38;2;255;255;255"));

        matrix.set_brightness(51).unwrap();
        matrix.compose(&canvas);
        assert!(matrix.buffer.contains("38;2;51;51;51"));
    }

    #[test]
    fn shading_never_overflows() {
        let mut matrix = TerminalMatrix::new("left", 3, ColorMode::Greyscale);
        matrix.set_brightness(255).unwrap();
        assert_eq!(matrix.shade(255), 255);
        matrix.set_brightness(0).unwrap();
        assert_eq!(matrix.shade(255), 0);
    }
}
