//! Measurements, budgets and the verdict of the performance gate.
//!
//! A workload produces a [`Samples`] series per metric. A budget names a
//! metric, a statistic of its distribution (median or 90th percentile) and
//! the largest acceptable value. Budgets are read from a file that only
//! people edit; nothing here writes it or adjusts a limit.

use std::{collections::BTreeMap, fmt, fs, path::Path, process::Command};

use serde::{Deserialize, Serialize};

/// What a metric's samples count.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Unit {
    Milliseconds,
    Mebibytes,
}

impl fmt::Display for Unit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Unit::Milliseconds => "ms",
            Unit::Mebibytes => "MiB",
        })
    }
}

/// A summary of one metric's samples. Percentiles use the nearest-rank
/// method, so every reported value is one that was actually measured.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Distribution {
    pub unit: Unit,
    pub count: usize,
    pub min: f64,
    pub p50: f64,
    pub p90: f64,
    pub max: f64,
    pub mean: f64,
    /// Every sample in the order it was taken.
    pub samples: Vec<f64>,
}

/// Samples of one metric.
#[derive(Clone, Debug, PartialEq)]
pub struct Samples {
    pub unit: Unit,
    pub values: Vec<f64>,
}

impl Samples {
    /// An empty series measured in `unit`.
    pub fn new(unit: Unit) -> Self {
        Self {
            unit,
            values: Vec::new(),
        }
    }

    /// Adds one measurement.
    pub fn push(&mut self, value: f64) {
        self.values.push(value);
    }

    /// Summarizes the samples; `None` when there are none.
    pub fn distribution(&self) -> Option<Distribution> {
        if self.values.is_empty() {
            return None;
        }
        let mut sorted = self.values.clone();
        sorted.sort_by(f64::total_cmp);
        let rank = |fraction: f64| {
            let index = (fraction * sorted.len() as f64).ceil() as usize;
            sorted[index.clamp(1, sorted.len()) - 1]
        };
        Some(Distribution {
            unit: self.unit,
            count: sorted.len(),
            min: sorted[0],
            p50: rank(0.5),
            p90: rank(0.9),
            max: sorted[sorted.len() - 1],
            mean: sorted.iter().sum::<f64>() / sorted.len() as f64,
            samples: self.values.clone(),
        })
    }
}

/// Which value of a distribution a budget limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Statistic {
    P50,
    P90,
    Max,
}

impl Statistic {
    fn of(self, distribution: &Distribution) -> f64 {
        match self {
            Statistic::P50 => distribution.p50,
            Statistic::P90 => distribution.p90,
            Statistic::Max => distribution.max,
        }
    }
}

/// The largest acceptable values for one metric.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Budget {
    pub unit: Unit,
    /// Limits by statistic, e.g. `p50 = 120`.
    pub limits: BTreeMap<Statistic, f64>,
    /// Why this limit is where it is: the measurements it was derived from.
    pub basis: String,
}

/// All budgets, by metric name.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Budgets {
    pub metric: BTreeMap<String, Budget>,
}

impl Budgets {
    /// Reads and validates the budget file: every budget needs at least one
    /// positive, finite limit and a stated basis.
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = fs::read_to_string(path)
            .map_err(|error| format!("cannot read budgets {}: {error}", path.display()))?;
        let budgets: Budgets = toml::from_str(&text)
            .map_err(|error| format!("invalid budgets {}: {error}", path.display()))?;
        for (name, budget) in &budgets.metric {
            if budget.limits.is_empty() {
                return Err(format!("budget `{name}` has no limit"));
            }
            if let Some((statistic, limit)) = budget
                .limits
                .iter()
                .find(|(_, limit)| !(limit.is_finite() && **limit > 0.0))
            {
                return Err(format!(
                    "budget `{name}` has an invalid {statistic:?} limit {limit}"
                ));
            }
        }
        Ok(budgets)
    }
}

/// The outcome for one metric.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Verdict {
    pub metric: String,
    pub distribution: Option<Distribution>,
    pub budget: Option<Budget>,
    /// Each limit that was exceeded, e.g. `p90 312.4 ms > 250 ms`.
    pub violations: Vec<String>,
    /// Why the metric cannot be judged, when it cannot.
    pub problem: Option<String>,
}

impl Verdict {
    /// Whether the metric was measured, budgeted and within every limit.
    pub fn passed(&self) -> bool {
        self.violations.is_empty() && self.problem.is_none()
    }
}

/// Compares `samples` with `budget`.
///
/// A metric without a budget, without samples, or measured in another unit
/// than its budget fails: an unchecked number must never look like a pass.
pub fn judge(metric: &str, samples: Option<&Samples>, budget: Option<&Budget>) -> Verdict {
    let distribution = samples.and_then(Samples::distribution);
    let mut verdict = Verdict {
        metric: metric.to_owned(),
        distribution: distribution.clone(),
        budget: budget.cloned(),
        violations: Vec::new(),
        problem: None,
    };
    match (distribution, budget) {
        (None, _) => verdict.problem = Some("no samples were measured".to_owned()),
        (Some(_), None) => verdict.problem = Some("no budget is defined".to_owned()),
        (Some(distribution), Some(budget)) if distribution.unit != budget.unit => {
            verdict.problem = Some(format!(
                "measured in {} but budgeted in {}",
                distribution.unit, budget.unit
            ));
        }
        (Some(distribution), Some(budget)) => {
            for (statistic, limit) in &budget.limits {
                let value = statistic.of(&distribution);
                if value > *limit {
                    verdict.violations.push(format!(
                        "{statistic:?} {value:.1} {} > {limit} {}",
                        budget.unit, budget.unit
                    ));
                }
            }
        }
    }
    verdict
}

