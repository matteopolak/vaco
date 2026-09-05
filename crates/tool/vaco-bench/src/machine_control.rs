//! Fail-closed checks for the dedicated Linux benchmark runners.
//!
//! The macro benchmark protocol needs a controlled machine before a result can
//! become a regression baseline. This module only observes state: changing a
//! governor, CPU topology, or IRQ routing is an administrator action made
//! outside the benchmark process. Missing privileges therefore produce a
//! failed preflight, never an optimistic guess.

#[cfg(any(test, target_os = "linux"))]
use std::collections::BTreeSet;
#[cfg(any(test, target_os = "linux"))]
use std::fs;
#[cfg(any(test, target_os = "linux"))]
use std::path::Path;

/// One machine-control condition and the observation made for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineCheck {
    /// Stable name used in diagnostics and automation.
    pub name: &'static str,
    /// Whether this condition must pass before a result can gate a regression.
    pub required: bool,
    /// Whether the observed state meets the condition.
    pub passed: bool,
    /// Human-readable evidence or the reason validation was impossible.
    pub detail: String,
}

/// Result of checking the host required for a gated macro benchmark.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineControlReport {
    checks: Vec<MachineCheck>,
}

impl MachineControlReport {
    /// All observed checks, including non-gating recorded context.
    #[must_use]
    pub fn checks(&self) -> &[MachineCheck] {
        &self.checks
    }

