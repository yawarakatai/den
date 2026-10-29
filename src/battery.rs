use std::{collections::HashMap, fs, io, path::Path};

use anyhow::{Context, Result};

use crate::model::BatteryReading;

type Properties = HashMap<String, String>;

pub fn collect(root: &Path) -> Result<Vec<BatteryReading>> {
    let entries = fs::read_dir(root)
        .with_context(|| format!("Cannot read power supplies at {}", root.display()))?;
    let mut devices = Vec::new();
    for entry in entries {
        let entry = entry.context("Cannot enumerate power supplies")?;
        let path = entry.path().join("uevent");
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            // Devices can disappear between enumeration and reading.
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error).with_context(|| format!("Cannot read {}", path.display()));
            }
        };
        let properties = text
            .lines()
            .filter_map(|line| line.split_once('='))
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .collect::<Properties>();
        if property(&properties, "SCOPE") == Some("Device") {
            continue;
        }
        devices.push((entry.file_name().to_string_lossy().into_owned(), properties));
    }
    let online: Vec<bool> = devices
        .iter()
        .filter(|(_, values)| property(values, "TYPE") != Some("Battery"))
        .filter_map(|(_, values)| match property(values, "ONLINE") {
            Some("0") => Some(false),
            Some("1") => Some(true),
            _ => None,
        })
        .collect();
    let external_power = (!online.is_empty()).then(|| online.iter().any(|value| *value));
    let mut batteries = devices
        .into_iter()
        .filter(|(_, values)| {
            property(values, "TYPE") == Some("Battery") && property(values, "PRESENT") != Some("0")
        })
        .map(|(name, values)| parse_battery(name, &values, external_power))
        .collect::<Vec<_>>();
    batteries.sort_by(|left, right| left.battery.cmp(&right.battery));
    Ok(batteries)
}

fn property<'a>(values: &'a Properties, name: &str) -> Option<&'a str> {
    values
        .get(&format!("POWER_SUPPLY_{name}"))
        .map(String::as_str)
}

fn number(values: &Properties, name: &str) -> Option<f64> {
    property(values, name)?
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite() && *value >= 0.0)
}

fn micro(values: &Properties, name: &str) -> Option<f64> {
    number(values, name).map(|value| value / 1_000_000.0)
}

