use crate::model::Sample;

pub const HOUR: i64 = 3_600;
pub const HOURS: usize = 24;

#[derive(Clone, Debug, Default)]
pub struct HourUsage {
    pub start: i64,
    pub energy_wh: f64,
    pub covered_seconds: i64,
}

impl HourUsage {
    pub fn energy(&self) -> Option<f64> {
        (self.covered_seconds > 0).then_some(self.energy_wh)
    }
}

// Only the buckets and one preceding measurement are retained, even when a
// recorder writes every second. Raw measurements remain in SQLite.
pub struct UsageHistory {
    pub hours: [HourUsage; HOURS],
    previous: Option<Sample>,
    end: i64,
    covered_until: i64,
}

impl UsageHistory {
    pub fn new(end: i64) -> Self {
        let start = end.div_euclid(HOUR) * HOUR - (HOURS as i64 - 1) * HOUR;
        Self {
            hours: std::array::from_fn(|index| HourUsage {
                start: start + index as i64 * HOUR,
                ..Default::default()
            }),
            previous: None,
            end,
            covered_until: start,
        }
    }

    pub fn start(&self) -> i64 {
        self.hours[0].start
    }

    pub fn total(&self) -> Option<f64> {
        self.hours
            .iter()
            .any(|hour| hour.energy().is_some())
            .then(|| self.hours.iter().map(|hour| hour.energy_wh).sum())
    }

