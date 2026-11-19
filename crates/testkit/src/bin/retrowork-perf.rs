//! Performance gate: measures an optimized build against fixed budgets.
//!
//! Workloads, all deterministic:
//! - startup of the real application until its window exists and has
//!   reported a prepared frame with elements, and its memory;
//! - opening and filtering a generated 200,000-record log in the real window,
//!   driven by X11 input, and the memory that takes;
//! - time from X11 input to observing the changed semantic state, for small
//!   interactions with that log open;
//! - parsing and filtering the same log without a window;
//! - building the platform with every module, and the overhead of the
//!   out-of-process plugin protocol, cold and warm;
//! - discovering a generated 40-package Cargo workspace, without and with
//!   the window, and streaming 100,000 lines of tool output.
//!
//! Window timings end when the automation socket reports the expected state
//! in the elements registered while a frame was prepared (layout and
//! prepaint). They include the polling and the observation round trip and
//! exclude painting, GPU work and presentation, so they are neither the
//! latency a user sees nor a bound on it. They are coarse regression
//! baselines, not product latency targets.
//!
//! A negative control measures a plugin that is deliberately slow; the gate
//! fails unless that measurement is rejected by the plugin budget.
//!
//! Exit status: 0 when every metric is within budget and the negative control
//! was rejected; 1 otherwise; 2 when the environment cannot run the
//! measurements or the build is not optimized.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use retrowork_cargo::CargoProvider;
use retrowork_demo::{Options, build_platform};
use retrowork_logs::{query::Query, record::parse_log};
use retrowork_platform::{
    CommandId, ModuleId,
    external::ProcessHost,
    task::{self, ProcessSpec, TaskEvent},
};
use retrowork_projects::{DISCOVERY_TIMEOUT, MAX_DISCOVERY_BYTES, ProjectProvider};
use retrowork_testkit::{
    driver::{Outcome, UiDriver},
    e2e::{App, InputBackend, Setup, XServer},
    perf::{Budgets, Environment, Samples, Unit, Verdict, judge, memory_of, synthetic_log},
    scenarios::{BOTTOM_OUTPUT, BOTTOM_RECORD, BOTTOM_STATISTICS, PROJECTS_TAB},
};
use retrowork_workbench::semantics::UiSnapshot;
use serde::Serialize;
use serde_json::{Value, json};

const USAGE: &str = "\
Measures an optimized build of RetroWork against the budgets in perf/budgets.toml.

Usage: retrowork-perf [OPTIONS]

Options:
  --budgets <FILE>     Budget file [default: perf/budgets.toml]
  --artifacts <DIR>    Where the report is written [default: target/perf]
  --packages <DIR>     Sample packages [default: ./packages]
  -h, --help           Print this help

Environment:
  RETROWORK_E2E_DISPLAY   Use this X display instead of starting a nested Xwayland
";

const LARGE_LOG_LINES: usize = 200_000;
const LARGE_LOG_SEED: u64 = 20_261_117;
const LARGE_SOURCE: &str = "view.logs.sources.node.source:large";
const PLUGIN_EXPORTED_RECORDS: usize = 5_000;
/// Longest any single measured interaction may take before the run is
/// declared broken rather than slow.
const GIVE_UP: Duration = Duration::from_secs(60);

#[derive(Serialize)]
struct Report {
    schema: u32,
    passed: bool,
    environment: Environment,
    workload: Value,
    verdicts: Vec<Verdict>,
    negative_control: NegativeControl,
}

#[derive(Serialize)]
struct NegativeControl {
    description: &'static str,
    verdict: Verdict,
    /// Whether the gate rejected the deliberately slow case, as it must.
    rejected: bool,
}

fn main() -> ExitCode {
    let mut budgets = PathBuf::from("perf/budgets.toml");
    let mut artifacts = PathBuf::from("target/perf");
    let mut packages = PathBuf::from("packages");
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |name: &str| match args.next() {
            Some(value) => Ok(PathBuf::from(value)),
            None => Err(format!("`{name}` needs a value")),
        };
        let parsed = match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "--budgets" => value("--budgets").map(|path| budgets = path),
            "--artifacts" => value("--artifacts").map(|path| artifacts = path),
            "--packages" => value("--packages").map(|path| packages = path),
            other => Err(format!("unknown argument `{other}`")),
        };
        if let Err(message) = parsed {
            eprintln!("retrowork-perf: {message}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    }
    if cfg!(debug_assertions) {
        eprintln!(
            "retrowork-perf: this is a debug build; performance is only measured with --release"
        );
        return ExitCode::from(2);
    }
    match run(&budgets, &artifacts, &packages) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(message) => {
            eprintln!("retrowork-perf: cannot measure: {message}");
            ExitCode::from(2)
        }
    }
}

