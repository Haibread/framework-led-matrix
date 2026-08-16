//! The control socket: changing what a panel shows without restarting.
//!
//! One line in, one line out, over a Unix socket. Text rather than a binary
//! encoding so the whole thing stays debuggable with `socat` and readable in a
//! terminal — there are three verbs, none of them worth a codec.

use std::fmt;
use std::str::FromStr;

use anyhow::{Result, bail};

use crate::scene::SceneKind;

/// Which module a request is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, clap::ValueEnum)]
pub enum PanelName {
    /// The left-hand module.
    Left,
    /// The right-hand module.
    Right,
}

impl PanelName {
    /// Every panel, in the order they are reported.
    pub const ALL: [Self; 2] = [Self::Left, Self::Right];

    /// The label used in logs and on the wire.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
        }
    }
}

impl fmt::Display for PanelName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

impl FromStr for PanelName {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        match text {
            "left" => Ok(Self::Left),
            "right" => Ok(Self::Right),
            other => bail!("unknown panel {other:?}, expected left or right"),
        }
    }
}

/// What a panel shows: one scene, or several stacked top to bottom.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SceneSpec(Vec<SceneKind>);

impl SceneSpec {
    /// The scenes, in the order they are stacked.
    #[must_use]
    pub fn scenes(&self) -> &[SceneKind] {
        &self.0
    }

    /// A panel showing one scene, which is a stack of one.
    #[must_use]
    pub fn single(kind: SceneKind) -> Self {
        Self(vec![kind])
    }
}

impl fmt::Display for SceneSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<String> = self.0.iter().map(ToString::to_string).collect();
        f.write_str(&names.join(","))
    }
}

impl FromStr for SceneSpec {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        let scenes = text
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(scene_from_str)
            .collect::<Result<Vec<_>>>()?;
        anyhow::ensure!(!scenes.is_empty(), "no scene named");
        Ok(Self(scenes))
    }
}

/// Something asked of a running daemon.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// Show `scene` on `panel`.
    Set {
        /// The module to change.
        panel: PanelName,
        /// What it should show, stacked top to bottom.
        scene: SceneSpec,
    },
    /// Set the brightness of every panel.
    Brightness(u8),
    /// Report what each panel is showing.
    Status,
}

impl fmt::Display for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Set { panel, scene } => write!(f, "set {panel} {scene}"),
            Self::Brightness(level) => write!(f, "brightness {level}"),
            Self::Status => f.write_str("status"),
        }
    }
}

impl FromStr for Request {
    type Err = anyhow::Error;

    fn from_str(line: &str) -> Result<Self> {
        let mut words = line.split_whitespace();
        match words.next() {
            Some("set") => {
                let (Some(panel), Some(scene)) = (words.next(), words.next()) else {
                    bail!("set needs a panel and a scene, e.g. `set left clock,cpu`");
                };
                Ok(Self::Set {
                    panel: panel.parse()?,
                    scene: scene.parse()?,
                })
            }
            Some("brightness") => {
                let Some(level) = words.next() else {
                    bail!("brightness needs a level from 0 to 255");
                };
                Ok(Self::Brightness(level.parse()?))
            }
            Some("status") => Ok(Self::Status),
            Some(other) => bail!("unknown command {other:?}"),
            None => bail!("empty request"),
        }
    }
}

/// Parses a scene name, reusing the spelling the command line accepts.
///
/// Public because the saved state writes scenes with `Display` and has to read
/// them back the same way; one spelling, one parser.
pub fn scene_from_str(name: &str) -> Result<SceneKind> {
    match name {
        "pong" => Ok(SceneKind::Pong),
        "snake" => Ok(SceneKind::Snake),
        "tetris" => Ok(SceneKind::Tetris),
        "clock" => Ok(SceneKind::Clock),
        "cpu" => Ok(SceneKind::Cpu),
        "ram" => Ok(SceneKind::Ram),
        "net" => Ok(SceneKind::Net),
        "disk" => Ok(SceneKind::Disk),
        "volume" => Ok(SceneKind::Volume),
        "media" => Ok(SceneKind::Media),
        "battery" => Ok(SceneKind::Battery),
        "off" => Ok(SceneKind::Off),
        other => {
            bail!(
                "unknown scene {other:?}, expected pong, snake, tetris, clock, cpu, ram, net, disk, volume, media, battery or off"
            )
        }
    }
}

