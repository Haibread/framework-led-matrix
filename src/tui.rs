//! Composing panels while watching them.
//!
//! Everything here talks to a running daemon over the same socket `ledmat set`
//! uses; nothing is drawn twice. The preview is the daemon's own frames rather
//! than a second simulation, so what the terminal shows is what the modules
//! show — including the live processor figures and the spectrum, which a second
//! copy could not have without opening a second capture.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::Duration;

use anyhow::{Context, Result};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::{Terminal, prelude::CrosstermBackend};

use crate::canvas::{self, Canvas};
use crate::control::{PanelName, Request, Response, SceneSpec};
use crate::scene::{self, SceneKind};

/// How long the loop waits for a key before redrawing.
///
/// Twenty a second, which is the rate frames arrive at: waiting longer would
/// make the preview stutter, waiting less would spin for nothing.
const TICK: Duration = Duration::from_millis(50);

/// The panel, in pixels. Written out because the canvas counts in `i32` and the
/// terminal in `u16`; a test keeps the two in step.
const PIXELS_ACROSS: u16 = 9;
const PIXELS_DOWN: u16 = 34;

/// Columns the catalogue cannot do without: a cursor, a truncated name and the
/// row count. Below this the panels get the room instead.
const CATALOGUE_MIN_WIDTH: u16 = 18;

/// Columns the longest scene name needs, cursor and row count included.
///
/// Measured rather than written down: the names have already grown once, and a
/// hand-picked number would have quietly clipped `speakers-spectrum`.
fn catalogue_ideal_width() -> u16 {
    let longest = SceneKind::ALL
        .iter()
        .map(|kind| kind.to_string().chars().count())
        .max()
        .unwrap_or(0);
    // "> " + name + " " + two digits, inside a border.
    u16::try_from(longest + 9).unwrap_or(CATALOGUE_MIN_WIDTH)
}

/// The width a scene name is padded to in the catalogue.
fn name_column() -> usize {
    SceneKind::ALL
        .iter()
        .map(|kind| kind.to_string().chars().count())
        .max()
        .unwrap_or(0)
}

/// How big a pixel is drawn.
///
/// A terminal cell is about twice as tall as it is wide. Counting a cell as one
/// unit across and two units down, a pixel is square whenever the cells it
/// spans across equal the half-lines it spans down — 1 and 1, 2 and 2, 4 and 4.
/// Anything else draws the LEDs as rectangles, which is what this used to do.
#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
struct Scale(u16);

impl Scale {
    /// Every size on offer, largest first.
    ///
    /// Three is worth having and is not obvious: at three half-lines a pixel,
    /// pixel boundaries land in the middle of a line. That is fine, because the
    /// half-line is the unit being drawn — and it is what lets a 68-row window
    /// hold a 51-row panel when the 68-row one is three rows out of reach.
    const ALL: [Self; 4] = [Self(4), Self(3), Self(2), Self(1)];

    /// Cells one pixel takes across, which is also the half-lines it takes
    /// down: equal, or the pixel is not square.
    const fn cells_across(self) -> u16 {
        self.0
    }

    /// Lines the pixels themselves occupy.
    const fn lines_down(self) -> u16 {
        PIXELS_DOWN * self.0 / 2
    }

    /// The box a panel needs, borders included.
    const fn box_size(self) -> (u16, u16) {
        (
            self.cells_across() * PIXELS_ACROSS + 2,
            self.lines_down() + 2,
        )
    }

    /// Whether two of these boxes and a catalogue fit, with a row for the state.
    const fn fits(self, area: Rect) -> bool {
        let (width, height) = self.box_size();
        // Strictly taller than the boxes: the extra row is the status line.
        area.width >= width * 2 + CATALOGUE_MIN_WIDTH && area.height > height
    }

    /// The smallest size on offer, which is what "too small" is measured
    /// against.
    fn smallest() -> Self {
        Self::ALL[Self::ALL.len() - 1]
    }

    /// The largest scale this terminal can hold, if any can.
    fn fitting(area: Rect) -> Option<Self> {
        Self::ALL.into_iter().find(|scale| scale.fits(area))
    }
}

/// What the panels are showing and what is being composed for them.
struct App {
    socket: PathBuf,
    /// The last frame each panel sent.
    frames: [Canvas; 2],
    /// What each panel is showing, as the daemon last reported it.
    showing: [Vec<SceneKind>; 2],
    /// The stack being composed for each panel. Empty until something is
    /// picked: starting it off as a copy of what was showing meant the first
    /// `space` appended to a full panel and could not be applied.
    drafts: [Vec<SceneKind>; 2],
    /// Which panel the keys act on.
    focus: usize,
    /// Where the cursor sits in the catalogue.
    selected: usize,
    brightness: u8,
    status: String,
    /// Whether the key list is over everything else.
    helping: bool,
}

