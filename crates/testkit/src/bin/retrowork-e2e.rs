//! End-to-end runner: drives the real demo executable in a real window.
//!
//! Exit status: 0 when every scenario passed and every negative control was
//! detected; 1 when a scenario failed or a negative control went undetected;
//! 2 when the environment cannot run UI tests (the message says what is
//! missing). It never reports success for something it did not run.

use std::{
    collections::BTreeSet,
    fs,
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
    process::ExitCode,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use retrowork_testkit::{
    driver::{Outcome, Ui, UiDriver},
    e2e::{App, InputBackend, Setup, XServer},
    sabotage::{Fault, Sabotage},
    scenarios::{
        self, BOTTOM_STATISTICS, SCENARIOS, SOURCE_API, SOURCE_AUTH, Scenario, TAB_API, TAB_AUTH,
    },
};
use serde::Serialize;

const USAGE: &str = "\
Drives the RetroWork demo executable through its real window.

Usage: retrowork-e2e [OPTIONS]

Options:
  --input <xtest|injected>   How input is delivered [default: xtest]
                             xtest:    X server events (operating-system input)
                             injected: events handed to the window in-process
  --only <SCENARIO>          Run one scenario
  --artifacts <DIR>          Where evidence is written [default: target/e2e]
  --packages <DIR>           Packages the application loads [default: ./packages]
  --fixtures <DIR>           Projects each instance can open as fixtures/<name>
                             [default: ./fixtures/projects]
  --skip-negative-controls   Do not run the checks that broken input is detected
  --list                     List scenarios and exit
  -h, --help                 Print this help

Environment:
  RETROWORK_E2E_DISPLAY      Use this X display instead of starting a nested Xwayland
";

struct Args {
    input: InputBackend,
    only: Option<String>,
    artifacts: Option<PathBuf>,
    packages: PathBuf,
    fixtures: PathBuf,
    negative_controls: bool,
}

#[derive(Serialize)]
struct Report {
    schema: u32,
    tier: &'static str,
    /// `xtest` (operating-system input) or `injected`.
    input: &'static str,
    operating_system_input: bool,
    display_kind: &'static str,
    display: String,
    /// The GPU adapter the application's window reported, or `unknown: …`.
    renderer: String,
    application: String,
    started_unix_seconds: u64,
    duration_ms: u128,
    passed: bool,
    scenarios: Vec<ScenarioReport>,
    negative_controls: Vec<ControlReport>,
    /// Present for full runs only.
    control_coverage: Option<CoverageReport>,
}

#[derive(Serialize)]
struct ScenarioReport {
    name: String,
    proves: String,
    status: &'static str,
    duration_ms: u128,
    error: Option<String>,
    /// Files in `<artifacts>/<name>/`.
    evidence: Vec<String>,
}

#[derive(Serialize)]
struct ControlReport {
    name: &'static str,
    fault: String,
    scenario: &'static str,
    /// Whether the scenario failed, as it must.
    detected: bool,
    failure: Option<String>,
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(Some(args)) => args,
        Ok(None) => return ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("retrowork-e2e: {message}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match run(args) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(message) => {
            eprintln!("retrowork-e2e: UI tests cannot run here: {message}");
            ExitCode::from(2)
        }
    }
}

fn parse_args() -> Result<Option<Args>, String> {
    let mut args = Args {
        input: InputBackend::XTest,
        only: None,
        artifacts: None,
        packages: PathBuf::from("packages"),
        fixtures: PathBuf::from("fixtures/projects"),
        negative_controls: true,
    };
    let mut raw = std::env::args().skip(1);
    while let Some(arg) = raw.next() {
        let mut value = |name: &str| raw.next().ok_or_else(|| format!("`{name}` needs a value"));
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(None);
            }
            "--list" => {
                for scenario in SCENARIOS {
                    println!("{:<44} {}", scenario.name, scenario.proves);
                }
                println!("{:<44} {}", PERSISTENCE, PERSISTENCE_PROVES);
                return Ok(None);
            }
            "--input" => {
                args.input = match value("--input")?.as_str() {
                    "xtest" => InputBackend::XTest,
                    "injected" => InputBackend::Injected,
                    other => return Err(format!("unknown input backend `{other}`")),
                }
            }
            "--only" => args.only = Some(value("--only")?),
            "--artifacts" => args.artifacts = Some(value("--artifacts")?.into()),
            "--packages" => args.packages = value("--packages")?.into(),
            "--fixtures" => args.fixtures = value("--fixtures")?.into(),
            "--skip-negative-controls" => args.negative_controls = false,
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    if let Some(only) = &args.only
        && scenarios::find(only).is_none()
        && only != PERSISTENCE
    {
        return Err(format!("no scenario `{only}`; see --list"));
    }
    Ok(Some(args))
}

