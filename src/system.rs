//! Reading what the widgets display: processor, memory and battery.
//!
//! Every file this touches is parsed by a pure function that takes the text,
//! so the tests exercise real kernel output without depending on the machine
//! they run on — a battery test that only passes on a laptop is not a test.

use std::fs;
use std::path::PathBuf;

/// Where the kernel reports processor time.
const PROC_STAT: &str = "/proc/stat";
/// Where the kernel reports memory.
const PROC_MEMINFO: &str = "/proc/meminfo";
/// Where the kernel reports power supplies.
const POWER_SUPPLY: &str = "/sys/class/power_supply";

/// A reading of cumulative processor time.
///
/// Only useful next to another one: the kernel counts ticks since boot, so a
/// single sample says nothing about current load.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpuSample {
    idle: u64,
    total: u64,
}

impl CpuSample {
    /// The share of time spent working between this sample and a later one.
    ///
    /// Returns `None` when the counters have not moved, which is what a reading
    /// taken twice in the same tick looks like.
    #[must_use]
    pub fn busy_since(self, earlier: Self) -> Option<f32> {
        let total = self.total.checked_sub(earlier.total)?;
        let idle = self.idle.checked_sub(earlier.idle)?;
        if total == 0 {
            return None;
        }
        // The counters are tick counts, small enough that this stays exact.
        let busy = total.saturating_sub(idle);
        Some(ratio(busy, total))
    }
}

/// Parses the aggregate `cpu` line of `/proc/stat`.
#[must_use]
pub fn parse_cpu(stat: &str) -> Option<CpuSample> {
    let line = stat.lines().find(|line| line.starts_with("cpu "))?;
    let fields: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .filter_map(|field| field.parse().ok())
        .collect();

    // user nice system idle iowait irq softirq ...; idle and iowait are both
    // time the processor had nothing to do.
    let idle = fields.get(3)?.checked_add(*fields.get(4).unwrap_or(&0))?;
    let total = fields.iter().try_fold(0u64, |sum, f| sum.checked_add(*f))?;
    Some(CpuSample { idle, total })
}

/// Parses `/proc/meminfo` into the share of memory in use.
#[must_use]
pub fn parse_memory(meminfo: &str) -> Option<f32> {
    let field = |name: &str| -> Option<u64> {
        meminfo
            .lines()
            .find(|line| line.starts_with(name))?
            .split_whitespace()
            .nth(1)?
            .parse()
            .ok()
    };

    let total = field("MemTotal:")?;
    // MemAvailable is the kernel's own estimate of what a new process could
    // claim; MemFree would count cache as used and read as a permanent 90%.
    let available = field("MemAvailable:")?;
    if total == 0 {
        return None;
    }
    Some(ratio(total.saturating_sub(available), total))
}

/// What a battery is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Battery {
    /// Charge level, 0 to 100.
    pub capacity: u8,
    /// Whether it is currently filling up.
    pub charging: bool,
}

/// Parses a `capacity` and a `status` file into a battery reading.
#[must_use]
pub fn parse_battery(capacity: &str, status: &str) -> Option<Battery> {
    Some(Battery {
        capacity: capacity.trim().parse::<u8>().ok()?.min(100),
        charging: matches!(status.trim(), "Charging" | "Full"),
    })
}

/// Reads the processor counters.
#[must_use]
pub fn read_cpu() -> Option<CpuSample> {
    parse_cpu(&fs::read_to_string(PROC_STAT).ok()?)
}

/// Reads the share of memory in use.
#[must_use]
pub fn read_memory() -> Option<f32> {
    parse_memory(&fs::read_to_string(PROC_MEMINFO).ok()?)
}

/// Reads the first battery the machine reports.
///
/// The name is not fixed — this laptop calls it `BAT1` — so it is looked up
/// rather than assumed.
#[must_use]
pub fn read_battery() -> Option<Battery> {
    let directory = battery_directory()?;
    parse_battery(
        &fs::read_to_string(directory.join("capacity")).ok()?,
        &fs::read_to_string(directory.join("status")).ok()?,
    )
}

/// Finds the power supply that is actually a battery.
fn battery_directory() -> Option<PathBuf> {
    let mut batteries: Vec<PathBuf> = fs::read_dir(POWER_SUPPLY)
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("BAT"))
        .map(|entry| entry.path())
        .collect();
    batteries.sort();
    batteries.into_iter().next()
}