impl App {
    /// Starts with whatever the daemon says it is already showing.
    fn new(socket: PathBuf) -> Self {
        let mut app = Self {
            socket,
            frames: [Canvas::new(), Canvas::new()],
            showing: [Vec::new(), Vec::new()],
            drafts: [Vec::new(), Vec::new()],
            focus: 0,
            selected: 0,
            brightness: 30,
            status: String::new(),
            helping: false,
        };
        app.read_status();
        app
    }

    /// The panel the keys act on.
    const fn panel(&self) -> PanelName {
        if self.focus == 0 {
            PanelName::Left
        } else {
            PanelName::Right
        }
    }

    /// Asks the daemon what each panel shows, and starts the drafts there.
    fn read_status(&mut self) {
        let Ok(response) = ask(&self.socket, &Request::Status) else {
            "no daemon listening".clone_into(&mut self.status);
            return;
        };
        // `left=clock,cpu right=tetris`
        for (index, part) in response.message().split_whitespace().enumerate().take(2) {
            let Some((_, spec)) = part.split_once('=') else {
                continue;
            };
            if let Ok(parsed) = spec.parse::<SceneSpec>() {
                self.showing[index] = parsed.scenes().to_vec();
            }
        }
    }

    /// What `enter` would put on the panel: the draft, or failing that the one
    /// scene under the cursor.
    fn effective(&self) -> Vec<SceneKind> {
        if self.drafts[self.focus].is_empty() {
            SceneKind::ALL
                .get(self.selected)
                .copied()
                .into_iter()
                .collect()
        } else {
            self.drafts[self.focus].clone()
        }
    }

    /// Rows that would take, and what is left over.
    fn budget(&self) -> (i32, i32) {
        let needed = rows_for(&self.effective());
        (needed, canvas::HEIGHT - needed)
    }

    /// Adds a scene to the draft, or explains why it cannot go there.
    ///
    /// Refusing here says so at the keystroke that did it, rather than leaving
    /// an unapplicable stack to be discovered at `enter`.
    fn add(&mut self, kind: SceneKind) {
        let focus = self.focus;
        let mut proposed = self.drafts[focus].clone();
        proposed.push(kind);

        let needed = rows_for(&proposed);
        if needed > canvas::HEIGHT {
            self.status = format!(
                "no room for {kind}: that stack needs {needed} rows of {} — c starts over",
                canvas::HEIGHT
            );
            return;
        }
        self.drafts[focus] = proposed;
    }

    /// Sends the focused draft to the daemon.
    ///
    /// With nothing composed, this shows the scene under the cursor on its own:
    /// picking one scene is what this is used for nine times in ten, and it
    /// should not cost three keystrokes.
    fn apply(&mut self) {
        let panel = self.panel();
        let focus = self.focus;
        if self.drafts[focus].is_empty() {
            let Some(kind) = SceneKind::ALL.get(self.selected) else {
                "nothing to apply".clone_into(&mut self.status);
                return;
            };
            self.drafts[focus].push(*kind);
        }
        let draft = &self.drafts[focus];

        let spec: SceneSpec = match draft
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",")
            .parse()
        {
            Ok(spec) => spec,
            Err(error) => {
                self.status = error.to_string();
                return;
            }
        };

        self.status = match ask(&self.socket, &Request::Set { panel, scene: spec }) {
            Ok(response) => {
                // Applied, so the draft has become what is showing.
                self.showing[focus] = std::mem::take(&mut self.drafts[focus]);
                response.message().to_owned()
            }
            Err(error) => error.to_string(),
        };
    }

    /// Nudges the brightness of every panel.
    fn set_brightness(&mut self, level: u8) {
        self.brightness = level;
        self.status = match ask(&self.socket, &Request::Brightness(level)) {
            Ok(response) => response.message().to_owned(),
            Err(error) => error.to_string(),
        };
    }
}

/// Sends one request on its own connection and reads the answer.
///
/// A fresh connection each time rather than one held open: the watcher already
/// owns a stream, and a request is three bytes.
fn ask(socket: &Path, request: &Request) -> Result<Response> {
    let stream = UnixStream::connect(socket)
        .with_context(|| format!("no daemon on {}", socket.display()))?;
    let mut writer = &stream;
    writeln!(writer, "{request}")?;
    writer.flush()?;

    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line)?;
    line.parse()
}

/// Opens a watch stream and forwards frames as they arrive.
fn watch(socket: &Path) -> Result<Receiver<(PanelName, Canvas)>> {
    let stream = UnixStream::connect(socket)
        .with_context(|| format!("no daemon on {}", socket.display()))?;
    let mut writer = &stream;
    writeln!(writer, "{}", Request::Watch)?;
    writer.flush()?;

    let (sender, receiver) = channel();
    std::thread::spawn(move || {
        for line in BufReader::new(&stream).lines().map_while(Result::ok) {
            let mut words = line.split_whitespace();
            if words.next() != Some("frame") {
                continue;
            }
            let (Some(panel), Some(hex)) = (words.next(), words.next()) else {
                continue;
            };
            let (Ok(panel), Some(canvas)) = (panel.parse::<PanelName>(), Canvas::from_hex(hex))
            else {
                continue;
            };
            // The receiver going away is how this ends.
            if sender.send((panel, canvas)).is_err() {
                return;
            }
        }
    });
    Ok(receiver)
}

