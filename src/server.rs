//! The daemon side of the control socket.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tracing::{debug, info, warn};

use crate::canvas::Canvas;
use crate::cli::ColorModeChoice;
use crate::control::{PanelName, Request, Response, SceneSpec};
use crate::runner::Command;
use crate::scene::{AnyScene, Stack};
use crate::state::{self, State};

/// Everything the socket needs to act on a request.
pub struct Control {
    /// One channel per running panel; a panel started off has no entry.
    panels: HashMap<PanelName, Sender<Command<Stack>>>,
    /// What each panel is showing, for `status`.
    showing: Mutex<HashMap<PanelName, SceneSpec>>,
    /// Colour mode override from the command line.
    color_mode: ColorModeChoice,
    /// Seed for scenes built later, so a seeded run stays reproducible.
    seed: Option<u64>,
    /// What each panel is drawing, refreshed by its own thread.
    mirrors: HashMap<PanelName, Arc<Mutex<Canvas>>>,
    /// What to write back, and where, so a restart picks up where it left.
    saved: Mutex<State>,
    state_path: PathBuf,
}

impl Control {
    /// Wraps the panel channels the daemon just started.
    #[must_use]
    pub fn new(
        panels: HashMap<PanelName, Sender<Command<Stack>>>,
        showing: HashMap<PanelName, SceneSpec>,
        color_mode: ColorModeChoice,
        seed: Option<u64>,
        mirrors: HashMap<PanelName, Arc<Mutex<Canvas>>>,
        brightness: u8,
        state_path: PathBuf,
    ) -> Self {
        let mut saved = State {
            brightness: Some(brightness),
            ..State::default()
        };
        for (panel, scene) in &showing {
            saved.set_scene(*panel, scene.clone());
        }

        Self {
            panels,
            mirrors,
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
        if let Err(error) = state::save(&self.state_path, &saved) {
            warn!(?error, "could not save the setup");
        }
    }

    /// Carries out one request.
    fn handle(&self, request: Request) -> Response {
        match request {
            Request::Set { panel, scene } => self.set(panel, &scene),
            Request::Brightness(level) => self.brightness(level),
            Request::Status => self.status(),
            // Handled by the connection itself, which has to keep writing.
            Request::Watch => Response::Ok(String::new()),
        }
    }

    /// What a panel is drawing right now, if it is running.
    fn frame(&self, panel: PanelName) -> Option<Canvas> {
        let mirror = self.mirrors.get(&panel)?;
        mirror.lock().ok().map(|canvas| canvas.clone())
    }

    /// Switches a panel to another set of scenes.
    fn set(&self, panel: PanelName, spec: &SceneSpec) -> Response {
        let Some(channel) = self.panels.get(&panel) else {
            return Response::Error(format!(
                "the {panel} panel is not running; it was started off"
            ));
        };

        // A stack takes the colour mode of the scene at its head: mixing a
        // game and a widget on one panel would otherwise have no answer.
        let mode = self.color_mode.resolve(
            spec.scenes()
                .first()
                .map_or(crate::device::ColorMode::Greyscale, |kind| {
                    kind.preferred_color_mode()
                }),
        );

        let scene = match AnyScene::stack(spec, self.seed, mode) {
            Ok(stack) => stack,
            Err(error) => return Response::Error(error.to_string()),
        };

        if channel.send(Command::SetScene { scene, mode }).is_err() {
            return Response::Error(format!("the {panel} panel has stopped"));
        }

        if let Ok(mut showing) = self.showing.lock() {
            showing.insert(panel, spec.clone());
        }
        let remembered = spec.clone();
        self.remember(move |saved| saved.set_scene(panel, remembered));
        Response::Ok(format!("{panel} now shows {spec}"))
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
                let spec = showing
                    .get(panel)
                    .map_or_else(|| "off".to_owned(), ToString::to_string);
                format!("{panel}={spec}")
            })
            .collect();
        Response::Ok(report.join(" "))
    }
}

/// How often a watcher is sent a fresh pair of frames.
const WATCH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