    /// True only when every required control is verified.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.checks
            .iter()
            .filter(|check| check.required)
            .all(|check| check.passed)
    }

    /// A concise failure explanation suitable for a command-line error.
    #[must_use]
    pub fn failure_summary(&self) -> String {
        self.checks
            .iter()
            .filter(|check| check.required && !check.passed)
            .map(|check| format!("{}: {}", check.name, check.detail))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// Verify the controls required by plan 12 §4.4 for this process.
///
/// Linux is the only supported platform because its documented preconditions
/// are Linux kernel controls. Other hosts fail closed rather than treating an
/// elapsed-time measurement as a gating-quality result.
#[must_use]
pub fn verify_machine_control() -> MachineControlReport {
    #[cfg(target_os = "linux")]
    {
        verify_linux_root(Path::new("/"))
    }
    #[cfg(not(target_os = "linux"))]
    {
        MachineControlReport {
            checks: vec![MachineCheck {
                name: "platform",
                required: true,
                passed: false,
                detail: "gated macro benchmarks require a controlled Linux reference runner"
                    .to_owned(),
            }],
        }
    }
}

/// Inspect a Linux-like filesystem tree for fixture-based tests.
#[must_use]
#[cfg(any(test, target_os = "linux"))]
pub(crate) fn verify_linux_root(root: &Path) -> MachineControlReport {
    let mut checks = Vec::new();
    let online = read_cpu_list(root, "sys/devices/system/cpu/online");
    let allowed = read_allowed_cpu(root);

    match &online {
        Ok(cpus) if !cpus.is_empty() => {
            let mut bad = Vec::new();
            for cpu in cpus {
                let path = format!("sys/devices/system/cpu/cpu{cpu}/cpufreq/scaling_governor");
                match read(root, &path) {
                    Ok(value) if value == "performance" => {}
                    Ok(value) => bad.push(format!("cpu{cpu}={value}")),
                    Err(error) => bad.push(format!("cpu{cpu}: {error}")),
                }
            }
            checks.push(required_check(
                "governor",
                bad.is_empty(),
                if bad.is_empty() {
                    "all online CPUs report performance".to_owned()
                } else {
                    format!("require performance governor ({})", bad.join(", "))
                },
            ));
        }
        Ok(_) => checks.push(required_check("governor", false, "no online CPUs reported")),
        Err(error) => checks.push(required_check("governor", false, error)),
    }

    checks.push(check_turbo(root));
    checks.push(check_smt(root));

    let pinned_cpu = match allowed {
        Ok(cpus) if cpus.len() == 1 => {
            if let Some(cpu) = cpus.iter().next().copied() {
                checks.push(required_check(
                    "affinity",
                    true,
                    format!("process allowed CPU list is cpu{cpu}"),
                ));
                Some(cpu)
            } else {
                checks.push(required_check("affinity", false, "no allowed CPU reported"));
                None
            }
        }
        Ok(cpus) => {
            checks.push(required_check(
                "affinity",
                false,
                format!(
                    "process must be pinned to exactly one CPU, got {}",
                    render_cpus(&cpus)
                ),
            ));
            None
        }
        Err(error) => {
            checks.push(required_check("affinity", false, error));
            None
        }
    };

    checks.push(check_nohz_full(root, pinned_cpu));
    checks.push(check_aslr(root));
    checks.push(check_thp(root));
    checks.push(check_irq_affinity(root, pinned_cpu));
    checks.push(record_thermal(root));
    checks.push(record_kernel(root));

    MachineControlReport { checks }
}

#[cfg(any(test, target_os = "linux"))]
fn required_check(name: &'static str, passed: bool, detail: impl Into<String>) -> MachineCheck {
    MachineCheck {
        name,
        required: true,
        passed,
        detail: detail.into(),
    }
}

#[cfg(any(test, target_os = "linux"))]
fn recorded_check(name: &'static str, passed: bool, detail: impl Into<String>) -> MachineCheck {
    MachineCheck {
        name,
        required: false,
        passed,
        detail: detail.into(),
    }
}

#[cfg(any(test, target_os = "linux"))]
fn check_turbo(root: &Path) -> MachineCheck {
    let intel = read(root, "sys/devices/system/cpu/intel_pstate/no_turbo");
    let amd = read(root, "sys/devices/system/cpu/cpufreq/boost");
    match (intel, amd) {
        (Ok(value), _) if value == "1" => required_check("turbo", true, "intel_pstate no_turbo=1"),
        (_, Ok(value)) if value == "0" => required_check("turbo", true, "cpufreq boost=0"),
        (Ok(value), _) => required_check(
            "turbo",
            false,
            format!("intel_pstate no_turbo={value}, require 1"),
        ),
        (_, Ok(value)) => {
            required_check("turbo", false, format!("cpufreq boost={value}, require 0"))
        }
        (Err(intel), Err(amd)) => required_check(
            "turbo",
            false,
            format!("cannot verify turbo disablement (intel: {intel}; cpufreq: {amd})"),
        ),
    }
}

#[cfg(any(test, target_os = "linux"))]
fn check_smt(root: &Path) -> MachineCheck {
    match read(root, "sys/devices/system/cpu/smt/control") {
        Ok(value) if value == "off" || value == "forceoff" => {
            required_check("smt", true, format!("SMT control={value}"))
        }
        Ok(value) => required_check("smt", false, format!("SMT control={value}, require off")),
        Err(error) => required_check("smt", false, error),
    }
}

#[cfg(any(test, target_os = "linux"))]
fn check_nohz_full(root: &Path, pinned_cpu: Option<u32>) -> MachineCheck {
    let Some(cpu) = pinned_cpu else {
        return required_check(
            "nohz_full",
            false,
            "cannot validate without a single pinned CPU",
        );
    };
    match read_cpu_list(root, "sys/devices/system/cpu/nohz_full") {
        Ok(cpus) if cpus.contains(&cpu) => {
            required_check("nohz_full", true, format!("cpu{cpu} is nohz_full"))
        }
        Ok(cpus) => required_check(
            "nohz_full",
            false,
            format!("cpu{cpu} is absent from nohz_full ({})", render_cpus(&cpus)),
        ),
        Err(error) => required_check("nohz_full", false, error),
    }
}

#[cfg(any(test, target_os = "linux"))]
fn check_aslr(root: &Path) -> MachineCheck {
    match read(root, "proc/sys/kernel/randomize_va_space") {
        Ok(value) if value == "0" => required_check("aslr", true, "randomize_va_space=0"),
        Ok(value) => required_check(
            "aslr",
            false,
            format!("randomize_va_space={value}, require 0"),
        ),
        Err(error) => required_check("aslr", false, error),
    }
}

#[cfg(any(test, target_os = "linux"))]
fn check_thp(root: &Path) -> MachineCheck {
    match read(root, "sys/kernel/mm/transparent_hugepage/enabled") {
        Ok(value) => recorded_check(
            "thp",
            value.split_whitespace().any(|choice| choice == "[madvise]"),
            format!("transparent huge pages: {value}"),
        ),
        Err(error) => recorded_check("thp", false, error),
    }
}

#[cfg(any(test, target_os = "linux"))]
fn check_irq_affinity(root: &Path, pinned_cpu: Option<u32>) -> MachineCheck {
    let Some(cpu) = pinned_cpu else {
        return required_check(
            "irq_affinity",
            false,
            "cannot validate without a single pinned CPU",
        );
    };
    let directory = root.join("proc/irq");
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) => {
            return required_check(
                "irq_affinity",
                false,
                format!("{}: {error}", directory.display()),
            );
        }
    };
    let mut seen = 0usize;
    let mut routed = Vec::new();
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().parse::<u32>().is_err() {
            continue;
        }
        let path = entry.path().join("smp_affinity_list");
        let Ok(value) = fs::read_to_string(&path) else {
            continue;
        };
        seen += 1;
        if parse_cpu_list(value.trim()).is_ok_and(|cpus| cpus.contains(&cpu)) {
            routed.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    if seen == 0 {
        required_check("irq_affinity", false, "no readable IRQ affinity lists")
    } else if routed.is_empty() {
        required_check("irq_affinity", true, format!("{seen} IRQs avoid cpu{cpu}"))
    } else {
        required_check(
            "irq_affinity",
            false,
            format!("IRQs {} are routed to cpu{cpu}", routed.join(", ")),
        )
    }
}

