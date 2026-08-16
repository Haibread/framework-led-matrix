//! The fixed-rate render loop driving one panel.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tracing::{debug, info, warn};

use crate::canvas::Canvas;
use crate::device::{ColorMode, Matrix};
use crate::scene::Scene;

/// Something the control socket asks of a running panel.
pub enum Command<S> {
    /// Show a different scene, in the colour mode that scene wants.
    SetScene {
        /// The scene to switch to.
        scene: S,
        /// The mode it should be drawn in.
        mode: ColorMode,
    },
    /// Change the panel brightness.
    SetBrightness(u8),
}

/// What a panel is set up with, and what the control socket can change.
#[derive(Clone, Copy, Debug)]
pub struct PanelSettings {
    /// Name used in logs and on the socket.
    pub label: &'static str,
    /// Frames per second the loop aims for.
    pub fps: u32,
    /// Starting brightness.
    pub brightness: u8,
    /// Colour mode the panel is opened in.
    pub mode: ColorMode,
}

/// Longest delta handed to a scene.
///
/// If the thread is starved — or the laptop comes back from suspend — the wall
/// clock jumps. Without this cap the scene would integrate that jump in one
/// step and teleport everything across the panel.
const MAX_FRAME_DELTA: Duration = Duration::from_millis(100);

/// How often to report progress at debug level.
const FRAME_LOG_INTERVAL: u64 = 600;

/// Consecutive dropped frames tolerated before a panel gives up.
///
/// A timed-out write is usually a USB hiccup — the modules do it when they have
/// just been re-enumerated, for instance. The firmware resynchronises on the
/// next magic bytes, so the real cost is one glitched frame, whereas quitting on
/// the first failure takes the panel down for the rest of the session. At 30 fps
/// this is roughly a second of patience before calling the module dead.
const MAX_DROPPED_FRAMES: u32 = 30;

/// How long to wait before the first attempt at reopening a panel, and the
/// ceiling the wait doubles up to.
///
/// A module coming back from suspend reappears within a second or so; one that
/// has been unplugged may never come back, and polling it every second forever
/// is just noise in the journal.
const RECONNECT_DELAY_MIN: Duration = Duration::from_millis(250);
const RECONNECT_DELAY_MAX: Duration = Duration::from_secs(30);

/// Granularity at which a wait checks for shutdown.
///
/// Without this a `SIGTERM` arriving during a 30-second backoff would be
/// ignored until it elapsed, and systemd would eventually lose patience.
const SHUTDOWN_POLL: Duration = Duration::from_millis(100);