fn io<T>(result: std::io::Result<T>, what: &str) -> Outcome<T> {
    result.map_err(|error| format!("{what}: {error}"))
}

fn copy_dir(from: &Path, to: &Path) -> Outcome {
    io(fs::create_dir_all(to), "create directory")?;
    for entry in io(fs::read_dir(from), &format!("read {}", from.display()))? {
        let entry = io(entry, "read directory entry")?;
        let target = to.join(entry.file_name());
        if io(entry.file_type(), "file type")?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            io(fs::copy(entry.path(), &target), "copy file")?;
        }
    }
    Ok(())
}

/// The sample packages plus one that contributes the generated large log.
fn prepare_packages(samples: &Path, into: &Path, log: &str) -> Outcome {
    copy_dir(samples, into)?;
    let large = into.join("perf-large");
    io(fs::create_dir_all(&large), "create package")?;
    io(fs::write(large.join("large.log"), log), "write large log")?;
    io(
        fs::write(
            large.join("package.toml"),
            "id = \"perf.large\"\nname = \"Large log\"\nversion = \"0.1.0\"\nkind = \"declarative\"\nrequires-api = \"^0.1\"\n\n[[dependency]]\nid = \"retrowork.logs\"\nversion = \"^0.1\"\n\n[[extension]]\npoint = \"logs.sources\"\ndata = { id = \"large\", title = \"large.log\", group = \"perf\", path = \"large.log\" }\n",
        ),
        "write package manifest",
    )
}