#[cfg(any(test, target_os = "linux"))]
fn record_thermal(root: &Path) -> MachineCheck {
    let directory = root.join("sys/class/thermal");
    let Ok(entries) = fs::read_dir(&directory) else {
        return recorded_check("thermal", false, "thermal zones unavailable");
    };
    let readings: Vec<_> = entries
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("thermal_zone")
        })
        .filter_map(|entry| fs::read_to_string(entry.path().join("temp")).ok())
        .map(|value| value.trim().to_owned())
        .collect();
    if readings.is_empty() {
        recorded_check("thermal", false, "no readable thermal zones")
    } else {
        recorded_check(
            "thermal",
            true,
            format!("zone temperatures: {}", readings.join(", ")),
        )
    }
}

#[cfg(any(test, target_os = "linux"))]
fn record_kernel(root: &Path) -> MachineCheck {
    match read(root, "proc/sys/kernel/osrelease") {
        Ok(value) => recorded_check("kernel", true, value),
        Err(error) => recorded_check("kernel", false, error),
    }
}

#[cfg(any(test, target_os = "linux"))]
fn read_allowed_cpu(root: &Path) -> Result<BTreeSet<u32>, String> {
    let status = read(root, "proc/self/status")?;
    let Some(value) = status.lines().find_map(|line| {
        line.strip_prefix("Cpus_allowed_list:\t")
            .or_else(|| line.strip_prefix("Cpus_allowed_list:"))
    }) else {
        return Err("proc/self/status has no Cpus_allowed_list".to_owned());
    };
    parse_cpu_list(value.trim())
}

#[cfg(any(test, target_os = "linux"))]
fn read_cpu_list(root: &Path, relative: &str) -> Result<BTreeSet<u32>, String> {
    parse_cpu_list(&read(root, relative)?)
}