/// Runs `scene` on the panel `open` gives, until `shutdown` is raised.
///
/// This blocks, so it belongs on its own thread. On the way out it clears the
/// panel: a daemon that exits leaving the LEDs lit is a daemon you notice for
/// the wrong reasons.
///
/// `open` is a factory rather than a ready-made panel because a panel does not
/// survive forever: suspending the laptop puts the module to sleep and
/// unplugging it takes the device node away. Either way the fix is the same —
/// open it again — so the loop keeps the means to do that.
///
/// # Errors
///
/// Fails only if the panel cannot be opened at all to begin with. Once running,
/// a panel that stops responding is reopened rather than abandoned.
pub fn run_panel<M: Matrix, S: Scene>(
    settings: PanelSettings,
    open: impl Fn(ColorMode) -> Result<M>,
    mut scene: S,
    commands: &Receiver<Command<S>>,
    shutdown: &Arc<AtomicBool>,
) -> Result<()> {
    let PanelSettings {
        label,
        fps,
        mut brightness,
        mut mode,
    } = settings;
    let frame_time = Duration::from_secs_f64(1.0 / f64::from(fps));

    let mut matrix = open(mode).with_context(|| format!("opening the {label} panel"))?;
    matrix
        .set_brightness(brightness)
        .with_context(|| format!("setting brightness on the {label} panel"))?;

    info!(
        panel = label,
        scene = scene.name(),
        fps,
        brightness,
        "panel started"
    );

    let mut canvas = Canvas::new();
    let mut previous = Instant::now();
    let mut deadline = Instant::now();
    let mut frames: u64 = 0;
    let mut dropped: u32 = 0;

    while !shutdown.load(Ordering::Relaxed) {
        let now = Instant::now();
        let delta = (now - previous).min(MAX_FRAME_DELTA);
        previous = now;

        // Drain whatever the control socket asked for since the last frame.
        while let Ok(command) = commands.try_recv() {
            match command {
                Command::SetScene {
                    scene: next,
                    mode: wanted,
                } => {
                    scene = next;
                    info!(panel = label, scene = scene.name(), "scene changed");

                    // The colour mode is chosen when the port is opened, so a
                    // scene that wants the other one needs a fresh panel.
                    if wanted != mode {
                        mode = wanted;
                        match open(mode).and_then(|mut fresh| {
                            fresh.set_brightness(brightness)?;
                            Ok(fresh)
                        }) {
                            Ok(fresh) => matrix = fresh,
                            Err(error) => {
                                warn!(panel = label, ?error, "could not switch colour mode");
                            }
                        }
                    }
                }
                Command::SetBrightness(level) => {
                    brightness = level;
                    if let Err(error) = matrix.set_brightness(level) {
                        warn!(panel = label, ?error, "could not set brightness");
                    }
                }
            }
        }

        scene.update(delta);
        canvas.clear();
        scene.render(&mut canvas);

        match matrix.draw(&canvas) {
            Ok(()) => dropped = 0,
            Err(error) => {
                dropped += 1;
                warn!(panel = label, dropped, ?error, "dropped a frame");

                if dropped >= MAX_DROPPED_FRAMES {
                    warn!(panel = label, "panel stopped responding, reopening");
                    let Some(fresh) = reopen(label, &open, mode, brightness, shutdown) else {
                        break;
                    };
                    matrix = fresh;
                    dropped = 0;
                }
            }
        }

        frames += 1;
        if frames % FRAME_LOG_INTERVAL == 0 {
            debug!(panel = label, frames, "still rendering");
        }

        deadline += frame_time;
        let now = Instant::now();
        if deadline > now {
            thread::sleep(deadline - now);
        } else {
            // Running behind: give up on catching up rather than spinning.
            deadline = now;
        }
    }

    // Best effort: if the module is gone there is nothing left to clear, and
    // failing here would report a shutdown as a crash.
    if let Err(error) = matrix.clear() {
        warn!(
            panel = label,
            ?error,
            "could not clear the panel on the way out"
        );
    }
    info!(panel = label, frames, "panel stopped");
    Ok(())
}

/// Reopens a panel, backing off between attempts.
///
/// Returns `None` when shutdown is raised while waiting, so the caller stops
/// instead of retrying into a process that is trying to exit.
fn reopen<M: Matrix>(
    label: &'static str,
    open: &impl Fn(ColorMode) -> Result<M>,
    mode: ColorMode,
    brightness: u8,
    shutdown: &Arc<AtomicBool>,
) -> Option<M> {
    let mut delay = RECONNECT_DELAY_MIN;

    while !shutdown.load(Ordering::Relaxed) {
        wait(delay, shutdown);
        if shutdown.load(Ordering::Relaxed) {
            break;
        }

        match open(mode).and_then(|mut matrix| {
            matrix.set_brightness(brightness)?;
            Ok(matrix)
        }) {
            Ok(matrix) => {
                info!(panel = label, "panel reopened");
                return Some(matrix);
            }
            Err(error) => {
                warn!(
                    panel = label,
                    ?error,
                    retry_in_ms = delay.as_millis(),
                    "could not reopen the panel"
                );
                delay = (delay * 2).min(RECONNECT_DELAY_MAX);
            }
        }
    }

    None
}

/// Sleeps for `total`, giving up early once shutdown is raised.
fn wait(total: Duration, shutdown: &Arc<AtomicBool>) {
    let deadline = Instant::now() + total;
    while !shutdown.load(Ordering::Relaxed) {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        thread::sleep(left.min(SHUTDOWN_POLL));
    }
}

#[cfg(test)]
mod tests {
    use super::{Command, MAX_DROPPED_FRAMES, PanelSettings, run_panel};
    use crate::device::{ColorMode, MockMatrix};
    use crate::scene::MockScene;
    use anyhow::{Result, anyhow};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::mpsc::{Receiver, Sender, channel};

