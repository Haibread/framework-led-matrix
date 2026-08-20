# framework-led-matrix

Drives the two LED Matrix input modules of a Framework 16 with self-playing
games and widgets.

Each module is a 9x34 grid of LEDs with 8-bit brightness per LED, exposed as a
USB serial device. `ledmat` renders every frame on the host and pushes raw
pixels, so a scene can be anything you can draw — the firmware's built-in
patterns are not involved.

Widgets adapt to the room they are given. A panel shows a stack of one or more
scenes — `clock,cpu,battery` — and each declares the rows it needs, then
draws differently depending on what it gets. A clock alone takes the whole
panel and uses a 4x7 face; the same clock in a stack of three falls back to 3x5
and drops its seconds bar. One implementation, both renderings.

Colour mode is a property of the scene, not a global setting: a game wants
motion and takes black and white, while a widget that changes once a second can
afford the shading. `--color-mode` overrides that per run.

Nine scenes ship today. Four games, playing on their own:

- **pong** — two robot paddles with capped speed, a reaction delay and a fresh
  aiming error each exchange, so rallies end in actual points instead of running
  forever.
- **snake** — plans a path to the food, simulates walking it, and only commits
  if it can still reach its own tail afterwards. When nothing is safe it chases
  its tail until the board opens up. A run ends at forty cells, past which the
  snake reads as a tangle rather than a line; the body then retracts into the
  head and a new game starts. Dying, which is rare, flashes instead.
- **invaders** — nine columns is exactly a rank of invaders, which is why this
  one fits a panel that defeats most games. The gunner leads its shots, because
  a bullet takes most of a second to climb and the rank walks a pixel a step,
  and it shelters behind the bunkers rather than running the length of the
  panel. Three lives, and waves that cycle through four different ranks: a rank
  spanning most of the width turns at a wall every other step and descends with
  every turn, so the wide ones are the ones that come down on you.
- **tetris** — tries every rotation in every column, drops the piece and scores
  the well it would leave behind. Buried holes weigh heaviest, because they are
  what actually ends a game; a bot that only chased line clears would top out in
  under a minute.

And five widgets, which read the machine rather than play. Each takes what it is
given and shows more detail with more rows:

| Scene | Least it needs | What it does with more |
| --- | --- | --- |
| `clock` | 11 rows | 4x7 digits from 15 rows, a seconds bar from 17 |
| `cpu` | 3 rows | the figure itself from 6 rows, a sliding history from 11 |
| `ram` | 3 rows | the same, one shade dimmer so the two read apart |
| `net` | 7 rows | out above the rule, in below; deeper histories with room |
| `disk` | 7 rows | the same grammar as `net`: two widgets, one thing to learn |
| `battery` | 5 rows | stands upright from 20 rows instead of lying down |
| `volume` | 5 rows | the speaker cone grows with the band, over a full-width bar |
| `speakers-spectrum` | 8 rows | nine bands of what is coming **out** of the sound card |
| `mic-spectrum` | 8 rows | the same nine bands, listening to the microphone instead |
| `off` | — | nothing; blanks a panel without stopping its thread |

`cpu,ram` composes exactly what a combined gauge would draw, so there is no
separate widget for it — the stack does the composing.

`speakers-spectrum` and `mic-spectrum` are the same widget pointed at opposite
ends of the sound card: one draws what is being played, the other what is being
heard. The output one was called `spectrum`, which said neither; that spelling
and the short `speakers` / `mic` both still parse.
Silence draws a lit floor rather than nothing, so a quiet machine does not look
like a broken widget.

Network and disk are on a logarithmic scale: traffic spans six orders of
magnitude between a keepalive and a download, and a linear scale would leave
everything but the peak a single row tall.

## Requirements

- Rust 1.97.1 (pinned in `rust-toolchain.toml`)
- A Framework 16 with at least one LED Matrix module
- `pre-commit` and `actionlint` for the git hooks
- `wpctl` and `parec` at runtime, for the `volume` and spectrum widgets

No system libraries are needed to build: the serial dependency's `libudev`
feature is off, since the modules are opened by path and never enumerated.

## Install

```bash
cargo install --path . --root ~/.local
```

The binary is called `ledmat`, and lands in `~/.local/bin` — which is on `PATH`
on most desktop setups, and is where the systemd unit below expects it. Plain
`cargo install --path .` puts it in `~/.cargo/bin` instead, which is often not
on `PATH` at all; if you prefer that, point `ExecStart` at it.

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

## Control it while it runs

The daemon listens on a Unix socket, so scenes can be swapped without a
restart:

```bash
ledmat status
ledmat set left clock
ledmat set right clock,cpu,battery
ledmat brightness 60
```

Scenes separated by commas stack from the top, with a dotted rule between them.
A stack that cannot fit is refused outright rather than silently truncated:

```
$ ledmat set left pong,snake
Error: these scenes need 69 rows and the panel has 34
```

`set <panel> off` blanks a panel and keeps its thread — unlike `--left-scene
off` at startup, which never opens that module at all.

The socket lives at `$XDG_RUNTIME_DIR/ledmat.sock` and speaks one line in, one
line out, so it is equally usable by hand:

```bash
echo status | socat - UNIX-CONNECT:$XDG_RUNTIME_DIR/ledmat.sock
```

