use std::{
    collections::{HashMap, VecDeque},
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};

use crate::{
    battery,
    model::{BatteryReading, Sample, SampleTime, Snapshot, unix_now},
    power::{self, PowerSettings},
    predict::{Estimate, time_to_empty, time_to_full},
    storage::Store,
    usage::{HOUR, UsageHistory},
};

pub const LIVE_INTERVAL: Duration = Duration::from_secs(1);
const HISTORY_REFRESH: Duration = Duration::from_secs(5);
const LIVE_SECONDS: i64 = 600;

// Live sensor data must not wait for the recorder's disk-write interval. The
// viewer keeps a bounded, volatile series and only ever opens history read-only.
pub struct Monitor {
    pub recorded: Option<Snapshot>,
    pub history: Vec<Sample>,
    pub usage: Option<UsageHistory>,
    pub usage_error: Option<String>,
    pub power_settings: PowerSettings,
    pub selected: Option<String>,
    pub live_enabled: bool,
    pub live_error: Option<String>,
    pub history_error: Option<String>,
    pub now: i64,
    current: Option<Snapshot>,
    live_history: HashMap<String, VecDeque<Sample>>,
    database: PathBuf,
    sysfs: PathBuf,
    boot_id: String,
    store: Option<Store>,
    history_refreshed: Option<Instant>,
    power_refreshed: Option<Instant>,
    history_key: Option<(i64, String, i64)>,
    usage_key: Option<(i64, String, i64)>,
}

impl Monitor {
    pub fn new(database: &Path, sysfs: &Path, live_enabled: bool) -> Result<Self> {
        let boot_id = if live_enabled {
            fs::read_to_string("/proc/sys/kernel/random/boot_id")
                .context("Cannot read the Linux boot ID")?
                .trim()
                .to_owned()
        } else {
            String::new()
        };
        Ok(Self {
            recorded: None,
            history: Vec::new(),
            usage: None,
            usage_error: None,
            power_settings: PowerSettings::default(),
            selected: None,
            live_enabled,
            live_error: None,
            history_error: None,
            now: unix_now()?,
            current: None,
            live_history: HashMap::new(),
            database: database.to_owned(),
            sysfs: sysfs.to_owned(),
            boot_id,
            store: None,
            history_refreshed: None,
            power_refreshed: None,
            history_key: None,
            usage_key: None,
        })
    }

    pub fn tick(&mut self, range: i64) -> Result<()> {
        self.now = unix_now()?;
        if self.live_enabled {
            self.poll_live(SampleTime::now(&self.boot_id)?);
            if self
                .power_refreshed
                .is_none_or(|at| at.elapsed() >= HISTORY_REFRESH)
            {
                self.power_settings = power::sys_root(&self.sysfs)
                    .map(PowerSettings::collect)
                    .unwrap_or_default();
                self.power_refreshed = Some(Instant::now());
            }
        }
        if self
            .history_refreshed
            .is_none_or(|at| at.elapsed() >= HISTORY_REFRESH)
        {
            self.refresh_history(range);
        }
        Ok(())
    }

    fn poll_live(&mut self, time: SampleTime) {
        match battery::collect(&self.sysfs) {
            Ok(readings) => {
                self.remember_live(time, readings);
                self.live_error = None;
            }
            Err(error) => {
                // Explicit missing samples break the graph, even if the next
                // successful read is only a second away.
                self.append_live(&time, &[]);
                self.live_error = Some(format!("Sensor unavailable: {error:#}"));
            }
        }
    }

    pub fn remember_live(&mut self, time: SampleTime, readings: Vec<BatteryReading>) {
        self.now = time.timestamp;
        self.append_live(&time, &readings);
        self.current = Some(Snapshot {
            frame_id: 0,
            time,
            interval_secs: 1,
            batteries: readings,
        });
        self.ensure_selection();
    }

    fn append_live(&mut self, time: &SampleTime, readings: &[BatteryReading]) {
        for reading in readings {
            self.live_history
                .entry(reading.battery.clone())
                .or_default();
        }
        for (name, series) in &mut self.live_history {
            let reading = readings
                .iter()
                .find(|reading| &reading.battery == name)
                .cloned()
                .unwrap_or_else(|| BatteryReading {
                    battery: name.clone(),
                    status: "Unknown".into(),
                    ..Default::default()
                });
            series.push_back(Sample {
                time: time.clone(),
                interval_secs: 1,
                reading,
            });
            while series.front().is_some_and(|sample| {
                series.len() > LIVE_SECONDS as usize + 1
                    || time.boottime_ms - sample.time.boottime_ms > LIVE_SECONDS * 1_000
            }) {
                series.pop_front();
            }
        }
    }