/// Runs the interface until the user quits.
///
/// # Errors
///
/// Fails if the terminal cannot be taken over, or if there is no daemon to
/// watch.
pub fn run(socket: PathBuf) -> Result<()> {
    let frames = watch(&socket)?;
    let mut app = App::new(socket);

    // A panic between here and the restore at the end would otherwise leave the
    // shell in raw mode inside the alternate screen, with the message painted
    // somewhere invisible. Put the terminal back first, then let it through.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        previous(info);
    }));

    enable_raw_mode().context("taking over the terminal")?;
    let mut out = std::io::stdout();
    execute!(out, EnterAlternateScreen).context("switching screens")?;
    let mut terminal = Terminal::new(CrosstermBackend::new(out)).context("starting the display")?;

    let outcome = loop {
        loop {
            match frames.try_recv() {
                Ok((panel, canvas)) => {
                    let slot = usize::from(panel == PanelName::Right);
                    app.frames[slot] = canvas;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    "the daemon stopped".clone_into(&mut app.status);
                    break;
                }
            }
        }

        if let Err(error) = terminal.draw(|frame| draw(frame.area(), frame, &app)) {
            break Err(error.into());
        }

        match event::poll(TICK) {
            Ok(true) => match event::read() {
                Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                    if handle(&mut app, key) {
                        break Ok(());
                    }
                }
                Ok(_) => {}
                Err(error) => break Err(error.into()),
            },
            Ok(false) => {}
            Err(error) => break Err(error.into()),
        }
    };

    // Put the terminal back whatever happened, then report.
    restore();
    terminal.show_cursor().ok();
    outcome
}

/// Hands the terminal back: ordinary mode, ordinary screen, visible cursor.
///
/// Called on the way out and again from the panic hook, so it has to be happy
/// running twice and to ignore anything that goes wrong.
fn restore() {
    disable_raw_mode().ok();
    execute!(
        std::io::stdout(),
        LeaveAlternateScreen,
        ratatui::crossterm::cursor::Show
    )
    .ok();
}

/// Acts on a key, returning true when it is time to leave.
fn handle(app: &mut App, key: KeyEvent) -> bool {
    // Raw mode means no SIGINT: Ctrl-C arrives as an ordinary key, and it used
    // to arrive as the `c` that clears a draft. This arm has to come first.
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c' | 'd'))
    {
        return true;
    }

    // Any key dismisses the last message, so the reminders are never more than
    // one keystroke away, and closes the help.
    app.status.clear();
    if app.helping {
        app.helping = false;
        return false;
    }

    match key.code {
        KeyCode::Char('?') => app.helping = true,
        KeyCode::Char('q') => return true,
        KeyCode::Tab => app.focus = 1 - app.focus,
        KeyCode::Up | KeyCode::Char('k') => {
            app.selected = app.selected.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.selected = (app.selected + 1).min(SceneKind::ALL.len() - 1);
        }
        KeyCode::Char(' ') => {
            if let Some(kind) = SceneKind::ALL.get(app.selected) {
                app.add(*kind);
            }
        }
        KeyCode::Char('x') | KeyCode::Backspace => {
            let focus = app.focus;
            app.drafts[focus].pop();
        }
        // Esc backs out of what is being composed. It used to leave the whole
        // interface, which is a poor answer to a key people press to cancel.
        KeyCode::Char('c') | KeyCode::Esc => {
            let focus = app.focus;
            app.drafts[focus].clear();
        }
        KeyCode::Enter => app.apply(),
        KeyCode::Char('+') => {
            let level = app.brightness.saturating_add(5);
            app.set_brightness(level);
        }
        KeyCode::Char('-') => {
            let level = app.brightness.saturating_sub(5);
            app.set_brightness(level);
        }
        _ => {}
    }
    false
}

