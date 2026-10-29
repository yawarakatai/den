use std::{
    fs::{DirBuilder, File, OpenOptions},
    io,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::Path,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Row, params};

use crate::{
    model::{BatteryReading, Sample, SampleTime, Snapshot},
    usage::UsageHistory,
};

const SCHEMA_VERSION: i64 = 1;
const READING_COLUMNS: &str = "battery, status, external_power, capacity_percent, energy_now_wh,
    energy_full_wh, energy_design_wh, power_w, voltage_v, current_a,
    charge_now_ah, charge_full_ah, charge_design_ah, energy_estimated";

pub struct Store {
    connection: Connection,
    _writer_lock: Option<File>,
}

impl Store {
    pub fn writer(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
            DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)
                .with_context(|| format!("Cannot create {}", parent.display()))?;
        }
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o640)
            .open(path)
        {
            Ok(_) => (),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(error).context("Cannot create the history database"),
        }
        let mut lock_path = path.as_os_str().to_os_string();
        lock_path.push(".lock");
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o640)
            .open(&lock_path)
            .context("Cannot open the recorder lock file")?;
        rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive)
            .context("Cannot lock history; another recorder may already be running")?;
        let mut connection = Connection::open(path)
            .with_context(|| format!("Cannot open database {}", path.display()))?;
        connection.busy_timeout(Duration::from_secs(2))?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version > SCHEMA_VERSION {
            bail!("Database schema {version} is newer than supported schema {SCHEMA_VERSION}");
        }
        // Rollback journaling lets read-only viewers open the database even after
        // the daemon exits, without needing writable WAL/shared-memory files.
        connection.execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = DELETE;")?;
        if version == 0 {
            let transaction = connection.transaction()?;
            transaction.execute_batch(
                "CREATE TABLE frames (
                    id INTEGER PRIMARY KEY,
                    timestamp INTEGER NOT NULL,
                    boot_id TEXT NOT NULL,
                    boottime_ms INTEGER NOT NULL,
                    monotonic_ms INTEGER NOT NULL,
                    interval_secs INTEGER NOT NULL CHECK (interval_secs > 0)
                );
                CREATE INDEX frames_timestamp ON frames(timestamp);
                CREATE TABLE samples (
                    frame_id INTEGER NOT NULL REFERENCES frames(id) ON DELETE CASCADE,
                    battery TEXT NOT NULL,
                    status TEXT NOT NULL,
                    external_power INTEGER,
                    capacity_percent REAL,
                    energy_now_wh REAL,
                    energy_full_wh REAL,
                    energy_design_wh REAL,
                    power_w REAL,
                    voltage_v REAL,
                    current_a REAL,
                    charge_now_ah REAL,
                    charge_full_ah REAL,
                    charge_design_ah REAL,
                    energy_estimated INTEGER NOT NULL,
                    PRIMARY KEY(frame_id, battery)
                );
                PRAGMA user_version = 1;",
            )?;
            transaction.commit()?;
        }
        Ok(Self {
            connection,
            _writer_lock: Some(lock),
        })
    }

    pub fn reader(path: &Path) -> Result<Self> {
        let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!(
                "Cannot read history at {}. Start the daemon first; for the system service, check membership in the den group",
                path.display()
            ))?;
        // History is best-effort in a live UI: a busy writer must not stall
        // sensor sampling or key handling for seconds.
        connection.busy_timeout(Duration::from_millis(20))?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version != SCHEMA_VERSION {
            bail!("Unsupported database schema {version}; expected {SCHEMA_VERSION}");
        }
        Ok(Self {
            connection,
            _writer_lock: None,
        })
    }

    pub fn record(
        &mut self,
        time: &SampleTime,
        interval_secs: u64,
        readings: &[BatteryReading],
    ) -> Result<()> {
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "INSERT INTO frames(timestamp, boot_id, boottime_ms, monotonic_ms, interval_secs)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                time.timestamp,
                time.boot_id,
                time.boottime_ms,
                time.monotonic_ms,
                interval_secs
            ],
        )?;
        let frame_id = transaction.last_insert_rowid();
        {
            let mut insert = transaction.prepare_cached(
                "INSERT INTO samples VALUES (
                    ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15
                )",
            )?;
            for reading in readings {
                insert.execute(params![
                    frame_id,
                    reading.battery,
                    reading.status,
                    reading.external_power,
                    reading.capacity_percent,
                    reading.energy_now_wh,
                    reading.energy_full_wh,
                    reading.energy_design_wh,
                    reading.power_w,
                    reading.voltage_v,
                    reading.current_a,
                    reading.charge_now_ah,
                    reading.charge_full_ah,
                    reading.charge_design_ah,
                    reading.energy_estimated,
                ])?;
            }
        }
        transaction
            .commit()
            .context("Cannot commit battery measurements")
    }

    pub fn prune(&self, cutoff: i64) -> Result<()> {
        // Keep the latest heartbeat even if the wall clock has moved backwards.
        self.connection.execute(
            "DELETE FROM frames WHERE timestamp < ?1 AND id != (SELECT MAX(id) FROM frames)",
            [cutoff],
        )?;
        Ok(())
    }

    pub fn latest(&mut self) -> Result<Option<Snapshot>> {
        let transaction = self.connection.transaction()?;
        let frame = transaction
            .query_row(
                "SELECT id, timestamp, boot_id, boottime_ms, monotonic_ms, interval_secs
             FROM frames ORDER BY id DESC LIMIT 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        read_time(row, 1)?,
                        row.get::<_, u64>(5)?,
                    ))
                },
            )
            .optional()?;
        let Some((frame_id, time, interval_secs)) = frame else {
            return Ok(None);
        };
        let batteries = {
            let mut statement = transaction.prepare(&format!(
                "SELECT {READING_COLUMNS} FROM samples WHERE frame_id = ?1 ORDER BY battery"
            ))?;
            statement
                .query_map([frame_id], |row| read_reading(row, 0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        transaction.commit()?;
        Ok(Some(Snapshot {
            frame_id,
            time,
            interval_secs,
            batteries,
        }))
    }

    pub fn usage(&self, battery: &str, end: i64, frame_id: i64) -> Result<UsageHistory> {
        let mut usage = UsageHistory::new(end);
        // Include one preceding frame to interpolate the first bucket boundary.
        // LEFT JOIN preserves missing-battery heartbeats as explicit gaps.
        let mut statement = self.connection.prepare(
            "WITH frame_ids AS (
                SELECT id FROM frames WHERE timestamp >= ?2 AND timestamp <= ?4 AND id <= ?3
                UNION ALL
                SELECT id FROM (
                    SELECT id FROM frames WHERE timestamp < ?2 AND id <= ?3
                    ORDER BY timestamp DESC, id DESC LIMIT 1
                )
             )
             SELECT f.timestamp, f.boot_id, f.boottime_ms, f.monotonic_ms,
                    f.interval_secs, s.status, s.power_w
             FROM frame_ids ids JOIN frames f ON f.id = ids.id
             LEFT JOIN samples s ON s.frame_id = f.id AND s.battery = ?1
             ORDER BY f.id",
        )?;
        let rows = statement.query_map(params![battery, usage.start(), frame_id, end], |row| {
            Ok(Sample {
                time: read_time(row, 0)?,
                interval_secs: row.get(4)?,
                reading: BatteryReading {
                    battery: battery.to_owned(),
                    status: row
                        .get::<_, Option<String>>(5)?
                        .unwrap_or_else(|| "Unknown".into()),
                    power_w: row.get(6)?,
                    ..Default::default()
                },
            })
        })?;
        for row in rows {
            usage.push(row?);
        }
        Ok(usage)
    }

    pub fn history(&self, battery: &str, since: i64, frame_id: i64) -> Result<Vec<Sample>> {
        let mut statement = self.connection.prepare(&format!(
            "SELECT f.timestamp, f.boot_id, f.boottime_ms, f.monotonic_ms,
                    f.interval_secs, {READING_COLUMNS}
             FROM frames f JOIN samples s ON s.frame_id = f.id
             WHERE s.battery = ?1 AND f.timestamp >= ?2 AND f.id <= ?3
             ORDER BY f.id"
        ))?;
        Ok(statement
            .query_map(params![battery, since, frame_id], |row| {
                Ok(Sample {
                    time: read_time(row, 0)?,
                    interval_secs: row.get(4)?,
                    reading: read_reading(row, 5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

fn read_time(row: &Row<'_>, offset: usize) -> rusqlite::Result<SampleTime> {
    Ok(SampleTime {
        timestamp: row.get(offset)?,
        boot_id: row.get(offset + 1)?,
        boottime_ms: row.get(offset + 2)?,
        monotonic_ms: row.get(offset + 3)?,
    })
}

fn read_reading(row: &Row<'_>, offset: usize) -> rusqlite::Result<BatteryReading> {
    Ok(BatteryReading {
        battery: row.get(offset)?,
        status: row.get(offset + 1)?,
        external_power: row.get(offset + 2)?,
        capacity_percent: row.get(offset + 3)?,
        energy_now_wh: row.get(offset + 4)?,
        energy_full_wh: row.get(offset + 5)?,
        energy_design_wh: row.get(offset + 6)?,
        power_w: row.get(offset + 7)?,
        voltage_v: row.get(offset + 8)?,
        current_a: row.get(offset + 9)?,
        charge_now_ah: row.get(offset + 10)?,
        charge_full_ah: row.get(offset + 11)?,
        charge_design_ah: row.get(offset + 12)?,
        energy_estimated: row.get(offset + 13)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::test_time;

    #[test]
    fn roundtrip_retention_and_empty_heartbeat() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.sqlite3");
        let mut writer = Store::writer(&path).unwrap();
        let reading = BatteryReading {
            battery: "BAT0".into(),
            status: "Discharging".into(),
            energy_now_wh: Some(20.0),
            charge_now_ah: Some(2.0),
            energy_estimated: true,
            ..Default::default()
        };
        writer
            .record(&test_time(0), 30, std::slice::from_ref(&reading))
            .unwrap();
        let mut reader = Store::reader(&path).unwrap();
        let snapshot = reader.latest().unwrap().unwrap();
        assert_eq!(snapshot.batteries, vec![reading]);
        let history = reader.history("BAT0", 0, snapshot.frame_id).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].time, test_time(0));
        writer.record(&test_time(30), 30, &[]).unwrap();
        writer.prune(test_time(60).timestamp).unwrap();
        let snapshot = reader.latest().unwrap().unwrap();
        assert!(snapshot.batteries.is_empty());
        assert_eq!(snapshot.time, test_time(30));
        assert!(
            reader
                .history("BAT0", 0, snapshot.frame_id)
                .unwrap()
                .is_empty()
        );
        drop(reader);
        drop(writer);
        let mut reader = Store::reader(&path).unwrap();
        assert!(reader.latest().unwrap().is_some());
        assert!(reader.connection.execute("DELETE FROM frames", []).is_err());
    }

    #[test]
    fn only_one_recorder_can_write_a_history() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.sqlite3");
        let writer = Store::writer(&path).unwrap();
        assert!(Store::writer(&path).is_err());
        assert!(Store::reader(&path).is_ok());
        drop(writer);
        assert!(Store::writer(&path).is_ok());
    }

    #[test]
    fn reading_does_not_create_a_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.sqlite3");
        assert!(Store::reader(&path).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn stopped_history_needs_no_writable_sidecars() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.sqlite3");
        let mut writer = Store::writer(&path).unwrap();
        writer.record(&test_time(0), 30, &[]).unwrap();
        drop(writer);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o440)).unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o550)).unwrap();
        let result = Store::reader(&path).and_then(|mut reader| reader.latest());
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(result.unwrap().is_some());
        assert!(!dir.path().join("history.sqlite3-wal").exists());
        assert!(!dir.path().join("history.sqlite3-shm").exists());
    }

    #[test]
    fn usage_keeps_empty_heartbeats_battery_identity_and_the_snapshot_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.sqlite3");
        let mut writer = Store::writer(&path).unwrap();
        let reading = BatteryReading {
            battery: "BAT0".into(),
            status: "Discharging".into(),
            power_w: Some(12.0),
            ..Default::default()
        };
        for second in [0, 30, 60, 90, 120, 150] {
            let readings = if second == 60 {
                vec![]
            } else {
                vec![reading.clone()]
            };
            writer.record(&test_time(second), 30, &readings).unwrap();
        }
        let reader = Store::reader(&path).unwrap();
        let usage = reader.usage("BAT0", test_time(150).timestamp, 5).unwrap();
        assert!((usage.total().unwrap() - 0.2).abs() < 1e-9);
        assert_eq!(
            usage.hours.iter().map(|h| h.covered_seconds).sum::<i64>(),
            60
        );
        assert_eq!(
            reader
                .usage("BAT1", test_time(150).timestamp, 6)
                .unwrap()
                .total(),
            None
        );
        assert_eq!(
            reader
                .usage("BAT0", test_time(0).timestamp, 6)
                .unwrap()
                .total(),
            None
        );
        assert_eq!(writer.latest().unwrap().unwrap().frame_id, 6);
    }

    #[test]
    fn usage_reads_a_preceding_frame_for_the_first_hour_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.sqlite3");
        let mut writer = Store::writer(&path).unwrap();
        let end = test_time(0).timestamp;
        let start = UsageHistory::new(end).start();
        for timestamp in [start - 15, start + 15] {
            let time = test_time(timestamp - end);
            writer
                .record(
                    &time,
                    30,
                    &[BatteryReading {
                        battery: "BAT0".into(),
                        status: "Discharging".into(),
                        power_w: Some(12.0),
                        ..Default::default()
                    }],
                )
                .unwrap();
        }
        let usage = Store::reader(&path).unwrap().usage("BAT0", end, 2).unwrap();
        assert_eq!(usage.hours[0].covered_seconds, 15);
        assert!((usage.total().unwrap() - 0.05).abs() < 1e-9);
    }

    #[test]
    fn rejects_a_future_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.sqlite3");
        let connection = Connection::open(&path).unwrap();
        connection.pragma_update(None, "user_version", 2).unwrap();
        drop(connection);
        assert!(Store::writer(&path).is_err());
        assert!(Store::reader(&path).is_err());
    }
}