/// Serves the control socket until the task is dropped.
///
/// # Errors
///
/// Fails if the socket cannot be bound — usually a stale socket owned by
/// another user, or a runtime directory that does not exist.
pub async fn serve(path: PathBuf, control: Control) -> Result<()> {
    let listener = bind(&path)?;
    info!(socket = %path.display(), "control socket ready");

    // One task per connection, because `watch` never returns: answering
    // connections one after another in this loop meant a single watcher wedged
    // the socket shut for everybody, itself included.
    let control = Arc::new(control);
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let control = Arc::clone(&control);
                tokio::spawn(async move {
                    if let Err(error) = handle_connection(stream, &control).await {
                        debug!(?error, "control connection ended badly");
                    }
                });
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

    let request = match line.trim().parse::<Request>() {
        Ok(request) => request,
        Err(error) => {
            let mut stream = reader.into_inner();
            let refusal = Response::Error(error.to_string());
            stream.write_all(format!("{refusal}\n").as_bytes()).await?;
            return stream.flush().await.map_err(Into::into);
        }
    };
    debug!(%request, "control request");

    if request == Request::Watch {
        return stream_frames(reader.into_inner(), control).await;
    }

    let response = control.handle(request);
    let mut stream = reader.into_inner();
    stream.write_all(format!("{response}\n").as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}

/// Sends what each panel is drawing, until the caller goes away.
///
/// Twenty a second rather than the panel's own rate: it is a preview, and the
/// difference is invisible next to the cost of asking every panel for a copy.
async fn stream_frames(mut stream: UnixStream, control: &Control) -> Result<()> {
    let mut ticker = tokio::time::interval(WATCH_INTERVAL);
    loop {
        ticker.tick().await;
        for panel in PanelName::ALL {
            let Some(canvas) = control.frame(panel) else {
                continue;
            };
            let line = format!("frame {panel} {}\n", canvas.to_hex());
            // A watcher hanging up is how this ends, and it is not a fault.
            if stream.write_all(line.as_bytes()).await.is_err() {
                debug!("a watcher went away");
                return Ok(());
            }
        }
        if stream.flush().await.is_err() {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Control;
    use crate::canvas::Canvas;
    use crate::cli::ColorModeChoice;
    use crate::control::{PanelName, Request, Response, SceneSpec};
    use crate::runner::Command;
    use crate::scene::{SceneKind, Stack};
    use std::collections::HashMap;
    use std::sync::mpsc::{Receiver, channel};
    use std::sync::{Arc, Mutex};

    /// A control plane with the left panel running and the right one off.
    fn control() -> (Control, Receiver<Command<Stack>>) {
        let (sender, receiver) = channel();
        let mut panels = HashMap::new();
        panels.insert(PanelName::Left, sender);

        let mut showing = HashMap::new();
        showing.insert(PanelName::Left, SceneSpec::single(SceneKind::Pong));

        let mut mirrors = HashMap::new();
        mirrors.insert(PanelName::Left, Arc::new(Mutex::new(Canvas::new())));

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
                mirrors,
                30,
                state_path,
            ),
            receiver,
        )
    }

    #[tokio::test]
    async fn a_watcher_does_not_wedge_the_socket_shut() {
        // `watch` never returns. Answering connections one at a time meant the
        // first watcher locked everyone out — including the interface that had
        // just opened it, which then hung before drawing anything.
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let (control, _receiver) = control();
        let path = std::env::temp_dir().join(format!(
            "ledmat-wedge-{}-{:?}.sock",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_file(&path);
        let serving = tokio::spawn(super::serve(path.clone(), control));

        // Wait for the socket to appear rather than guessing at a delay.
        let mut watcher = loop {
            match tokio::net::UnixStream::connect(&path).await {
                Ok(stream) => break stream,
                Err(_) => tokio::task::yield_now().await,
            }
        };
        watcher
            .write_all(b"watch\n")
            .await
            .expect("asking to watch");
        watcher.flush().await.expect("asking to watch");

        let asking = async {
            let mut stream = tokio::net::UnixStream::connect(&path)
                .await
                .expect("a second connection");
            stream.write_all(b"status\n").await.expect("asking");
            stream.flush().await.expect("asking");
            let mut line = String::new();
            BufReader::new(stream)
                .read_line(&mut line)
                .await
                .expect("reading the answer");
            line
        };

        let answer = tokio::time::timeout(std::time::Duration::from_secs(5), asking)
            .await
            .expect("the socket was wedged shut by the watcher");
        assert!(
            answer.parse::<Response>().is_ok_and(|it| it.succeeded()),
            "unexpected answer: {answer}"
        );

        serving.abort();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_watcher_is_given_what_the_panel_is_drawing() {
        // The preview is only worth having if it is the panel's own frame and
        // not a second guess at it.
        let (control, _receiver) = control();
        let mut drawn = Canvas::new();
        drawn.set(4, 17, 0xC3);

        if let Some(mirror) = control.mirrors.get(&PanelName::Left) {
            if let Ok(mut slot) = mirror.lock() {
                slot.clone_from(&drawn);
            }
        }

        assert_eq!(control.frame(PanelName::Left), Some(drawn));
    }

    #[test]
    fn a_panel_that_is_not_running_has_no_frame() {
        let (control, _receiver) = control();
        assert_eq!(control.frame(PanelName::Right), None);
    }

    #[test]
    fn setting_a_scene_reaches_the_panel() {
        let (control, receiver) = control();

        let response = control.handle(Request::Set {
            panel: PanelName::Left,
            scene: SceneSpec::single(SceneKind::Snake),
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
            scene: SceneSpec::single(SceneKind::Off),
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
            scene: SceneSpec::single(SceneKind::Pong),
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
            scene: SceneSpec::single(SceneKind::Snake),
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
            scene: SceneSpec::single(SceneKind::Snake),
        });

        assert!(!response.succeeded());
        assert!(response.message().contains("stopped"), "{response}");
    }
}