/// Paints the whole screen.
fn draw(area: Rect, frame: &mut ratatui::Frame, app: &App) {
    let Some(scale) = Scale::fitting(area) else {
        // Drawing anyway would clip the panels without saying so, and a clipped
        // preview is a lying preview.
        let (need_width, need_height) = Scale::smallest().box_size();
        frame.render_widget(
            Paragraph::new(format!(
                "terminal too small: {}x{} needed, {}x{} given",
                need_width * 2 + CATALOGUE_MIN_WIDTH,
                need_height + 1,
                area.width,
                area.height,
            ))
            .style(Style::default().fg(Color::Yellow)),
            area,
        );
        return;
    };

    let titles: Vec<String> = PanelName::ALL
        .into_iter()
        .enumerate()
        .map(|(index, panel)| {
            let showing = spec_of(&app.showing[index]);
            if app.drafts[index].is_empty() {
                format!(" {panel} · {showing} ")
            } else {
                format!(" {panel} · {showing} → {} ", spec_of(&app.drafts[index]))
            }
        })
        .collect();

    let widest = titles
        .iter()
        .map(|title| u16::try_from(title.chars().count()).unwrap_or(u16::MAX))
        .max()
        .unwrap_or(0);
    let content = centred(area, scale, widest);

    let boxes = Rect {
        height: content.height - 1,
        ..content
    };
    // The state runs from the block's left edge to the window's right one: it
    // is one long line of reminders, and there is no reason to clip it against
    // a margin that has nothing in it.
    let state = Rect {
        y: content.y + content.height - 1,
        height: 1,
        width: area.width.saturating_sub(content.x),
        ..content
    };

    let panel_width = (content.width - catalogue_width(content.width, scale)) / 2;
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(panel_width),
            Constraint::Length(panel_width),
            Constraint::Min(CATALOGUE_MIN_WIDTH),
        ])
        .split(boxes);

    for (index, title) in titles.into_iter().enumerate() {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(border_style(app.focus == index));
        frame.render_widget(
            Paragraph::new(panel_lines(&app.frames[index], scale))
                .alignment(Alignment::Center)
                .block(block),
            columns[index],
        );
    }

    frame.render_widget(catalogue(app), columns[2]);
    frame.render_widget(Paragraph::new(status_line(app)), state);

    if app.helping {
        let panel = help_area(boxes);
        frame.render_widget(Clear, panel);
        frame.render_widget(help(), panel);
    }
}

/// The columns the catalogue takes: its comfortable width when the terminal can
/// afford it, its bare minimum otherwise.
fn catalogue_width(available: u16, scale: Scale) -> u16 {
    let (box_width, _) = scale.box_size();
    let ideal = catalogue_ideal_width();
    if available >= box_width * 2 + ideal {
        ideal
    } else {
        CATALOGUE_MIN_WIDTH
    }
}

/// Where the interface sits in the window.
///
/// It has a size of its own — two panels of fixed pixels, a catalogue, one row
/// of state — so a big screen gets a centred block with margins rather than a
/// catalogue stretched across half a monitor and rows of nothing underneath.
fn centred(area: Rect, scale: Scale, widest_title: u16) -> Rect {
    let (box_width, box_height) = scale.box_size();
    let catalogue = catalogue_width(area.width, scale);

    // Panels grow past their pixels only far enough to spell out their titles,
    // corners included; the pixels are then centred in what that leaves.
    let room_each = area.width.saturating_sub(catalogue) / 2;
    let panel_width = box_width
        .max(widest_title.saturating_add(2))
        .min(room_each.max(box_width));

    let width = (panel_width * 2 + catalogue).min(area.width);
    let height = (box_height + 1).min(area.height);
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}

/// Where the key list goes: over the middle of the interface, big enough for
/// every line and no bigger.
fn help_area(over: Rect) -> Rect {
    let rows = u16::try_from(KEYS.len()).unwrap_or(u16::MAX);
    let height = rows.saturating_add(2).min(over.height);
    // Measured from the text rather than guessed at: a help with its own last
    // words cut off would be a poor sort of help.
    let longest = KEYS
        .iter()
        .map(|(key, what)| key.chars().count().max(KEY_COLUMN) + what.chars().count() + 6)
        .max()
        .unwrap_or(0);
    let width = u16::try_from(longest).unwrap_or(u16::MAX).min(over.width);
    Rect::new(
        over.x + (over.width - width) / 2,
        over.y + (over.height - height) / 2,
        width,
        height,
    )
}

/// The style of a block's border, brighter when it has the focus.
fn border_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

