use std::{
    fs,
    path::Path,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};

use crate::{DaemonArgs, battery, model::SampleTime, storage::Store};

pub fn run(args: &DaemonArgs, sysfs: &Path, mut store: Store) -> Result<()> {
    let boot_id = fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .context("Cannot read the Linux boot ID")?;
    let boot_id = boot_id.trim();
    let interval = Duration::from_secs(args.interval);
    let mut last_prune = None;
    loop {
        let started = Instant::now();
        let result = (|| -> Result<()> {
            let readings = battery::collect(sysfs)?;
            let time = SampleTime::now(boot_id)?;
            store.record(&time, args.interval, &readings)?;
            if last_prune.is_none_or(|at: Instant| at.elapsed() >= Duration::from_secs(3_600)) {
                store.prune(time.timestamp - i64::from(args.retention_days) * 86_400)?;
                last_prune = Some(Instant::now());
            }
            if args.once {
                println!(
                    "Recorded {} batteries at {}",
                    readings.len(),
                    time.timestamp
                );
            }
            Ok(())
        })();
        if args.once {
            return result;
        }
        if let Err(error) = result {
            eprintln!("den: measurement failed: {error:#}");
        }
        // No catch-up burst after a slow read or a resume from sleep.
        thread::sleep(interval.saturating_sub(started.elapsed()));
    }
}
