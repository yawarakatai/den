use std::mem::MaybeUninit;

use crate::predict::{Estimate, format_range};

pub fn format(estimate: &Estimate, base: i64, width: usize) -> (String, String) {
    format_with(estimate, base, width, calendar)
}

fn format_with(
    estimate: &Estimate,
    base: i64,
    width: usize,
    convert: impl Fn(i64, bool) -> Option<ClockTime>,
) -> (String, String) {
    let range = format_range(estimate, width);
    let spaced = range.text.replace('–', " – ").replace('≥', "≥ ");
    let compact = range.text.replace(' ', "").replace('–', " – ");
    let duration =
        labeled("In", &[spaced, compact, range.text.replace(' ', "")], width).unwrap_or(range.text);
    let arrival = clock_range(
        base,
        range.lower_seconds,
        range.upper_seconds,
        width,
        convert,
    )
    .unwrap_or_else(|| "At  N/A".into());
    (duration, arrival)
}

fn labeled(label: &str, choices: &[String], width: usize) -> Option<String> {
    for text in choices {
        let labeled = format!("{label}  {text}");
        if labeled.chars().count() <= width {
            return Some(labeled);
        }
    }
    choices.iter().find(|s| s.chars().count() <= width).cloned()
}

#[derive(Clone, Copy)]
struct ClockTime {
    hour: i32,
    minute: i32,
    offset: libc::c_long,
}

fn calendar(timestamp: i64, local: bool) -> Option<ClockTime> {
    let timestamp: libc::time_t = timestamp;
    let mut result = MaybeUninit::<libc::tm>::uninit();
    // SAFETY: both pointers are valid; the output is read only after successful
    // conversion. The reentrant libc functions do not share an output buffer.
    let result = unsafe {
        let converted = if local {
            libc::localtime_r(&timestamp, result.as_mut_ptr())
        } else {
            libc::gmtime_r(&timestamp, result.as_mut_ptr())
        };
        if converted.is_null() {
            return None;
        }
        result.assume_init()
    };
    Some(ClockTime {
        hour: result.tm_hour,
        minute: result.tm_min,
        offset: result.tm_gmtoff,
    })
}

fn clock_range(
    base: i64,
    lower: u64,
    upper: Option<u64>,
    width: usize,
    convert: impl Fn(i64, bool) -> Option<ClockTime>,
) -> Option<String> {
    let end = |seconds| base.checked_add(i64::try_from(seconds).ok()?);
    let first = end(lower)?;
    let last = match upper {
        Some(seconds) => Some(end(seconds)?),
        None => None,
    };
    let anchor = convert(base, true)?;
    let mut low = convert(first, true)?;
    let mut high = match last {
        Some(t) => Some(convert(t, true)?),
        None => None,
    };
    // A local clock can run backwards across a DST transition. Use an explicit
    // UTC range instead of presenting an apparently reversed interval.
    let utc = anchor.offset != low.offset || high.is_some_and(|high| high.offset != low.offset);
    if utc {
        low = convert(first, false)?;
        high = match last {
            Some(t) => Some(convert(t, false)?),
            None => None,
        };
    }
    let clock = |time: ClockTime| format!("{:02}:{:02}", time.hour, time.minute);
    // In already conveys the elapsed days; keep At focused on clock times.
    let wide = if let Some(high) = high {
        format!("{} – {}", clock(low), clock(high))
    } else {
        format!("≥ {}", clock(low))
    };
    let zone = if utc { "UTC " } else { "" };
    let choices = [
        format!("{zone}{wide}"),
        format!("{zone}{}", wide.replace(' ', "")),
    ];
    labeled("At", &choices, width)
}

#[cfg(test)]
mod tests {
    use super::*;

    const JAN1: i64 = 1_704_067_200;

    fn estimate(lower: u64, upper: Option<u64>) -> Estimate {
        Estimate {
            seconds: lower,
            lower_seconds: lower,
            upper_seconds: upper,
            method: "smoothed power",
        }
    }

    fn utc(timestamp: i64, _: bool) -> Option<ClockTime> {
        calendar(timestamp, false)
    }

