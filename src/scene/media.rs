//! What is playing, over MPRIS.
//!
//! Every desktop player publishes the same interface on the session bus, so
//! this works with Spotify, a browser tab or a video player without knowing
//! which. Nine columns hold barely two characters, so the title scrolls and the
//! play state lives in the progress rule rather than stealing width from it.

use std::collections::HashMap;
use std::time::Duration;

use tracing::debug;
use zbus::blocking::{Connection, fdo::DBusProxy, fdo::PropertiesProxy};
use zbus::names::InterfaceName;
use zbus::zvariant::{ObjectPath, OwnedValue};

use crate::canvas::{self, Canvas};
use crate::device::ColorMode;
use crate::font;
use crate::poller::Poller;
use crate::scene::{Area, Scene};

/// How often the bus is asked what is playing.
const POLL_INTERVAL: Duration = Duration::from_millis(700);

/// Rows for a line of text and a progress rule under it.
const MIN_HEIGHT: i32 = 7;

/// Pixels the title moves per second.
const SCROLL_SPEED: f32 = 4.0;
/// How long the title rests before it starts over.
const SCROLL_PAUSE: f32 = 1.5;

const RULE_LEVEL: u8 = 22;
const PROGRESS_LEVEL: u8 = 160;

/// The MPRIS well-known name every player takes a variant of.
const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";
const PLAYER_INTERFACE: &str = "org.mpris.MediaPlayer2.Player";
const PLAYER_PATH: &str = "/org/mpris/MediaPlayer2";

/// What a player is doing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Track {
    /// Title and artist, already joined for display.
    pub label: String,
    /// Whether it is playing rather than paused.
    pub playing: bool,
    /// How far through, 0.0 to 1.0, when the player says.
    pub progress: f32,
}

/// Joins the metadata a player publishes into one line.
///
/// Artist first: on a panel this narrow the title scrolls past anyway, and the
/// artist is what tells you whether you care.
#[must_use]
pub fn label_from(title: Option<&str>, artist: Option<&str>) -> String {
    match (artist, title) {
        (Some(artist), Some(title)) if !artist.is_empty() => format!("{artist} - {title}"),
        (_, Some(title)) => title.to_owned(),
        (Some(artist), None) => artist.to_owned(),
        (None, None) => String::new(),
    }
}

/// How far through a track is, guarding the divisions.
#[must_use]
pub fn progress_of(position: i64, length: i64) -> f32 {
    if position <= 0 || length <= 0 {
        return 0.0;
    }
    // Both are microseconds, and f64 holds those exactly past any real track
    // length; the clamp then makes the narrowing to f32 harmless.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        reason = "microsecond counts stay well inside f64, and the ratio is clamped"
    )]
    {
        let ratio = position as f64 / length as f64;
        ratio.clamp(0.0, 1.0) as f32
    }
}

/// Asks the session bus what is playing.
fn read() -> Option<Track> {
    let connection = Connection::session().ok()?;
    let names = DBusProxy::new(&connection).ok()?.list_names().ok()?;

    // The first player that says it is playing wins; failing that, the first
    // one at all, so a paused track still shows.
    let mut fallback = None;
    for name in names {
        let name = name.as_str();
        if !name.starts_with(MPRIS_PREFIX) {
            continue;
        }
        let Some(track) = read_player(&connection, name) else {
            continue;
        };
        if track.playing {
            return Some(track);
        }
        fallback.get_or_insert(track);
    }
    fallback.or_else(|| Some(Track::default()))
}

