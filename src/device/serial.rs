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
use crate::device::{BW_THRESHOLD, ColorMode, Matrix};

/// Payload length of a black-and-white frame: one bit per LED, rounded up.
const DRAW_BYTES: usize = 39;

const _: () = assert!(DRAW_BYTES * 8 >= crate::canvas::PIXELS);

/// Prefix identifying a command to the module.
const MAGIC: [u8; 2] = [0x32, 0xAC];

/// Set the global brightness.
const CMD_BRIGHTNESS: u8 = 0x00;
/// Put the module to sleep, or wake it up.
const CMD_SLEEP: u8 = 0x03;
/// Draw a whole black-and-white image in one command.
const CMD_DRAW_BW: u8 = 0x06;
/// Stage one greyscale column in the module's back buffer.
const CMD_STAGE_COLUMN: u8 = 0x07;
/// Commit every staged column to the panel.
const CMD_COMMIT_COLUMNS: u8 = 0x08;

/// The port is USB CDC, so the baud rate is ignored — this is a placeholder the
/// serial API insists on.
const BAUD_RATE: u32 = 115_200;

/// A write should never block for long; a stalled module is a bug, not a wait.
const WRITE_TIMEOUT: Duration = Duration::from_millis(250);

/// The port type used against real hardware.
///
/// `serialport::new().open()` hands back a boxed trait object and the crate
/// exposes no concrete type to name, so this one indirection is forced on us.
type Port = Box<dyn serialport::SerialPort>;

/// A real LED matrix module reached over a serial port.
///
/// Generic over where the bytes go so the wire protocol can be tested against a
/// buffer. Both protocol bugs this module has had — a batched frame, and partial
/// column updates — were invisible from the outside: every write returned
/// success while the panel showed the wrong thing. The only way to catch them is
/// to assert on the exact byte stream.
pub struct SerialMatrix<W = Port> {
    port: W,
    mode: ColorMode,
    frame: Vec<u8>,
    /// What the panel is known to be showing, or `None` when that is unknown.
    ///
    /// Only used to skip a frame identical to the last one. See [`Self::draw`].
    displayed: Option<Canvas>,
}

impl SerialMatrix<Port> {
    /// Opens the module at `path`, e.g. `/dev/ttyACM0`.
    ///
    /// # Errors
    ///
    /// Fails if the port cannot be opened — usually a wrong path, or missing
    /// permissions on the device node.
    pub fn open(path: &str, mode: ColorMode) -> Result<Self> {
        let port = serialport::new(path, BAUD_RATE)
            .timeout(WRITE_TIMEOUT)
            .open()
            .with_context(|| format!("opening LED matrix at {path}"))?;

        info!(device = path, %mode, "opened LED matrix");
        Ok(Self::new(port, mode))
    }
}