fn run(budgets_path: &Path, artifacts: &Path, samples: &Path) -> Outcome<bool> {
    let budgets = Budgets::load(budgets_path)?;
    let binaries = io(std::env::current_exe(), "locate this executable")?
        .parent()
        .map(Path::to_owned)
        .ok_or("this executable has no parent directory")?;
    let demo = binaries.join("retrowork-demo");
    let plugin = binaries.join("retrowork-plugin-sample");
    for needed in [&demo, &plugin] {
        if !needed.is_file() {
            return Err(format!(
                "{} is not built; run `cargo build --release -p retrowork-demo -p retrowork-plugin-sample`",
                needed.display()
            ));
        }
    }
    if artifacts.exists() {
        io(fs::remove_dir_all(artifacts), "clear old results")?;
    }
    io(
        fs::create_dir_all(artifacts),
        "create the results directory",
    )?;
    let artifacts = io(fs::canonicalize(artifacts), "results directory")?;

    let (log, errors) = synthetic_log(LARGE_LOG_LINES, LARGE_LOG_SEED);
    let packages = artifacts.join("packages");
    prepare_packages(samples, &packages, &log)?;
    let projects = artifacts.join("projects");
    generate_workspace(&projects.join(LARGE_WORKSPACE), LARGE_WORKSPACE_UNITS)?;
    generate_workspace(&projects.join(SMALL_WORKSPACE), 1)?;

    let sockets = std::env::temp_dir().join(format!("retrowork-perf-{}", std::process::id()));
    let _ = fs::remove_dir_all(&sockets);
    io(fs::create_dir(&sockets), "create socket directory")?;

    let server = XServer::start(&artifacts)?;
    // Captured before any measurement, so the load average is the load the
    // run started under; the renderer is filled in once a window reports it.
    let mut environment = Environment::capture(
        format!(
            "{} X display {}, window 1200x780, input XTEST",
            server.kind, server.display
        ),
        "unknown: no window has reported it yet".to_owned(),
    );
    println!(
        "retrowork-perf: {} ({} CPUs), {}, load {}",
        environment.cpu, environment.logical_cpus, environment.display, environment.load_average
    );
    let setup = Setup {
        demo,
        packages: packages.clone(),
        input: InputBackend::XTest,
        sockets: sockets.clone(),
        fixtures: artifacts.join("projects"),
    };

    let mut metrics: BTreeMap<String, Samples> = BTreeMap::new();
    println!("  measuring startup");
    measure_startup(&server, &setup, &artifacts, &mut metrics)?;
    println!("  measuring the large log in the window");
    environment.renderer = measure_window(&server, &setup, &artifacts, errors, &mut metrics)?;
    println!("  renderer: {}", environment.renderer);
    println!("  measuring log processing");
    measure_processing(&log, &mut metrics);
    println!("  measuring project discovery and tool output");
    measure_projects(&projects.join(LARGE_WORKSPACE), &mut metrics)?;
    println!("  measuring platform and plugin overhead");
    measure_platform_and_plugins(&packages, &plugin, &log, &mut metrics)?;
    println!("  measuring the deliberately slow plugin");
    let slow = measure_slow_plugin(&artifacts, &log)?;
    let _ = fs::remove_dir_all(&sockets);

    let mut names: Vec<&String> = budgets.metric.keys().chain(metrics.keys()).collect();
    names.sort();
    names.dedup();
    let verdicts: Vec<Verdict> = names
        .into_iter()
        .map(|name| judge(name, metrics.get(name), budgets.metric.get(name)))
        .collect();
    let control = judge(
        "plugin_round_trip",
        Some(&slow),
        budgets.metric.get("plugin_round_trip"),
    );
    let rejected = !control.violations.is_empty();
    let passed = rejected && verdicts.iter().all(Verdict::passed);

    print_table(&verdicts);
    println!(
        "  {} negative control: a plugin that sleeps 300 ms per request {}",
        if rejected { "PASS" } else { "FAIL" },
        if rejected {
            format!("is rejected ({})", control.violations.join("; "))
        } else {
            "was NOT rejected by the plugin budget".to_owned()
        }
    );

    let report = Report {
        schema: 1,
        passed,
        environment,
        workload: json!({
            "large_log_lines": LARGE_LOG_LINES,
            "large_log_seed": LARGE_LOG_SEED,
            "large_log_errors": errors,
            "large_log_bytes": log.len(),
            "plugin_exported_records": PLUGIN_EXPORTED_RECORDS,
            "build_profile": "release",
        }),
        verdicts,
        negative_control: NegativeControl {
            description: "process plugin that sleeps 300 ms before every reply, judged by the plugin_round_trip budget",
            verdict: control,
            rejected,
        },
    };
    let path = artifacts.join("report.json");
    io(
        fs::write(
            &path,
            serde_json::to_vec_pretty(&report).map_err(|error| error.to_string())?,
        ),
        "write the report",
    )?;
    println!(
        "retrowork-perf: {}; report: {}",
        if passed { "within budget" } else { "FAILED" },
        path.display()
    );
    Ok(passed)
}

fn print_table(verdicts: &[Verdict]) {
    println!(
        "  {:<26} {:>5} {:>9} {:>9} {:>9} {:>9}  limits",
        "metric", "n", "min", "p50", "p90", "max"
    );
    for verdict in verdicts {
        let status = if verdict.passed() { "ok  " } else { "FAIL" };
        let limits = verdict.budget.as_ref().map_or("none".to_owned(), |budget| {
            budget
                .limits
                .iter()
                .map(|(statistic, limit)| format!("{statistic:?}<={limit}"))
                .collect::<Vec<_>>()
                .join(" ")
        });
        match &verdict.distribution {
            Some(d) => println!(
                "{status} {:<26} {:>5} {:>9.1} {:>9.1} {:>9.1} {:>9.1}  {limits} {}",
                verdict.metric, d.count, d.min, d.p50, d.p90, d.max, d.unit
            ),
            None => println!("{status} {:<26} no samples", verdict.metric),
        }
        for violation in &verdict.violations {
            println!("       over budget: {violation}");
        }
        if let Some(problem) = &verdict.problem {
            println!("       {problem}");
        }
    }
}