    /// The settings every test below shares; 240 fps keeps them quick.
    fn settings(label: &'static str, brightness: u8) -> PanelSettings {
        PanelSettings {
            label,
            fps: 240,
            brightness,
            mode: ColorMode::Bw,
        }
    }

    /// A command channel whose sending half is kept alive for the test.
    fn idle_commands() -> (Sender<Command<MockScene>>, Receiver<Command<MockScene>>) {
        channel()
    }

    /// A scene that does nothing, cheaply.
    fn idle_scene() -> MockScene {
        let mut scene = MockScene::new();
        scene.expect_name().returning(|| "test");
        scene.expect_update().returning(|_| ());
        scene.expect_render().returning(|_| ());
        scene
    }

    /// Hands the loop one prepared panel, then refuses to open another.
    fn once(matrix: MockMatrix) -> impl Fn(ColorMode) -> Result<MockMatrix> {
        let cell = std::sync::Mutex::new(Some(matrix));
        move |_mode| {
            cell.lock()
                .expect("lock")
                .take()
                .ok_or_else(|| anyhow!("no second panel in this test"))
        }
    }

    #[test]
    fn the_panel_is_cleared_when_the_loop_stops() {
        let shutdown = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&shutdown);
        let drawn = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&drawn);

        let mut matrix = MockMatrix::new();
        matrix
            .expect_set_brightness()
            .times(1)
            .returning(|_| Ok(()));
        matrix.expect_draw().times(3).returning(move |_| {
            if counter.fetch_add(1, Ordering::Relaxed) >= 2 {
                stop.store(true, Ordering::Relaxed);
            }
            Ok(())
        });
        matrix.expect_clear().times(1).returning(|| Ok(()));

        let (_keep, commands) = idle_commands();
        run_panel(
            settings("left", 30),
            once(matrix),
            idle_scene(),
            &commands,
            &shutdown,
        )
        .expect("clean shutdown");
        assert_eq!(drawn.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn the_configured_brightness_reaches_the_panel() {
        let shutdown = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&shutdown);

        let mut matrix = MockMatrix::new();
        matrix
            .expect_set_brightness()
            .withf(|level| *level == 42)
            .times(1)
            .returning(|_| Ok(()));
        matrix.expect_draw().returning(move |_| {
            stop.store(true, Ordering::Relaxed);
            Ok(())
        });
        matrix.expect_clear().returning(|| Ok(()));

        let (_keep, commands) = idle_commands();
        run_panel(
            settings("right", 42),
            once(matrix),
            idle_scene(),
            &commands,
            &shutdown,
        )
        .expect("clean shutdown");
    }

    #[test]
    fn a_panel_that_cannot_be_opened_at_all_is_an_error() {
        let shutdown = Arc::new(AtomicBool::new(false));
        let open = |_mode| -> Result<MockMatrix> { Err(anyhow!("permission denied")) };
        let (_keep, commands) = idle_commands();

        let error = run_panel(
            settings("left", 30),
            open,
            idle_scene(),
            &commands,
            &shutdown,
        )
        .expect_err("an unopenable panel must be reported");
        assert!(error.to_string().contains("left"), "lost the panel label");
    }

    #[test]
    fn a_module_that_stops_responding_is_reopened_not_abandoned() {
        // Suspending the laptop puts the module to sleep and unplugging it takes
        // the device node away; both used to end the panel for the rest of the
        // session. The loop now opens a new one instead.
        let shutdown = Arc::new(AtomicBool::new(false));
        let opens = Arc::new(AtomicUsize::new(0));

        let stop = Arc::clone(&shutdown);
        let counter = Arc::clone(&opens);
        let open = move |_mode| {
            // The second open proves the panel was reopened; wind down there.
            if counter.fetch_add(1, Ordering::Relaxed) >= 1 {
                stop.store(true, Ordering::Relaxed);
            }

            let mut matrix = MockMatrix::new();
            matrix.expect_set_brightness().returning(|_| Ok(()));
            matrix
                .expect_draw()
                .returning(|_| Err(anyhow!("Operation timed out")));
            matrix.expect_clear().returning(|| Ok(()));
            Ok(matrix)
        };

        let (_keep, commands) = idle_commands();
        run_panel(
            settings("left", 30),
            open,
            idle_scene(),
            &commands,
            &shutdown,
        )
        .expect("clean shutdown");
        assert!(
            opens.load(Ordering::Relaxed) >= 2,
            "the panel was never reopened"
        );
    }

    #[test]
    fn a_usb_hiccup_does_not_take_the_panel_down() {
        // The modules time out a write now and then, typically just after being
        // re-enumerated. One bad frame must not cost a reconnection.
        let shutdown = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&shutdown);
        let attempts = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&attempts);

        let mut matrix = MockMatrix::new();
        matrix.expect_set_brightness().returning(|_| Ok(()));
        matrix.expect_draw().returning(move |_| {
            let attempt = counter.fetch_add(1, Ordering::Relaxed);
            if attempt >= 40 {
                stop.store(true, Ordering::Relaxed);
            }
            // Fail every other frame, forever, without ever failing twice in a
            // row: the panel should keep going regardless.
            if attempt % 2 == 0 {
                Err(anyhow!("Operation timed out"))
            } else {
                Ok(())
            }
        });
        matrix.expect_clear().times(1).returning(|| Ok(()));

        // `once` refuses a second panel, so this passing proves no reconnection
        // was attempted.
        let (_keep, commands) = idle_commands();
        run_panel(
            settings("left", 30),
            once(matrix),
            idle_scene(),
            &commands,
            &shutdown,
        )
        .expect("intermittent failures must not stop the panel");
        assert!(attempts.load(Ordering::Relaxed) > 40);
    }

    #[test]
    fn shutdown_during_a_reconnect_backoff_is_not_ignored() {
        // Waiting out a 30-second backoff before noticing SIGTERM would have
        // systemd give up on the service and kill it.
        let shutdown = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&shutdown);
        let opens = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&opens);

        let open = move |_mode| -> Result<MockMatrix> {
            if counter.fetch_add(1, Ordering::Relaxed) == 0 {
                let mut matrix = MockMatrix::new();
                matrix.expect_set_brightness().returning(|_| Ok(()));
                matrix
                    .expect_draw()
                    .returning(|_| Err(anyhow!("module unplugged")));
                matrix.expect_clear().returning(|| Ok(()));
                return Ok(matrix);
            }
            // The module never comes back; shutdown is what ends this.
            stop.store(true, Ordering::Relaxed);
            Err(anyhow!("still gone"))
        };

        let (_keep, commands) = idle_commands();
        let started = std::time::Instant::now();
        run_panel(
            settings("left", 30),
            open,
            idle_scene(),
            &commands,
            &shutdown,
        )
        .expect("clean shutdown");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "shutdown took {:?}",
            started.elapsed()
        );
        assert!(opens.load(Ordering::Relaxed) >= 2);
    }

    #[test]
    fn frames_are_dropped_before_a_reconnection_is_attempted() {
        let shutdown = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&shutdown);
        let draws = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&draws);
        let opens = Arc::new(AtomicUsize::new(0));
        let open_counter = Arc::clone(&opens);

        let open = move |_mode| {
            if open_counter.fetch_add(1, Ordering::Relaxed) >= 1 {
                stop.store(true, Ordering::Relaxed);
            }
            let counter = Arc::clone(&counter);
            let mut matrix = MockMatrix::new();
            matrix.expect_set_brightness().returning(|_| Ok(()));
            matrix.expect_draw().returning(move |_| {
                counter.fetch_add(1, Ordering::Relaxed);
                Err(anyhow!("Operation timed out"))
            });
            matrix.expect_clear().returning(|| Ok(()));
            Ok(matrix)
        };

        let (_keep, commands) = idle_commands();
        run_panel(
            settings("left", 30),
            open,
            idle_scene(),
            &commands,
            &shutdown,
        )
        .expect("clean shutdown");
        assert_eq!(
            draws.load(Ordering::Relaxed),
            MAX_DROPPED_FRAMES as usize,
            "reconnected at the wrong moment"
        );
    }
}