/// Reads one player's properties.
fn read_player(connection: &Connection, name: &str) -> Option<Track> {
    let path = ObjectPath::try_from(PLAYER_PATH).ok()?;
    let proxy = PropertiesProxy::builder(connection)
        .destination(name.to_owned())
        .ok()?
        .path(path)
        .ok()?
        .build()
        .ok()?;

    // The proxy takes the interface name by value each time, so it is rebuilt
    // rather than shared.
    let interface = || InterfaceName::try_from(PLAYER_INTERFACE).ok();
    let status: String = proxy
        .get(interface()?, "PlaybackStatus")
        .ok()
        .and_then(|value| String::try_from(value).ok())
        .unwrap_or_default();

    let metadata: HashMap<String, OwnedValue> = proxy
        .get(interface()?, "Metadata")
        .ok()
        .and_then(|value| HashMap::try_from(value).ok())
        .unwrap_or_default();

    let text = |key: &str| -> Option<String> {
        let value = metadata.get(key)?;
        if let Ok(single) = String::try_from(value.clone()) {
            return Some(single);
        }
        // xesam:artist is a list; the first name is enough on nine columns.
        Vec::<String>::try_from(value.clone())
            .ok()?
            .into_iter()
            .next()
    };

    let length = metadata
        .get("mpris:length")
        .and_then(|value| i64::try_from(value.clone()).ok())
        .unwrap_or(0);
    let position = proxy
        .get(interface()?, "Position")
        .ok()
        .and_then(|value| i64::try_from(value).ok())
        .unwrap_or(0);

    debug!(player = name, status, "read a player");
    Some(Track {
        label: label_from(
            text("xesam:title").as_deref(),
            text("xesam:artist").as_deref(),
        ),
        playing: status == "Playing",
        progress: progress_of(position, length),
    })
}

/// What is playing.
pub struct Media {
    mode: ColorMode,
    poller: Poller<Track>,
    elapsed: f32,
}

impl Media {
    /// Starts the widget and its reader.
    #[must_use]
    pub fn new(mode: ColorMode) -> Self {
        // Nothing is read here on purpose. A scene is built from inside the
        // async runtime, and the blocking D-Bus client starts a runtime of its
        // own, which panics outright when there already is one. The poller's
        // thread has no such context, so the first answer arrives there.
        Self {
            mode,
            poller: Poller::spawn("media", Track::default(), POLL_INTERVAL, read),
            elapsed: 0.0,
        }
    }

    /// Where the title has scrolled to.
    ///
    /// It rests at the start, walks left until its end clears the panel, then
    /// starts over — rather than looping seamlessly, which makes it impossible
    /// to tell where a title begins.
    fn scroll_offset(&self, width: i32) -> i32 {
        let travel = width - canvas::WIDTH;
        if travel <= 0 {
            return 0;
        }
        let span = f32::from(u8::try_from(travel).unwrap_or(u8::MAX));
        let cycle = span / SCROLL_SPEED + SCROLL_PAUSE * 2.0;
        let phase = self.elapsed % cycle;

        if phase < SCROLL_PAUSE {
            return 0;
        }
        let moving = phase - SCROLL_PAUSE;
        -canvas::to_pixel((moving * SCROLL_SPEED).min(span))
    }
}

impl Scene for Media {
    fn name(&self) -> &'static str {
        "media"
    }

    fn min_height(&self) -> i32 {
        MIN_HEIGHT
    }

    fn update(&mut self, delta: Duration) {
        self.elapsed += delta.as_secs_f32();
    }

    fn render(&self, canvas: &mut Canvas, area: Area) {
        let track = self.poller.latest();
        let rule = area.bottom();

        if track.label.is_empty() {
            // Nothing playing anywhere: an empty rule rather than a blank band,
            // so the widget still reads as present.
            canvas.hline(0, canvas::WIDTH - 1, rule, RULE_LEVEL);
            return;
        }

        let text_top = area.top + (area.height - 1 - font::GLYPH_HEIGHT).max(0) / 2;
        if track.playing {
            let offset = self.scroll_offset(font::text_width(&track.label));
            font::draw_text(canvas, &track.label, offset, text_top, u8::MAX);
        } else {
            // Paused: two bars in the middle, which no title can be mistaken
            // for, and the scroll stops so the panel goes still.
            for row in 0..font::GLYPH_HEIGHT {
                canvas.set_max(3, text_top + row, 220);
                canvas.set_max(5, text_top + row, 220);
            }
        }

        canvas.hline(0, canvas::WIDTH - 1, rule, RULE_LEVEL);
        let head = canvas::to_pixel(track.progress.clamp(0.0, 1.0) * 9.0);
        let body = if self.mode == ColorMode::Bw {
            u8::MAX
        } else {
            PROGRESS_LEVEL
        };
        for x in 0..head {
            canvas.set_max(x, rule, body);
        }
        canvas.set_max((head - 1).max(0), rule, u8::MAX);
    }
}

#[cfg(test)]
mod tests {
    use super::{MIN_HEIGHT, Media, Track, label_from, progress_of};
    use crate::canvas::Canvas;
    use crate::device::ColorMode;
    use crate::poller::Poller;
    use crate::scene::{Area, Scene};
    use std::time::Duration;