fn io<T>(result: std::io::Result<T>, what: &str) -> Outcome<T> {
    result.map_err(|error| format!("{what}: {error}"))
}

/// Runs everything. `Ok(passed)`; `Err` means the environment is unusable.
fn run(args: Args) -> Outcome<bool> {
    let binaries = io(std::env::current_exe(), "cannot locate this executable")?
        .parent()
        .map(Path::to_owned)
        .ok_or("this executable has no parent directory")?;
    let demo = binaries.join("retrowork-demo");
    for needed in [&demo, &binaries.join("retrowork-plugin-sample")] {
        if !needed.is_file() {
            return Err(format!(
                "{} is not built. Run `cargo build -p retrowork-demo -p retrowork-plugin-sample` first.",
                needed.display()
            ));
        }
    }
    let fixtures = io(
        fs::canonicalize(&args.fixtures),
        &format!("fixtures directory {}", args.fixtures.display()),
    )?;
    let packages = io(
        fs::canonicalize(&args.packages),
        &format!("packages directory {}", args.packages.display()),
    )?;
    let artifacts = args
        .artifacts
        .clone()
        .unwrap_or_else(|| binaries.join("../e2e"));
    if artifacts.exists() {
        io(fs::remove_dir_all(&artifacts), "cannot clear old evidence")?;
    }
    io(
        fs::create_dir_all(&artifacts),
        "cannot create the evidence directory",
    )?;
    let artifacts = io(fs::canonicalize(&artifacts), "evidence directory")?;

    let sockets = std::env::temp_dir().join(format!("retrowork-e2e-{}", std::process::id()));
    let _ = fs::remove_dir_all(&sockets);
    io(
        fs::create_dir(&sockets),
        "cannot create the socket directory",
    )?;
    io(
        fs::set_permissions(&sockets, fs::Permissions::from_mode(0o700)),
        "cannot protect the socket directory",
    )?;

    let server = XServer::start(&artifacts)?;
    let setup = Setup {
        demo,
        packages,
        input: args.input,
        sockets: sockets.clone(),
        fixtures,
    };
    println!(
        "retrowork-e2e: {} on {} X display {}, input: {}",
        setup.demo.display(),
        server.kind,
        server.display,
        setup.input.name()
    );
    let renderer = {
        let directory = artifacts.join("renderer-probe");
        let mut app = App::launch(&server, &setup, &directory.join("state"), &directory, "app")?;
        app.renderer()
    };
    println!("retrowork-e2e: renderer: {renderer}");

    let started = Instant::now();
    let wanted = |name: &str| args.only.as_deref().is_none_or(|only| only == name);
    let mut reports = Vec::new();
    let mut coverage = Coverage::default();
    for scenario in SCENARIOS.iter().filter(|scenario| wanted(scenario.name)) {
        // Only what a passing scenario operated counts as exercised.
        let mut seen = Coverage::default();
        let report = run_shared(&server, &setup, &artifacts, scenario, &mut seen);
        coverage.merge(seen, report.status == "passed");
        reports.push(report);
    }
    if wanted(PERSISTENCE) {
        let mut seen = Coverage::default();
        let report = run_persistence(&server, &setup, &artifacts, &mut seen);
        coverage.merge(seen, report.status == "passed");
        reports.push(report);
    }
    // The gate is about the whole suite; a partial run cannot satisfy it.
    let control_coverage = args.only.is_none().then(|| coverage.report());
    let controls = if args.negative_controls && args.only.is_none() {
        run_negative_controls(&server, &setup, &artifacts)
    } else {
        Vec::new()
    };
    let _ = fs::remove_dir_all(&sockets);

    let passed = !reports.is_empty()
        && reports.iter().all(|report| report.status == "passed")
        && controls.iter().all(|control| control.detected)
        && control_coverage
            .as_ref()
            .is_none_or(|coverage| coverage.controls > 0 && coverage.missing.is_empty());
    let report = Report {
        schema: 1,
        tier: "rendered-window-e2e",
        input: setup.input.name(),
        operating_system_input: setup.input == InputBackend::XTest,
        display_kind: server.kind,
        display: server.display.clone(),
        renderer,
        application: setup.demo.display().to_string(),
        started_unix_seconds: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |now| now.as_secs()),
        duration_ms: started.elapsed().as_millis(),
        passed,
        scenarios: reports,
        negative_controls: controls,
        control_coverage,
    };
    let report_path = artifacts.join("report.json");
    io(
        fs::write(
            &report_path,
            serde_json::to_vec_pretty(&report).map_err(|error| error.to_string())?,
        ),
        "cannot write the report",
    )?;

    let failed = report
        .scenarios
        .iter()
        .filter(|scenario| scenario.status != "passed")
        .count();
    let undetected = report
        .negative_controls
        .iter()
        .filter(|control| !control.detected)
        .count();
    if let Some(coverage) = &report.control_coverage {
        println!(
            "retrowork-e2e: contributed controls operated through the UI: {} of {}",
            coverage.exercised, coverage.controls
        );
        for control in &coverage.missing {
            println!(
                "  NOT EXERCISED {control}: add it to a scenario in crates/testkit/src/scenarios.rs"
            );
        }
    }
    println!(
        "retrowork-e2e: {} scenarios, {failed} failed; {} negative controls, {undetected} undetected; report: {}",
        report.scenarios.len(),
        report.negative_controls.len(),
        report_path.display()
    );
    Ok(passed)
}