fn parse_battery(
    name: String,
    values: &Properties,
    external_power: Option<bool>,
) -> BatteryReading {
    let voltage_v = micro(values, "VOLTAGE_NOW").filter(|value| *value > 0.0);
    // Use one reference voltage for charge-based capacities; retain raw charge
    // values because this conversion is an approximation, not measured energy.
    let reference_voltage = micro(values, "VOLTAGE_MIN_DESIGN")
        .filter(|value| *value > 0.0)
        .or(voltage_v);
    let charge_now_ah = micro(values, "CHARGE_NOW");
    let charge_full_ah = micro(values, "CHARGE_FULL");
    let charge_design_ah = micro(values, "CHARGE_FULL_DESIGN");
    let measured_energy = micro(values, "ENERGY_NOW");
    let energy_now_wh = measured_energy.or_else(|| Some(charge_now_ah? * reference_voltage?));
    let energy_full_wh = micro(values, "ENERGY_FULL")
        .or_else(|| Some(charge_full_ah? * reference_voltage?))
        .filter(|value| *value > 0.0);
    let energy_design_wh = micro(values, "ENERGY_FULL_DESIGN")
        .or_else(|| Some(charge_design_ah? * reference_voltage?))
        .filter(|value| *value > 0.0);
    let current_a = property(values, "CURRENT_NOW")
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite())
        .map(|value| value.abs() / 1_000_000.0);
    let reported_power = property(values, "POWER_NOW")
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite())
        .map(|value| value.abs() / 1_000_000.0);
    // Some drivers expose a zero POWER_NOW while CURRENT_NOW is usable. Do not
    // let that zero mask a second, measured source; retain a real zero otherwise.
    let power_w = reported_power
        .filter(|value| *value > 0.0)
        .or_else(|| Some(current_a? * voltage_v?))
        .or(reported_power);
    let capacity_percent = number(values, "CAPACITY")
        .filter(|value| *value <= 100.0)
        .or_else(|| Some(energy_now_wh? / energy_full_wh? * 100.0))
        .filter(|value| (0.0..=100.0).contains(value));
    let status = property(values, "STATUS")
        .filter(|value| matches!(*value, "Charging" | "Discharging" | "Full" | "Not charging"))
        .unwrap_or("Unknown");
    BatteryReading {
        battery: name,
        status: status.into(),
        external_power,
        capacity_percent,
        energy_now_wh,
        energy_full_wh,
        energy_design_wh,
        power_w,
        voltage_v,
        current_a,
        charge_now_ah,
        charge_full_ah,
        charge_design_ah,
        energy_estimated: measured_energy.is_none() && energy_now_wh.is_some(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn supply(root: &Path, name: &str, text: &str) {
        let path = root.join(name);
        fs::create_dir(&path).unwrap();
        fs::write(path.join("uevent"), text).unwrap();
    }

    #[test]
    fn normalizes_energy_units_and_external_power() {
        let dir = tempfile::tempdir().unwrap();
        supply(
            dir.path(),
            "AC",
            "POWER_SUPPLY_TYPE=Mains\nPOWER_SUPPLY_ONLINE=1\n",
        );
        supply(
            dir.path(),
            "BAT0",
            "POWER_SUPPLY_TYPE=Battery\nPOWER_SUPPLY_STATUS=Charging\nPOWER_SUPPLY_ENERGY_NOW=25000000\nPOWER_SUPPLY_ENERGY_FULL=50000000\nPOWER_SUPPLY_POWER_NOW=-10000000\n",
        );
        let readings = collect(dir.path()).unwrap();
        assert_eq!(readings.len(), 1);
        let reading = &readings[0];
        assert_eq!(reading.external_power, Some(true));
        assert_eq!(reading.energy_now_wh, Some(25.0));
        assert_eq!(reading.capacity_percent, Some(50.0));
        assert_eq!(reading.power_w, Some(10.0));
        assert!(!reading.energy_estimated);
    }

    #[test]
    fn charge_conversion_preserves_raw_values() {
        let dir = tempfile::tempdir().unwrap();
        supply(
            dir.path(),
            "BAT1",
            "POWER_SUPPLY_TYPE=Battery\nPOWER_SUPPLY_CHARGE_NOW=2000000\nPOWER_SUPPLY_CHARGE_FULL=4000000\nPOWER_SUPPLY_VOLTAGE_MIN_DESIGN=10000000\nPOWER_SUPPLY_VOLTAGE_NOW=11000000\nPOWER_SUPPLY_CURRENT_NOW=-1000000\n",
        );
        let readings = collect(dir.path()).unwrap();
        let reading = &readings[0];
        assert_eq!(reading.charge_now_ah, Some(2.0));
        assert_eq!(reading.energy_now_wh, Some(20.0));
        assert_eq!(reading.energy_full_wh, Some(40.0));
        assert_eq!(reading.power_w, Some(11.0));
        assert!(reading.energy_estimated);
    }

    #[test]
    fn zero_power_does_not_mask_measured_current() {
        let values = [
            ("POWER_SUPPLY_POWER_NOW".into(), "0".into()),
            ("POWER_SUPPLY_CURRENT_NOW".into(), "2000000".into()),
            ("POWER_SUPPLY_VOLTAGE_NOW".into(), "12000000".into()),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            parse_battery("BAT0".into(), &values, None).power_w,
            Some(24.0)
        );
    }

    #[test]
    fn an_actual_zero_is_not_replaced_with_a_stale_or_invented_rate() {
        let values = [("POWER_SUPPLY_POWER_NOW".into(), "0".into())]
            .into_iter()
            .collect();
        assert_eq!(
            parse_battery("BAT0".into(), &values, None).power_w,
            Some(0.0)
        );
    }

    #[test]
    fn excludes_peripherals_and_absent_batteries_and_keeps_missing_values() {
        let dir = tempfile::tempdir().unwrap();
        supply(
            dir.path(),
            "mouse",
            "POWER_SUPPLY_TYPE=Battery\nPOWER_SUPPLY_SCOPE=Device\n",
        );
        supply(
            dir.path(),
            "BAT1",
            "POWER_SUPPLY_TYPE=Battery\nPOWER_SUPPLY_PRESENT=0\n",
        );
        supply(
            dir.path(),
            "BAT0",
            "POWER_SUPPLY_TYPE=Battery\nPOWER_SUPPLY_CAPACITY=NaN\nPOWER_SUPPLY_POWER_NOW=invalid\n",
        );
        let readings = collect(dir.path()).unwrap();
        assert_eq!(readings.len(), 1);
        assert_eq!(readings[0].capacity_percent, None);
        assert_eq!(readings[0].power_w, None);
        assert_eq!(readings[0].external_power, None);
    }
}