/// A spec as it would be typed, or a placeholder when empty.
fn spec_of(draft: &[SceneKind]) -> String {
    if draft.is_empty() {
        return "empty".to_owned();
    }
    draft
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

/// One frame, drawn so that a pixel comes out square at any scale.
///
/// Everything is half-blocks: a line carries two half-lines, the top one lit by
/// the foreground and the bottom one by the background. That covers every scale
/// with one rule — at two half-lines a pixel both halves land on the same pixel
/// and the line reads as solid, at three they straddle two pixels, and none of
/// it needs a special case.
fn panel_lines(canvas: &Canvas, scale: Scale) -> Vec<Line<'static>> {
    let cell = "\u{2580}".repeat(scale.cells_across() as usize);
    (0..scale.lines_down())
        .map(|row| {
            let pixel = |half: u16| i32::from(half / scale.0);
            let (top, bottom) = (pixel(row * 2), pixel(row * 2 + 1));
            Line::from(
                (0..canvas::WIDTH)
                    .map(|x| {
                        Span::styled(
                            cell.clone(),
                            Style::default()
                                .fg(shade(canvas.get(x, top)))
                                .bg(shade(canvas.get(x, bottom))),
                        )
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

/// A pixel's brightness as a terminal colour.
fn shade(value: u8) -> Color {
    if value == 0 {
        return Color::Rgb(18, 18, 20);
    }
    // The modules are white LEDs behind a diffuser; a warm white reads closer
    // to them than a pure one.
    let warm = u8::try_from(u16::from(value) * 235 / 255).unwrap_or(value);
    Color::Rgb(value, value, warm)
}

/// The scene catalogue, with the cursor and what each one needs.
fn catalogue(app: &App) -> Paragraph<'static> {
    let lines: Vec<Line> = SceneKind::ALL
        .iter()
        .enumerate()
        .map(|(index, kind)| {
            let cursor = if index == app.selected { "▸" } else { " " };
            let text = format!(
                "{cursor} {:<width$} {:>2}",
                kind.to_string(),
                kind.min_height(),
                width = name_column()
            );
            let style = if index == app.selected {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };
            Line::styled(text, style)
        })
        .collect();

    Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" scenes ")
            .border_style(border_style(false)),
    )
}

/// The one line at the bottom: what is being composed, and whether it fits.
///
/// A message from the daemon joins the line rather than replacing it. It used
/// to take the whole row, reminders included, and nothing ever put them back —
/// so one keystroke left you in front of an interface with no visible way in.
fn status_line(app: &App) -> Line<'static> {
    let (needed, spare) = app.budget();
    let fit = match spare {
        short if short < 0 => format!("{} rows short", -short),
        0 => "nothing spare".to_owned(),
        spare => format!("{spare} spare"),
    };

    let dim = Style::default().fg(Color::DarkGray);
    // Always what `enter` would do, so the row count is never about something
    // other than the key about to be pressed.
    let mut spans = vec![Span::styled(
        format!(
            " {} · {} · {needed} rows · {fit} · {} bright",
            app.panel(),
            spec_of(&app.effective()),
            app.brightness,
        ),
        dim,
    )];

    if !app.status.is_empty() {
        spans.push(Span::styled(
            format!("   {}", app.status),
            Style::default().fg(Color::Yellow),
        ));
    }

    // Short enough to survive beside a message, and `?` covers the rest.
    spans.push(Span::styled("   enter", Style::default().fg(Color::Gray)));
    spans.push(Span::styled(" apply  ", dim));
    spans.push(Span::styled("?", Style::default().fg(Color::Gray)));
    spans.push(Span::styled(" help  ", dim));
    spans.push(Span::styled("q", Style::default().fg(Color::Gray)));
    spans.push(Span::styled(" quit", dim));
    Line::from(spans)
}

/// Rows a stack of these scenes needs, rules between them included.
fn rows_for(draft: &[SceneKind]) -> i32 {
    let minimums: Vec<i32> = draft.iter().map(|kind| kind.min_height()).collect();
    scene::needed_for(&minimums)
}

/// Columns the key names are right-aligned in.
const KEY_COLUMN: usize = 14;

/// Every key, in the order the help lists them.
const KEYS: [(&str, &str); 9] = [
    ("tab", "compose the other panel"),
    ("↑ ↓  k j", "move through the scenes"),
    ("space", "add the selected scene to the stack"),
    ("x", "drop the last scene off the stack"),
    ("c  esc", "drop what is being composed"),
    ("enter", "show the selection, or the stack if any"),
    ("+ -", "brightness, five at a time"),
    ("?", "this help"),
    ("q  ctrl-c", "leave"),
];

/// The help overlay, shown over the middle of the interface.
fn help() -> Paragraph<'static> {
    let lines: Vec<Line> = KEYS
        .iter()
        .map(|(key, what)| {
            Line::from(vec![
                Span::styled(
                    format!("  {key:>KEY_COLUMN$}  "),
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled((*what).to_owned(), Style::default().fg(Color::Gray)),
            ])
        })
        .collect();

    Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" keys · any key closes ")
            .border_style(Style::default().fg(Color::Yellow)),
    )
}

#[cfg(test)]
mod tests {
    use super::{App, Scale, handle, spec_of, status_line};
    use crate::control::PanelName;
    use crate::scene::SceneKind;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::path::PathBuf;

    /// A key with no modifier held.
    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// An interface with no daemon behind it: everything but applying works.
    fn app() -> App {
        App {
            socket: PathBuf::from("/nonexistent/ledmat.sock"),
            frames: [crate::canvas::Canvas::new(), crate::canvas::Canvas::new()],
            showing: [Vec::new(), Vec::new()],
            drafts: [Vec::new(), Vec::new()],
            focus: 0,
            selected: 0,
            brightness: 30,
            status: String::new(),
            helping: false,
        }
    }