#[cfg(any(test, target_os = "linux"))]
fn read(root: &Path, relative: &str) -> Result<String, String> {
    let path = root.join(relative);
    fs::read_to_string(&path)
        .map(|value| value.trim().to_owned())
        .map_err(|error| format!("{}: {error}", path.display()))
}

#[cfg(any(test, target_os = "linux"))]
fn parse_cpu_list(value: &str) -> Result<BTreeSet<u32>, String> {
    let value = value.trim();
    if value.is_empty() || value == "(null)" {
        return Ok(BTreeSet::new());
    }
    let mut cpus = BTreeSet::new();
    for component in value.split(',') {
        let (start, end) = if let Some((start, end)) = component.split_once('-') {
            (parse_cpu(start)?, parse_cpu(end)?)
        } else {
                let cpu = parse_cpu(component)?;
                (cpu, cpu)
        };
        if start > end {
            return Err(format!("invalid descending CPU range {component:?}"));
        }
        for cpu in start..=end {
            cpus.insert(cpu);
        }
    }
    Ok(cpus)
}

#[cfg(any(test, target_os = "linux"))]
fn parse_cpu(value: &str) -> Result<u32, String> {
    value
        .trim()
        .parse()
        .map_err(|_| format!("invalid CPU list component {value:?}"))
}

#[cfg(any(test, target_os = "linux"))]
fn render_cpus(cpus: &BTreeSet<u32>) -> String {
    cpus.iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "test fixture setup uses direct diagnostics")]
mod tests {
    use super::{parse_cpu_list, verify_linux_root};
    use std::fs;
    use std::path::Path;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn fixture_root(label: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("vaco-machine-control-{label}-{nonce}"));
        fs::create_dir_all(&root).expect("create fixture root");
        root
    }

    fn write(root: &Path, relative: &str, value: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().expect("fixture file has parent")).expect("create parent");
        fs::write(path, value).expect("write fixture");
    }

    fn controlled_fixture(root: &Path) {
        write(root, "sys/devices/system/cpu/online", "0-1\n");
        write(
            root,
            "sys/devices/system/cpu/cpu0/cpufreq/scaling_governor",
            "performance\n",
        );
        write(
            root,
            "sys/devices/system/cpu/cpu1/cpufreq/scaling_governor",
            "performance\n",
        );
        write(root, "sys/devices/system/cpu/intel_pstate/no_turbo", "1\n");
        write(root, "sys/devices/system/cpu/smt/control", "off\n");
        write(
            root,
            "proc/self/status",
            "Name:\tvaco-bench\nCpus_allowed_list:\t1\n",
        );
        write(root, "sys/devices/system/cpu/nohz_full", "1\n");
        write(root, "proc/sys/kernel/randomize_va_space", "0\n");
        write(
            root,
            "sys/kernel/mm/transparent_hugepage/enabled",
            "always [madvise] never\n",
        );
        write(root, "proc/irq/1/smp_affinity_list", "0\n");
        write(root, "sys/class/thermal/thermal_zone0/temp", "42000\n");
        write(root, "proc/sys/kernel/osrelease", "test-kernel\n");
    }

    #[test]
    fn parses_cpu_lists_and_rejects_descending_ranges() {
        assert_eq!(parse_cpu_list("0-2,5").expect("valid list").len(), 4);
        assert!(parse_cpu_list("3-1").is_err());
        assert!(parse_cpu_list("x").is_err());
    }

    #[test]
    fn controlled_linux_fixture_is_ready() {
        let root = fixture_root("ready");
        controlled_fixture(&root);
        let report = verify_linux_root(&root);
        assert!(report.is_ready(), "{}", report.failure_summary());
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn missing_control_fails_closed_with_its_name() {
        let root = fixture_root("fail");
        controlled_fixture(&root);
        write(&root, "proc/sys/kernel/randomize_va_space", "2\n");
        let report = verify_linux_root(&root);
        assert!(!report.is_ready());
        assert!(report.failure_summary().contains("aslr"));
        fs::remove_dir_all(root).expect("remove fixture");
    }
}