Switching scenes can change the colour mode — a game wants `bw`, a widget wants
`greyscale` — and since the mode is fixed when the serial port is opened, the
panel is reopened underneath when that happens.

## Compose it in a terminal

`ledmat set` is fine once you know what you want. Working out what you want is
easier with the panels in front of you:

```bash
ledmat tui
```

Both panels are mirrored live, twenty frames a second, next to the catalogue of
scenes. Build a stack, watch the row budget, apply it when it looks right.

Picking one scene is a move and `Enter`. `Space` is only for stacking several,
and refuses a scene there is no room for at the keystroke rather than at
`Enter` — a game fills all 34 rows on its own, so nothing stacks onto one.

| Key | What it does |
| --- | --- |
| `Tab` | Switch which panel you are composing |
| `↑` `↓` / `k` `j` | Move through the scenes |
| `Enter` | Show the selected scene — or the draft stack, if you started one |
| `Space` | Add the selected scene to a draft stack |
| `x` / `Backspace` | Drop the last scene from the draft |
| `c` / `Esc` | Drop the draft |
| `+` `-` | Brightness, five at a time |
| `?` | The same list, over the interface |
| `q` / `Ctrl-C` | Leave |

The pixels are drawn square, which a terminal cell is not: a cell counts as one
unit across and two down, so a pixel spans as many cells across as it does
half-lines down. That gives four sizes — a panel of 9x17, 18x34, 27x51 or
36x68 — and the largest the window can hold is the one used. Below roughly 40
by 20 the interface says it has no room rather than quietly clipping a panel.

Everything else keeps its own size too, so a large screen gets margins around a
centred block rather than a catalogue stretched across half a monitor.

The bottom line carries the whole state: the draft, the rows it needs against
the 34 there are, the brightness, whatever the daemon last answered, and the
keys. A draft that does not fit says how many rows it is short before you apply
it. A message from the daemon joins that line instead of replacing it, and the
next keystroke puts it away.

The mirror is one more verb on the same socket. `watch` never returns; it
writes a line per frame per panel, 612 hex characters, two per pixel:

```bash
echo watch | socat - UNIX-CONNECT:$XDG_RUNTIME_DIR/ledmat.sock
```

```
frame left 000000...1f1f1f...
frame right 000000...
```

## It remembers

Whatever `ledmat set` and `ledmat brightness` last applied is written down and
restored on the next start, so a reboot does not undo an afternoon of fiddling.
The file is three keys and a comment, meant to be readable:

```
left=clock
right=battery
brightness=45
```

Anything given explicitly wins over it — a flag, or an environment variable —
then the saved setup, then the defaults. That order is why the shipped systemd
unit sets no scene: pinning one there would quietly defeat the whole thing.

## Configuration

Every option can be set as a flag or an environment variable. Flags win.

| Flag | Environment variable | Default | Meaning |
| --- | --- | --- | --- |
| `--left-device` | `LEFT_DEVICE` | `/dev/led-matrix-left` | Serial device of the left module |
| `--right-device` | `RIGHT_DEVICE` | `/dev/led-matrix-right` | Serial device of the right module |
| `--left-scene` | `LEFT_SCENE` | saved, else `pong` | one or more scenes, comma separated |
| `--right-scene` | `RIGHT_SCENE` | saved, else `snake` | one or more scenes, comma separated |
| `--brightness` | `BRIGHTNESS` | saved, else `30` | 0 to 255; the modules sit under your hands, past ~80 is a desk lamp |
| `--fps` | `FPS` | `30` | 1 to 60 |
| `--color-mode` | `COLOR_MODE` | `auto` | `auto` (per scene), `greyscale` (shading, ~6 fps) or `bw` (no shading, ~30 fps) |
| `--simulate` | `SIMULATE` | `false` | Draw in the terminal instead of on the modules |
| `--seed` | `SEED` | — | Seed the scenes for a reproducible run |
| `--log-filter` | `LOG_FILTER` | `info` | `tracing` filter directive |
| `--socket` | `SOCKET_PATH` | `$XDG_RUNTIME_DIR/ledmat.sock` | Where the control socket lives |
| `--state` | `STATE_PATH` | `$XDG_STATE_HOME/ledmat/state` | Where the current setup is remembered |

## Run it in the background

A user unit is provided:

```bash
cargo install --path . --root ~/.local
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
| `src/system.rs` | Reading the processor, memory and battery |
| `src/state.rs` | Remembering the setup across restarts |
| `src/runner.rs` | The fixed-rate loop driving one panel |
| `src/control.rs` | The socket protocol, shared by both ends |
| `src/server.rs` | The daemon side of the socket |
| `src/tui.rs` | The terminal composer, `ledmat tui` |

### Adding a scene

1. Implement `Scene` (`name`, `min_height`, `update(delta)`, `render(canvas,
   area)`) in `src/scene/<name>.rs`. Draw relative to `area.top`, never past
   `area.bottom()`, and use `area.height` to decide how much detail to show —
   a widget that overflows its band scribbles on its neighbour.
2. Add it to `SceneKind` and `AnyScene` in `src/scene.rs`, and give it a colour
   mode in `SceneKind::preferred_color_mode`. Anything that changes more than a
   few times a second wants `Bw`; a widget wants `Greyscale`, which costs
   nothing because an unchanged frame is never resent.
3. Add its name to `parse_scene` in `src/control.rs` so the socket accepts it.

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