    /// Renders the whole interface off-screen, as text.
    fn rendered(app: &App, width: u16, height: u16) -> String {
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).expect("the test backend");
        terminal
            .draw(|frame| super::draw(frame.area(), frame, app))
            .expect("drawing");
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_interface_fits_the_terminal_it_is_given() {
        // Both panels, the catalogue and the status line have to land inside an
        // ordinary 80x24 window, or the thing is unusable where it is used.
        let mut app = app();
        // On the focused panel: the status line only ever speaks about that one.
        app.drafts[0] = vec![SceneKind::Clock, SceneKind::Cpu, SceneKind::Battery];
        let drawn = rendered(&app, 80, 24);

        assert!(drawn.contains("left"), "the left panel lost its title");
        assert!(drawn.contains("right"), "the right panel lost its title");
        assert!(drawn.contains("clock"), "the catalogue is missing");
        // The status line rides with the block rather than the window, so it
        // is the line right under the boxes.
        let under = drawn
            .lines()
            .skip_while(|line| !line.contains('\u{2514}'))
            .nth(1)
            .unwrap_or_default();
        assert!(
            under.contains("clock,cpu,battery") && under.contains("21 rows"),
            "the status line is not under the boxes: {drawn}"
        );
        // Whatever the window does to the rest of the line, the key that
        // actually changes a panel has to survive the truncation.
        assert!(
            under.contains("enter apply"),
            "applying is no longer advertised at 80 columns: {drawn}"
        );
        assert_eq!(drawn.lines().count(), 24);
    }

    #[test]
    fn the_panel_box_holds_a_whole_panel() {
        // Written out rather than computed, since the cast would have to be
        // fallible in a const; this keeps it honest if the canvas ever changes.
        assert_eq!(i32::from(super::PIXELS_ACROSS), crate::canvas::WIDTH);
        assert_eq!(i32::from(super::PIXELS_DOWN), crate::canvas::HEIGHT);
    }

    #[test]
    fn a_pixel_is_square_at_either_scale() {
        // A terminal cell is one unit wide and two units tall. A pixel is
        // square when the cells it spans across equal the units it spans down,
        // which is the whole reason there are exactly two scales.
        for scale in Scale::ALL {
            // Cells across against half-lines down, in the same units: a cell
            // is one across and two down, so a half-line is one down.
            let across = scale.cells_across();
            let down = scale.lines_down() * 2 / super::PIXELS_DOWN;
            assert_eq!(across, down, "{scale:?} draws rectangles, not pixels");
        }
    }

    #[test]
    fn every_scale_draws_the_whole_panel() {
        // The odd one is the point of the test: at three half-lines a pixel the
        // boundaries fall mid-line, and the line count still has to come out
        // whole and cover all 34 rows.
        for scale in Scale::ALL {
            let lines = super::panel_lines(&crate::canvas::Canvas::new(), scale);
            assert_eq!(
                lines.len(),
                scale.lines_down() as usize,
                "{scale:?} drew the wrong number of lines"
            );
            assert_eq!(
                u32::from(scale.lines_down()) * 2,
                u32::from(super::PIXELS_DOWN) * u32::from(scale.0),
                "{scale:?} does not cover the panel exactly"
            );
        }
    }

    #[test]
    fn the_largest_scale_that_fits_is_the_one_used() {
        use ratatui::layout::Rect;
        let at = |w, h| Scale::fitting(Rect::new(0, 0, w, h));

        // A wall of a screen gets the biggest pixels...
        assert_eq!(at(120, 80), Some(Scale(4)));
        // ...a 68-row window gets the odd size rather than dropping to half...
        assert_eq!(at(236, 68), Some(Scale(3)));
        // ...a tall-ish one the middle size...
        assert_eq!(at(100, 40), Some(Scale(2)));
        // ...an ordinary 80x24 falls back rather than clipping...
        assert_eq!(at(80, 24), Some(Scale(1)));
        // ...and too small says so instead of drawing a lie.
        assert_eq!(at(80, 12), None);
        assert_eq!(at(30, 40), None);
    }

    #[test]
    fn the_catalogue_holds_the_longest_name_it_has() {
        // The names grew from `spectrum` to `speakers-spectrum` once already;
        // a hand-picked width would have clipped them without a word.
        let longest = SceneKind::ALL
            .iter()
            .map(|kind| kind.to_string().chars().count())
            .max()
            .expect("the catalogue is not empty");
        let entry = format!(
            "> {:<width$} {:>2}",
            "x".repeat(longest),
            34,
            width = super::name_column()
        );
        assert!(
            usize::from(super::catalogue_ideal_width()) >= entry.chars().count() + 2,
            "{} columns will not hold {:?}",
            super::catalogue_ideal_width(),
            entry
        );
    }

