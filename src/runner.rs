//! The fixed-rate render loop driving one panel.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tracing::{debug, info, warn};

use crate::canvas::Canvas;
use crate::device::Matrix;
use crate::scene::Scene;

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

/// Runs `scene` on `matrix` until `shutdown` is raised.
///
/// This blocks, so it belongs on its own thread. On the way out it clears the
/// panel: a daemon that exits leaving the LEDs lit is a daemon you notice for
/// the wrong reasons.
///
/// # Errors
///
/// Fails if the panel rejects [`MAX_DROPPED_FRAMES`] writes in a row — typically
/// the module being unplugged.
pub fn run_panel(
    label: &'static str,
    mut matrix: impl Matrix,
    mut scene: impl Scene,
    fps: u32,
    brightness: u8,
    shutdown: &Arc<AtomicBool>,
) -> Result<()> {
    let frame_time = Duration::from_secs_f64(1.0 / f64::from(fps));

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

        scene.update(delta);
        canvas.clear();
        scene.render(&mut canvas);

        match matrix.draw(&canvas) {
            Ok(()) => dropped = 0,
            Err(error) => {
                dropped += 1;
                if dropped >= MAX_DROPPED_FRAMES {
                    return Err(error).with_context(|| {
                        format!("the {label} panel dropped {dropped} frames in a row")
                    });
                }
                warn!(panel = label, dropped, ?error, "dropped a frame");
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

    matrix
        .clear()
        .with_context(|| format!("clearing the {label} panel"))?;
    info!(panel = label, frames, "panel stopped");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::run_panel;
    use crate::device::MockMatrix;
    use crate::scene::MockScene;
    use anyhow::anyhow;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// A scene that does nothing, cheaply.
    fn idle_scene() -> MockScene {
        let mut scene = MockScene::new();
        scene.expect_name().returning(|| "test");
        scene.expect_update().returning(|_| ());
        scene.expect_render().returning(|_| ());
        scene
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

        run_panel("left", matrix, idle_scene(), 240, 30, &shutdown).expect("clean shutdown");
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

        run_panel("right", matrix, idle_scene(), 240, 42, &shutdown).expect("clean shutdown");
    }

    #[test]
    fn a_dead_module_stops_the_loop_with_context() {
        let shutdown = Arc::new(AtomicBool::new(false));

        let mut matrix = MockMatrix::new();
        matrix.expect_set_brightness().returning(|_| Ok(()));
        matrix
            .expect_draw()
            .times(super::MAX_DROPPED_FRAMES as usize)
            .returning(|_| Err(anyhow!("module unplugged")));
        // The panel is gone, so no clear is attempted.
        matrix.expect_clear().never();

        let error = run_panel("left", matrix, idle_scene(), 240, 30, &shutdown)
            .expect_err("a module that never accepts a frame must stop the panel");
        assert!(error.to_string().contains("left"), "lost the panel label");
    }

    #[test]
    fn a_usb_hiccup_does_not_take_the_panel_down() {
        // The modules time out a write now and then, typically just after being
        // re-enumerated. One bad frame must not end the session.
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

        run_panel("left", matrix, idle_scene(), 240, 30, &shutdown)
            .expect("intermittent failures must not stop the panel");
        assert!(attempts.load(Ordering::Relaxed) > 40);
    }

    #[test]
    fn a_shutdown_raised_before_the_first_frame_draws_nothing() {
        let shutdown = Arc::new(AtomicBool::new(true));

        let mut matrix = MockMatrix::new();
        matrix.expect_set_brightness().returning(|_| Ok(()));
        matrix.expect_draw().never();
        matrix.expect_clear().times(1).returning(|| Ok(()));

        run_panel("left", matrix, idle_scene(), 30, 30, &shutdown).expect("clean shutdown");
    }
}