/// What the daemon answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Response {
    /// The request was carried out; the text is for the user.
    Ok(String),
    /// The request was refused; the text says why.
    Error(String),
}

impl Response {
    /// Whether the request succeeded.
    #[must_use]
    pub fn succeeded(&self) -> bool {
        matches!(self, Self::Ok(_))
    }

    /// The human-readable part, without the status word.
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::Ok(text) | Self::Error(text) => text,
        }
    }
}

impl fmt::Display for Response {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ok(text) => write!(f, "ok {text}"),
            Self::Error(text) => write!(f, "error {text}"),
        }
    }
}

impl FromStr for Response {
    type Err = anyhow::Error;

    fn from_str(line: &str) -> Result<Self> {
        let line = line.trim_end();
        if let Some(rest) = line.strip_prefix("ok") {
            Ok(Self::Ok(rest.trim_start().to_owned()))
        } else if let Some(rest) = line.strip_prefix("error") {
            Ok(Self::Error(rest.trim_start().to_owned()))
        } else {
            bail!("malformed response {line:?}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{PanelName, Request, Response, SceneSpec};
    use crate::scene::SceneKind;

    #[test]
    fn a_stack_survives_a_round_trip() {
        // The spec is what the socket, the command line and the saved state all
        // speak, so one spelling has to serve all three.
        let spec: SceneSpec = "clock,cpu,battery".parse().expect("parse");
        assert_eq!(spec.scenes().len(), 3);
        assert_eq!(spec.to_string(), "clock,cpu,battery");
        assert_eq!(spec.to_string().parse::<SceneSpec>().unwrap(), spec);
    }

    #[test]
    fn a_spec_with_no_scene_is_refused() {
        assert!("".parse::<SceneSpec>().is_err());
        assert!(",,".parse::<SceneSpec>().is_err());
        assert!("clock,asteroids".parse::<SceneSpec>().is_err());
    }

    #[test]
    fn requests_survive_a_round_trip() {
        // Both ends share this parser, so a round trip is what proves the client
        // and the daemon cannot drift apart.
        for request in [
            Request::Set {
                panel: PanelName::Left,
                scene: SceneSpec::single(SceneKind::Snake),
            },
            Request::Set {
                panel: PanelName::Right,
                scene: SceneSpec::single(SceneKind::Off),
            },
            Request::Brightness(0),
            Request::Brightness(255),
            Request::Status,
        ] {
            let line = request.to_string();
            let parsed: Request = line.parse().expect(&line);
            assert_eq!(parsed, request, "round trip changed {line:?}");
        }
    }

    #[test]
    fn responses_survive_a_round_trip() {
        for response in [
            Response::Ok("left now shows snake".to_owned()),
            Response::Error("unknown panel".to_owned()),
            Response::Ok(String::new()),
        ] {
            let line = response.to_string();
            let parsed: Response = line.parse().expect(&line);
            assert_eq!(parsed, response);
        }
    }

    #[test]
    fn extra_whitespace_is_tolerated() {
        let parsed: Request = "  set   left    pong  ".parse().expect("parse");
        assert_eq!(
            parsed,
            Request::Set {
                panel: PanelName::Left,
                scene: SceneSpec::single(SceneKind::Pong)
            }
        );
    }

    #[test]
    fn nonsense_is_refused_with_a_useful_message() {
        let cases = [
            ("", "empty"),
            ("wibble", "unknown command"),
            ("set left", "needs a panel and a scene"),
            ("set middle pong", "unknown panel"),
            ("set left asteroids", "unknown scene"),
            ("brightness", "needs a level"),
            ("brightness loud", "invalid digit"),
        ];
        for (line, expected) in cases {
            let error = line
                .parse::<Request>()
                .expect_err(&format!("{line:?} should not parse"))
                .to_string();
            assert!(
                error.contains(expected),
                "{line:?} said {error:?}, expected something about {expected:?}"
            );
        }
    }

    #[test]
    fn a_brightness_over_255_is_refused() {
        assert!("brightness 256".parse::<Request>().is_err());
    }

    #[test]
    fn a_response_reports_its_own_outcome() {
        assert!(Response::Ok("fine".to_owned()).succeeded());
        assert!(!Response::Error("nope".to_owned()).succeeded());
        assert_eq!(Response::Error("nope".to_owned()).message(), "nope");
    }
}