    #[test]
    fn a_big_window_gets_margins_rather_than_a_stretched_catalogue() {
        use ratatui::layout::Rect;

        // Everything here has a size of its own, so the room a large screen
        // adds belongs in the margins — it used to go to the catalogue, which
        // became a mostly empty box across half the monitor.
        let screen = Rect::new(0, 0, 200, 60);
        let block = super::centred(screen, Scale(2), 26);

        let (box_width, box_height) = Scale(2).box_size();
        assert!(block.width < screen.width / 2, "still stretched: {block:?}");
        assert_eq!(block.height, box_height + 1, "rows of nothing crept back");
        assert!(block.width >= box_width * 2 + super::CATALOGUE_MIN_WIDTH);

        // Centred, so the margins match on both sides and above and below.
        assert_eq!(block.x, (screen.width - block.width) / 2);
        assert_eq!(block.y, (screen.height - block.height) / 2);
    }

    #[test]
    fn a_snug_window_keeps_every_column_it_has() {
        use ratatui::layout::Rect;

        // The other end: nothing to spare, so nothing may be given away.
        let (box_width, box_height) = Scale(1).box_size();
        let snug = Rect::new(
            0,
            0,
            box_width * 2 + super::CATALOGUE_MIN_WIDTH,
            box_height + 1,
        );
        let block = super::centred(snug, Scale(1), 40);
        assert_eq!((block.width, block.height), (snug.width, snug.height));
        assert_eq!((block.x, block.y), (0, 0));
    }

    #[test]
    fn a_scene_with_no_room_is_refused_at_the_keystroke() {
        // The draft opens on what the panel already shows, so adding to a game
        // built a stack that could never be applied — and said so only at
        // `enter`, which read as `enter` doing nothing at all.
        let mut app = app();
        app.drafts[0] = vec![SceneKind::Snake];
        app.selected = SceneKind::ALL
            .iter()
            .position(|kind| *kind == SceneKind::Clock)
            .expect("clock is in the catalogue");

        handle(&mut app, press(KeyCode::Char(' ')));
        assert_eq!(
            app.drafts[0],
            vec![SceneKind::Snake],
            "an impossible stack was built anyway"
        );
        assert!(app.status.contains("no room"), "no reason given");
        assert!(app.status.contains('c'), "no way out offered");
    }

    #[test]
    fn picking_one_scene_is_a_move_and_a_keystroke() {
        // The common case by far. It used to cost three keys — clear, add,
        // apply — because the draft opened pre-filled with what was showing.
        let mut app = app();
        app.showing[0] = vec![SceneKind::Snake];
        app.selected = SceneKind::ALL
            .iter()
            .position(|kind| *kind == SceneKind::Clock)
            .expect("clock is in the catalogue");

        // Nothing composed, so the line already describes what enter would do.
        assert_eq!(app.effective(), vec![SceneKind::Clock]);
        assert_eq!(app.budget(), (11, 23));
    }

    #[test]
    fn a_full_panel_is_no_obstacle_to_picking_another_scene() {
        // Adding to a game is refused, but replacing it outright is the point
        // of the tool and must not be caught by that refusal.
        let mut app = app();
        app.showing[0] = vec![SceneKind::Snake];
        app.selected = SceneKind::ALL
            .iter()
            .position(|kind| *kind == SceneKind::Clock)
            .expect("clock is in the catalogue");
        assert!(app.budget().1 >= 0, "picking a scene hit the row budget");
    }

    #[test]
    fn a_message_never_takes_the_keys_away() {
        // The whole line used to become the message, reminders and all, and
        // nothing put them back: one keystroke and the interface looked inert.
        let mut app = app();
        app.drafts[0] = vec![SceneKind::Clock];
        app.status = "nothing to apply".to_owned();

        let line: String = status_line(&app)
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(line.contains("nothing to apply"), "the message is gone");
        assert!(line.contains("clock"), "the draft is gone: {line}");
        assert!(line.contains("? help"), "the way in is gone: {line}");
        assert!(line.contains("enter apply"), "applying is unadvertised");
    }

    #[test]
    fn the_next_key_puts_the_message_away() {
        let mut app = app();
        app.status = "nothing to apply".to_owned();
        handle(&mut app, press(KeyCode::Tab));
        assert!(app.status.is_empty(), "the message outstayed its keystroke");
    }

    #[test]
    fn the_help_lists_every_key_and_any_key_closes_it() {
        let mut app = app();
        handle(&mut app, press(KeyCode::Char('?')));
        assert!(app.helping, "? did not open the help");

        let mut listed = String::new();
        for (key, what) in super::KEYS {
            listed.push_str(key);
            listed.push(' ');
            listed.push_str(what);
            listed.push(' ');
        }
        for expected in ["tab", "space", "enter", "?", "ctrl-c"] {
            assert!(listed.contains(expected), "{expected} is not documented");
        }
        // The box has to hold the longest line, or the help itself is unhelpful.
        let longest = super::KEYS
            .iter()
            .map(|(key, what)| {
                key.chars().count().max(super::KEY_COLUMN) + what.chars().count() + 6
            })
            .max()
            .unwrap_or(0);
        let area = super::help_area(ratatui::layout::Rect::new(0, 0, 200, 60));
        assert!(
            usize::from(area.width) >= longest,
            "the help is {} wide and needs {longest}",
            area.width
        );

        // Any key at all, and it must not also act on what is underneath.
        let before = app.drafts[0].clone();
        handle(&mut app, press(KeyCode::Char(' ')));
        assert!(!app.helping, "the help would not close");
        assert_eq!(app.drafts[0], before, "the key fell through to the draft");
    }

