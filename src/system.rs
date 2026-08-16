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
/// Where the kernel reports network interfaces.
const PROC_NET_DEV: &str = "/proc/net/dev";
/// Where the kernel reports block devices.
const PROC_DISKSTATS: &str = "/proc/diskstats";

/// Bytes counted in each direction since boot.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    /// Bytes sent, or written.
    pub out: u64,
    /// Bytes received, or read.
    pub incoming: u64,
}

impl Counters {
    /// Bytes a second in each direction between two readings.
    ///
    /// A counter that went backwards — a suspend, an interface coming back —
    /// reads as nothing rather than as an enormous burst.
    #[must_use]
    pub fn rates_since(self, earlier: Self, seconds: f64) -> (f64, f64) {
        if seconds <= 0.0 {
            return (0.0, 0.0);
        }
        let rate = |now: u64, before: u64| {
            // A byte count that needs more than 52 bits of precision is an
            // exabyte in one sample; the rounding is irrelevant either way.
            #[allow(
                clippy::cast_precision_loss,
                reason = "byte counters never reach the mantissa's limit in practice"
            )]
            now.checked_sub(before)
                .map_or(0.0, |delta| delta as f64 / seconds)
        };
        (
            rate(self.out, earlier.out),
            rate(self.incoming, earlier.incoming),
        )
    }
}

/// Sums the byte counters of every real network interface.
///
/// The loopback is skipped: it carries every local connection and would drown
/// out the link you actually care about.
#[must_use]
pub fn parse_network(dev: &str) -> Counters {
    let mut totals = Counters::default();
    for line in dev.lines().skip(2) {
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        if name.trim() == "lo" {
            continue;
        }
        let fields: Vec<u64> = rest
            .split_whitespace()
            .map(|field| field.parse().unwrap_or(0))
            .collect();
        // receive bytes first, then eight more receive fields before transmit.
        totals.incoming += fields.first().copied().unwrap_or(0);
        totals.out += fields.get(8).copied().unwrap_or(0);
    }
    totals
}

/// Sums the sector counters of every whole disk.
#[must_use]
pub fn parse_disk(diskstats: &str) -> Counters {
    /// The kernel reports these in 512-byte sectors whatever the device says.
    const SECTOR: u64 = 512;

    let mut totals = Counters::default();
    for line in diskstats.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let (Some(name), Some(read), Some(written)) = (fields.get(2), fields.get(5), fields.get(9))
        else {
            continue;
        };
        if !is_whole_disk(name) {
            continue;
        }
        totals.incoming += read.parse::<u64>().unwrap_or(0) * SECTOR;
        totals.out += written.parse::<u64>().unwrap_or(0) * SECTOR;
    }
    totals
}

/// Whether a block device is a disk rather than one of its partitions.
///
/// Counting both would double every byte, since a partition's traffic is also
/// counted against its disk.
#[must_use]
pub fn is_whole_disk(name: &str) -> bool {
    if name.starts_with("loop") || name.starts_with("ram") || name.starts_with("zram") {
        return false;
    }
    if let Some(rest) = name.strip_prefix("nvme") {
        // nvme0n1 is a disk, nvme0n1p2 is a partition.
        return !rest.contains('p');
    }
    if name.starts_with("sd") || name.starts_with("hd") || name.starts_with("vd") {
        return !name.ends_with(|c: char| c.is_ascii_digit());
    }
    if name.starts_with("mmcblk") {
        return !name.contains('p');
    }
    true
}

/// Reads the network counters.
#[must_use]
pub fn read_network() -> Option<Counters> {
    Some(parse_network(&fs::read_to_string(PROC_NET_DEV).ok()?))
}

/// Reads the disk counters.
#[must_use]
pub fn read_disk() -> Option<Counters> {
    Some(parse_disk(&fs::read_to_string(PROC_DISKSTATS).ok()?))
}

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
    use super::{
        is_whole_disk, parse_battery, parse_cpu, parse_disk, parse_memory, parse_network, ratio,
    };

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

    /// Real output, trimmed but otherwise verbatim.
    const NET_DEV: &str = "Inter-|   Receive                       |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo:  202624    2185    0    0    0     0          0         0   202624    2185    0    0    0     0       0          0
  wlan0: 5000000   1000    0    0    0     0          0         0  2000000     900    0    0    0     0       0          0
";

    const DISKSTATS: &str = "   7       0 loop0 10 0 20 0 0 0 0 0 0 0 0 0 0 0 0 0 0
 259       0 nvme0n1 235654 17426 16758963 107217 773498 12679 38410756 1095631 0 0 0 0 0 0 0 0 0
 259       1 nvme0n1p1 4119 6609 606755 1479 281 13 10052 458 0 0 0 0 0 0 0 0 0
";

    #[test]
    fn the_loopback_is_left_out_of_the_network_total() {
        // Every local connection goes over lo; counting it would drown out the
        // link you actually care about.
        let counters = parse_network(NET_DEV);
        assert_eq!(counters.incoming, 5_000_000, "lo was counted");
        assert_eq!(counters.out, 2_000_000);
    }

    #[test]
    fn a_partition_is_not_counted_alongside_its_disk() {
        // The kernel counts a partition's traffic against its disk as well, so
        // adding both would double every byte.
        let counters = parse_disk(DISKSTATS);
        assert_eq!(
            counters.incoming,
            16_758_963 * 512,
            "a partition slipped in"
        );
        assert_eq!(counters.out, 38_410_756 * 512);
    }

    #[test]
    fn whole_disks_are_told_apart_from_their_partitions() {
        for whole in ["nvme0n1", "sda", "vdb", "mmcblk0", "dm-0"] {
            assert!(is_whole_disk(whole), "{whole} was taken for a partition");
        }
        for part in ["nvme0n1p1", "sda1", "mmcblk0p2", "loop0", "ram3", "zram0"] {
            assert!(!is_whole_disk(part), "{part} was taken for a disk");
        }
    }

    #[test]
    fn a_counter_going_backwards_reads_as_nothing() {
        // Suspending, or an interface coming back, resets these. Wrapping the
        // subtraction would paint a full-scale burst.
        let now = super::Counters {
            out: 10,
            incoming: 10,
        };
        let later = super::Counters {
            out: 1_000,
            incoming: 1_000,
        };
        assert_eq!(now.rates_since(later, 1.0), (0.0, 0.0));
        assert_eq!(later.rates_since(now, 1.0), (990.0, 990.0));
        assert_eq!(
            later.rates_since(now, 0.0),
            (0.0, 0.0),
            "no division by zero"
        );
    }

    #[test]
    fn rates_are_per_second_not_per_sample() {
        let before = super::Counters {
            out: 0,
            incoming: 0,
        };
        let after = super::Counters {
            out: 100,
            incoming: 200,
        };
        assert_eq!(after.rates_since(before, 2.0), (50.0, 100.0));
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