/// Conditions a measurement was taken under.
#[derive(Clone, Debug, Serialize)]
pub struct Environment {
    pub cpu: String,
    pub logical_cpus: usize,
    pub memory_total_mib: u64,
    pub kernel: String,
    pub rustc: String,
    pub build_profile: &'static str,
    /// How the application was displayed.
    pub display: String,
    /// The GPU adapter the application's window reported, or `unknown` with
    /// the reason it could not be read.
    pub renderer: String,
    /// Vulkan driver manifests found on disk. Several may be installed; this
    /// says nothing about which one the window uses.
    pub installed_vulkan_drivers: Vec<String>,
    /// Load average at the start of the run, from `/proc/loadavg`.
    pub load_average: String,
}

impl Environment {
    /// Records this machine's conditions. `display` describes how the
    /// application under test is shown, `renderer` the adapter it reported.
    pub fn capture(display: String, renderer: String) -> Self {
        let read = |path: &str| fs::read_to_string(path).unwrap_or_default();
        let cpuinfo = read("/proc/cpuinfo");
        let meminfo = read("/proc/meminfo");
        let output = |program: &str, args: &[&str]| {
            Command::new(program)
                .args(args)
                .output()
                .ok()
                .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
                .unwrap_or_default()
        };
        let mut installed_vulkan_drivers: Vec<String> =
            ["/usr/share/vulkan/icd.d", "/etc/vulkan/icd.d"]
                .iter()
                .filter_map(|directory| fs::read_dir(directory).ok())
                .flatten()
                .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
                .collect();
        installed_vulkan_drivers.sort();
        Self {
            cpu: cpuinfo
                .lines()
                .find_map(|line| line.strip_prefix("model name"))
                .and_then(|rest| rest.split_once(':'))
                .map(|(_, name)| name.trim().to_owned())
                .unwrap_or_else(|| "unknown".to_owned()),
            logical_cpus: std::thread::available_parallelism().map_or(0, usize::from),
            memory_total_mib: meminfo
                .lines()
                .find_map(|line| line.strip_prefix("MemTotal:"))
                .and_then(|rest| {
                    rest.trim()
                        .trim_end_matches("kB")
                        .trim()
                        .parse::<u64>()
                        .ok()
                })
                .map_or(0, |kib| kib / 1024),
            kernel: read("/proc/sys/kernel/osrelease").trim().to_owned(),
            rustc: output("rustc", &["--version"]),
            build_profile: if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            },
            display,
            renderer,
            installed_vulkan_drivers,
            load_average: read("/proc/loadavg").trim().to_owned(),
        }
    }
}

/// Resident and peak resident memory of process `pid`, in MiB.
pub fn memory_of(pid: u32) -> Result<(f64, f64), String> {
    let status = fs::read_to_string(format!("/proc/{pid}/status"))
        .map_err(|error| format!("cannot read memory of process {pid}: {error}"))?;
    let field = |name: &str| {
        status
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .and_then(|rest| {
                rest.trim()
                    .trim_end_matches("kB")
                    .trim()
                    .parse::<f64>()
                    .ok()
            })
            .map(|kib| kib / 1024.0)
            .ok_or_else(|| format!("process {pid} reports no {name}"))
    };
    Ok((field("VmRSS:")?, field("VmHWM:")?))
}