    #[test]
    fn control_c_leaves_instead_of_clearing_the_draft() {
        // Raw mode swallows SIGINT, so this is the only thing that can quit on
        // Ctrl-C — and plain `c` still has to clear.
        let mut app = app();
        app.drafts[0] = vec![SceneKind::Clock];
        assert!(handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
        ));
        assert_eq!(
            app.drafts[0],
            vec![SceneKind::Clock],
            "it cleared the draft"
        );

        assert!(!handle(&mut app, press(KeyCode::Char('c'))));
        assert!(app.drafts[0].is_empty(), "plain c stopped clearing");
    }

    #[test]
    fn a_scene_is_added_to_the_panel_that_has_the_focus() {
        let mut app = app();
        handle(&mut app, press(KeyCode::Tab));
        handle(&mut app, press(KeyCode::Char(' ')));

        assert!(app.drafts[0].is_empty(), "the wrong panel took the scene");
        assert_eq!(app.drafts[1], vec![SceneKind::ALL[0]]);
        assert_eq!(app.panel(), PanelName::Right);
    }

    #[test]
    fn the_cursor_stays_inside_the_catalogue() {
        let mut app = app();
        for _ in 0..50 {
            handle(&mut app, press(KeyCode::Up));
        }
        assert_eq!(app.selected, 0, "the cursor ran off the top");

        for _ in 0..50 {
            handle(&mut app, press(KeyCode::Down));
        }
        assert_eq!(app.selected, SceneKind::ALL.len() - 1, "ran off the bottom");
    }

    #[test]
    fn removing_takes_the_last_scene_off() {
        // Two scenes that fit together: a pair of games never could, and is
        // now refused outright rather than stacked into something unusable.
        let mut app = app();
        app.selected = SceneKind::ALL
            .iter()
            .position(|kind| *kind == SceneKind::Cpu)
            .expect("cpu is in the catalogue");
        handle(&mut app, press(KeyCode::Char(' ')));
        handle(&mut app, press(KeyCode::Down));
        handle(&mut app, press(KeyCode::Char(' ')));
        assert_eq!(app.drafts[0], vec![SceneKind::Cpu, SceneKind::Ram]);

        handle(&mut app, press(KeyCode::Char('x')));
        assert_eq!(app.drafts[0], vec![SceneKind::Cpu]);

        handle(&mut app, press(KeyCode::Char('c')));
        assert!(app.drafts[0].is_empty(), "clearing left something behind");
    }

    #[test]
    fn removing_from_an_empty_panel_is_not_a_crash() {
        let mut app = app();
        for _ in 0..5 {
            handle(&mut app, press(KeyCode::Char('x')));
        }
        assert!(app.drafts[0].is_empty());
    }

    #[test]
    fn quitting_is_the_only_thing_that_ends_the_loop() {
        let mut app = app();
        assert!(handle(&mut app, press(KeyCode::Char('q'))));
        // Esc backs out of a draft; leaving on it was a trap for a key people
        // press to cancel.
        app.drafts[0] = vec![SceneKind::Clock];
        assert!(!handle(&mut app, press(KeyCode::Esc)));
        assert!(app.drafts[0].is_empty(), "esc no longer cancels");
        assert!(!handle(&mut app, press(KeyCode::Tab)));
        assert!(
            !handle(&mut app, press(KeyCode::Enter)),
            "applying must not quit"
        );
    }

    #[test]
    fn the_status_line_says_how_much_room_is_left() {
        let mut app = app();
        // A clock needs eleven of the thirty-four rows.
        app.drafts[0] = vec![SceneKind::Clock];
        let line = status_line(&app).to_string();
        assert!(line.contains("11 rows"), "{line}");
        assert!(line.contains("23 spare"), "{line}");
    }

    #[test]
    fn the_status_line_says_when_a_stack_does_not_fit() {
        // Two games need the whole panel each: the answer has to be the
        // shortfall, not silence.
        let mut app = app();
        app.drafts[0] = vec![SceneKind::Pong, SceneKind::Snake];
        let line = status_line(&app).to_string();
        assert!(line.contains("rows short"), "{line}");
    }

    #[test]
    fn an_empty_panel_reads_as_empty_rather_than_blank() {
        assert_eq!(spec_of(&[]), "empty");
        assert_eq!(spec_of(&[SceneKind::Clock, SceneKind::Cpu]), "clock,cpu");
    }

    #[test]
    fn applying_without_a_daemon_reports_it_instead_of_panicking() {
        let mut app = app();
        app.drafts[0] = vec![SceneKind::Clock];
        app.apply();
        assert!(!app.status.is_empty(), "a failure to apply said nothing");
    }
}