    fn showing(track: Track) -> Media {
        Media {
            mode: ColorMode::Greyscale,
            poller: Poller::spawn("test", track, Duration::from_secs(3600), || None),
            elapsed: 0.0,
        }
    }

    fn drawn(media: &Media, height: i32) -> Canvas {
        let mut canvas = Canvas::new();
        media.render(&mut canvas, Area { top: 0, height });
        canvas
    }

    #[test]
    fn the_artist_comes_before_the_title() {
        assert_eq!(
            label_from(Some("Around"), Some("Daft Punk")),
            "Daft Punk - Around"
        );
        assert_eq!(label_from(Some("Solo"), None), "Solo");
        assert_eq!(label_from(None, Some("Band")), "Band");
        assert_eq!(label_from(None, None), "");
        // An empty artist string is what a browser tab publishes.
        assert_eq!(label_from(Some("Tab"), Some("")), "Tab");
    }

    #[test]
    fn progress_guards_its_divisions() {
        assert!(progress_of(0, 1000) < f32::EPSILON);
        assert!(progress_of(500, 0) < f32::EPSILON, "a stream has no length");
        assert!(progress_of(-1, 1000) < f32::EPSILON);
        assert!(
            (progress_of(2000, 1000) - 1.0).abs() < f32::EPSILON,
            "past the end is the end"
        );
        assert!((progress_of(500, 1000) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn nothing_playing_still_draws_the_rule() {
        let canvas = drawn(&showing(Track::default()), MIN_HEIGHT);
        assert_ne!(canvas, Canvas::new(), "the widget vanished entirely");
        let lit = (0..9)
            .filter(|x| canvas.get(*x, MIN_HEIGHT - 1) > 0)
            .count();
        assert_eq!(lit, 9, "the rule is not the full width");
    }

    #[test]
    fn a_paused_track_shows_bars_rather_than_its_title() {
        let playing = showing(Track {
            label: "AB".to_owned(),
            playing: true,
            progress: 0.0,
        });
        let paused = showing(Track {
            label: "AB".to_owned(),
            playing: false,
            progress: 0.0,
        });
        assert_ne!(
            drawn(&playing, MIN_HEIGHT),
            drawn(&paused, MIN_HEIGHT),
            "pause looks the same as play"
        );
    }

    #[test]
    fn the_progress_rule_fills_left_to_right() {
        let head = |progress| {
            let canvas = drawn(
                &showing(Track {
                    label: "AB".to_owned(),
                    playing: true,
                    progress,
                }),
                MIN_HEIGHT,
            );
            (0..9)
                .filter(|x| canvas.get(*x, MIN_HEIGHT - 1) == 255)
                .count()
        };
        assert!(head(1.0) > 0, "a finished track showed no progress");
    }

    #[test]
    fn a_long_title_scrolls_and_starts_over() {
        let mut media = showing(Track {
            label: "A VERY LONG TITLE INDEED".to_owned(),
            playing: true,
            progress: 0.0,
        });

        let mut seen = std::collections::HashSet::new();
        for _ in 0..200 {
            media.update(Duration::from_millis(100));
            let canvas = drawn(&media, MIN_HEIGHT);
            seen.insert((0..9).map(|x| canvas.get(x, 1)).collect::<Vec<_>>());
        }
        assert!(seen.len() > 3, "the title never moved");
    }

    #[test]
    fn a_short_title_does_not_scroll() {
        let mut media = showing(Track {
            label: "AB".to_owned(),
            playing: true,
            progress: 0.0,
        });
        let first = drawn(&media, MIN_HEIGHT);
        for _ in 0..30 {
            media.update(Duration::from_millis(100));
        }
        assert_eq!(drawn(&media, MIN_HEIGHT), first, "a title that fits moved");
    }

    #[test]
    fn nothing_is_drawn_outside_the_area() {
        let media = showing(Track {
            label: "SOMETHING LONG".to_owned(),
            playing: true,
            progress: 1.0,
        });
        for height in MIN_HEIGHT..=34 {
            for top in [0, (34 - height) / 2, 34 - height] {
                let mut canvas = Canvas::new();
                media.render(&mut canvas, Area { top, height });
                for y in 0..34 {
                    if y < top || y >= top + height {
                        for x in 0..9 {
                            assert_eq!(canvas.get(x, y), 0, "row {y} for {top}+{height}");
                        }
                    }
                }
            }
        }
    }
}