fn record(metrics: &mut BTreeMap<String, Samples>, name: &str, unit: Unit, value: f64) {
    metrics
        .entry(name.to_owned())
        .or_insert_with(|| Samples::new(unit))
        .push(value);
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// Fresh application starts; the first is a warm-up of disk caches.
fn measure_startup(
    server: &XServer,
    setup: &Setup,
    artifacts: &Path,
    metrics: &mut BTreeMap<String, Samples>,
) -> Outcome {
    for run in 0..8 {
        let directory = artifacts.join(format!("startup-{run}"));
        let mut app = App::launch(server, setup, &directory.join("state"), &directory, "app")?;
        // One round trip so the first frame's work has settled.
        app.snapshot()?;
        let (rss, _) = memory_of(app.pid())?;
        if run > 0 {
            record(
                metrics,
                "startup_to_ready_window",
                Unit::Milliseconds,
                millis(app.startup),
            );
            record(metrics, "rss_idle", Unit::Mebibytes, rss);
        }
    }
    Ok(())
}

/// Sends input and returns how long until `condition` holds in the
/// semantic state of a prepared frame, polled every millisecond. The time
/// includes the polling and at least one observation round trip; it does
/// not include painting or presentation.
fn timed(
    app: &mut App<'_>,
    input: impl FnOnce(&mut App<'_>) -> Outcome,
    condition: impl Fn(&UiSnapshot) -> bool,
) -> Outcome<f64> {
    let started = Instant::now();
    input(app)?;
    loop {
        if condition(&app.snapshot()?) {
            return Ok(millis(started.elapsed()));
        }
        if started.elapsed() > GIVE_UP {
            return Err(format!("no response within {GIVE_UP:?}"));
        }
        thread::sleep(Duration::from_millis(1));
    }
}

/// Waits without timing, for preconditions.
fn settle(app: &mut App<'_>, what: &str, condition: impl Fn(&UiSnapshot) -> bool) -> Outcome {
    let started = Instant::now();
    while !condition(&app.snapshot()?) {
        if started.elapsed() > GIVE_UP {
            return Err(format!("timed out waiting for {what}"));
        }
        thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

fn center(app: &mut App<'_>, id: &str) -> Outcome<(f32, f32)> {
    settle(app, id, |snapshot| {
        snapshot.find(id).is_some_and(|node| node.visible.is_some())
    })?;
    let snapshot = app.snapshot()?;
    let node = snapshot.find(id).ok_or(format!("`{id}` disappeared"))?;
    Ok(node.visible.ok_or(format!("`{id}` is hidden"))?.center())
}

fn summary_is(expected: String) -> impl Fn(&UiSnapshot) -> bool {
    move |snapshot| snapshot.text("doc.summary") == Some(expected.as_str())
}

fn selected(id: &'static str) -> impl Fn(&UiSnapshot) -> bool {
    move |snapshot| snapshot.find(id).is_some_and(|node| node.selected)
}

/// Measures the large log in one application window and returns the GPU
/// adapter that window reported.
fn measure_window(
    server: &XServer,
    setup: &Setup,
    artifacts: &Path,
    errors: usize,
    metrics: &mut BTreeMap<String, Samples>,
) -> Outcome<String> {
    let directory = artifacts.join("window");
    let mut app = App::launch(server, setup, &directory.join("state"), &directory, "app")?;
    let renderer = app.renderer();
    let all = format!("{LARGE_LOG_LINES} records");
    let only_errors = format!("{errors} of {LARGE_LOG_LINES} records");

    // Open the log by clicking it in Sources; close it with Ctrl+W.
    for run in 0..6 {
        let (x, y) = center(&mut app, LARGE_SOURCE)?;
        let opened = timed(
            &mut app,
            |app| app.pointer_click(x, y, 1),
            |snapshot| {
                snapshot.text("doc.summary") == Some(all.as_str())
                    && snapshot.find("doc.row.0").is_some()
            },
        )?;
        let (rss, _) = memory_of(app.pid())?;
        if run > 0 {
            record(metrics, "large_log_open", Unit::Milliseconds, opened);
            record(metrics, "rss_large_log", Unit::Mebibytes, rss);
        }
        if run < 5 {
            app.key("ctrl-w")?;
            settle(&mut app, "the log to close", |snapshot| {
                snapshot.find("doc.empty").is_some()
            })?;
        }
    }
    app.screenshot("large-log-open")?;

    // Filter to errors and back, with the shortcuts of the sample package.
    for run in 0..9 {
        let narrowed = timed(
            &mut app,
            |app| app.key("ctrl-1"),
            summary_is(only_errors.clone()),
        )?;
        let widened = timed(
            &mut app,
            |app| app.key("ctrl-shift-f"),
            summary_is(all.clone()),
        )?;
        if run > 0 {
            record(metrics, "large_log_filter", Unit::Milliseconds, narrowed);
            record(metrics, "large_log_filter", Unit::Milliseconds, widened);
        }
    }

    // Small interactions with the large log open.
    for run in 0..11 {
        let (x, y) = center(&mut app, BOTTOM_STATISTICS)?;
        let clicked = timed(
            &mut app,
            |app| app.pointer_click(x, y, 1),
            selected(BOTTOM_STATISTICS),
        )?;
        let (x, y) = center(&mut app, BOTTOM_RECORD)?;
        app.pointer_click(x, y, 1)?;
        settle(&mut app, "the Record tab", selected(BOTTOM_RECORD))?;
        let shortcut = timed(&mut app, |app| app.key("ctrl-4"), selected(BOTTOM_OUTPUT))?;
        if run > 0 {
            record(
                metrics,
                "click_to_semantic_state",
                Unit::Milliseconds,
                clicked,
            );
            record(
                metrics,
                "key_to_semantic_state",
                Unit::Milliseconds,
                shortcut,
            );
        }
    }
    app.screenshot("after-interactions")?;

    // What one observation costs, to read the numbers above against.
    for _ in 0..30 {
        let started = Instant::now();
        app.snapshot()?;
        record(
            metrics,
            "observation_round_trip",
            Unit::Milliseconds,
            millis(started.elapsed()),
        );
    }
    let (_, peak) = memory_of(app.pid())?;
    record(metrics, "peak_rss_large_log", Unit::Mebibytes, peak);

    // Open the generated workspace by typing its path, as a user would.
    let (x, y) = center(&mut app, PROJECTS_TAB)?;
    app.pointer_click(x, y, 1)?;
    settle(&mut app, "the Projects tab", selected(PROJECTS_TAB))?;
    let units = |count: usize| {
        move |snapshot: &UiSnapshot| {
            snapshot
                .with_prefix("view.projects.tree.node.p")
                .iter()
                .filter(|node| node.id.contains("/unit:"))
                .count()
                == count
        }
    };
    let close_project = |app: &mut App<'_>| -> Outcome {
        let (x, y) = center(app, "menu.file")?;
        app.pointer_click(x, y, 1)?;
        settle(app, "Close Project item", |snapshot| {
            snapshot.find("menu.file.close-project").is_some()
        })?;
        let (x, y) = center(app, "menu.file.close-project")?;
        app.pointer_click(x, y, 1)?;
        settle(app, "closed project", |snapshot| {
            snapshot.find("view.projects.tree.empty").is_some()
        })?;
        Ok(())
    };
    for run in 0..6 {
        // A one-package tree in between, so that the timed condition can
        // only be met by the newly opened tree, not by a stale frame.
        app.key("ctrl-o")?;
        app.choose_folder(Some(&format!("fixtures/{SMALL_WORKSPACE}")))?;
        settle(&mut app, "the small project", units(1))?;
        close_project(&mut app)?;
        app.key("ctrl-o")?;
        let prompt = app.prepare_folder(Some(&format!("fixtures/{LARGE_WORKSPACE}")))?;
        let opened = timed(
            &mut app,
            |app| app.finish_picker(prompt),
            units(LARGE_WORKSPACE_UNITS),
        )?;
        if run > 0 {
            record(metrics, "project_open_to_tree", Unit::Milliseconds, opened);
        }
        if run < 5 {
            close_project(&mut app)?;
        }
    }
    app.screenshot("large-project-open")?;
    Ok(renderer)
}

/// Times `work` after `warm_up` unrecorded runs.
fn repeat(warm_up: usize, runs: usize, mut work: impl FnMut()) -> Vec<f64> {
    for _ in 0..warm_up {
        work();
    }
    (0..runs)
        .map(|_| {
            let started = Instant::now();
            work();
            millis(started.elapsed())
        })
        .collect()
}

const LARGE_WORKSPACE: &str = "big";
const LARGE_WORKSPACE_UNITS: usize = 40;
const SMALL_WORKSPACE: &str = "small";

/// A Cargo workspace of `units` dependency-free packages, each with a
/// library, a binary and an integration test.
fn generate_workspace(root: &Path, units: usize) -> Outcome {
    let names: Vec<String> = (0..units).map(|n| format!("unit{n:02}")).collect();
    let members: Vec<String> = names.iter().map(|name| format!("\"{name}\"")).collect();
    io(fs::create_dir_all(root), "create the workspace")?;
    io(
        fs::write(
            root.join("Cargo.toml"),
            format!(
                "[workspace]\nresolver = \"2\"\nmembers = [{}]\n",
                members.join(", ")
            ),
        ),
        "write the workspace manifest",
    )?;
    retrowork_projects::config::ProjectConfig::new(
        root.file_name().unwrap().to_string_lossy().into(),
        "cargo".into(),
        vec![],
    )
    .create(root)?;
    for name in &names {
        let package = root.join(name);
        io(fs::create_dir_all(package.join("src")), "create a package")?;
        io(
            fs::create_dir_all(package.join("tests")),
            "create a package",
        )?;
        let files = [
            (
                "Cargo.toml",
                format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
            ),
            ("src/lib.rs", "pub fn value() -> u32 { 1 }\n".to_owned()),
            (
                "src/main.rs",
                "fn main() { println!(\"ok\"); }\n".to_owned(),
            ),
            ("tests/it.rs", "#[test]\nfn works() {}\n".to_owned()),
        ];
        for (file, content) in files {
            io(
                fs::write(package.join(file), content),
                "write a package file",
            )?;
        }
    }
    Ok(())
}

/// Cargo discovery and output streaming, without the window.
fn measure_projects(workspace: &Path, metrics: &mut BTreeMap<String, Samples>) -> Outcome {
    let provider = CargoProvider;
    let spec = provider.discovery(workspace);
    let discover = || -> Outcome<usize> {
        let captured = task::capture(&spec, MAX_DISCOVERY_BYTES, DISCOVERY_TIMEOUT)?;
        Ok(provider.parse_discovery(workspace, &captured)?.units.len())
    };
    for run in 0..11 {
        let started = Instant::now();
        let units = discover()?;
        let elapsed = millis(started.elapsed());
        if units != LARGE_WORKSPACE_UNITS {
            return Err(format!(
                "discovery found {units} packages, expected {LARGE_WORKSPACE_UNITS}"
            ));
        }
        if run > 0 {
            record(
                metrics,
                "project_discovery_40_units",
                Unit::Milliseconds,
                elapsed,
            );
        }
    }

    let project = provider.parse_discovery(
        workspace,
        &task::capture(&spec, MAX_DISCOVERY_BYTES, DISCOVERY_TIMEOUT)?,
    )?;
    let lines = 100_000;
    for run in 0..6 {
        let (sender, receiver) = mpsc::channel();
        let project = project.clone();
        let started = Instant::now();
        let mut kept = 0usize;
        let _handle = task::spawn(
            &ProcessSpec::new("seq", workspace).args(["1", &lines.to_string()]),
            move |event| match event {
                TaskEvent::Output(batch) => {
                    for line in &batch.lines {
                        kept += CargoProvider.interpret(&project, line).len();
                    }
                }
                TaskEvent::Finished { outcome, .. } => {
                    let _ = sender.send((outcome, kept));
                }
            },
        )?;
        let (outcome, seen) = receiver
            .recv_timeout(GIVE_UP)
            .map_err(|_| "the output workload did not finish".to_owned())?;
        let elapsed = millis(started.elapsed());
        if !outcome.succeeded() || seen != lines {
            return Err(format!(
                "output workload ended with {outcome:?} after {seen} lines"
            ));
        }
        if run > 0 {
            record(
                metrics,
                "task_output_100k_lines",
                Unit::Milliseconds,
                elapsed,
            );
        }
    }
    Ok(())
}

fn measure_processing(log: &str, metrics: &mut BTreeMap<String, Samples>) {
    for value in repeat(2, 10, || {
        std::hint::black_box(parse_log(std::hint::black_box(log)));
    }) {
        record(metrics, "log_parse_200k", Unit::Milliseconds, value);
    }
    let records = parse_log(log);
    for query in ["level:error", "component:auth timeout", "payment refused"] {
        let query = Query::parse(query);
        for value in repeat(2, 10, || {
            std::hint::black_box(
                records
                    .iter()
                    .filter(|record| query.matches(record))
                    .count(),
            );
        }) {
            record(metrics, "log_filter_200k", Unit::Milliseconds, value);
        }
    }
}

/// The context the host sends a plugin for a log with its first
/// [`PLUGIN_EXPORTED_RECORDS`] records shown.
fn plugin_context(log: &str) -> Value {
    let records: Vec<Value> = parse_log(log)
        .into_iter()
        .take(PLUGIN_EXPORTED_RECORDS)
        .map(|record| {
            json!({
                "line": record.line, "timestamp": record.timestamp,
                "level": record.level.as_str(), "component": record.component,
                "message": record.message,
            })
        })
        .collect();
    json!({ "document": {
        "kind": "logs.source", "resource": "large", "title": "large.log",
        "data": { "records": records, "truncated": true },
    } })
}

fn round_trip(host: &ProcessHost, context: &Value) -> Outcome<f64> {
    let (sender, receiver) = mpsc::channel();
    let started = Instant::now();
    host.submit(
        CommandId::literal("sample.top-components.run"),
        context.clone(),
        move |result| {
            let _ = sender.send(result.map(drop));
        },
    );
    match receiver.recv_timeout(GIVE_UP) {
        Ok(Ok(())) => Ok(millis(started.elapsed())),
        Ok(Err(error)) => Err(format!("plugin request failed: {error}")),
        Err(_) => Err("plugin request got no outcome".to_owned()),
    }
}

fn plugin_host(program: &Path) -> ProcessHost {
    ProcessHost::new(
        ModuleId::literal("sample.top-components"),
        program.to_owned(),
        Duration::from_secs(10),
    )
}

fn measure_platform_and_plugins(
    packages: &Path,
    plugin: &Path,
    log: &str,
    metrics: &mut BTreeMap<String, Samples>,
) -> Outcome {
    let options = Options {
        samples: true,
        packages: Some(packages.to_owned()),
        state_dir: None,
        ..Options::default()
    };
    let check = build_platform(&options);
    if let Some(module) = check.modules().iter().find(|module| !module.is_active()) {
        return Err(format!(
            "module {} did not load: {:?}",
            module.manifest.id, module.state
        ));
    }
    drop(check);
    for value in repeat(2, 20, || drop(build_platform(&options))) {
        record(metrics, "platform_build", Unit::Milliseconds, value);
    }

    let context = plugin_context(log);
    for _ in 0..5 {
        let host = plugin_host(plugin);
        record(
            metrics,
            "plugin_cold_start",
            Unit::Milliseconds,
            round_trip(&host, &context)?,
        );
    }
    let host = plugin_host(plugin);
    for run in 0..33 {
        let value = round_trip(&host, &context)?;
        if run >= 3 {
            record(metrics, "plugin_round_trip", Unit::Milliseconds, value);
        }
    }
    Ok(())
}

/// The negative control: the sample protocol answered by a plugin that
/// sleeps before every reply.
fn measure_slow_plugin(artifacts: &Path, log: &str) -> Outcome<Samples> {
    let script = artifacts.join("slow-plugin.sh");
    io(
        fs::write(
            &script,
            "#!/bin/sh\nread hello\necho '{\"type\":\"ready\",\"protocol\":0,\"plugin\":\"sample.top-components\"}'\nwhile read line; do\n  n=$(printf '%s' \"$line\" | sed 's/.*\"request\":\\([0-9]*\\).*/\\1/')\n  sleep 0.3\n  echo \"{\\\"type\\\":\\\"result\\\",\\\"request\\\":$n,\\\"effects\\\":[]}\"\ndone\n",
        ),
        "write the slow plugin",
    )?;
    let made_executable = std::process::Command::new("chmod")
        .arg("755")
        .arg(&script)
        .status();
    if !made_executable.is_ok_and(|status| status.success()) {
        return Err("cannot make the slow plugin executable".to_owned());
    }
    let context = plugin_context(log);
    let host = plugin_host(&script);
    let mut samples = Samples::new(Unit::Milliseconds);
    for run in 0..8 {
        let value = round_trip(&host, &context)?;
        if run >= 3 {
            samples.push(value);
        }
    }
    Ok(samples)
}