    pub fn push(&mut self, sample: Sample) {
        let previous = self.previous.replace(sample);
        let (Some(previous), Some(current)) = (previous, self.previous.as_ref()) else {
            return;
        };
        if previous.reading.battery != current.reading.battery
            || !current.time.follows(
                &previous.time,
                current.interval_secs.max(previous.interval_secs),
            )
            || current.time.timestamp <= previous.time.timestamp
            || previous.reading.status != current.reading.status
        {
            return;
        }
        let power = match current.reading.status.as_str() {
            "Discharging" => previous.reading.power_w.zip(current.reading.power_w),
            // Input power is not battery use and must never cancel out discharge.
            "Charging" | "Full" | "Not charging" => Some((0.0, 0.0)),
            _ => None,
        };
        let Some((first, last)) = power.filter(|(first, last)| {
            first.is_finite() && last.is_finite() && *first >= 0.0 && *last >= 0.0
        }) else {
            return;
        };
        let wall_seconds = (current.time.timestamp - previous.time.timestamp) as f64;
        let elapsed = (current.time.boottime_ms - previous.time.boottime_ms) as f64 / 1_000.0;
        let mut cursor = previous.time.timestamp.max(self.covered_until);
        let end = current.time.timestamp.min(self.end);
        while cursor < end {
            let index = ((cursor - self.hours[0].start) / HOUR) as usize;
            let hour = &mut self.hours[index];
            let stop = end.min(hour.start + HOUR);
            let from = (cursor - previous.time.timestamp) as f64 / wall_seconds;
            let to = (stop - previous.time.timestamp) as f64 / wall_seconds;
            let at_start = first + (last - first) * from;
            let at_end = first + (last - first) * to;
            let energy = (at_start / 2.0 + at_end / 2.0) / HOUR as f64 * elapsed * (to - from);
            if energy.is_finite() && (hour.energy_wh + energy).is_finite() {
                hour.energy_wh += energy;
                hour.covered_seconds += stop - cursor;
            }
            cursor = stop;
        }
        // After a wall-clock rollback, do not count the same wall-time bucket twice.
        self.covered_until = self.covered_until.max(end);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{BatteryReading, test_time};

    const END: i64 = 1_700_006_400; // UTC hour boundary

    fn sample(seconds: i64, power: Option<f64>, status: &str) -> Sample {
        let mut time = test_time(seconds);
        time.timestamp = END + seconds;
        Sample {
            time,
            interval_secs: 30,
            reading: BatteryReading {
                battery: "BAT0".into(),
                status: status.into(),
                power_w: power,
                ..Default::default()
            },
        }
    }

    #[test]
    fn integrates_trapezoids_across_hour_boundaries_and_clips_the_window() {
        let mut usage = UsageHistory::new(END + 30);
        for second in [-30, 0, 30] {
            usage.push(sample(
                second,
                Some(12.0 + second as f64 / 10.0),
                "Discharging",
            ));
        }
        assert!((usage.total().unwrap() - 0.2).abs() < 1e-9);
        assert!((usage.hours[22].energy_wh - 0.0875).abs() < 1e-9);
        assert!((usage.hours[23].energy_wh - 0.1125).abs() < 1e-9);
        assert_eq!(usage.hours[23].covered_seconds, 30);
        assert!(usage.hours[0].energy().is_none());

        let mut clipped = UsageHistory::new(END + 30);
        let start = clipped.start() - END;
        clipped.push(sample(start - 15, Some(12.0), "Discharging"));
        clipped.push(sample(start + 15, Some(12.0), "Discharging"));
        assert!((clipped.total().unwrap() - 0.05).abs() < 1e-9);
        assert_eq!(clipped.hours[0].covered_seconds, 15);
    }

    #[test]
    fn charging_does_not_cancel_discharge_and_zero_is_not_missing() {
        let mut usage = UsageHistory::new(END + 120);
        for (second, power, status) in [
            (0, Some(12.0), "Discharging"),
            (30, Some(12.0), "Discharging"),
            (60, Some(100.0), "Charging"),
            (90, Some(100.0), "Charging"),
            (120, Some(100.0), "Charging"),
        ] {
            usage.push(sample(second, power, status));
        }
        assert!((usage.total().unwrap() - 0.1).abs() < 1e-9);
        assert_eq!(usage.hours[23].covered_seconds, 90);

        let mut zero = UsageHistory::new(END + 30);
        zero.push(sample(0, Some(0.0), "Discharging"));
        zero.push(sample(30, Some(0.0), "Discharging"));
        assert_eq!(zero.total(), Some(0.0));
        assert_eq!(zero.hours[23].energy(), Some(0.0));
        assert_eq!(zero.hours[22].energy(), None);
    }

    #[test]
    fn missing_rates_heartbeats_and_unknown_status_break_coverage() {
        let mut usage = UsageHistory::new(END + 180);
        for (second, power, status) in [
            (0, Some(12.0), "Discharging"),
            (30, None, "Discharging"),
            (60, Some(12.0), "Discharging"),
            (90, None, "Unknown"),
            (120, Some(12.0), "Discharging"),
            (150, Some(12.0), "Discharging"),
            (180, Some(f64::NAN), "Discharging"),
        ] {
            usage.push(sample(second, power, status));
        }
        assert_eq!(usage.hours[23].covered_seconds, 30);
        assert!((usage.total().unwrap() - 0.1).abs() < 1e-9);
    }

    #[test]
    fn rejects_suspend_reboots_and_long_recording_gaps() {
        for change in 0..3 {
            let mut usage = UsageHistory::new(END + 600);
            usage.push(sample(0, Some(12.0), "Discharging"));
            let mut next = sample(30, Some(12.0), "Discharging");
            match change {
                0 => next.time.monotonic_ms -= 10_000,
                1 => next.time.boot_id = "another-boot".into(),
                _ => next = sample(600, Some(12.0), "Discharging"),
            }
            usage.push(next);
            assert_eq!(usage.total(), None);
        }
    }

    #[test]
    fn backward_clocks_do_not_double_count_and_future_samples_are_clipped() {
        let mut usage = UsageHistory::new(END + 30);
        for second in [0, 30, 0, 30, 60] {
            usage.push(sample(second, Some(12.0), "Discharging"));
        }
        assert_eq!(usage.hours[23].covered_seconds, 30);
        assert!((usage.total().unwrap() - 0.1).abs() < 1e-9);
    }
}
