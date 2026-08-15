# framework-led-matrix

Drives the two LED Matrix input modules of a Framework 16 with self-playing
games and, in time, widgets.

Each module is a 9x34 grid of LEDs with 8-bit brightness per LED, exposed as a
USB serial device. `ledmat` renders every frame on the host and pushes raw
pixels, so a scene can be anything you can draw — the firmware's built-in
patterns are not involved.

Two scenes ship today, both playing on their own:

- **pong** — two robot paddles with capped speed, a reaction delay and a fresh
  aiming error each exchange, so rallies end in actual points instead of running
  forever.
- **snake** — plans a path to the food, simulates walking it, and only commits
  if it can still reach its own tail afterwards. When nothing is safe it chases
  its tail until the board opens up.

## Requirements

- Rust 1.97.1 (pinned in `rust-toolchain.toml`)
- A Framework 16 with at least one LED Matrix module
- `pre-commit` for the git hooks

## Install

```bash
cargo install --path .
```

The binary is called `ledmat`.

### Device permissions

The modules appear as `/dev/ttyACM*`, owned by `root:uucp` with mode `0660`, so
a normal user cannot open them. The shipped udev rule fixes that and gives each
module a stable name:

```bash
sudo cp packaging/60-framework-led-matrix.rules /etc/udev/rules.d/
sudo udevadm control --reload && sudo udevadm trigger
```

You should then have `/dev/led-matrix-left` and `/dev/led-matrix-right`, both
writable by the user of the active session.

The `60-` prefix matters. `73-seat-late.rules` is what turns the `uaccess` tag
into an ACL, and udev runs rule files in lexical order, so a `99-` file would
set the tag after the only rule that reads it — you would get the symlinks with
no permission behind them.

Both modules report the **same** USB serial number, so the rule tells them apart
by which port they are plugged into. Those port ids are machine-specific: if the
panels come out swapped, swap the two `KERNELS` values in the rule. To check
yours:

```bash
udevadm info -a -n /dev/ttyACM0 | grep -m1 'KERNELS=="[0-9]-'
```

## Run

```bash
ledmat
```

Pong on the left, snake on the right, at 30 fps. Stop it with Ctrl-C: both
panels are cleared on the way out, so nothing stays lit.

No hardware, or no permissions yet? Render to the terminal instead:

```bash
ledmat --simulate
```

Pick what goes where:

```bash
ledmat --left-scene snake --right-scene off --brightness 60
```

## Configuration

Every option can be set as a flag or an environment variable. Flags win.

| Flag | Environment variable | Default | Meaning |
| --- | --- | --- | --- |
| `--left-device` | `LEFT_DEVICE` | `/dev/led-matrix-left` | Serial device of the left module |
| `--right-device` | `RIGHT_DEVICE` | `/dev/led-matrix-right` | Serial device of the right module |
| `--left-scene` | `LEFT_SCENE` | `pong` | `pong`, `snake` or `off` |
| `--right-scene` | `RIGHT_SCENE` | `snake` | `pong`, `snake` or `off` |
| `--brightness` | `BRIGHTNESS` | `30` | 0 to 255; the modules sit under your hands, past ~80 is a desk lamp |
| `--fps` | `FPS` | `30` | 1 to 60 |
| `--color-mode` | `COLOR_MODE` | `greyscale` | `greyscale` (shading, ~6 fps) or `bw` (no shading, ~6x faster) |
| `--simulate` | `SIMULATE` | `false` | Draw in the terminal instead of on the modules |
| `--seed` | `SEED` | — | Seed the scenes for a reproducible run |
| `--log-filter` | `LOG_FILTER` | `info` | `tracing` filter directive |

## Run it in the background

A user unit is provided:

```bash
cargo install --path .
mkdir -p ~/.config/systemd/user
cp packaging/ledmat.service ~/.config/systemd/user/
systemctl --user enable --now ledmat.service
```

Logs go to the journal:

```bash
journalctl --user -u ledmat -f
```

## Development

```bash
pre-commit install
cargo test
cargo clippy --all-targets -- -D warnings
```

Scenes are tested headless: the games are simulated for thousands of steps and
checked for invariants (the ball never leaves the field, the snake never
overlaps itself and survives long enough to be worth watching). `--simulate`
covers the rest, and the mocked `Matrix` trait covers the render loop.

### Layout

| Path | Role |
| --- | --- |
| `src/canvas.rs` | The 9x34 greyscale framebuffer |
| `src/device/serial.rs` | The wire protocol, over USB CDC |
| `src/device/terminal.rs` | The terminal preview used by `--simulate` |
| `src/scene/` | One file per scene |
| `src/runner.rs` | The fixed-rate loop driving one panel |

### Adding a scene

1. Implement `Scene` (`name`, `update(delta)`, `render(canvas)`) in
   `src/scene/<name>.rs`. It gets a cleared canvas and owns nothing else.
2. Add it to `SceneKind` and `AnyScene` in `src/scene.rs`.

That is the whole contract — no serial, no timing, no frame budget to think
about.

## Protocol notes

Commands are `[0x32, 0xAC, <command>, <args...>]`. A frame is up to nine
`StageGreyColumn` (`0x07`) messages, each carrying a column index and 34
brightness bytes, followed by one `CommitColumns` (`0x08`) so the panel swaps
the whole image at once. Brightness is `0x00`.

Two firmware properties dictate how this is sent, and both are easy to trip
over because breaking either one still reports success on every write:

- **One command per write.** The firmware reads into a 64-byte buffer and parses
  exactly one command per read. Batching a whole frame into one 345-byte write
  gets the first column parsed and everything after it — including the commit —
  silently dropped, leaving the panel dark.
- **Every column, every frame.** Committing runs
  `grid = col_buffer.clone(); col_buffer = percentage(0)` — it *zeroes* the
  staging buffer. Sending only the columns that changed therefore commits a
  frame that is black everywhere else, and the panel strobes. There is no
  partial update to be had; the only frame worth skipping is one identical to
  the last, which is what a still picture costs nothing.

This sets a hard ceiling. The module drains roughly 60 commands a second, and a
greyscale frame is ten of them, so the panel tops out at about **6 fps**. The
link is nowhere near saturated — it is the firmware's command rate that binds.

`--color-mode bw` takes the other side of that trade: a `DisplayBwImage`
(`0x06`) frame is a single 39-byte command, so it runs about six times faster
and loses the per-LED brightness the scenes use for the antialiased ball and the
snake's fading body. Pixels at or above brightness 40 count as lit, which keeps
the snake's tail (45) visible and the pong midline (18) dark.

One trap in that path: the firmware reads bit `x + WIDTH * y` and writes it to
column `8 - x`, so it mirrors the image, while the greyscale path does not. The
encoder flips it back, otherwise the two modes show each other's reflection —
which is nearly invisible on symmetric scenes and very annoying to find later.

## Licence

MIT
