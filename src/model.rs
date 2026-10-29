use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rustix::time::{ClockId, clock_gettime};

#[derive(Clone, Debug, PartialEq)]
pub struct SampleTime {
    pub timestamp: i64,
    pub boot_id: String,
    pub boottime_ms: i64,
    pub monotonic_ms: i64,
}

impl SampleTime {
    pub fn now(boot_id: &str) -> Result<Self> {
        let millis = |clock| {
            let value = clock_gettime(clock);
            value.tv_sec * 1_000 + value.tv_nsec / 1_000_000
        };
        Ok(Self {
            timestamp: unix_now()?,
            boot_id: boot_id.to_owned(),
            boottime_ms: millis(ClockId::Boottime),
            monotonic_ms: millis(ClockId::Monotonic),
        })
    }

    pub fn follows(&self, previous: &Self, interval_secs: u64) -> bool {
        let elapsed = self.boottime_ms - previous.boottime_ms;
        let awake = self.monotonic_ms - previous.monotonic_ms;
        self.boot_id == previous.boot_id
            && self.timestamp >= previous.timestamp
            && elapsed > 0
            && awake > 0
            && elapsed <= (interval_secs.max(1) * 3 * 1_000) as i64
            && (elapsed - awake).abs() < 5_000
            // A wall-clock adjustment must not look like a long discharge interval.
            && ((self.timestamp - previous.timestamp) * 1_000 - elapsed).abs() < 5_000
    }
}

pub fn unix_now() -> Result<i64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("The system clock is before the Unix epoch")?
        .as_secs() as i64)
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct BatteryReading {
    pub battery: String,
    pub status: String,
    pub external_power: Option<bool>,
    pub capacity_percent: Option<f64>,
    pub energy_now_wh: Option<f64>,
    pub energy_full_wh: Option<f64>,
    pub energy_design_wh: Option<f64>,
    pub power_w: Option<f64>,
    pub voltage_v: Option<f64>,
    pub current_a: Option<f64>,
    pub charge_now_ah: Option<f64>,
    pub charge_full_ah: Option<f64>,
    pub charge_design_ah: Option<f64>,
    pub energy_estimated: bool,
}

#[derive(Clone, Debug)]
pub struct Sample {
    pub time: SampleTime,
    pub interval_secs: u64,
    pub reading: BatteryReading,
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub frame_id: i64,
    pub time: SampleTime,
    pub interval_secs: u64,
    pub batteries: Vec<BatteryReading>,
}

impl Snapshot {
    pub fn is_stale(&self, now: i64) -> bool {
        now < self.time.timestamp
            || now - self.time.timestamp > (self.interval_secs * 3).max(90) as i64
    }
}

#[cfg(test)]
pub fn test_time(seconds: i64) -> SampleTime {
    SampleTime {
        timestamp: 1_700_000_000 + seconds,
        boot_id: "test-boot".into(),
        boottime_ms: seconds * 1_000,
        monotonic_ms: seconds * 1_000,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continuity_rejects_suspend_restart_and_clock_changes() {
        let first = test_time(0);
        let next = test_time(30);
        assert!(next.follows(&first, 30));
        let mut changed = next.clone();
        changed.boot_id = "another-boot".into();
        assert!(!changed.follows(&first, 30));
        changed = next.clone();
        changed.monotonic_ms = 10_000;
        assert!(!changed.follows(&first, 30));
        changed = next.clone();
        changed.timestamp += 3_600;
        assert!(!changed.follows(&first, 30));
        assert!(!test_time(300).follows(&first, 30));
    }
}
