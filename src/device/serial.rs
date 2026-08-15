//! The Framework LED Matrix wire protocol, over USB CDC.
//!
//! Every command is `[0x32, 0xAC, <command>, <args...>]`. A frame is sent as
//! nine staged columns followed by a commit, so the module swaps the whole
//! image at once instead of tearing halfway through.

use std::io::Write;
use std::time::Duration;

use anyhow::{Context, Result};
use tracing::{debug, info, warn};

use crate::canvas::{self, Canvas, ROWS};
use crate::device::Matrix;

/// Prefix identifying a command to the module.
const MAGIC: [u8; 2] = [0x32, 0xAC];

/// Set the global brightness.
const CMD_BRIGHTNESS: u8 = 0x00;
/// Put the module to sleep, or wake it up.
const CMD_SLEEP: u8 = 0x03;
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
    /// What the panel is known to be showing, or `None` when that is unknown.
    ///
    /// Kept so only the columns that actually changed are sent. See [`Self::draw`].
    displayed: Option<Canvas>,
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
        let mut matrix = Self {
            port,
            frame: Vec::with_capacity(MAX_COMMAND_LEN),
            // Nothing is known about what the module is showing yet, so the
            // first frame is sent in full.
            displayed: None,
        };

        // A sleeping module stops draining its USB buffer, so the first frames
        // sent to it time out instead of being drawn. Waking it up front turns
        // that into a single command that may fail rather than a dead panel.
        if let Err(error) = matrix.wake() {
            warn!(device = path, ?error, "could not wake the module");
        }

        Ok(matrix)
    }

    /// Tells the module to wake up.
    fn wake(&mut self) -> Result<()> {
        debug!("waking the module");
        self.send(CMD_SLEEP, &[0])
    }

    /// Sends a command with its arguments.
    fn send(&mut self, command: u8, args: &[u8]) -> Result<()> {
        self.port
            .write_all(&command_frame(command, args))
            .with_context(|| format!("sending command {command:#04x}"))?;
        Ok(())
    }
}

/// Builds one command message.
fn command_frame(command: u8, args: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(MAGIC.len() + 1 + args.len());
    message.extend_from_slice(&MAGIC);
    message.push(command);
    message.extend_from_slice(args);
    message
}

impl Matrix for SerialMatrix {
    fn set_brightness(&mut self, level: u8) -> Result<()> {
        debug!(level, "setting brightness");
        self.send(CMD_BRIGHTNESS, &[level])
    }

    /// Sends the columns that changed, then commits them.
    ///
    /// Two constraints shape this.
    ///
    /// One write per command, never a batched frame: the firmware reads into a
    /// 64-byte buffer and parses exactly one command per read, so a single
    /// 345-byte write of a whole frame gets its first column parsed and
    /// everything after it — including the commit — silently dropped. The panel
    /// then stays dark while every write still reports success.
    ///
    /// And it drains roughly one command per main-loop iteration, measured at
    /// about 60 a second. A blind full frame is ten of them, which caps the
    /// panel at 6 fps. Sending only what moved — two to four columns for these
    /// scenes — is what buys back a watchable frame rate.
    fn draw(&mut self, canvas: &Canvas) -> Result<()> {
        let mut staged = 0;

        for x in 0..canvas::WIDTH {
            if !column_changed(self.displayed.as_ref(), canvas, x) {
                continue;
            }

            self.frame.clear();
            encode_column(canvas, x, &mut self.frame);
            if let Err(error) = self.port.write_all(&self.frame) {
                // The panel is now in an unknown state: resend everything next
                // time rather than leaving a stale column behind.
                self.displayed = None;
                return Err(error).with_context(|| format!("staging column {x}"));
            }
            staged += 1;
        }

        if staged == 0 {
            return Ok(());
        }

        if let Err(error) = self.send(CMD_COMMIT_COLUMNS, &[]) {
            self.displayed = None;
            return Err(error).context("writing frame to the module");
        }

        debug!(staged, "committed a frame");
        self.displayed = Some(canvas.clone());
        Ok(())
    }

    fn clear(&mut self) -> Result<()> {
        debug!("clearing panel");
        self.draw(&Canvas::new())
    }
}

/// Length of the longest command sent to the module: one staged column.
const MAX_COMMAND_LEN: usize = MAGIC.len() + 2 + ROWS;

/// Size of the firmware's serial read buffer.
///
/// It parses one command per read, so a command longer than this could never be
/// received whole.
const FIRMWARE_READ_BUFFER: usize = 64;

const _: () = assert!(MAX_COMMAND_LEN <= FIRMWARE_READ_BUFFER);

/// Whether column `x` needs resending, given what the panel is showing.
///
/// An unknown panel state counts as changed: every column is resent.
fn column_changed(displayed: Option<&Canvas>, next: &Canvas, x: i32) -> bool {
    displayed.is_none_or(|shown| shown.column(x) != next.column(x))
}