/// Divides two counters into a `0.0..=1.0` share.
fn ratio(part: u64, whole: u64) -> f32 {
    if whole == 0 {
        return 0.0;
    }
    // Scaled to a thousandth and divided in float: enough for a 34-pixel bar,
    // and it keeps the tick counters away from f32's 24-bit mantissa.
    let permille = part.saturating_mul(1000) / whole;
    let permille = u16::try_from(permille).unwrap_or(1000).min(1000);
    f32::from(permille) / 1000.0
}

#[cfg(test)]
mod tests {
    use super::{parse_battery, parse_cpu, parse_memory, ratio};

    /// Real output, kept verbatim so a kernel format change would show up here.
    const STAT: &str = "cpu  535580 9822 144780 5599014 4777 38597 17644 0 0 0
cpu0 33141 636 9161 349533 322 3410 4544 0 0 0
intr 123
";

    const MEMINFO: &str = "MemTotal:       32139888 kB
MemFree:         1234567 kB
MemAvailable:   20883996 kB
Buffers:          123456 kB
";

    #[test]
    fn the_processor_line_is_read_not_the_per_core_ones() {
        let sample = parse_cpu(STAT).expect("parse");
        let expected_total: u64 = 535_580 + 9_822 + 144_780 + 5_599_014 + 4_777 + 38_597 + 17_644;
        assert_eq!(sample.busy_since(sample), None, "no time has passed");

        // A second sample one tick busier.
        let later = parse_cpu(&STAT.replace("535580", "535680")).expect("parse");
        let busy = later.busy_since(sample).expect("a share");
        assert!(busy > 0.99, "100 busy ticks and no idle ones gave {busy}");
        let _ = expected_total;
    }

    #[test]
    fn idle_time_counts_as_idle() {
        let before = parse_cpu(STAT).expect("parse");
        let after = parse_cpu(&STAT.replace("5599014", "5599114")).expect("parse");
        let busy = after.busy_since(before).expect("a share");
        assert!(busy < 0.01, "100 idle ticks gave {busy} busy");
    }

    #[test]
    fn counters_going_backwards_are_refused_rather_than_wrapped() {
        // Suspend and resume can do this; a wrapped subtraction would show a
        // full bar for one frame.
        let later = parse_cpu(STAT).expect("parse");
        let earlier = parse_cpu(&STAT.replace("5599014", "5699014")).expect("parse");
        assert_eq!(later.busy_since(earlier), None);
    }

    #[test]
    fn memory_in_use_ignores_cache() {
        let used = parse_memory(MEMINFO).expect("parse");
        // 32139888 total, 20883996 available -> about 35% in use.
        assert!((0.34..0.36).contains(&used), "got {used}");
    }

    #[test]
    fn a_truncated_file_is_refused_rather_than_guessed() {
        assert!(parse_cpu("").is_none());
        assert!(parse_cpu("cpu\n").is_none());
        assert!(parse_memory("MemTotal: 100 kB\n").is_none(), "no available");
        assert!(parse_memory("").is_none());
    }

    #[test]
    fn a_battery_reading_is_clamped_and_its_status_understood() {
        assert_eq!(
            parse_battery("80\n", "Discharging\n").expect("parse"),
            super::Battery {
                capacity: 80,
                charging: false
            }
        );
        assert!(parse_battery("100", "Charging").expect("parse").charging);
        assert!(parse_battery("100", "Full").expect("parse").charging);
        assert_eq!(parse_battery("255", "Full").expect("parse").capacity, 100);
        assert!(parse_battery("", "Full").is_none());
    }

    #[test]
    fn ratios_stay_inside_the_unit_range() {
        assert!((ratio(0, 100) - 0.0).abs() < 1e-6);
        assert!((ratio(100, 100) - 1.0).abs() < 1e-6);
        assert!((ratio(1, 2) - 0.5).abs() < 1e-3);
        assert!((ratio(5, 0) - 0.0).abs() < 1e-6, "no division by zero");
        assert!(ratio(u64::MAX, 1) <= 1.0, "clamped, not overflowed");
    }
}
