use crate::model::Sample;

#[derive(Debug)]
pub struct Estimate {
    pub seconds: u64,
    pub method: &'static str,
}

// Predictors consume measured history, never previous predictions. A learned
// predictor can replace this baseline without changing collection or storage.
pub fn time_to_empty(history: &[Sample]) -> Option<Estimate> {
    time_to_target(history, false)
}

pub fn time_to_full(history: &[Sample]) -> Option<Estimate> {
    time_to_target(history, true)
}

fn time_to_target(history: &[Sample], charging: bool) -> Option<Estimate> {
    let latest = history.last()?;
    let compatible = |sample: &Sample| {
        sample.reading.status == if charging { "Charging" } else { "Discharging" }
            && sample.reading.external_power != Some(!charging)
    };
    if !compatible(latest) {
        return None;
    }
    let energy = latest
        .reading
        .energy_now_wh
        .filter(|value| value.is_finite() && *value >= 0.0)?;
    let remaining = if charging {
        latest.reading.energy_full_wh? - energy
    } else {
        energy
    };
    if !remaining.is_finite() || remaining <= 0.0 {
        return None;
    }
    let mut start = history.len() - 1;
    while start > 0 {
        let current = &history[start];
        let previous = &history[start - 1];
        if !compatible(previous)
            || current.reading.battery != previous.reading.battery
            || !current.time.follows(
                &previous.time,
                current.interval_secs.max(previous.interval_secs),
            )
            || latest.time.boottime_ms - previous.time.boottime_ms > 10 * 60 * 1_000
        {
            break;
        }
        start -= 1;
    }
    let window = &history[start..];
    if window.len() < 3 || latest.time.boottime_ms - window[0].time.boottime_ms < 60_000 {
        return None;
    }
    let valid_power = |power: f64| power.is_finite() && (0.1..2_000.0).contains(&power);
    let measured = window
        .iter()
        .all(|sample| sample.reading.power_w.is_some_and(valid_power));
    let power = if measured {
        let mut smoothed = window[0].reading.power_w?;
        for pair in window.windows(2) {
            let elapsed = (pair[1].time.boottime_ms - pair[0].time.boottime_ms) as f64 / 1_000.0;
            let alpha = 1.0 - (-elapsed / 120.0).exp();
            smoothed += alpha * (pair[1].reading.power_w? - smoothed);
        }
        smoothed
    } else {
        // Average across the window, not individual steps: energy readings are
        // often quantized and can stay unchanged for several samples.
        let first = &window[0];
        if window
            .iter()
            .any(|sample| sample.reading.energy_estimated != latest.reading.energy_estimated)
        {
            return None;
        }
        let delta = if charging {
            energy - first.reading.energy_now_wh?
        } else {
            first.reading.energy_now_wh? - energy
        };
        let elapsed = (latest.time.boottime_ms - first.time.boottime_ms) as f64 / 1_000.0;
        delta * 3_600.0 / elapsed
    };
    if !valid_power(power) {
        return None;
    }
    // Charging is a rate-based baseline, not a model of taper near full.
    let seconds = remaining / power * 3_600.0;
    if !seconds.is_finite() || !(1.0..=7.0 * 86_400.0).contains(&seconds) {
        return None;
    }
    Some(Estimate {
        seconds: seconds.round() as u64,
        method: if measured {
            "smoothed power"
        } else {
            "energy trend"
        },
    })
}

