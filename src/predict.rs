use crate::model::Sample;

#[derive(Debug)]
pub struct Estimate {
    pub seconds: u64,
    pub lower_seconds: u64,
    pub upper_seconds: Option<u64>,
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
    let mut rates = if measured {
        minute_rates(window)
    } else {
        Vec::new()
    };
    rates.sort_by(f64::total_cmp);
    // This is a recent-use scenario band, not a calibrated probability interval.
    // A modest rate margin prevents a flat sensor trace implying perfect certainty.
    let slow = quantile(&rates, 0.1).unwrap_or(power).min(power * 0.8);
    let fast = quantile(&rates, 0.9).unwrap_or(power).max(power * 1.2);
    let lower = remaining / fast * 3_600.0;
    let upper = remaining / slow * 3_600.0;
    Some(Estimate {
        seconds: seconds.round() as u64,
        lower_seconds: lower.floor() as u64,
        upper_seconds: (upper.is_finite() && upper <= 7.0 * 86_400.0).then(|| upper.ceil() as u64),
        method: if measured {
            "smoothed power"
        } else {
            "energy trend"
        },
    })
}

fn minute_rates(window: &[Sample]) -> Vec<f64> {
    let mut rates = Vec::new();
    let origin = window[0].time.boottime_ms;
    let mut boundary = 60.0;
    let mut integral = 0.0;
    let mut covered = 0.0;
    for pair in window.windows(2) {
        let (Some(first), Some(last)) = (pair[0].reading.power_w, pair[1].reading.power_w) else {
            return Vec::new();
        };
        let start = (pair[0].time.boottime_ms - origin) as f64 / 1_000.0;
        let end = (pair[1].time.boottime_ms - origin) as f64 / 1_000.0;
        let mut cursor = start;
        while cursor < end {
            let stop = end.min(boundary);
            let from = first + (last - first) * (cursor - start) / (end - start);
            let to = first + (last - first) * (stop - start) / (end - start);
            integral += (from + to) / 2.0 * (stop - cursor);
            covered += stop - cursor;
            cursor = stop;
            if stop == boundary {
                rates.push(integral / covered);
                integral = 0.0;
                covered = 0.0;
                boundary += 60.0;
            }
        }
    }
    rates
}

fn quantile(sorted: &[f64], fraction: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let position = fraction * (sorted.len() - 1) as f64;
    let low = position.floor() as usize;
    let high = position.ceil() as usize;
    Some(sorted[low] + (sorted[high] - sorted[low]) * (position - low as f64))
}

pub struct DisplayRange {
    pub text: String,
    pub lower_seconds: u64,
    pub upper_seconds: Option<u64>,
}