    pub fn snapshot(&self) -> Option<&Snapshot> {
        if self.live_enabled {
            self.current.as_ref()
        } else {
            self.recorded.as_ref()
        }
    }

    pub fn reading(&self) -> Option<&BatteryReading> {
        self.snapshot()?
            .batteries
            .iter()
            .find(|reading| Some(&reading.battery) == self.selected.as_ref())
    }

    pub fn source_stale(&self) -> bool {
        let Some(snapshot) = self.snapshot() else {
            return true;
        };
        if self.live_enabled {
            self.live_error.is_some()
                || self.now < snapshot.time.timestamp
                || self.now - snapshot.time.timestamp > 3
        } else {
            snapshot.is_stale(self.now)
        }
    }

    pub fn plot_end(&self) -> i64 {
        if self.live_enabled {
            self.now
        } else {
            self.recorded
                .as_ref()
                .map_or(self.now, |snapshot| snapshot.time.timestamp)
        }
    }

    pub fn samples(&self) -> impl Iterator<Item = &Sample> {
        let selected = self.selected.as_deref().unwrap_or("");
        let live = self
            .live_history
            .get(selected)
            .filter(|_| self.live_enabled);
        let cutoff = live
            .and_then(|series| series.front())
            .map(|sample| sample.time.timestamp);
        self.history
            .iter()
            .filter(move |sample| {
                sample.reading.battery == selected
                    && cutoff.is_none_or(|cutoff| sample.time.timestamp < cutoff)
            })
            .chain(live.into_iter().flat_map(|series| series.iter()))
    }

    pub fn estimate(&self) -> Option<Estimate> {
        if self.source_stale() {
            return None;
        }
        let recent: Vec<_> = self
            .samples()
            .filter(|sample| sample.time.timestamp >= self.plot_end() - LIVE_SECONDS)
            .cloned()
            .collect();
        match self.reading()?.status.as_str() {
            "Charging" => time_to_full(&recent),
            "Discharging" => time_to_empty(&recent),
            _ => None,
        }
    }

    fn ensure_selection(&mut self) {
        let Some(snapshot) = self.snapshot() else {
            return;
        };
        if snapshot
            .batteries
            .iter()
            .any(|reading| Some(&reading.battery) == self.selected.as_ref())
        {
            return;
        }
        if let Some(first) = snapshot.batteries.first() {
            self.selected = Some(first.battery.clone());
        }
    }

    pub fn next_battery(&mut self) {
        let Some(snapshot) = self.snapshot() else {
            return;
        };
        if snapshot.batteries.is_empty() {
            return;
        }
        let index = snapshot
            .batteries
            .iter()
            .position(|reading| Some(&reading.battery) == self.selected.as_ref())
            .unwrap_or(0);
        self.selected = Some(
            snapshot.batteries[(index + 1) % snapshot.batteries.len()]
                .battery
                .clone(),
        );
    }

    pub fn refresh_history(&mut self, range: i64) {
        let result = self.load_history(range.max(LIVE_SECONDS));
        self.history_refreshed = Some(Instant::now());
        match result {
            Ok(()) => {
                self.history_error = None;
                self.refresh_usage();
            }
            Err(error) => {
                self.history_error = Some(format!("History unavailable: {error:#}"));
                self.usage_error = self.history_error.clone();
                self.usage = None;
                self.usage_key = None;
                self.store = None;
                // A new database at the same path can reuse frame IDs.
                self.history_key = None;
            }
        }
    }

    fn refresh_usage(&mut self) {
        let (Some(snapshot), Some(selected), Some(store)) =
            (&self.recorded, &self.selected, &self.store)
        else {
            self.usage = None;
            self.usage_key = None;
            self.usage_error = None;
            return;
        };
        let end = self.plot_end();
        let key = (snapshot.frame_id, selected.clone(), end.div_euclid(HOUR));
        if self.usage_key.as_ref() == Some(&key) {
            return;
        }
        match store.usage(selected, end, snapshot.frame_id) {
            Ok(usage) => {
                self.usage = Some(usage);
                self.usage_key = Some(key);
                self.usage_error = None;
            }
            Err(error) => {
                self.usage = None;
                self.usage_key = None;
                self.usage_error = Some(format!("Usage history unavailable: {error:#}"));
            }
        }
    }

