//! The Framework LED Matrix wire protocol, over USB CDC.
//!
//! Every command is `[0x32, 0xAC, <command>, <args...>]`. A frame is sent as
//! nine staged columns followed by a commit, so the module swaps the whole
//! image at once instead of tearing halfway through.

use std::io::Write;
use std::time::Duration;

use anyhow::{Context, Result};
use tracing::{debug, info};

use crate::canvas::{self, Canvas, ROWS};
use crate::device::Matrix;

/// Prefix identifying a command to the module.
const MAGIC: [u8; 2] = [0x32, 0xAC];

/// Set the global brightness.
const CMD_BRIGHTNESS: u8 = 0x00;
/// Stage one greyscale column in the module's back buffer.
const CMD_STAGE_COLUMN: u8 = 0x07;
/// Commit every staged column to the panel.
const CMD_COMMIT_COLUMNS: u8 = 0x08;

/// The port is USB CDC, so the baud rate is ignored — this is a placeholder the
/// serial API insists on.
const BAUD_RATE: u32 = 115_200;

/// A write should never block for long; a stalled module is a bug, not a wait.
const WRITE_TIMEOUT: Duration = Duration::from_millis(250);

/// A real LED matrix module reached over a serial port.
pub struct SerialMatrix {
    // `serialport::new().open()` hands back a boxed trait object; the crate
    // exposes no concrete type to name here, so static dispatch is impossible.
    port: Box<dyn serialport::SerialPort>,
    frame: Vec<u8>,
}

impl SerialMatrix {
    /// Opens the module at `path`, e.g. `/dev/ttyACM0`.
    ///
    /// # Errors
    ///
    /// Fails if the port cannot be opened — usually a wrong path, or missing
    /// permissions on the device node.
    pub fn open(path: &str) -> Result<Self> {
        let port = serialport::new(path, BAUD_RATE)
            .timeout(WRITE_TIMEOUT)
            .open()
            .with_context(|| format!("opening LED matrix at {path}"))?;

        info!(device = path, "opened LED matrix");
        Ok(Self {
            port,
            frame: Vec::with_capacity(frame_len()),
        })
    }

    /// Sends a command with no arguments beyond its id.
    fn send(&mut self, command: u8, args: &[u8]) -> Result<()> {
        let mut message = Vec::with_capacity(MAGIC.len() + 1 + args.len());
        message.extend_from_slice(&MAGIC);
        message.push(command);
        message.extend_from_slice(args);
        self.port
            .write_all(&message)
            .with_context(|| format!("sending command {command:#04x}"))?;
        Ok(())
    }
}

impl Matrix for SerialMatrix {
    fn set_brightness(&mut self, level: u8) -> Result<()> {
        debug!(level, "setting brightness");
        self.send(CMD_BRIGHTNESS, &[level])
    }

    fn draw(&mut self, canvas: &Canvas) -> Result<()> {
        self.frame.clear();
        encode_frame(canvas, &mut self.frame);
        self.port
            .write_all(&self.frame)
            .context("writing frame to the module")?;
        Ok(())
    }

    fn clear(&mut self) -> Result<()> {
        debug!("clearing panel");
        self.draw(&Canvas::new())
    }
}

/// Length of one encoded frame: nine staged columns plus the commit.
fn frame_len() -> usize {
    let column = MAGIC.len() + 2 + ROWS;
    let commit = MAGIC.len() + 1;
    column * 9 + commit
}

/// Serialises a canvas as staged columns followed by a commit.
fn encode_frame(canvas: &Canvas, out: &mut Vec<u8>) {
    for x in 0..canvas::WIDTH {
        out.extend_from_slice(&MAGIC);
        out.push(CMD_STAGE_COLUMN);
        // The column index is `0..9`, so this conversion cannot fail.
        out.push(u8::try_from(x).unwrap_or(0));
        out.extend_from_slice(&canvas.column(x));
    }
    out.extend_from_slice(&MAGIC);
    out.push(CMD_COMMIT_COLUMNS);
}

#[cfg(test)]
mod tests {
    use super::{CMD_COMMIT_COLUMNS, CMD_STAGE_COLUMN, MAGIC, ROWS, encode_frame, frame_len};
    use crate::canvas::Canvas;

    fn encode(canvas: &Canvas) -> Vec<u8> {
        let mut out = Vec::new();
        encode_frame(canvas, &mut out);
        out
    }

    #[test]
    fn a_frame_has_the_advertised_length() {
        assert_eq!(encode(&Canvas::new()).len(), frame_len());
    }

    #[test]
    fn columns_are_staged_in_order_then_committed() {
        let frame = encode(&Canvas::new());
        let stride = MAGIC.len() + 2 + ROWS;

        for x in 0..9 {
            let header = &frame[x * stride..x * stride + 4];
            assert_eq!(header[0], MAGIC[0]);
            assert_eq!(header[1], MAGIC[1]);
            assert_eq!(header[2], CMD_STAGE_COLUMN);
            assert_eq!(
                header[3],
                u8::try_from(x).unwrap(),
                "column {x} announced the wrong index"
            );
        }

        let commit = &frame[frame.len() - 3..];
        assert_eq!(commit, [MAGIC[0], MAGIC[1], CMD_COMMIT_COLUMNS]);
    }

    #[test]
    fn pixels_land_at_the_right_offset() {
        let mut canvas = Canvas::new();
        canvas.set(0, 0, 0xAA);
        canvas.set(8, 33, 0xBB);

        let frame = encode(&canvas);
        let stride = MAGIC.len() + 2 + ROWS;
        let payload = MAGIC.len() + 2;

        assert_eq!(frame[payload], 0xAA, "top-left pixel");
        assert_eq!(frame[8 * stride + payload + 33], 0xBB, "bottom-right pixel");
    }

    #[test]
    fn encoding_reuses_the_buffer_without_appending() {
        let mut out = Vec::new();
        encode_frame(&Canvas::new(), &mut out);
        let first = out.len();
        out.clear();
        encode_frame(&Canvas::new(), &mut out);
        assert_eq!(out.len(), first);
    }
}