pub fn format_range(estimate: &Estimate, available: usize) -> DisplayRange {
    // Outward five-minute rounding avoids presenting second-by-second precision.
    let lower = estimate.lower_seconds / 300 * 300;
    let upper = estimate.upper_seconds.map(|s| s.div_ceil(300) * 300);
    let long = match upper {
        Some(upper) => format!("{}–{}", format_duration(lower), format_duration(upper)),
        None => format!("≥{}", format_duration(lower)),
    };
    let compact = long.replace(' ', "");
    let (text, lower, upper) = if long.chars().count() <= available {
        (long, lower, upper)
    } else if compact.chars().count() <= available {
        (compact, lower, upper)
    } else {
        let lower = lower / 3_600 * 3_600;
        let upper = upper.map(|s| s.div_ceil(3_600) * 3_600);
        let text = match upper {
            Some(upper) => format!("{}–{}h", lower / 3_600, upper / 3_600),
            None => format!("≥{}h", lower / 3_600),
        };
        (text, lower, upper)
    };
    DisplayRange {
        text,
        lower_seconds: lower,
        upper_seconds: upper,
    }
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
    fn constant_load_predicts_energy_divided_by_power_with_a_nonzero_scenario_band() {
        let estimate = time_to_empty(&history()).unwrap();
        assert_eq!(estimate.seconds, 7_200);
        assert_eq!(estimate.lower_seconds, 6_000);
        assert_eq!(estimate.upper_seconds, Some(9_000));
    }

    #[test]
    fn minute_rates_are_time_weighted_and_independent_of_poll_density() {
        let template = history()[0].clone();
        let data: Vec<_> = [0, 3, 17, 45, 60, 61, 90, 120]
            .into_iter()
            .map(|second| {
                let mut sample = template.clone();
                sample.time = test_time(second);
                sample.reading.power_w = Some(5.0 + second as f64 / 6.0);
                sample
            })
            .collect();
        let rates = minute_rates(&data);
        assert_eq!(rates.len(), 2);
        assert!((rates[0] - 10.0).abs() < 1e-9);
        assert!((rates[1] - 20.0).abs() < 1e-9);
    }

    #[test]
    fn changing_minute_loads_widen_the_band_beyond_the_minimum_margin() {
        let template = history()[0].clone();
        let data: Vec<_> = (0..=600)
            .map(|second| {
                let mut sample = template.clone();
                sample.time = test_time(second);
                sample.interval_secs = 1;
                sample.reading.power_w = Some(if second < 300 { 5.0 } else { 25.0 });
                sample
            })
            .collect();
        let estimate = time_to_empty(&data).unwrap();
        assert!(estimate.lower_seconds < estimate.seconds);
        assert!(estimate.upper_seconds.unwrap() > estimate.seconds * 2);
        assert!(estimate.upper_seconds.unwrap() >= 14_400);
    }

    #[test]
    fn energy_fallback_has_a_band_and_unbounded_slow_scenarios_are_not_capped_as_promises() {
        let mut data = history();
        for (index, sample) in data.iter_mut().enumerate() {
            sample.reading.power_w = None;
            sample.reading.energy_now_wh = Some(20.0 - index as f64 / 12.0);
        }
        let estimate = time_to_empty(&data).unwrap();
        assert!(estimate.lower_seconds < estimate.seconds);
        assert!(estimate.upper_seconds.unwrap() > estimate.seconds);
        for sample in &mut data {
            sample.reading.power_w = Some(0.15);
            sample.reading.energy_now_wh = Some(24.0);
        }
        let estimate = time_to_empty(&data).unwrap();
        assert!(estimate.upper_seconds.is_none());
        assert!(format_range(&estimate, 15).text.starts_with('≥'));
    }

    #[test]
    fn range_format_rounds_outwards_and_fits_summary_cards() {
        let mut estimate = Estimate {
            seconds: 4_500,
            lower_seconds: 3_750,
            upper_seconds: Some(5_625),
            method: "smoothed power",
        };
        assert_eq!(format_range(&estimate, 30).text, "1h 00m–1h 35m");
        estimate.lower_seconds = 43_201;
        estimate.upper_seconds = Some(54_001);
        assert_eq!(format_range(&estimate, 14).text, "12h00m–15h05m");
        estimate.lower_seconds = 432_000;
        estimate.upper_seconds = Some(540_001);
        assert_eq!(format_range(&estimate, 15).text, "120h00m–150h05m");
        let compact = format_range(&estimate, 14);
        assert_eq!(compact.text, "120–151h");
        assert_eq!(compact.lower_seconds, 120 * 3_600);
        assert_eq!(compact.upper_seconds, Some(151 * 3_600));
        estimate.lower_seconds = 1_200;
        estimate.upper_seconds = Some(2_400);
        assert_eq!(format_range(&estimate, 15).text, "20m–40m");
        estimate.lower_seconds = 10;
        estimate.upper_seconds = Some(30);
        assert_eq!(format_range(&estimate, 15).text, "0m–5m");
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
        let estimate = time_to_full(&data).unwrap();
        assert_eq!(estimate.seconds, 10_800);
        assert_eq!(estimate.lower_seconds, 9_000);
        assert_eq!(estimate.upper_seconds, Some(13_500));
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