    #[test]
    fn in_and_at_share_the_displayed_bounds_and_one_reference_time() {
        let (duration, at) = format_with(
            &estimate(900, Some(2_400)),
            JAN1 + 21 * 3_600 + 8 * 60,
            24,
            utc,
        );
        assert_eq!(duration, "In  15m – 40m");
        assert_eq!(at, "At  21:23 – 21:48");
        let (duration, at) = format_with(&estimate(901, Some(2_401)), JAN1, 24, utc);
        assert_eq!(duration, "In  15m – 45m");
        assert_eq!(at, "At  00:15 – 00:45");
    }

    #[test]
    fn clock_times_roll_over_midnight_and_years_without_day_suffixes() {
        assert_eq!(
            format_with(&estimate(900, Some(2_400)), JAN1 - 1_200, 32, utc).1,
            "At  23:55 – 00:20"
        );
        assert_eq!(
            format_with(&estimate(900, Some(2_400)), JAN1 - 600, 32, utc).1,
            "At  00:05 – 00:30"
        );
        assert_eq!(
            format_with(
                &estimate(86_400 + 900, Some(2 * 86_400 + 2_400)),
                JAN1,
                32,
                utc
            )
            .1,
            "At  00:15 – 00:40"
        );
    }

    #[test]
    fn local_time_and_leap_day_conversion_do_not_add_date_markers() {
        let convert = |timestamp: i64, local: bool| {
            let offset = if local { 9 * 3_600 } else { 0 };
            let mut clock = calendar(timestamp + offset, false)?;
            clock.offset = offset;
            Some(clock)
        };
        assert_eq!(
            format_with(
                &estimate(900, Some(2_400)),
                JAN1 + 14 * 3_600 + 50 * 60,
                32,
                convert
            )
            .1,
            "At  00:05 – 00:30"
        );
        let feb28 = JAN1 + 58 * 86_400;
        assert_eq!(
            format_with(
                &estimate(2 * 86_400 + 900, Some(3 * 86_400 + 2_400)),
                feb28,
                32,
                utc
            )
            .1,
            "At  00:15 – 00:40"
        );
    }

    #[test]
    fn compact_cards_keep_complete_values_without_day_offsets() {
        for width in [15, 17, 22, 48] {
            for bounds in [
                (900, Some(2_400)),
                (3_750, Some(5_625)),
                (432_000, Some(540_001)),
                (900, None),
            ] {
                let (duration, at) = format_with(&estimate(bounds.0, bounds.1), JAN1, width, utc);
                assert!(duration.chars().count() <= width, "{width}: {duration}");
                assert!(at.chars().count() <= width, "{width}: {at}");
                assert_ne!(at, "At  N/A");
            }
        }
        let (duration, at) = format_with(&estimate(900, Some(2_400)), JAN1, 15, utc);
        assert_eq!(duration, "In  15m – 40m");
        assert_eq!(at, "At  00:15–00:40");
        assert_eq!(
            format_with(&estimate(900, Some(2_400)), JAN1 - 1_200, 15, utc).1,
            "At  23:55–00:20"
        );
    }

    #[test]
    fn open_ranges_and_unavailable_clocks_do_not_fabricate_endpoints() {
        assert_eq!(
            format_with(&estimate(900, None), JAN1, 24, utc),
            ("In  ≥ 15m".into(), "At  ≥ 00:15".into())
        );
        assert_eq!(
            format_with(&estimate(900, Some(2_400)), i64::MAX, 24, utc).1,
            "At  N/A"
        );
        let (duration, at) = format_with(&estimate(900, Some(2_400)), JAN1, 24, |_, _| None);
        assert_eq!(duration, "In  15m – 40m");
        assert_eq!(at, "At  N/A");
    }

    #[test]
    fn utc_offset_changes_are_shown_in_utc_not_as_reversed_local_clocks() {
        let base = JAN1 + 5 * 3_600;
        let convert = |timestamp: i64, local: bool| {
            let offset = if !local {
                0
            } else if timestamp < base + 3_600 {
                -4 * 3_600
            } else {
                -5 * 3_600
            };
            let mut clock = calendar(timestamp + offset, false)?;
            clock.offset = offset;
            Some(clock)
        };
        assert_eq!(
            format_with(&estimate(2_700, Some(4_500)), base, 24, convert).1,
            "At  UTC 05:45 – 06:15"
        );
        assert_eq!(
            format_with(&estimate(2_700, Some(4_500)), base, 15, convert).1,
            "UTC 05:45–06:15"
        );
    }
}