/// Serialises column `x` of the canvas as one `StageGreyColumn` command.
fn encode_column(canvas: &Canvas, x: i32, out: &mut Vec<u8>) {
    out.extend_from_slice(&MAGIC);
    out.push(CMD_STAGE_COLUMN);
    // The column index is `0..9`, so this conversion cannot fail.
    out.push(u8::try_from(x).unwrap_or(0));
    out.extend_from_slice(&canvas.column(x));
}

#[cfg(test)]
mod tests {
    use super::{
        CMD_BRIGHTNESS, CMD_SLEEP, CMD_STAGE_COLUMN, FIRMWARE_READ_BUFFER, MAGIC, MAX_COMMAND_LEN,
        ROWS, column_changed, command_frame, encode_column,
    };
    use crate::canvas::{self, Canvas};

    fn encode(canvas: &Canvas, x: i32) -> Vec<u8> {
        let mut out = Vec::new();
        encode_column(canvas, x, &mut out);
        out
    }

    #[test]
    fn a_column_command_fits_in_one_firmware_read() {
        // This is the whole reason a frame is nine writes instead of one: the
        // firmware parses a single command per 64-byte read, so anything longer
        // than its buffer, or batched behind another command, is dropped. The
        // budget itself is checked at compile time next to the constants.
        let length = encode(&Canvas::new(), 0).len();
        assert_eq!(length, MAX_COMMAND_LEN);
        assert!(length < FIRMWARE_READ_BUFFER, "{length} bytes will not fit");
    }

    #[test]
    fn a_column_announces_its_index_after_the_magic_prefix() {
        for x in 0..9 {
            let column = encode(&Canvas::new(), x);
            assert_eq!(column[0], MAGIC[0]);
            assert_eq!(column[1], MAGIC[1]);
            assert_eq!(column[2], CMD_STAGE_COLUMN);
            assert_eq!(
                column[3],
                u8::try_from(x).unwrap(),
                "column {x} announced the wrong index"
            );
        }
    }

    #[test]
    fn pixels_land_at_the_right_offset() {
        let mut canvas = Canvas::new();
        canvas.set(0, 0, 0xAA);
        canvas.set(8, 33, 0xBB);

        let payload = MAGIC.len() + 2;
        assert_eq!(encode(&canvas, 0)[payload], 0xAA, "top-left pixel");
        assert_eq!(encode(&canvas, 8)[payload + 33], 0xBB, "bottom-right pixel");
    }

    #[test]
    fn a_column_carries_one_byte_per_row() {
        let column = encode(&Canvas::new(), 4);
        assert_eq!(column.len() - MAGIC.len() - 2, ROWS);
    }

    #[test]
    fn an_unknown_panel_gets_every_column_resent() {
        let canvas = Canvas::new();
        for x in 0..canvas::WIDTH {
            assert!(column_changed(None, &canvas, x));
        }
    }

    #[test]
    fn an_unchanged_panel_gets_nothing_resent() {
        let canvas = Canvas::new();
        for x in 0..canvas::WIDTH {
            assert!(!column_changed(Some(&canvas), &canvas, x));
        }
    }

    #[test]
    fn only_the_columns_that_moved_are_resent() {
        // The whole frame rate rests on this: the module drains about 60
        // commands a second, so a blind ten-command frame caps the panel at 6
        // fps. A moving ball touches two columns, not nine.
        let displayed = Canvas::new();
        let mut next = displayed.clone();
        next.set(3, 10, 255);
        next.set(4, 10, 128);

        let resent: Vec<i32> = (0..canvas::WIDTH)
            .filter(|x| column_changed(Some(&displayed), &next, *x))
            .collect();
        assert_eq!(resent, vec![3, 4]);
    }

    #[test]
    fn a_dimmed_pixel_counts_as_a_change() {
        // Brightness-only differences must not be missed, or the antialiased
        // ball would smear across the panel.
        let mut displayed = Canvas::new();
        displayed.set(5, 20, 255);
        let mut next = displayed.clone();
        next.set(5, 20, 254);

        assert!(column_changed(Some(&displayed), &next, 5));
        assert!(!column_changed(Some(&displayed), &next, 4));
    }

    #[test]
    fn a_command_carries_the_magic_prefix_then_its_arguments() {
        assert_eq!(
            command_frame(CMD_BRIGHTNESS, &[40]),
            vec![MAGIC[0], MAGIC[1], CMD_BRIGHTNESS, 40]
        );
    }

    #[test]
    fn waking_the_module_is_sleep_with_a_zero() {
        // `0` is awake: the argument says whether to sleep, not whether to wake.
        assert_eq!(
            command_frame(CMD_SLEEP, &[0]),
            vec![MAGIC[0], MAGIC[1], 0x03, 0]
        );
    }

    #[test]
    fn encoding_reuses_the_buffer_without_appending() {
        let mut out = Vec::new();
        encode_column(&Canvas::new(), 0, &mut out);
        let first = out.len();
        out.clear();
        encode_column(&Canvas::new(), 0, &mut out);
        assert_eq!(out.len(), first);
    }
}
