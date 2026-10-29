use std::{path::Path, process::Command};

use rusqlite::Connection;

fn binary() -> Command {
    Command::new(env!("CARGO_BIN_EXE_den"))
}

#[test]
fn records_fixture_and_exits_without_starting_a_service() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("state/history.sqlite3");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/power_supply");
    let output = binary()
        .arg("--database")
        .arg(&database)
        .args(["daemon", "--once", "--sysfs"])
        .arg(fixture)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Recorded 1 batteries"));
    let connection = Connection::open(&database).unwrap();
    let values: (String, f64, f64, bool) = connection
        .query_row(
            "SELECT battery, energy_now_wh, power_w, external_power FROM samples",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(values, ("BAT0".into(), 25.0, 10.0, false));
    let count: i64 = connection
        .query_row("SELECT COUNT(*) FROM frames", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn noninteractive_monitor_does_not_create_missing_history() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("missing.sqlite3");
    let output = binary().arg("--database").arg(&database).output().unwrap();
    assert!(!output.status.success());
    assert!(!database.exists());
    assert!(String::from_utf8_lossy(&output.stderr).contains("requires an interactive terminal"));
}

#[test]
fn rejects_zero_interval_before_creating_history() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("missing.sqlite3");
    let output = binary()
        .arg("--database")
        .arg(&database)
        .args(["daemon", "--once", "--interval", "0"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!database.exists());
}
