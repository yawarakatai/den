use std::{collections::BTreeSet, fs, path::Path};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PowerSettings {
    pub platform: Option<String>,
    pub driver: Option<String>,
    pub pstate: Option<String>,
    pub governor: Option<String>,
    pub epp: Option<String>,
}

impl PowerSettings {
    pub fn collect(sys: &Path) -> Self {
        let cpu = sys.join("devices/system/cpu");
        let policies: Vec<_> = fs::read_dir(cpu.join("cpufreq"))
            .ok()
            .and_then(|entries| entries.collect::<Result<Vec<_>, _>>().ok())
            .unwrap_or_default()
            .into_iter()
            .filter(|entry| {
                entry.file_name().to_str().is_some_and(|name| {
                    name.strip_prefix("policy")
                        .is_some_and(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
                })
            })
            .map(|entry| entry.path())
            .collect();
        let common = |field: &str| {
            if policies.is_empty() {
                return None;
            }
            let values: BTreeSet<_> = policies
                .iter()
                .map(|path| read_value(&path.join(field)))
                .collect::<Option<_>>()?;
            if values.len() == 1 {
                values.into_iter().next()
            } else {
                Some("Mixed".into())
            }
        };
        let driver = common("scaling_driver");
        let pstate = match driver.as_deref() {
            Some("intel_pstate" | "intel_cpufreq") => read_value(&cpu.join("intel_pstate/status")),
            Some(name) if name.starts_with("amd-pstate") => {
                read_value(&cpu.join("amd_pstate/status"))
            }
            _ => None,
        };
        Self {
            platform: read_value(&sys.join("firmware/acpi/platform_profile")),
            driver,
            pstate,
            governor: common("scaling_governor"),
            epp: common("energy_performance_preference"),
        }
    }
}

// A custom power_supply fixture must never accidentally read the host's CPU
// settings. Standard .../class/power_supply trees share a matching sysfs root.
pub fn sys_root(supply: &Path) -> Option<&Path> {
    let class = supply.parent()?;
    (supply.file_name()? == "power_supply" && class.file_name()? == "class")
        .then(|| class.parent())
        .flatten()
}

fn read_value(path: &Path) -> Option<String> {
    let value = fs::read_to_string(path).ok()?;
    let value = value.trim();
    (!value.is_empty()
        && value.len() <= 128
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')))
    .then(|| value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(root: &Path, path: &str, value: &str) {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, value).unwrap();
    }

    #[test]
    fn reads_platform_and_actual_cpu_controls_without_guessing_a_manager() {
        let dir = tempfile::tempdir().unwrap();
        put(dir.path(), "firmware/acpi/platform_profile", "balanced\n");
        for policy in [0, 4] {
            for (name, value) in [
                ("scaling_driver", "intel_pstate"),
                ("scaling_governor", "powersave"),
                ("energy_performance_preference", "balance_power"),
            ] {
                put(
                    dir.path(),
                    &format!("devices/system/cpu/cpufreq/policy{policy}/{name}"),
                    value,
                );
            }
        }
        put(
            dir.path(),
            "devices/system/cpu/intel_pstate/status",
            "active\n",
        );
        let settings = PowerSettings::collect(dir.path());
        assert_eq!(settings.platform.as_deref(), Some("balanced"));
        assert_eq!(settings.driver.as_deref(), Some("intel_pstate"));
        assert_eq!(settings.pstate.as_deref(), Some("active"));
        assert_eq!(settings.governor.as_deref(), Some("powersave"));
        assert_eq!(settings.epp.as_deref(), Some("balance_power"));
    }

    #[test]
    fn mixed_and_incomplete_policies_are_not_reported_as_one_uniform_setting() {
        let dir = tempfile::tempdir().unwrap();
        put(
            dir.path(),
            "devices/system/cpu/cpufreq/policy0/scaling_governor",
            "powersave",
        );
        put(
            dir.path(),
            "devices/system/cpu/cpufreq/policy1/scaling_governor",
            "performance",
        );
        put(
            dir.path(),
            "devices/system/cpu/cpufreq/policy0/energy_performance_preference",
            "power",
        );
        let settings = PowerSettings::collect(dir.path());
        assert_eq!(settings.governor.as_deref(), Some("Mixed"));
        assert_eq!(settings.epp, None);
    }

    #[test]
    fn amd_pstate_and_unsupported_systems_are_handled() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(PowerSettings::collect(dir.path()), PowerSettings::default());
        put(
            dir.path(),
            "devices/system/cpu/cpufreq/policy0/scaling_driver",
            "amd-pstate-epp",
        );
        put(dir.path(), "devices/system/cpu/amd_pstate/status", "active");
        put(dir.path(), "firmware/acpi/platform_profile", "\x1b[31mbad");
        let settings = PowerSettings::collect(dir.path());
        assert_eq!(settings.pstate.as_deref(), Some("active"));
        assert_eq!(settings.platform, None);
    }

    #[test]
    fn fixture_roots_are_isolated() {
        assert_eq!(
            sys_root(Path::new("/sys/class/power_supply")),
            Some(Path::new("/sys"))
        );
        assert_eq!(
            sys_root(Path::new("fixtures/sys/class/power_supply")),
            Some(Path::new("fixtures/sys"))
        );
        assert_eq!(sys_root(Path::new("fixtures/power_supply")), None);
    }
}