impl<W: Write> SerialMatrix<W> {
    /// Wraps a byte sink as a module, waking it up.
    ///
    /// The wake happens here rather than in [`Self::open`] so that every way of
    /// building a module goes through it — including the tests, which is the
    /// only way to prove it is still being sent.
    fn new(port: W, mode: ColorMode) -> Self {
        let mut matrix = Self {
            port,
            mode,
            frame: Vec::with_capacity(MAX_COMMAND_LEN),
            // Nothing is known about what the module is showing yet, so the
            // first frame is sent in full.
            displayed: None,
        };

        // A sleeping module stops draining its USB buffer, so the first frames
        // sent to it time out instead of being drawn. Waking it up front turns
        // that into a single command that may fail rather than a dead panel.
        if let Err(error) = matrix.wake() {
            warn!(?error, "could not wake the module");
        }

        matrix
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

impl<W: Write> Matrix for SerialMatrix<W> {
    fn set_brightness(&mut self, level: u8) -> Result<()> {
        debug!(level, "setting brightness");
        self.send(CMD_BRIGHTNESS, &[level])
    }

    /// Stages all nine columns, then commits them.
    ///
    /// Two firmware properties dictate this, and breaking either one still
    /// reports success on every write.
    ///
    /// One write per command, never a batched frame: the firmware reads into a
    /// 64-byte buffer and parses exactly one command per read, so a single
    /// 345-byte write of a whole frame gets its first column parsed and
    /// everything after it — including the commit — silently dropped. The panel
    /// then stays dark.
    ///
    /// And every column, every time. Committing does
    /// `grid = col_buffer.clone(); col_buffer = percentage(0)` — it *zeroes* the
    /// staging buffer. Sending only the columns that changed therefore commits a
    /// frame that is black everywhere else, and the panel strobes as each frame
    /// shows just the pixels that moved. There is no partial update to be had
    /// here; the only frame worth skipping is one that is identical to the last.
    fn draw(&mut self, canvas: &Canvas) -> Result<()> {
        if self.displayed.as_ref() == Some(canvas) {
            return Ok(());
        }

        // Until the frame lands, the panel's contents are unknown.
        self.displayed = None;

        match self.mode {
            ColorMode::Greyscale => {
                for x in 0..canvas::WIDTH {
                    self.frame.clear();
                    encode_column(canvas, x, &mut self.frame);
                    self.port
                        .write_all(&self.frame)
                        .with_context(|| format!("staging column {x}"))?;
                }
                self.send(CMD_COMMIT_COLUMNS, &[])
                    .context("writing frame to the module")?;
            }
            ColorMode::Bw => {
                self.send(CMD_DRAW_BW, &encode_bw(canvas))
                    .context("writing frame to the module")?;
            }
        }

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

/// Bit-packs the canvas as the payload of one `DisplayBwImage` command.
///
/// Bit `x + WIDTH * y`, least significant bit first, lights the LED at column
/// `x` — the same way round as the greyscale path.
///
/// This used to flip the axis, on the belief that the firmware mirrored this
/// command and had to be compensated for. It does not. Nothing caught it for
/// months because every scene that runs in black and white — two paddles, a
/// snake, falling blocks, a rank of invaders — is near enough symmetric that a
/// reflection is invisible. It took writing a digit on the panel, the first
/// thing drawn here with a right way round, to see it.
fn encode_bw(canvas: &Canvas) -> [u8; DRAW_BYTES] {
    let mut bytes = [0u8; DRAW_BYTES];

    for y in 0..canvas::HEIGHT {
        for x in 0..canvas::WIDTH {
            if canvas.get(x, y) < BW_THRESHOLD {
                continue;
            }
            let Ok(index) = usize::try_from(x + canvas::WIDTH * y) else {
                continue;
            };
            bytes[index / 8] |= 1 << (index % 8);
        }
    }

    bytes
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
        CMD_BRIGHTNESS, CMD_COMMIT_COLUMNS, CMD_DRAW_BW, CMD_SLEEP, CMD_STAGE_COLUMN, DRAW_BYTES,
        FIRMWARE_READ_BUFFER, MAGIC, MAX_COMMAND_LEN, ROWS, SerialMatrix, command_frame, encode_bw,
        encode_column,
    };
    use crate::canvas::Canvas;
    use crate::device::{BW_THRESHOLD, ColorMode, Matrix};

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

    /// A port that keeps each write separate.
    ///
    /// The boundaries are the point: the firmware parses one command per read,
    /// so a test that only sees the concatenated bytes cannot tell a correct
    /// frame from one batched into a single write.
    #[derive(Default)]
    struct Recorder {
        writes: Vec<Vec<u8>>,
    }

    impl std::io::Write for Recorder {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.writes.push(buf.to_vec());
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_frame_is_nine_column_writes_then_a_commit() {
        // The regression test for two separate bugs that each left the panel
        // wrong while every write reported success: batching the whole frame
        // into one write, and staging only the columns that changed. Committing
        // zeroes the firmware's staging buffer, so a frame is all nine columns
        // or it is a strobe.
        let mut matrix = matrix(ColorMode::Greyscale);
        matrix.draw(&Canvas::new()).expect("draw into the recorder");

        let writes = &matrix.port.writes;
        assert_eq!(writes.len(), 10, "expected nine columns and a commit");

        for (x, command) in writes.iter().take(9).enumerate() {
            assert_eq!(
                command.len(),
                MAX_COMMAND_LEN,
                "column {x} is the wrong size"
            );
            assert_eq!(command[0..2], MAGIC, "column {x} lost its magic prefix");
            assert_eq!(command[2], CMD_STAGE_COLUMN, "write {x} is not a column");
            assert_eq!(command[3], u8::try_from(x).unwrap(), "wrong column index");
        }
        assert_eq!(writes[9], [MAGIC[0], MAGIC[1], CMD_COMMIT_COLUMNS]);
    }

    /// A module with the wake-up write already dropped, so the write counts
    /// below describe only what the test itself triggered.
    fn matrix(mode: ColorMode) -> SerialMatrix<Recorder> {
        let mut matrix = SerialMatrix::new(Recorder::default(), mode);
        matrix.port.writes.clear();
        matrix
    }

    #[test]
    fn a_new_module_is_woken_before_anything_else() {
        // A module left asleep does not drain its USB buffer, so the first
        // frames time out instead of being drawn — which used to kill the panel
        // outright. Nothing else proves this command is still sent.
        let matrix = SerialMatrix::new(Recorder::default(), ColorMode::Bw);

        let first = matrix.port.writes.first().expect("nothing was sent");
        assert_eq!(first, &[MAGIC[0], MAGIC[1], CMD_SLEEP, 0]);
    }

    #[test]
    fn a_black_and_white_frame_is_a_single_write() {
        // The entire point of the mode: one command instead of ten, which is
        // what lifts the panel off its ~6 fps ceiling.
        let mut matrix = matrix(ColorMode::Bw);
        matrix.draw(&Canvas::new()).unwrap();

        let writes = &matrix.port.writes;
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0][0..2], MAGIC);
        assert_eq!(writes[0][2], CMD_DRAW_BW);
        assert_eq!(writes[0].len(), MAGIC.len() + 1 + DRAW_BYTES);
        assert!(writes[0].len() <= FIRMWARE_READ_BUFFER);
    }

    #[test]
    fn black_and_white_bits_land_where_the_firmware_looks_for_them() {
        // Bit `x + WIDTH * y` lights column `x`, the same way round as the
        // greyscale path. This used to assert the opposite, and so kept a
        // mirrored picture in place: every black and white scene is close
        // enough to symmetric that the reflection never showed, until a digit
        // was written on the panel.
        let mut canvas = Canvas::new();
        canvas.set(0, 0, 255);

        let bytes = encode_bw(&canvas);
        assert_eq!(bytes[0], 0b0000_0001, "column 0 did not set bit 0");
        assert_eq!(bytes[1], 0, "a lone pixel lit a second byte");
    }

    #[test]
    fn a_picture_survives_the_wire_unchanged() {
        // Decoding the payload back and comparing it to what went in is the
        // only check that catches a whole-axis mistake: every other test here
        // asserts against the encoder's own idea of the format, which is
        // exactly what was wrong for six days. Text is the case that matters,
        // because it is the only thing drawn that has a right way round.
        let mut canvas = Canvas::new();
        crate::font::draw_digit(&mut canvas, 2, 0, 0, 255);
        crate::font::draw_digit(&mut canvas, 7, 5, 28, 255);
        canvas.set(0, 17, 255);
        canvas.set(crate::canvas::WIDTH - 1, 33, 255);

        let bytes = encode_bw(&canvas);
        for y in 0..crate::canvas::HEIGHT {
            for x in 0..crate::canvas::WIDTH {
                let index = usize::try_from(x + crate::canvas::WIDTH * y).expect("a panel");
                let lit = bytes[index / 8] & (1 << (index % 8)) != 0;
                assert_eq!(
                    lit,
                    canvas.get(x, y) >= BW_THRESHOLD,
                    "the wire disagrees with the canvas at {x},{y}"
                );
            }
        }
    }

    #[test]
    fn the_two_modes_agree_on_which_way_round_the_panel_is() {
        // The bug this replaces lived between the two paths: greyscale drew a
        // picture and black and white drew its reflection. Nothing compared
        // them, so nothing noticed.
        let mut canvas = Canvas::new();
        canvas.set(0, 5, 255);

        let bytes = encode_bw(&canvas);
        let mut column = Vec::new();
        encode_column(&canvas, 0, &mut column);

        // Greyscale names the column outright; black and white has to agree.
        assert_eq!(
            column[MAGIC.len() + 1],
            0,
            "greyscale staged another column"
        );
        let index = usize::try_from(crate::canvas::WIDTH * 5).expect("a panel");
        assert!(
            bytes[index / 8] & (1 << (index % 8)) != 0,
            "black and white lit a different column from greyscale"
        );
    }

    #[test]
    fn black_and_white_covers_the_far_corner() {
        let mut canvas = Canvas::new();
        canvas.set(8, 33, 255);

        let bytes = encode_bw(&canvas);
        // Column 8 of the last row is index 8 + 9 * 33 = 305, the last bit
        // of the payload: the far corner is what catches an encoder that is
        // one pixel or one axis out.
        assert_eq!(bytes[305 / 8] & (1 << (305 % 8)), 1 << (305 % 8));
    }

    #[test]
    fn black_and_white_thresholds_dim_pixels_away() {
        let mut canvas = Canvas::new();
        canvas.set(4, 4, BW_THRESHOLD - 1);
        assert_eq!(encode_bw(&canvas), [0u8; DRAW_BYTES], "a dim pixel lit up");

        canvas.set(4, 4, BW_THRESHOLD);
        assert_ne!(
            encode_bw(&canvas),
            [0u8; DRAW_BYTES],
            "a lit pixel vanished"
        );
    }

    #[test]
    fn the_snake_tail_survives_the_threshold_but_the_midline_does_not() {
        // The two values the threshold was picked around; if either flips, the
        // scenes lose their tail or grow a permanent stripe.
        let mut canvas = Canvas::new();
        canvas.set(0, 0, 45); // snake tail
        canvas.set(1, 0, 18); // pong midline

        let bytes = encode_bw(&canvas);
        assert_ne!(bytes, [0u8; DRAW_BYTES], "the snake tail went dark");
        assert_eq!(bytes[0], 0b0000_0001, "only the tail should be lit");
        assert_eq!(bytes[1], 0);
    }

    #[test]
    fn every_write_fits_in_one_firmware_read() {
        let mut matrix = matrix(ColorMode::Greyscale);
        matrix.set_brightness(40).unwrap();
        matrix.wake().unwrap();
        matrix.draw(&Canvas::new()).unwrap();

        for command in &matrix.port.writes {
            assert!(
                command.len() <= FIRMWARE_READ_BUFFER,
                "a {}-byte write cannot be read whole",
                command.len()
            );
        }
    }

    #[test]
    fn an_unchanged_frame_is_not_resent() {
        // Worth skipping because the module drains only about 60 commands a
        // second: a still picture should cost nothing.
        let mut matrix = matrix(ColorMode::Greyscale);
        let canvas = Canvas::new();

        matrix.draw(&canvas).unwrap();
        matrix.draw(&canvas).unwrap();

        assert_eq!(matrix.port.writes.len(), 10, "resent an identical frame");
    }

    #[test]
    fn a_one_pixel_change_still_resends_every_column() {
        let mut matrix = matrix(ColorMode::Greyscale);
        matrix.draw(&Canvas::new()).unwrap();

        let mut moved = Canvas::new();
        moved.set(3, 10, 255);
        matrix.draw(&moved).unwrap();

        assert_eq!(matrix.port.writes.len(), 20, "partial update would strobe");
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