/// A synthetic log in the format the Log Explorer reads: `lines` records,
/// the same for the same `seed`. Returns the text and its number of errors.
pub fn synthetic_log(lines: usize, seed: u64) -> (String, usize) {
    const COMPONENTS: [&str; 8] = [
        "auth.session",
        "auth.db",
        "api.gateway",
        "api.orders",
        "kernel.net",
        "kernel.fs",
        "billing.invoice",
        "search.index",
    ];
    const WORDS: [&str; 12] = [
        "request",
        "completed",
        "user",
        "timeout",
        "retry",
        "connection",
        "cache",
        "miss",
        "payment",
        "queued",
        "refused",
        "flushed",
    ];
    let mut state = seed.max(1);
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut text = String::with_capacity(lines * 80);
    let mut errors = 0;
    for line in 0..lines {
        let roll = next() % 100;
        let level = match roll {
            0..=2 => {
                errors += 1;
                "ERROR"
            }
            3..=9 => "WARN",
            10..=69 => "INFO",
            70..=89 => "DEBUG",
            _ => "TRACE",
        };
        let component = COMPONENTS[(next() % COMPONENTS.len() as u64) as usize];
        let seconds = line as u64;
        text.push_str(&format!(
            "2026-11-17T{:02}:{:02}:{:02}Z {level} {component}: {} {} {} id={}\n",
            seconds / 3600 % 24,
            seconds / 60 % 60,
            seconds % 60,
            WORDS[(next() % 12) as usize],
            WORDS[(next() % 12) as usize],
            WORDS[(next() % 12) as usize],
            next() % 100_000,
        ));
    }
    (text, errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples(values: &[f64]) -> Samples {
        Samples {
            unit: Unit::Milliseconds,
            values: values.to_vec(),
        }
    }

    fn budget(limits: &[(Statistic, f64)]) -> Budget {
        Budget {
            unit: Unit::Milliseconds,
            limits: limits.iter().copied().collect(),
            basis: "test".into(),
        }
    }

    #[test]
    fn percentiles_are_measured_values_by_nearest_rank() {
        let distribution = samples(&[5.0, 1.0, 4.0, 2.0, 3.0, 10.0, 6.0, 7.0, 8.0, 9.0])
            .distribution()
            .unwrap();
        assert_eq!(
            (
                distribution.min,
                distribution.p50,
                distribution.p90,
                distribution.max
            ),
            (1.0, 5.0, 9.0, 10.0)
        );
        assert_eq!(distribution.mean, 5.5);
        assert_eq!(distribution.samples[0], 5.0, "samples keep their order");
        let single = samples(&[7.0]).distribution().unwrap();
        assert_eq!((single.p50, single.p90), (7.0, 7.0));
        assert!(samples(&[]).distribution().is_none());
    }

    #[test]
    fn a_value_over_any_limit_is_a_violation() {
        let measured = samples(&[10.0, 11.0, 12.0, 13.0, 100.0]);
        let within = budget(&[(Statistic::P50, 12.0), (Statistic::P90, 100.0)]);
        assert!(judge("m", Some(&measured), Some(&within)).passed());

        let tight = budget(&[(Statistic::P50, 11.9), (Statistic::P90, 99.0)]);
        let verdict = judge("m", Some(&measured), Some(&tight));
        assert!(!verdict.passed());
        assert_eq!(
            verdict.violations,
            ["P50 12.0 ms > 11.9 ms", "P90 100.0 ms > 99 ms"]
        );
    }

    #[test]
    fn missing_samples_budget_or_matching_unit_fail_instead_of_passing() {
        let measured = samples(&[1.0]);
        let ok = budget(&[(Statistic::P50, 5.0)]);
        assert!(!judge("m", None, Some(&ok)).passed());
        assert!(!judge("m", Some(&samples(&[])), Some(&ok)).passed());
        assert!(!judge("m", Some(&measured), None).passed());
        let mut other_unit = ok.clone();
        other_unit.unit = Unit::Mebibytes;
        let verdict = judge("m", Some(&measured), Some(&other_unit));
        assert_eq!(
            verdict.problem.as_deref(),
            Some("measured in ms but budgeted in MiB")
        );
    }

    #[test]
    fn budget_file_is_validated() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("budgets.toml");
        let write = |text: &str| fs::write(&path, text).unwrap();

        write(
            "[metric.startup]\nunit = \"milliseconds\"\nbasis = \"x\"\nlimits = { p50 = 900, p90 = 1500 }\n",
        );
        let budgets = Budgets::load(&path).unwrap();
        assert_eq!(budgets.metric["startup"].limits[&Statistic::P90], 1500.0);

        write("[metric.startup]\nunit = \"milliseconds\"\nbasis = \"x\"\nlimits = {}\n");
        assert!(Budgets::load(&path).unwrap_err().contains("has no limit"));
        write("[metric.startup]\nunit = \"milliseconds\"\nbasis = \"x\"\nlimits = { p50 = 0 }\n");
        assert!(Budgets::load(&path).unwrap_err().contains("invalid"));
        write("[metric.startup]\nunit = \"seconds\"\nbasis = \"x\"\nlimits = { p50 = 1 }\n");
        assert!(Budgets::load(&path).is_err());
        write("[metric.startup]\nunit = \"milliseconds\"\nlimits = { p50 = 1 }\n");
        assert!(
            Budgets::load(&path).is_err(),
            "a budget must state its basis"
        );
    }

    #[test]
    fn synthetic_log_is_deterministic_and_parses_completely() {
        let (text, errors) = synthetic_log(2000, 7);
        assert_eq!(synthetic_log(2000, 7), (text.clone(), errors));
        assert_ne!(synthetic_log(2000, 8).0, text);
        let records = retrowork_logs::record::parse_log(&text);
        assert_eq!(records.len(), 2000);
        assert!(
            records
                .iter()
                .all(|record| record.level != retrowork_logs::record::Level::Unknown)
        );
        let counted = records
            .iter()
            .filter(|record| record.level == retrowork_logs::record::Level::Error)
            .count();
        assert_eq!(counted, errors);
        assert!(errors > 0);
    }

    #[test]
    fn memory_of_this_process_is_plausible() {
        let (rss, peak) = memory_of(std::process::id()).unwrap();
        assert!(rss > 0.0 && peak >= rss, "{rss} {peak}");
        assert!(memory_of(u32::MAX).is_err());
    }
}