    fn load_history(&mut self, range: i64) -> Result<()> {
        if self.store.is_none() {
            self.store = Some(Store::reader(&self.database)?);
        }
        let snapshot = self
            .store
            .as_mut()
            .context("History is not open")?
            .latest()?;
        if let Some(snapshot) = &snapshot {
            if self.selected.is_none() {
                self.selected = snapshot
                    .batteries
                    .first()
                    .map(|reading| reading.battery.clone());
            }
            if !self.live_enabled
                && !snapshot
                    .batteries
                    .iter()
                    .any(|reading| Some(&reading.battery) == self.selected.as_ref())
            {
                self.selected = snapshot
                    .batteries
                    .first()
                    .map(|reading| reading.battery.clone());
            }
            if let Some(selected) = &self.selected {
                let key = (snapshot.frame_id, selected.clone(), range);
                if self.history_key.as_ref() != Some(&key) {
                    let end = if self.live_enabled {
                        self.now
                    } else {
                        snapshot.time.timestamp
                    };
                    self.history = self
                        .store
                        .as_ref()
                        .context("History is not open")?
                        .history(selected, end - range, snapshot.frame_id)?;
                    self.history_key = Some(key);
                }
            } else {
                self.history.clear();
                self.history_key = None;
            }
        } else {
            self.history.clear();
            self.history_key = None;
        }
        self.recorded = snapshot;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::test_time;

    fn reading(power: f64, status: &str) -> BatteryReading {
        BatteryReading {
            battery: "BAT0".into(),
            status: status.into(),
            power_w: Some(power),
            energy_now_wh: Some(25.0),
            capacity_percent: Some(50.0),
            ..Default::default()
        }
    }

    #[test]
    fn live_data_advances_without_a_database_or_a_recorder() {
        let dir = tempfile::tempdir().unwrap();
        let mut monitor = Monitor::new(&dir.path().join("missing.db"), dir.path(), true).unwrap();
        monitor.remember_live(test_time(0), vec![reading(8.0, "Discharging")]);
        monitor.refresh_history(60);
        assert!(monitor.history_error.is_some());
        monitor.remember_live(test_time(1), vec![reading(0.0, "Charging")]);
        assert_eq!(monitor.reading().unwrap().power_w, Some(0.0));
        monitor.remember_live(test_time(2), vec![reading(32.0, "Charging")]);
        assert_eq!(monitor.reading().unwrap().power_w, Some(32.0));
        assert_eq!(monitor.samples().count(), 3);
        assert!(!dir.path().join("missing.db").exists());
    }

    #[test]
    fn live_samples_replace_overlapping_recorded_samples_but_do_not_modify_history() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        let mut writer = Store::writer(&path).unwrap();
        for second in [0, 30, 60] {
            writer
                .record(&test_time(second), 30, &[reading(8.0, "Discharging")])
                .unwrap();
        }
        let mut monitor = Monitor::new(&path, dir.path(), true).unwrap();
        monitor.remember_live(test_time(30), vec![reading(32.0, "Charging")]);
        monitor.remember_live(test_time(31), vec![reading(31.0, "Charging")]);
        monitor.refresh_history(60);
        let samples: Vec<_> = monitor.samples().collect();
        assert_eq!(samples.len(), 3);
        assert_eq!(samples[1].reading.power_w, Some(32.0));
        assert_eq!(writer.latest().unwrap().unwrap().time, test_time(60));
    }

    #[test]
    fn live_memory_is_bounded_and_missing_devices_break_the_series() {
        let dir = tempfile::tempdir().unwrap();
        let mut monitor = Monitor::new(&dir.path().join("missing.db"), dir.path(), true).unwrap();
        for second in 0..700 {
            monitor.remember_live(test_time(second), vec![reading(8.0, "Discharging")]);
        }
        assert_eq!(monitor.samples().count(), 601);
        monitor.remember_live(test_time(700), vec![]);
        assert!(monitor.reading().is_none());
        assert!(monitor.samples().last().unwrap().reading.power_w.is_none());
    }