/// Which contributed controls exist and which the scenarios operated.
#[derive(Default)]
struct Coverage {
    inventory: BTreeSet<String>,
    exercised: BTreeSet<String>,
}

#[derive(Serialize)]
struct CoverageReport {
    /// Menu items, toolbar buttons, dock tabs and shortcuts the application's
    /// modules contributed.
    controls: usize,
    /// How many of them a passing scenario operated through the real UI.
    exercised: usize,
    missing: Vec<String>,
}

impl Coverage {
    fn merge(&mut self, other: Coverage, count_exercised: bool) {
        self.inventory.extend(other.inventory);
        if count_exercised {
            self.exercised.extend(other.exercised);
        }
    }

    fn report(&self) -> CoverageReport {
        CoverageReport {
            controls: self.inventory.len(),
            exercised: self.inventory.intersection(&self.exercised).count(),
            missing: self
                .inventory
                .difference(&self.exercised)
                .cloned()
                .collect(),
        }
    }
}

/// Saves the last frame's elements and the event trace next to the
/// screenshots, and notes which controls the trace shows were operated.
fn save_state(
    app: &mut App<'_>,
    directory: &Path,
    tag: &str,
    evidence: &mut Vec<String>,
    coverage: &mut Coverage,
) {
    if let Ok(snapshot) = app.snapshot()
        && let Ok(json) = serde_json::to_vec_pretty(&snapshot)
        && fs::write(directory.join(format!("{tag}.snapshot.json")), json).is_ok()
    {
        evidence.push(format!("{tag}.snapshot.json"));
    }
    if let Ok(trace) = app.trace_since(0) {
        coverage.exercised.extend(
            trace
                .iter()
                .filter(|event| event.kind == "ui.activate")
                .map(|event| event.detail.clone()),
        );
        if let Ok(json) = serde_json::to_vec_pretty(&trace)
            && fs::write(directory.join(format!("{tag}.trace.json")), json).is_ok()
        {
            evidence.push(format!("{tag}.trace.json"));
        }
    }
    if let Ok(controls) = app.inventory() {
        coverage.inventory.extend(controls);
    }
}

fn finish(
    name: &str,
    proves: &str,
    started: Instant,
    outcome: Outcome,
    mut evidence: Vec<String>,
    directory: &Path,
) -> ScenarioReport {
    for log in [
        "app.stderr.log",
        "first.stderr.log",
        "second.stderr.log",
        "first.exit-report.json",
        "second.exit-report.json",
    ] {
        if directory.join(log).is_file() {
            evidence.push(log.to_owned());
        }
    }
    let duration = started.elapsed();
    match &outcome {
        Ok(()) => println!("  PASS {name} ({duration:.1?})"),
        Err(error) => println!("  FAIL {name} ({duration:.1?})\n       {error}"),
    }
    ScenarioReport {
        name: name.to_owned(),
        proves: proves.to_owned(),
        status: if outcome.is_ok() { "passed" } else { "failed" },
        duration_ms: duration.as_millis(),
        error: outcome.err(),
        evidence,
    }
}