pub fn format_duration(seconds: u64) -> String {
    let minutes = seconds.div_ceil(60);
    if minutes < 60 {
        format!("{minutes}m")
    } else {
        format!("{}h {:02}m", minutes / 60, minutes % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{BatteryReading, test_time};

    fn history() -> Vec<Sample> {
        [0, 30, 60, 90]
            .into_iter()
            .map(|seconds| Sample {
                time: test_time(seconds),
                interval_secs: 30,
                reading: BatteryReading {
                    battery: "BAT0".into(),
                    status: "Discharging".into(),
                    energy_now_wh: Some(20.0),
                    power_w: Some(10.0),
                    ..Default::default()
                },
            })
            .collect()
    }

    #[test]
    fn constant_load_predicts_energy_divided_by_power() {
        assert_eq!(time_to_empty(&history()).unwrap().seconds, 7_200);
    }

    #[test]
    fn requires_enough_contiguous_discharge_history() {
        let mut data = history();
        assert!(time_to_empty(&data[..2]).is_none());
        data[2].time.boot_id = "another-boot".into();
        assert!(time_to_empty(&data).is_none());
        data = history();
        data[3].reading.status = "Charging".into();
        assert!(time_to_empty(&data).is_none());
        data = history();
        data[3].time.monotonic_ms -= 10_000;
        assert!(time_to_empty(&data).is_none());
    }

    #[test]
    fn missing_power_can_use_an_energy_trend() {
        let mut data = history();
        for (index, sample) in data.iter_mut().enumerate() {
            sample.reading.power_w = None;
            sample.reading.energy_now_wh = Some(20.0 - index as f64 / 12.0);
        }
        let estimate = time_to_empty(&data).unwrap();
        assert_eq!(estimate.seconds, 7_110);
        assert_eq!(estimate.method, "energy trend");
    }

    #[test]
    fn quantized_energy_does_not_require_a_change_at_every_sample() {
        let mut data = history();
        for sample in &mut data {
            sample.reading.power_w = None;
        }
        data[3].reading.energy_now_wh = Some(19.75);
        assert_eq!(time_to_empty(&data).unwrap().seconds, 7_110);
    }

    fn charging_history() -> Vec<Sample> {
        history()
            .into_iter()
            .map(|mut sample| {
                sample.reading.status = "Charging".into();
                sample.reading.external_power = Some(true);
                sample.reading.energy_full_wh = Some(50.0);
                sample
            })
            .collect()
    }

    #[test]
    fn charging_predicts_unfilled_capacity_not_remaining_runtime() {
        let data = charging_history();
        assert_eq!(time_to_full(&data).unwrap().seconds, 10_800);
        assert!(time_to_empty(&data).is_none());
        assert!(time_to_full(&history()).is_none());
        assert!(time_to_full(&data[..2]).is_none());
    }

    #[test]
    fn recorded_history_can_continue_into_faster_live_samples() {
        for charging in [false, true] {
            let mut data = if charging {
                charging_history()
            } else {
                history()
            };
            data[3].time = test_time(75);
            data[3].interval_secs = 1;
            assert!(time_to_target(&data, charging).is_some());
            data[3].time = test_time(151);
            assert!(time_to_target(&data, charging).is_none());
        }
    }

    #[test]
    fn charging_can_use_increasing_energy_but_not_a_flat_or_falling_trend() {
        let mut data = charging_history();
        for sample in &mut data {
            sample.reading.power_w = None;
        }
        assert!(time_to_full(&data).is_none());
        data[3].reading.energy_now_wh = Some(20.25);
        let estimate = time_to_full(&data).unwrap();
        assert_eq!(estimate.seconds, 10_710);
        assert_eq!(estimate.method, "energy trend");
        data[3].reading.energy_now_wh = Some(19.75);
        assert!(time_to_full(&data).is_none());
    }

    #[test]
    fn charging_rejects_bad_capacity_power_states_and_discontinuous_history() {
        for capacity in [
            None,
            Some(0.0),
            Some(19.0),
            Some(f64::NAN),
            Some(f64::INFINITY),
        ] {
            let mut data = charging_history();
            data[3].reading.energy_full_wh = capacity;
            assert!(time_to_full(&data).is_none());
        }
        for status in ["Full", "Not charging", "Unknown"] {
            let mut data = charging_history();
            data[3].reading.status = status.into();
            assert!(time_to_full(&data).is_none());
        }
        let mut data = charging_history();
        data[3].reading.external_power = Some(false);
        assert!(time_to_full(&data).is_none());
        for interruption in 0..4 {
            let mut data = charging_history();
            match interruption {
                0 => data[2].reading.status = "Discharging".into(),
                1 => data[2].time.boot_id = "another-boot".into(),
                2 => data[3].time.monotonic_ms -= 10_000,
                _ => data[2].reading.battery = "BAT1".into(),
            }
            assert!(time_to_full(&data).is_none());
        }
    }

    #[test]
    fn zero_or_missing_rate_is_not_a_valid_prediction() {
        let mut data = history();
        for sample in &mut data {
            sample.reading.power_w = Some(0.0);
        }
        assert!(time_to_empty(&data).is_none());
        for sample in &mut data {
            sample.reading.power_w = None;
        }
        assert!(time_to_empty(&data).is_none());
    }
}
