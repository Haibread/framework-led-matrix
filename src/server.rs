//! The daemon side of the control socket.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc::Sender;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tracing::{debug, info, warn};

use crate::cli::ColorModeChoice;
use crate::control::{PanelName, Request, Response};
use crate::runner::Command;
use crate::scene::{AnyScene, SceneKind};
use crate::state::{self, State};

/// Everything the socket needs to act on a request.
pub struct Control {
    /// One channel per running panel; a panel started off has no entry.
    panels: HashMap<PanelName, Sender<Command<AnyScene>>>,
    /// What each panel is showing, for `status`.
    showing: Mutex<HashMap<PanelName, SceneKind>>,
    /// Colour mode override from the command line.
    color_mode: ColorModeChoice,
    /// Seed for scenes built later, so a seeded run stays reproducible.
    seed: Option<u64>,
    /// What to write back, and where, so a restart picks up where it left.
    saved: Mutex<State>,
    state_path: PathBuf,
}

impl Control {
    /// Wraps the panel channels the daemon just started.
    #[must_use]
    pub fn new(
        panels: HashMap<PanelName, Sender<Command<AnyScene>>>,
        showing: HashMap<PanelName, SceneKind>,
        color_mode: ColorModeChoice,
        seed: Option<u64>,
        brightness: u8,
        state_path: PathBuf,
    ) -> Self {
        let mut saved = State {
            brightness: Some(brightness),
            ..State::default()
        };
        for (panel, scene) in &showing {
            saved.set_scene(*panel, *scene);
        }

        Self {
            panels,
            showing: Mutex::new(showing),
            color_mode,
            seed,
            saved: Mutex::new(saved),
            state_path,
        }
    }

    /// Writes the setup down, so the next start comes up the same.
    ///
    /// A failure here costs the memory, not the command that was just carried
    /// out, so it is logged rather than reported back.
    fn remember(&self, change: impl FnOnce(&mut State)) {
        let Ok(mut saved) = self.saved.lock() else {
            return;
        };
        change(&mut saved);
        if let Err(error) = state::save(&self.state_path, *saved) {
            warn!(?error, "could not save the setup");
        }
    }

    /// Carries out one request.
    fn handle(&self, request: Request) -> Response {
        match request {
            Request::Set { panel, scene } => self.set(panel, scene),
            Request::Brightness(level) => self.brightness(level),
            Request::Status => self.status(),
        }
    }

    /// Switches a panel to another scene.
    fn set(&self, panel: PanelName, kind: SceneKind) -> Response {
        let Some(channel) = self.panels.get(&panel) else {
            return Response::Error(format!(
                "the {panel} panel is not running; it was started off"
            ));
        };

        let mode = self.color_mode.resolve(kind.preferred_color_mode());
        let scene = AnyScene::or_blank(kind, self.seed, mode);

        if channel.send(Command::SetScene { scene, mode }).is_err() {
            return Response::Error(format!("the {panel} panel has stopped"));
        }

        if let Ok(mut showing) = self.showing.lock() {
            showing.insert(panel, kind);
        }
        self.remember(|saved| saved.set_scene(panel, kind));
        Response::Ok(format!("{panel} now shows {kind}"))
    }

    /// Changes the brightness of every running panel.
    fn brightness(&self, level: u8) -> Response {
        let mut reached = 0;
        for channel in self.panels.values() {
            if channel.send(Command::SetBrightness(level)).is_ok() {
                reached += 1;
            }
        }

        if reached == 0 {
            return Response::Error("no panel is running".to_owned());
        }
        self.remember(|saved| saved.brightness = Some(level));
        Response::Ok(format!("brightness {level} on {reached} panel(s)"))
    }

    /// Reports what each panel is showing.
    fn status(&self) -> Response {
        let Ok(showing) = self.showing.lock() else {
            return Response::Error("the daemon lost track of its panels".to_owned());
        };

        let report: Vec<String> = PanelName::ALL
            .iter()
            .map(|panel| {
                let kind = showing
                    .get(panel)
                    .map_or_else(|| "off".to_owned(), ToString::to_string);
                format!("{panel}={kind}")
            })
            .collect();
        Response::Ok(report.join(" "))
    }
}

/// Serves the control socket until the task is dropped.
///
/// # Errors
///
/// Fails if the socket cannot be bound — usually a stale socket owned by
/// another user, or a runtime directory that does not exist.
pub async fn serve(path: PathBuf, control: Control) -> Result<()> {
    let listener = bind(&path)?;
    info!(socket = %path.display(), "control socket ready");

    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                if let Err(error) = handle_connection(stream, &control).await {
                    debug!(?error, "control connection ended badly");
                }
            }
            Err(error) => warn!(?error, "could not accept a control connection"),
        }
    }
}