fn run_shared(
    server: &XServer,
    setup: &Setup,
    artifacts: &Path,
    scenario: &Scenario,
    coverage: &mut Coverage,
) -> ScenarioReport {
    let started = Instant::now();
    let directory = artifacts.join(scenario.name);
    let mut evidence = Vec::new();
    let outcome = (|| {
        let mut app = App::launch(server, setup, &directory.join("state"), &directory, "app")?;
        println!("       started in {:.1?}", app.startup);
        let outcome = (scenario.run)(&mut Ui::new(&mut app));
        // Evidence of the final state, pass or fail.
        let last = if outcome.is_ok() { "final" } else { "failure" };
        let shot = app.screenshot(last);
        save_state(&mut app, &directory, "app", &mut evidence, coverage);
        evidence.extend(app.screenshots().iter().cloned());
        outcome.and(shot.map(drop))
    })();
    finish(
        scenario.name,
        scenario.proves,
        started,
        outcome,
        evidence,
        &directory,
    )
}

const PERSISTENCE: &str = "persistence-across-restart";
const PERSISTENCE_PROVES: &str =
    "File > Exit ends the process cleanly and the next start restores tabs, filter and panel";

fn run_persistence(
    server: &XServer,
    setup: &Setup,
    artifacts: &Path,
    coverage: &mut Coverage,
) -> ScenarioReport {
    let started = Instant::now();
    let directory = artifacts.join(PERSISTENCE);
    let state = directory.join("state");
    let mut evidence = Vec::new();
    let outcome = (|| {
        {
            let mut app = App::launch(server, setup, &state, &directory, "first")?;
            let result = (|| {
                let mut ui = Ui::new(&mut app);
                ui.click(SOURCE_AUTH)?;
                ui.click(SOURCE_API)?;
                ui.expect_selected(TAB_API, true)?;
                ui.click(TAB_AUTH)?;
                ui.press("ctrl-f")?;
                ui.expect_focused("doc.filter")?;
                ui.type_text("level:warn")?;
                ui.expect_text("doc.summary", "2 of 12 records")?;
                ui.click(BOTTOM_STATISTICS)?;
                ui.expect_selected(BOTTOM_STATISTICS, true)?;
                ui.checkpoint("before-exit")?;
                // Leave through the menu, like a user.
                ui.click("menu.file")?;
                ui.click("menu.file.exit")
            })();
            if result.is_err() {
                let _ = app.screenshot("failure");
            }
            save_state(&mut app, &directory, "first", &mut evidence, coverage);
            evidence.extend(app.screenshots().iter().map(|file| file.to_owned()));
            result?;
            confirm_exit(
                &mut app,
                &[
                    ("ui.mouse_down", ""),
                    ("ui.activate", "menu.file.exit"),
                    ("command.executed", "workbench.exit"),
                ],
                coverage,
            )?;
        }
        if !state.join("state.json").is_file() {
            return Err("no state file was written".to_owned());
        }

        let mut app = App::launch(server, setup, &state, &directory, "second")?;
        let result = (|| {
            let mut ui = Ui::new(&mut app);
            ui.expect_selected(TAB_AUTH, true)?;
            ui.expect_selected(TAB_API, false)?;
            ui.expect_text("doc.filter", "level:warn")?;
            ui.expect_text("doc.summary", "2 of 12 records")?;
            ui.expect_count("doc.row.", 2)?;
            ui.expect_selected(BOTTOM_STATISTICS, true)?;
            ui.expect_text("view.logstats.statistics.prop.shown", "2 of 12")?;
            ui.expect_text("status.message", "Restored 2 documents")?;
            ui.checkpoint("restored")?;
            // And the keyboard shortcut ends this instance.
            ui.press("ctrl-q")
        })();
        if result.is_err() {
            let _ = app.screenshot("failure");
        }
        save_state(&mut app, &directory, "second", &mut evidence, coverage);
        evidence.extend(app.screenshots().iter().map(|file| file.to_owned()));
        result?;
        confirm_exit(
            &mut app,
            &[
                ("ui.key_down", "ctrl-q"),
                ("ui.activate", "key.ctrl-q"),
                ("command.executed", "workbench.exit"),
            ],
            coverage,
        )
    })();
    finish(
        PERSISTENCE,
        PERSISTENCE_PROVES,
        started,
        outcome,
        evidence,
        &directory,
    )
}