    #[test]
    fn replug_fixture_is_observed_on_the_next_sensor_poll() {
        let dir = tempfile::tempdir().unwrap();
        let supply = dir.path().join("BAT0");
        fs::create_dir(&supply).unwrap();
        let mut monitor = Monitor::new(&dir.path().join("missing.db"), dir.path(), true).unwrap();
        for (second, status, power) in [
            (0, "Discharging", 8_000_000),
            (1, "Charging", 0),
            (2, "Charging", 32_000_000),
        ] {
            fs::write(supply.join("uevent"), format!(
                "POWER_SUPPLY_TYPE=Battery\nPOWER_SUPPLY_STATUS={status}\nPOWER_SUPPLY_POWER_NOW={power}\n"
            )).unwrap();
            monitor.poll_live(test_time(second));
            assert_eq!(monitor.reading().unwrap().status, status);
            assert_eq!(
                monitor.reading().unwrap().power_w,
                Some(power as f64 / 1_000_000.0)
            );
        }
        fs::remove_file(supply.join("uevent")).unwrap();
        // A missing device is an empty snapshot, not a cached non-zero rate.
        monitor.poll_live(test_time(3));
        assert!(monitor.reading().is_none());
    }

    #[test]
    fn an_error_disables_eta_and_inserts_a_gap_before_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let mut monitor = Monitor::new(
            &dir.path().join("missing.db"),
            &dir.path().join("missing-sysfs"),
            true,
        )
        .unwrap();
        for second in 0..=60 {
            monitor.remember_live(test_time(second), vec![reading(10.0, "Discharging")]);
        }
        assert!(monitor.estimate().is_some());
        monitor.poll_live(test_time(61));
        assert!(monitor.source_stale());
        assert!(monitor.estimate().is_none());
        assert!(monitor.samples().last().unwrap().reading.power_w.is_none());
    }

    #[test]
    fn charging_estimate_warms_up_after_a_transition_and_is_disabled_when_stale() {
        let dir = tempfile::tempdir().unwrap();
        let mut monitor = Monitor::new(&dir.path().join("missing.db"), dir.path(), true).unwrap();
        for second in 0..=60 {
            monitor.remember_live(test_time(second), vec![reading(10.0, "Discharging")]);
        }
        assert_eq!(monitor.estimate().unwrap().seconds, 9_000);
        for second in 61..=121 {
            let mut charging = reading(20.0, "Charging");
            charging.energy_full_wh = Some(50.0);
            monitor.remember_live(test_time(second), vec![charging]);
            if second < 121 {
                assert!(monitor.estimate().is_none());
            }
        }
        assert_eq!(monitor.estimate().unwrap().seconds, 4_500);
        monitor.now += 10;
        assert!(monitor.estimate().is_none());
    }

    #[test]
    fn locked_history_does_not_freeze_live_values_or_disable_a_live_estimate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        let mut writer = Store::writer(&path).unwrap();
        writer
            .record(&test_time(0), 30, &[reading(8.0, "Discharging")])
            .unwrap();
        let mut monitor = Monitor::new(&path, dir.path(), true).unwrap();
        for second in 0..=60 {
            monitor.remember_live(test_time(second), vec![reading(10.0, "Discharging")]);
        }
        monitor.refresh_history(60);
        let blocker = rusqlite::Connection::open(&path).unwrap();
        blocker.execute_batch("BEGIN EXCLUSIVE").unwrap();
        monitor.remember_live(test_time(61), vec![reading(14.0, "Discharging")]);
        monitor.refresh_history(60);
        assert!(monitor.history_error.is_some());
        assert_eq!(monitor.reading().unwrap().power_w, Some(14.0));
        assert!(!monitor.source_stale());
        assert!(monitor.estimate().is_some());
        blocker.execute_batch("ROLLBACK").unwrap();
        monitor.refresh_history(60);
        assert!(monitor.history_error.is_none());
    }

    #[test]
    fn usage_is_recorded_only_independent_of_the_graph_range_and_changes_battery() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        let mut writer = Store::writer(&path).unwrap();
        for second in [0, 30, 3_600, 3_630] {
            let first = reading(12.0, "Discharging");
            let mut second_battery = reading(24.0, "Discharging");
            second_battery.battery = "BAT1".into();
            writer
                .record(&test_time(second), 30, &[first, second_battery])
                .unwrap();
        }
        let mut monitor = Monitor::new(&path, dir.path(), true).unwrap();
        monitor.remember_live(test_time(3_630), vec![reading(100.0, "Discharging")]);
        monitor.refresh_history(60);
        assert!((monitor.usage.as_ref().unwrap().total().unwrap() - 0.2).abs() < 1e-9);
        assert!(
            monitor
                .history
                .iter()
                .all(|s| s.time.timestamp >= test_time(3_030).timestamp)
        );
        let key = monitor.usage_key.clone();
        monitor.refresh_history(300);
        assert_eq!(monitor.usage_key, key);
        assert!((monitor.usage.as_ref().unwrap().total().unwrap() - 0.2).abs() < 1e-9);
        monitor.selected = Some("BAT1".into());
        monitor.refresh_history(60);
        assert!((monitor.usage.as_ref().unwrap().total().unwrap() - 0.4).abs() < 1e-9);
        assert_ne!(monitor.usage_key, key);

        let start = monitor.usage.as_ref().unwrap().start();
        monitor.now += HOUR;
        monitor.refresh_history(60);
        assert_eq!(monitor.usage.as_ref().unwrap().start(), start + HOUR);
        assert!((monitor.usage.as_ref().unwrap().total().unwrap() - 0.4).abs() < 1e-9);
    }

    #[test]
    fn a_failed_usage_query_does_not_disable_live_readings_or_the_short_history() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        let mut writer = Store::writer(&path).unwrap();
        for second in [0, 30] {
            writer
                .record(&test_time(second), 30, &[reading(12.0, "Discharging")])
                .unwrap();
        }
        let mut monitor = Monitor::new(&path, dir.path(), true).unwrap();
        monitor.remember_live(test_time(30), vec![reading(8.0, "Discharging")]);
        monitor.refresh_history(60);
        assert!(monitor.usage.is_some());
        let blocker = rusqlite::Connection::open(&path).unwrap();
        blocker.execute_batch("BEGIN EXCLUSIVE").unwrap();
        monitor.usage_key = None;
        monitor.refresh_usage();
        assert!(monitor.usage_error.is_some());
        assert!(monitor.usage.is_none());
        assert!(monitor.history_error.is_none());
        assert_eq!(monitor.history.len(), 2);
        assert_eq!(monitor.reading().unwrap().power_w, Some(8.0));
        blocker.execute_batch("ROLLBACK").unwrap();
        monitor.refresh_usage();
        assert!(monitor.usage.is_some());
        assert!(monitor.usage_error.is_none());
    }

    #[test]
    fn power_settings_refresh_slowly_without_history_and_do_not_survive_read_failures() {
        let dir = tempfile::tempdir().unwrap();
        let sys = dir.path().join("sys");
        let supplies = sys.join("class/power_supply");
        std::fs::create_dir_all(&supplies).unwrap();
        let profile = sys.join("firmware/acpi/platform_profile");
        std::fs::create_dir_all(profile.parent().unwrap()).unwrap();
        std::fs::write(&profile, "balanced").unwrap();
        let database = dir.path().join("missing.db");
        let mut monitor = Monitor::new(&database, &supplies, true).unwrap();
        monitor.tick(60).unwrap();
        assert_eq!(monitor.power_settings.platform.as_deref(), Some("balanced"));
        std::fs::write(&profile, "performance").unwrap();
        monitor.tick(60).unwrap();
        assert_eq!(monitor.power_settings.platform.as_deref(), Some("balanced"));
        monitor.power_refreshed = Some(Instant::now() - HISTORY_REFRESH);
        monitor.tick(60).unwrap();
        assert_eq!(
            monitor.power_settings.platform.as_deref(),
            Some("performance")
        );
        std::fs::remove_file(profile).unwrap();
        monitor.power_refreshed = Some(Instant::now() - HISTORY_REFRESH);
        monitor.tick(60).unwrap();
        assert_eq!(monitor.power_settings.platform, None);
        assert!(!database.exists());
    }

    #[test]
    fn history_only_never_reads_live_sensors_and_anchors_at_the_recording() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        let mut writer = Store::writer(&path).unwrap();
        writer
            .record(&test_time(0), 30, &[reading(8.0, "Discharging")])
            .unwrap();
        let sys = dir.path().join("sys");
        std::fs::create_dir_all(sys.join("firmware/acpi")).unwrap();
        std::fs::write(sys.join("firmware/acpi/platform_profile"), "performance").unwrap();
        let mut monitor = Monitor::new(&path, &sys.join("class/power_supply"), false).unwrap();
        monitor.tick(60).unwrap();
        assert_eq!(monitor.plot_end(), test_time(0).timestamp);
        assert_eq!(monitor.samples().count(), 1);
        assert!(monitor.live_error.is_none());
        assert!(monitor.source_stale());
        assert_eq!(monitor.power_settings, PowerSettings::default());
        assert!(monitor.power_refreshed.is_none());
    }
}