/// Binds the socket, clearing a stale one left by a crashed daemon.
fn bind(path: &Path) -> Result<UnixListener> {
    match UnixListener::bind(path) {
        Ok(listener) => Ok(listener),
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
            // Someone is either listening, or died without cleaning up. A
            // connection attempt is the only way to tell the two apart.
            if std::os::unix::net::UnixStream::connect(path).is_ok() {
                anyhow::bail!("another ledmat is already listening on {}", path.display());
            }
            debug!(socket = %path.display(), "removing a stale socket");
            std::fs::remove_file(path)
                .with_context(|| format!("removing the stale socket at {}", path.display()))?;
            UnixListener::bind(path)
                .with_context(|| format!("binding the control socket at {}", path.display()))
        }
        Err(error) => {
            Err(error).with_context(|| format!("binding the control socket at {}", path.display()))
        }
    }
}

/// Reads one request from a connection and answers it.
async fn handle_connection(stream: UnixStream, control: &Control) -> Result<()> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await?;

    let response = match line.trim().parse::<Request>() {
        Ok(request) => {
            debug!(%request, "control request");
            control.handle(request)
        }
        Err(error) => Response::Error(error.to_string()),
    };

    let mut stream = reader.into_inner();
    stream.write_all(format!("{response}\n").as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::Control;
    use crate::cli::ColorModeChoice;
    use crate::control::{PanelName, Request, Response};
    use crate::runner::Command;
    use crate::scene::SceneKind;
    use std::collections::HashMap;
    use std::sync::mpsc::{Receiver, channel};

    /// A control plane with the left panel running and the right one off.
    fn control() -> (Control, Receiver<Command<crate::scene::AnyScene>>) {
        let (sender, receiver) = channel();
        let mut panels = HashMap::new();
        panels.insert(PanelName::Left, sender);

        let mut showing = HashMap::new();
        showing.insert(PanelName::Left, SceneKind::Pong);

        // A path under the temporary directory: the tests exercise saving, and
        // must not scribble on the real setup.
        let state_path = std::env::temp_dir()
            .join(format!("ledmat-test-{}", std::process::id()))
            .join("state");
        (
            Control::new(
                panels,
                showing,
                ColorModeChoice::Auto,
                Some(1),
                30,
                state_path,
            ),
            receiver,
        )
    }

    #[test]
    fn setting_a_scene_reaches_the_panel() {
        let (control, receiver) = control();

        let response = control.handle(Request::Set {
            panel: PanelName::Left,
            scene: SceneKind::Snake,
        });

        assert!(response.succeeded(), "{response}");
        assert!(response.message().contains("snake"));
        match receiver.try_recv().expect("the panel heard nothing") {
            Command::SetScene { mode, .. } => {
                assert_eq!(mode, SceneKind::Snake.preferred_color_mode());
            }
            Command::SetBrightness(_) => panic!("wrong command"),
        }
    }

    #[test]
    fn switching_a_panel_off_blanks_it_rather_than_stopping_it() {
        let (control, receiver) = control();

        let response = control.handle(Request::Set {
            panel: PanelName::Left,
            scene: SceneKind::Off,
        });

        assert!(response.succeeded(), "{response}");
        assert!(
            matches!(receiver.try_recv(), Ok(Command::SetScene { .. })),
            "a panel switched off should still be sent a scene"
        );
    }

    #[test]
    fn a_panel_that_was_never_started_says_so() {
        let (control, _receiver) = control();

        let response = control.handle(Request::Set {
            panel: PanelName::Right,
            scene: SceneKind::Pong,
        });

        assert!(!response.succeeded());
        assert!(response.message().contains("not running"), "{response}");
    }

    #[test]
    fn brightness_reaches_every_running_panel() {
        let (control, receiver) = control();

        let response = control.handle(Request::Brightness(12));

        assert!(response.succeeded(), "{response}");
        assert!(matches!(
            receiver.try_recv(),
            Ok(Command::SetBrightness(12))
        ));
    }

    #[test]
    fn status_reports_both_panels() {
        let (control, _receiver) = control();

        let response = control.handle(Request::Status);

        assert_eq!(response, Response::Ok("left=pong right=off".to_owned()));
    }

    #[test]
    fn status_follows_a_scene_change() {
        let (control, _receiver) = control();

        control.handle(Request::Set {
            panel: PanelName::Left,
            scene: SceneKind::Snake,
        });

        assert_eq!(
            control.handle(Request::Status),
            Response::Ok("left=snake right=off".to_owned())
        );
    }

    #[test]
    fn a_panel_whose_thread_has_gone_is_reported_not_ignored() {
        let (control, receiver) = control();
        drop(receiver);

        let response = control.handle(Request::Set {
            panel: PanelName::Left,
            scene: SceneKind::Snake,
        });

        assert!(!response.succeeded());
        assert!(response.message().contains("stopped"), "{response}");
    }
}