/// Waits for the application to quit by itself after the user asked it to,
/// then checks its exit report: the process must exit with success, and the
/// trace it wrote on the way out must show `expected` in order. The controls
/// that trace shows were operated count towards coverage.
fn confirm_exit(app: &mut App<'_>, expected: &[(&str, &str)], coverage: &mut Coverage) -> Outcome {
    let status = app.wait_for_exit(Duration::from_secs(10))?;
    if !status.success() {
        return Err(format!("the application exited with {status}"));
    }
    let report = app.exit_report()?;
    let mut wanted = expected.iter().peekable();
    for event in &report.trace {
        if let Some((kind, detail)) = wanted.peek()
            && event.kind == *kind
            && event.detail.contains(detail)
        {
            wanted.next();
        }
    }
    if let Some((kind, detail)) = wanted.next() {
        return Err(format!(
            "the exit report has no `{kind} {detail}` in the expected order {expected:?}"
        ));
    }
    coverage.inventory.extend(report.inventory);
    coverage.exercised.extend(
        report
            .trace
            .into_iter()
            .filter(|event| event.kind == "ui.activate")
            .map(|event| event.detail),
    );
    Ok(())
}

/// Runs scenarios with deliberately broken input. Each must fail; one that
/// passes means the suite would not notice dead input wiring.
fn run_negative_controls(server: &XServer, setup: &Setup, artifacts: &Path) -> Vec<ControlReport> {
    let controls: [(&'static str, Fault, &'static str); 4] = [
        (
            "dropped-pointer",
            Fault::DropPointer,
            "tree-selection-opens-log",
        ),
        (
            "misaimed-pointer",
            // 40 px lower is a different tree row.
            Fault::MisaimPointer { dx: 0., dy: 40. },
            "tree-selection-opens-log",
        ),
        (
            "dropped-keyboard",
            Fault::DropKeyboard,
            "filter-by-keyboard",
        ),
        (
            "dropped-keyboard-opening-a-project",
            Fault::DropKeyboard,
            "project-open-and-browse",
        ),
    ];
    let mut reports: Vec<_> = controls
        .into_iter()
        .map(|(name, fault, scenario)| {
            let directory = artifacts.join("negative-controls").join(name);
            let result = (|| {
                let mut app =
                    App::launch(server, setup, &directory.join("state"), &directory, "app")?;
                let run = scenarios::find(scenario)
                    .expect("control scenarios exist")
                    .run;
                let outcome = {
                    let mut broken = Sabotage::new(&mut app, fault, Duration::from_millis(1500));
                    run(&mut Ui::new(&mut broken))
                };
                let _ = app.screenshot("after");
                Ok::<_, String>(outcome)
            })();
            let (detected, failure) = match result {
                // The scenario failed because of the fault: detected.
                Ok(Err(failure)) => (true, Some(failure)),
                Ok(Ok(())) => (false, None),
                // The application did not even start: not evidence of anything.
                Err(error) => (false, Some(format!("could not run: {error}"))),
            };
            println!(
                "  {} negative control {name} ({fault:?} in {scenario})",
                if detected { "PASS" } else { "FAIL" }
            );
            ControlReport {
                name,
                fault: format!("{fault:?}"),
                scenario,
                detected,
                failure: failure.map(|text| text.lines().next().unwrap_or_default().to_owned()),
            }
        })
        .collect();
    reports.push(stale_frame_control(server, setup, artifacts));
    reports
}

/// Holds old pixels across real input and requires the frame check to reject them.
fn stale_frame_control(server: &XServer, setup: &Setup, artifacts: &Path) -> ControlReport {
    let directory = artifacts.join("negative-controls/stale-pixels");
    let result = (|| {
        let mut app = App::launch(server, setup, &directory.join("state"), &directory, "app")?;
        let mut ui = Ui::new(&mut app);
        ui.click("dock.bottom.tab.sample.counter")?;
        ui.expect_text("view.sample.counter.value", "Count: 0")?;
        app.screenshot("before")?;
        let before = app.snapshot()?;
        let (width, height, pixels) = app.capture_pixels()?;
        if !retrowork_testkit::e2e::pixels_match_frame(&pixels, width, height, &before) {
            return Err("could not establish the initial painted frame".to_owned());
        }
        let mut ui = Ui::new(&mut app);
        ui.click("view.sample.counter.increment")?;
        ui.expect_text("view.sample.counter.value", "Count: 1")?;
        let after = app.snapshot()?;
        let detected = !retrowork_testkit::e2e::pixels_match_frame(&pixels, width, height, &after);
        app.screenshot("after")?;
        Ok::<_, String>(detected)
    })();
    let detected = matches!(result, Ok(true));
    println!(
        "  {} negative control stale-pixels",
        if detected { "PASS" } else { "FAIL" }
    );
    ControlReport {
        name: "stale-pixels",
        fault: "Previous painted frame paired with current semantics".into(),
        scenario: "native-plugin-view",
        detected,
        failure: result.err(),
    }
}
