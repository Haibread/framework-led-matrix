//! A value refreshed on its own thread.
//!
//! Two widgets need answers from outside the process — the volume from
//! `WirePlumber`, the track from whatever is on the session bus — and neither can
//! be asked from the render loop: a D-Bus round trip that takes 50 ms would
//! cost a frame and a half every time. So the asking happens elsewhere and the
//! scene reads whatever the last answer was.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use tracing::debug;

/// How finely a sleeping poller checks whether it should stop.
const STOP_POLL: Duration = Duration::from_millis(100);

/// The latest value a background thread has managed to read.
pub struct Poller<T> {
    latest: Arc<Mutex<T>>,
    stop: Arc<AtomicBool>,
}

impl<T: Clone + Send + 'static> Poller<T> {
    /// Starts reading `read` every `interval`, beginning with `initial`.
    ///
    /// A read returning `None` leaves the previous value in place: a media
    /// player restarting should blank the widget for a moment, not for good.
    pub fn spawn(
        name: &'static str,
        initial: T,
        interval: Duration,
        mut read: impl FnMut() -> Option<T> + Send + 'static,
    ) -> Self {
        let latest = Arc::new(Mutex::new(initial));
        let stop = Arc::new(AtomicBool::new(false));

        let shared = Arc::clone(&latest);
        let stopping = Arc::clone(&stop);
        thread::spawn(move || {
            debug!(poller = name, "started");
            while !stopping.load(Ordering::Relaxed) {
                if let Some(value) = read() {
                    if let Ok(mut slot) = shared.lock() {
                        *slot = value;
                    }
                }
                wait(interval, &stopping);
            }
            debug!(poller = name, "stopped");
        });

        Self { latest, stop }
    }

    /// The most recent value read.
    #[must_use]
    pub fn latest(&self) -> T {
        self.latest.lock().map_or_else(
            |poisoned| poisoned.into_inner().clone(),
            |value| value.clone(),
        )
    }
}

impl<T> Drop for Poller<T> {
    fn drop(&mut self) {
        // Switching scenes drops the widget; without this its thread would
        // outlive it and keep polling for the rest of the session.
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Sleeps for `total`, giving up early once the flag is raised.
fn wait(total: Duration, stop: &Arc<AtomicBool>) {
    let deadline = Instant::now() + total;
    while !stop.load(Ordering::Relaxed) {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        thread::sleep(left.min(STOP_POLL));
    }
}

#[cfg(test)]
mod tests {
    use super::Poller;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[test]
    fn the_latest_value_reaches_the_reader() {
        let counter = Arc::new(AtomicUsize::new(0));
        let ticks = Arc::clone(&counter);
        let poller = Poller::spawn("test", 0, Duration::from_millis(5), move || {
            Some(ticks.fetch_add(1, Ordering::Relaxed) + 1)
        });

        for _ in 0..100 {
            if poller.latest() > 0 {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the poller never published anything");
    }

    #[test]
    fn a_failed_read_keeps_the_previous_value() {
        // A player closing, or wpctl missing for a moment, should not blank a
        // widget that had something to show.
        let first = Arc::new(AtomicUsize::new(0));
        let attempts = Arc::clone(&first);
        let poller = Poller::spawn("test", 0usize, Duration::from_millis(5), move || {
            if attempts.fetch_add(1, Ordering::Relaxed) == 0 {
                Some(42)
            } else {
                None
            }
        });

        for _ in 0..100 {
            if poller.latest() == 42 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(poller.latest(), 42, "a failed read wiped the value");
    }

    #[test]
    fn dropping_the_poller_stops_its_thread() {
        let counter = Arc::new(AtomicUsize::new(0));
        let ticks = Arc::clone(&counter);
        let poller = Poller::spawn("test", 0, Duration::from_millis(5), move || {
            Some(ticks.fetch_add(1, Ordering::Relaxed))
        });

        std::thread::sleep(Duration::from_millis(60));
        drop(poller);
        std::thread::sleep(Duration::from_millis(150));

        let after_drop = counter.load(Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(
            counter.load(Ordering::Relaxed),
            after_drop,
            "the thread outlived the poller"
        );
    }
}
