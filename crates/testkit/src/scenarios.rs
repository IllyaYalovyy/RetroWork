//! User workflows of the demo application, written once and run by every
//! driver: against GPUI's headless test platform and against the real
//! executable with X11 input.
//!
//! Each scenario starts from a freshly started application with the sample
//! packages and no saved state. A user-visible workflow that is not listed
//! here is not covered by UI tests.

use std::time::Duration;

use retrowork_workbench::semantics::UiSnapshot;

use crate::driver::{Outcome, Ui};

/// One user workflow.
pub struct Scenario {
    /// Stable name, used on the command line and in reports.
    pub name: &'static str,
    /// What the scenario proves, for reports.
    pub proves: &'static str,
    /// Performs the workflow and checks what the window shows.
    pub run: fn(&mut Ui<'_>) -> Outcome,
    /// Needs real processes (the plugin executable, Cargo) and real time, so
    /// it runs only against the real application.
    pub native_only: bool,
}

/// Element id of `auth.log` in the Sources tree.
pub const SOURCE_AUTH: &str = "view.logs.sources.node.source:auth";
/// Element id of `api.log` in the Sources tree.
pub const SOURCE_API: &str = "view.logs.sources.node.source:api";
/// Element id of `kernel.log` in the Sources tree.
pub const SOURCE_KERNEL: &str = "view.logs.sources.node.source:kernel";
/// Element id of the `auth.log` document tab.
pub const TAB_AUTH: &str = "doc.tab.logs.source:auth";
/// Element id of the `api.log` document tab.
pub const TAB_API: &str = "doc.tab.logs.source:api";
/// Element id of the `kernel.log` document tab.
pub const TAB_KERNEL: &str = "doc.tab.logs.source:kernel";
/// Element id of the Output tab in the bottom dock.
pub const BOTTOM_OUTPUT: &str = "dock.bottom.tab.output";
/// Element id of the Record tab in the bottom dock.
pub const BOTTOM_RECORD: &str = "dock.bottom.tab.logs.record";
/// Element id of the Statistics tab in the bottom dock.
pub const BOTTOM_STATISTICS: &str = "dock.bottom.tab.logstats.statistics";

/// Scenarios every driver runs, in the order reports list them.
pub const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: "source-edit-save-and-close",
        proves: "typing, undo, save and redo preserve text; dirty tabs block close and exit",
        run: source_edit_save_and_close,
        native_only: true,
    },
    Scenario {
        name: "project-import-and-multiple-roots",
        proves: "folder controls import, reject unconfigured Open, preserve multiple roots and copy source text",
        run: project_import_and_multiple_roots,
        native_only: true,
    },
    Scenario {
        name: "native-plugin-view",
        proves: "a typed plugin renderer handles pointer, Enter and Space through one control",
        run: native_plugin_view,
        native_only: false,
    },
    Scenario {
        name: "project-command-availability",
        proves: "unavailable project commands expose reasons and reject keyboard execution",
        run: project_command_availability,
        native_only: true,
    },
    Scenario {
        name: "tree-selection-opens-log",
        proves: "clicking a tree node changes the centre content",
        run: tree_selection_opens_log,
        native_only: false,
    },
    Scenario {
        name: "tabs-and-panels-switch",
        proves: "document tabs, bottom panels, tab close and tree folders respond to clicks",
        run: tabs_and_panels_switch,
        native_only: false,
    },
    Scenario {
        name: "menu-by-pointer",
        proves: "menus open, run their command, close on Escape, and occlude what is under them",
        run: menu_by_pointer,
        native_only: false,
    },
    Scenario {
        name: "toolbar-and-shortcut-run-plugin-command",
        proves: "a command contributed by a bundled plugin changes the Output from toolbar and keyboard",
        run: toolbar_and_shortcut_run_plugin_command,
        native_only: false,
    },
    Scenario {
        name: "filter-by-keyboard",
        proves: "a shortcut moves focus to the filter and typed text filters the records",
        run: filter_by_keyboard,
        native_only: false,
    },
    Scenario {
        name: "declarative-package-contributions",
        proves: "menu items and shortcuts from a data-only package drive a module's command",
        run: declarative_package_contributions,
        native_only: false,
    },
    Scenario {
        name: "record-detail",
        proves: "selecting a table row shows the record in the bottom dock",
        run: record_detail,
        native_only: false,
    },
    Scenario {
        name: "command-error-is-visible",
        proves: "a failing command reports in the status bar and the Output",
        run: command_error_is_visible,
        native_only: false,
    },
    Scenario {
        name: "contributed-controls-reach-their-commands",
        proves: "the remaining menu items, toolbar buttons, shortcuts and dock tabs do what they say",
        run: contributed_controls_reach_their_commands,
        native_only: false,
    },
    Scenario {
        name: "process-plugin-action",
        proves: "a command implemented by an out-of-process plugin changes the Output",
        run: process_plugin_action,
        native_only: true,
    },
    Scenario {
        name: "project-open-and-browse",
        proves: "the system picker opens a configured Cargo project whose targets open their source",
        run: project_open_and_browse,
        native_only: true,
    },
    Scenario {
        name: "project-build-and-run",
        proves: "Build and Run stream Cargo's output and report success",
        run: project_build_and_run,
        native_only: true,
    },
    Scenario {
        name: "project-workspace-dependencies",
        proves: "workspace dependencies and build scripts run through selected Cargo targets",
        run: project_workspace_dependencies,
        native_only: true,
    },
    Scenario {
        name: "project-diagnostics",
        proves: "a failed build lists its error in Problems, which opens the file at the line",
        run: project_diagnostics,
        native_only: true,
    },
    Scenario {
        name: "project-tests",
        proves: "Test reports passing and failing test runs for the selected package",
        run: project_tests,
        native_only: true,
    },
    Scenario {
        name: "ninja-project-actions",
        proves: "a second provider shares Build, Run and Test and adds its own Clean action",
        run: ninja_project_actions,
        native_only: true,
    },
    Scenario {
        name: "project-cancel",
        proves: "Stop ends a running program from the toolbar, the menu and the keyboard",
        run: project_cancel,
        native_only: true,
    },
];

/// Element id of the Projects tab in the left dock.
pub const PROJECTS_TAB: &str = "dock.left.tab.projects.tree";
/// Element id of the line that shows the state of the current task.
pub const TASK_HEADER: &str = "view.projects.output.header";
/// Element id of the Build Output tab in the bottom dock.
pub const BUILD_OUTPUT_TAB: &str = "dock.bottom.tab.projects.output";
/// Element id of the Problems tab in the bottom dock.
pub const PROBLEMS_TAB: &str = "dock.bottom.tab.projects.problems";

/// How long a step that waits for Cargo may take. Fixtures build from
/// scratch in every scenario.
const CARGO_PATIENCE: Duration = Duration::from_secs(180);

fn project_node(node: &str) -> String {
    format!("view.projects.tree.node.p1/{node}")
}

/// Chooses `fixtures/<name>` in the system picker and waits for the project tree.
fn type_project(ui: &mut Ui<'_>, name: &str, first_unit: &str) -> Outcome {
    ui.choose_folder(Some(&format!("fixtures/{name}")))?;
    let unit = project_node(&format!("unit:{first_unit}"));
    let snapshot = ui.allowing(CARGO_PATIENCE, |ui| {
        ui.wait_for(&format!("`{unit}` or an open failure"), |snapshot| {
            snapshot.find(&unit).is_some()
                || snapshot
                    .text("view.projects.tree.empty")
                    .is_some_and(|hint| hint.starts_with("Cannot open"))
        })
    })?;
    match snapshot.find(&unit) {
        Some(_) => Ok(()),
        None => Err(snapshot
            .text("view.projects.tree.empty")
            .unwrap_or_default()
            .to_owned()),
    }
}

/// Waits until some visible Build Output line contains `part`.
fn expect_build_line(ui: &mut Ui<'_>, part: &str) -> Outcome {
    ui.allowing(CARGO_PATIENCE, |ui| {
        ui.wait_for(
            &format!("a Build Output line containing {part:?}"),
            |snapshot| {
                snapshot
                    .with_prefix("view.projects.output.line.")
                    .iter()
                    .any(|line| line.label.contains(part))
            },
        )
        .map(drop)
    })
}

fn expect_task(ui: &mut Ui<'_>, part: &str) -> Outcome {
    ui.allowing(CARGO_PATIENCE, |ui| {
        ui.expect_text_contains(TASK_HEADER, part)
    })
}

/// The id and label of the selected document tab of kind `kind`.
fn selected_document(ui: &mut Ui<'_>, kind: &str) -> Outcome<(String, String)> {
    let prefix = format!("doc.tab.{kind}:");
    let snapshot = ui.wait_for(&format!("a selected `{kind}` document"), |snapshot| {
        snapshot
            .with_prefix(&prefix)
            .iter()
            .any(|tab| tab.selected && !tab.id.ends_with(".close"))
    })?;
    let tab = snapshot
        .with_prefix(&prefix)
        .into_iter()
        .find(|tab| tab.selected && !tab.id.ends_with(".close"))
        .expect("the condition found it");
    Ok((tab.id.clone(), tab.label.clone()))
}

fn project_open_and_browse(ui: &mut Ui<'_>) -> Outcome {
    ui.click(PROJECTS_TAB)?;
    ui.expect_selected(PROJECTS_TAB, true)?;
    ui.expect_text(
        "view.projects.tree.empty",
        "Use File > Import Project or Open Project.",
    )?;
    ui.checkpoint("empty")?;

    let mark = ui.trace_mark()?;
    ui.press("ctrl-o")?;
    type_project(ui, "workspace", "app")?;
    ui.expect_text("status.message", "Opened workspace (4 units, Cargo)")?;
    ui.expect_trace(
        mark,
        &[
            ("ui.key_down", "ctrl-o"),
            ("command.executed", "projects.open"),
            ("project.discovering", "fixtures/workspace"),
            ("project.opened", "workspace"),
        ],
    )?;
    for node in [
        "project",
        "unit:app",
        "unit:checks",
        "unit:greeter",
        "unit:slow",
        "target:app:bin:app",
        "target:greeter:lib:greeter",
        "target:greeter:test:greeting",
        "target:slow:bin:slow",
    ] {
        ui.expect_present(&project_node(node))?;
    }
    ui.expect_text(&project_node("project"), "expanded")?;
    ui.checkpoint("project-open")?;

    // Folders collapse and expand.
    ui.click(&project_node("unit:greeter"))?;
    ui.expect_absent(&project_node("target:greeter:test:greeting"))?;
    ui.expect_selected(&project_node("unit:greeter"), true)?;
    ui.click(&project_node("unit:greeter"))?;
    ui.expect_present(&project_node("target:greeter:test:greeting"))?;

    // Double-clicking a target opens its source.
    ui.double_click(&project_node("target:app:bin:app"))?;
    let (_, title) = selected_document(ui, "projects.source")?;
    if title != "main.rs" {
        return Err(format!("expected main.rs to open, got {title:?}"));
    }
    ui.expect_text_contains("doc.source", "fn main() {")?;
    ui.click("doc.source")?;
    ui.press("ctrl-home")?;
    ui.press("down")?;
    ui.expect_text_contains("doc.source", "greeter::greeting(\"RetroWork\")")?;
    ui.checkpoint("source-open")
}

/// Edits a real file through the editor and reopens it to verify the saved contents.
fn source_edit_save_and_close(ui: &mut Ui<'_>) -> Outcome {
    ui.expect_present(PROJECTS_TAB)?;
    ui.press("ctrl-o")?;
    type_project(ui, "workspace", "app")?;
    ui.double_click(&project_node("target:app:bin:app"))?;
    ui.expect_text_contains("doc.source", "fn main() {")?;
    ui.click("doc.source")?;
    ui.press("ctrl-a")?;
    ui.type_text("x")?;
    ui.expect_text("doc.source", "x")?;
    ui.expect_text("doc.save-state", "Modified")?;
    ui.press("ctrl-w")?;
    ui.expect_present("doc.source")?;
    ui.expect_text_contains("status.message", "Unsaved changes")?;
    ui.expect_present("dialog.discard.cancel")?;
    ui.press("escape")?;
    ui.expect_absent("dialog.discard.cancel")?;
    let (tab, _) = selected_document(ui, "projects.source")?;
    ui.click(&format!("{tab}.close"))?;
    ui.expect_present("doc.source")?;
    ui.click("dialog.discard.cancel")?;
    ui.expect_absent("dialog.discard.cancel")?;
    ui.press("ctrl-q")?;
    ui.expect_present("doc.source")?;
    ui.expect_text_contains("status.message", "Unsaved changes")?;
    ui.checkpoint("dirty-close-blocked")?;
    ui.click("dialog.discard.cancel")?;
    ui.expect_absent("dialog.discard.cancel")?;
    ui.press("ctrl-s")?;
    ui.expect_text("doc.save-state", "Saved")?;
    ui.click("doc.source")?;
    ui.press("ctrl-z")?;
    ui.press("ctrl-home")?;
    ui.expect_text_contains("doc.source", "fn main() {")?;
    ui.expect_text("doc.save-state", "Modified")?;
    ui.press("ctrl-y")?;
    ui.expect_text("doc.source", "x")?;
    ui.expect_text("doc.save-state", "Saved")?;
    ui.press("end")?;
    ui.type_text(" again")?;
    ui.click("menu.file")?;
    ui.click("menu.file.save")?;
    ui.expect_text("doc.save-state", "Saved")?;
    ui.checkpoint("saved-with-undo-history")?;
    ui.press("ctrl-w")?;
    ui.expect_absent("doc.source")?;
    ui.double_click(&project_node("target:app:bin:app"))?;
    ui.expect_text("doc.source", "x again")?;
    ui.expect_text("doc.save-state", "Saved")?;
    ui.checkpoint("reopened-from-disk")?;
    ui.click("doc.source")?;
    ui.press("ctrl-a")?;
    ui.type_text("discard this")?;
    ui.press("ctrl-w")?;
    ui.expect_present("dialog.discard.confirm")?;
    ui.checkpoint("discard-confirmation")?;
    ui.click("dialog.discard.confirm")?;
    ui.expect_absent("doc.source")?;
    ui.expect_absent("dialog.discard.confirm")?;
    ui.double_click(&project_node("target:app:bin:app"))?;
    ui.expect_text("doc.source", "x again")?;
    ui.checkpoint("discard-kept-disk-contents")?;
    ui.click("doc.source")?;
    ui.press("ctrl-a")?;
    ui.type_text("saved elsewhere")?;
    ui.press("ctrl-shift-s")?;
    ui.choose_save_path(None)?;
    ui.expect_text("doc.source", "saved elsewhere")?;
    ui.expect_text("doc.save-state", "Modified")?;
    ui.click("menu.file")?;
    ui.click("menu.file.save-as")?;
    ui.choose_save_path(Some("fixtures/workspace/app/src/renamed.rs"))?;
    ui.expect_text("doc.save-state", "Saved")?;
    let (_, title) = selected_document(ui, "projects.source")?;
    if title != "renamed.rs" {
        return Err(format!("Save As did not update the tab: {title}"));
    }
    ui.click("doc.source")?;
    ui.press("ctrl-z")?;
    ui.expect_text("doc.save-state", "Modified")?;
    ui.press("ctrl-y")?;
    ui.expect_text("doc.source", "saved elsewhere")?;
    ui.expect_text("doc.save-state", "Saved")?;
    ui.press("end")?;
    ui.type_text("!")?;
    ui.press("ctrl-s")?;
    ui.expect_text("doc.save-state", "Saved")?;
    ui.checkpoint("saved-as-new-file")?;
    ui.press("ctrl-w")?;
    ui.expect_absent("doc.source")?;
    ui.double_click(&project_node("target:app:bin:app"))?;
    ui.expect_text("doc.source", "x again")?;
    ui.checkpoint("save-as-left-original-unchanged")?;
    let (first_tab, _) = selected_document(ui, "projects.source")?;
    ui.click("doc.source")?;
    ui.press("ctrl-a")?;
    ui.type_text("first saved together")?;
    ui.double_click(&project_node("target:greeter:lib:greeter"))?;
    ui.expect_text_contains("doc.source", "greeting")?;
    let (second_tab, _) = selected_document(ui, "projects.source")?;
    ui.click("doc.source")?;
    ui.press("ctrl-a")?;
    ui.type_text("second saved together")?;
    ui.press("ctrl-alt-s")?;
    ui.expect_text("doc.save-state", "Saved")?;
    ui.expect_selected(&second_tab, true)?;
    ui.click(&first_tab)?;
    ui.expect_text("doc.source", "first saved together")?;
    ui.expect_text("doc.save-state", "Saved")?;
    ui.press("ctrl-w")?;
    ui.click(&second_tab)?;
    ui.press("ctrl-w")?;
    ui.double_click(&project_node("target:app:bin:app"))?;
    ui.expect_text("doc.source", "first saved together")?;
    ui.double_click(&project_node("target:greeter:lib:greeter"))?;
    ui.expect_text("doc.source", "second saved together")?;
    ui.click("doc.source")?;
    ui.press("end")?;
    ui.type_text("!")?;
    ui.click("menu.file")?;
    ui.click("menu.file.save-all")?;
    ui.expect_text("doc.save-state", "Saved")?;
    ui.checkpoint("save-all-preserved-both-files")?;
    ui.click("doc.source")?;
    ui.press("ctrl-a")?;
    ui.type_text("discard on reload")?;
    ui.click("menu.file")?;
    ui.click("menu.file.reload")?;
    ui.click("dialog.discard.cancel")?;
    ui.expect_text("doc.source", "discard on reload")?;
    ui.click("menu.file")?;
    ui.click("menu.file.reload")?;
    ui.click("dialog.discard.confirm")?;
    ui.expect_text("doc.source", "second saved together!")?;
    ui.expect_text("doc.save-state", "Saved")?;
    ui.expect_text_contains("status.message", "Reloaded")?;
    ui.click("doc.source")?;
    ui.press("ctrl-z")?;
    ui.expect_text("doc.source", "second saved together!")?;
    ui.expect_text("doc.save-state", "Saved")?;
    ui.checkpoint("reloaded-without-stale-undo")
}

fn project_build_and_run(ui: &mut Ui<'_>) -> Outcome {
    ui.click("menu.file")?;
    ui.click("menu.file.open-project")?;
    ui.expect_selected(PROJECTS_TAB, true)?;
    type_project(ui, "workspace", "app")?;

    let mark = ui.trace_mark()?;
    ui.click("toolbar.projects.build")?;
    ui.expect_selected(BUILD_OUTPUT_TAB, true)?;
    expect_task(ui, "Build workspace: succeeded")?;
    expect_build_line(ui, "Finished")?;
    ui.expect_trace(
        mark,
        &[
            ("ui.activate", "toolbar.projects.build"),
            ("task.started", "cargo build"),
            ("task.finished", "Build workspace: succeeded"),
        ],
    )?;
    ui.checkpoint("built")?;

    ui.click(&project_node("target:app:bin:app"))?;
    ui.expect_selected(&project_node("target:app:bin:app"), true)?;
    let mark = ui.trace_mark()?;
    ui.press("f6")?;
    expect_task(ui, "Run bin app: succeeded")?;
    expect_build_line(ui, "Hello, RetroWork!")?;
    ui.expect_trace(
        mark,
        &[
            ("ui.key_down", "f6"),
            ("task.started", "cargo run -p app --bin app"),
            ("task.finished", "Run bin app: succeeded"),
        ],
    )?;
    ui.expect_text_contains("status.message", "Run bin app: succeeded")?;
    ui.checkpoint("ran")
}

fn project_diagnostics(ui: &mut Ui<'_>) -> Outcome {
    ui.press("ctrl-o")?;
    type_project(ui, "broken", "broken")?;
    ui.click("menu.run")?;
    ui.click("menu.run.build")?;
    expect_task(ui, "Build broken: failed (exit code 101)")?;
    ui.expect_text_contains(TASK_HEADER, "1 error")?;
    expect_build_line(ui, "error: could not compile `broken`")?;
    ui.checkpoint("build-failed")?;

    ui.click(PROBLEMS_TAB)?;
    ui.expect_selected(PROBLEMS_TAB, true)?;
    let snapshot = ui.wait_for("an error in Problems", |snapshot| {
        snapshot
            .with_prefix("view.projects.problems.row.")
            .iter()
            .any(|row| {
                row.value
                    .as_deref()
                    .is_some_and(|cells| cells.starts_with("error\t"))
            })
    })?;
    let row = snapshot
        .with_prefix("view.projects.problems.row.")
        .into_iter()
        .find(|row| {
            row.value
                .as_deref()
                .is_some_and(|cells| cells.starts_with("error\t"))
        })
        .expect("the condition found it")
        .clone();
    let cells = row.value.clone().unwrap_or_default();
    if !cells.starts_with("error\tmismatched types\tsrc/main.rs:3:") {
        return Err(format!("unexpected problem: {cells:?}"));
    }
    ui.checkpoint("problems")?;

    ui.click(&row.id)?;
    ui.expect_selected(&row.id, true)?;
    let (_, title) = selected_document(ui, "projects.source")?;
    if title != "main.rs" {
        return Err(format!("expected main.rs to open, got {title:?}"));
    }
    ui.expect_text_contains("doc.position", "Ln 3,")?;
    ui.expect_text_contains("doc.source", "let count: u32 = \"three\";")?;

    ui.checkpoint("source-at-error")?;
    ui.press("ctrl-w")?;
    ui.click(&row.id)?;
    ui.expect_text_contains("doc.position", "Ln 3,")?;
    ui.checkpoint("source-reopened-at-error")?;

    ui.click(BUILD_OUTPUT_TAB)?;
    ui.expect_selected(BUILD_OUTPUT_TAB, true)?;
    ui.expect_text_contains(TASK_HEADER, "Build broken: failed")
}

fn project_tests(ui: &mut Ui<'_>) -> Outcome {
    ui.press("ctrl-o")?;
    type_project(ui, "workspace", "app")?;

    // Clicking a package folder selects it as the scope.
    ui.click(&project_node("unit:greeter"))?;
    ui.expect_selected(&project_node("unit:greeter"), true)?;
    ui.press("alt-f6")?;
    expect_task(ui, "Test greeter: succeeded")?;
    expect_build_line(ui, "test result: ok.")?;
    ui.checkpoint("tests-passed")?;

    ui.click(&project_node("unit:checks"))?;
    ui.expect_selected(&project_node("unit:checks"), true)?;
    ui.click("toolbar.projects.test")?;
    expect_task(ui, "Test checks: failed (exit code 101)")?;
    expect_build_line(ui, "test result: FAILED. 1 passed; 1 failed")?;
    ui.checkpoint("tests-failed")?;

    ui.press("f11")?;
    expect_task(ui, "Build checks: succeeded")?;
    ui.click("menu.run")?;
    ui.click("menu.run.test")?;
    expect_task(ui, "Test checks: failed (exit code 101)")
}

/// Starts the slow program, waits for its output, stops it with `stop`, and
/// checks that it really stopped.
fn run_and_stop(
    ui: &mut Ui<'_>,
    start: &mut dyn FnMut(&mut Ui<'_>) -> Outcome,
    stop: &mut dyn FnMut(&mut Ui<'_>) -> Outcome,
) -> Outcome {
    start(ui)?;
    expect_build_line(ui, "tick 2")?;
    ui.expect_text_contains(TASK_HEADER, "Run bin slow: running")?;
    let mark = ui.trace_mark()?;
    stop(ui)?;
    ui.expect_text_contains(TASK_HEADER, "Run bin slow: cancelled")?;
    ui.expect_trace(
        mark,
        &[
            ("command.executed", "projects.stop"),
            ("task.finished", "Run bin slow: cancelled"),
        ],
    )?;
    // Nothing is printed after the cancellation.
    let last_tick = |snapshot: &UiSnapshot| {
        snapshot
            .with_prefix("view.projects.output.line.")
            .iter()
            .filter_map(|line| line.label.strip_prefix("tick ")?.parse::<u32>().ok())
            .max()
    };
    let before = last_tick(&ui.wait_for("the output", |_| true)?);
    ui.expect_no_trace(mark, "task.started")?;
    let after = last_tick(&ui.wait_for("the output", |_| true)?);
    if before != after {
        return Err(format!(
            "the program kept printing after Stop: {before:?} then {after:?}"
        ));
    }
    Ok(())
}

fn project_cancel(ui: &mut Ui<'_>) -> Outcome {
    ui.press("ctrl-o")?;
    type_project(ui, "workspace", "app")?;
    ui.click(&project_node("target:slow:bin:slow"))?;
    ui.expect_selected(&project_node("target:slow:bin:slow"), true)?;

    run_and_stop(
        ui,
        &mut |ui| {
            ui.click("menu.run")?;
            ui.click("menu.run.run")
        },
        &mut |ui| ui.click("toolbar.projects.stop"),
    )?;
    ui.checkpoint("stopped-by-toolbar")?;
    run_and_stop(ui, &mut |ui| ui.click("toolbar.projects.run"), &mut |ui| {
        ui.press("ctrl-f2")
    })?;
    run_and_stop(ui, &mut |ui| ui.press("f6"), &mut |ui| {
        ui.click("menu.run")?;
        ui.click("menu.run.stop")
    })?;
    ui.checkpoint("stopped-by-menu")
}

/// The scenario called `name`.
pub fn find(name: &str) -> Option<&'static Scenario> {
    SCENARIOS.iter().find(|scenario| scenario.name == name)
}

/// Opens `auth.log` and waits until it is shown.
fn open_auth(ui: &mut Ui<'_>) -> Outcome {
    ui.click(SOURCE_AUTH)?;
    ui.expect_selected(TAB_AUTH, true)?;
    ui.expect_text("doc.summary", "12 records")
}

fn tree_selection_opens_log(ui: &mut Ui<'_>) -> Outcome {
    // The workbench names where documents come from: the left-dock views.
    ui.expect_text(
        "doc.empty",
        "No document is open. Open one from Sources or Projects.",
    )?;
    ui.expect_absent(TAB_AUTH)?;
    ui.checkpoint("start")?;

    let mark = ui.trace_mark()?;
    ui.click(SOURCE_AUTH)?;
    ui.expect_selected(TAB_AUTH, true)?;
    ui.expect_selected(SOURCE_AUTH, true)?;
    ui.expect_absent("doc.empty")?;
    ui.expect_text("doc.summary", "12 records")?;
    let snapshot = ui.expect_count("doc.row.", 12)?;
    let first = snapshot.text("doc.row.0").unwrap_or_default();
    if !first.ends_with("INFO\tauth.session\tuser 42 signed in") {
        return Err(format!("unexpected first row: {first:?}"));
    }
    ui.expect_text("status.message", "Opened auth.log (12 records)")?;
    // The document appeared because of pointer input, in that order.
    ui.expect_trace(
        mark,
        &[
            ("ui.mouse_down", ""),
            ("document.opened", "logs.source:auth"),
        ],
    )?;
    ui.checkpoint("auth-open")?;

    ui.click(SOURCE_KERNEL)?;
    ui.expect_selected(TAB_KERNEL, true)?;
    ui.expect_selected(TAB_AUTH, false)?;
    ui.expect_selected(SOURCE_AUTH, false)?;
    ui.expect_text("doc.summary", "8 records")?;
    ui.expect_count("doc.row.", 8)?;
    ui.checkpoint("kernel-open")
}

fn tabs_and_panels_switch(ui: &mut Ui<'_>) -> Outcome {
    open_auth(ui)?;
    ui.click(SOURCE_API)?;
    ui.expect_selected(TAB_API, true)?;
    ui.expect_text("doc.summary", "15 records")?;

    ui.click(TAB_AUTH)?;
    ui.expect_selected(TAB_AUTH, true)?;
    ui.expect_selected(TAB_API, false)?;
    ui.expect_text("doc.summary", "12 records")?;
    ui.expect_text_contains("doc.row.0", "user 42 signed in")?;

    ui.expect_selected(BOTTOM_OUTPUT, true)?;
    ui.expect_output_contains("Opened api.log (15 records)")?;
    ui.click(BOTTOM_STATISTICS)?;
    ui.expect_selected(BOTTOM_STATISTICS, true)?;
    ui.expect_selected(BOTTOM_OUTPUT, false)?;
    ui.expect_text("view.logstats.statistics.prop.shown", "12 of 12")?;
    ui.expect_text("view.logstats.statistics.prop.error", "3")?;
    ui.expect_absent("view.output.line.0")?;
    ui.checkpoint("statistics-panel")?;
    // The panel follows the active document.
    ui.click(TAB_API)?;
    ui.expect_text("view.logstats.statistics.prop.shown", "15 of 15")?;
    ui.click(BOTTOM_RECORD)?;
    ui.expect_present("view.logs.record.empty")?;

    // Closing a tab activates its neighbour.
    ui.click("doc.tab.logs.source:api.close")?;
    ui.expect_absent(TAB_API)?;
    ui.expect_selected(TAB_AUTH, true)?;
    ui.expect_text("doc.summary", "12 records")?;

    // Tree folders collapse and expand.
    ui.click("view.logs.sources.node.group:system")?;
    ui.expect_absent(SOURCE_KERNEL)?;
    ui.expect_text("view.logs.sources.node.group:system", "collapsed")?;
    ui.click("view.logs.sources.node.group:system")?;
    ui.expect_present(SOURCE_KERNEL)?;
    ui.checkpoint("end")
}

fn menu_by_pointer(ui: &mut Ui<'_>) -> Outcome {
    ui.expect_absent("menu.help.list-modules")?;
    let mark = ui.trace_mark()?;
    ui.click("menu.help")?;
    ui.expect_selected("menu.help", true)?;
    ui.expect_present("menu.help.list-modules")?;
    ui.checkpoint("menu-open")?;
    ui.click("menu.help.list-modules")?;
    ui.expect_absent("menu.help.list-modules")?;
    ui.expect_selected("menu.help", false)?;
    ui.expect_output_contains("retrowork.logs 0.1.0 [bundled] active")?;
    ui.expect_output_contains("sample.workspace 0.1.0 [declarative] active")?;
    ui.expect_text_contains("status.message", "Listed ")?;
    ui.expect_trace(
        mark,
        &[
            ("ui.mouse_down", ""),
            ("ui.mouse_down", ""),
            ("command.executed", "workbench.list-modules"),
        ],
    )?;
    ui.checkpoint("modules-listed")?;

    // Escape closes an open menu without running anything.
    let mark = ui.trace_mark()?;
    ui.click("menu.file")?;
    ui.expect_present("menu.file.exit")?;
    ui.press("escape")?;
    ui.expect_absent("menu.file.exit")?;
    ui.expect_no_trace(mark, "command.executed")?;

    // An open menu covers the toolbar: a click at a toolbar button's position
    // must close the menu and must not run the button's command. This only
    // holds if clicks go through real hit testing.
    // (Reload is the leftmost button, well clear of the Help popup itself;
    // had it run, it would fail visibly because no log is open.)
    let button = ui.visible("toolbar.logs.reload")?;
    let mark = ui.trace_mark()?;
    ui.click("menu.help")?;
    ui.expect_present("menu.help.list-modules")?;
    let (x, y) = button.visible.expect("the button is visible").center();
    ui.click_at(x, y)?;
    ui.expect_absent("menu.help.list-modules")?;
    ui.expect_no_trace(mark, "command.executed")?;
    ui.expect_no_trace(mark, "command.failed")?;
    // With the menu closed the same position does reach the button.
    ui.click_at(x, y)?;
    ui.expect_trace(mark, &[("command.failed", "logs.reload")])
}

fn toolbar_and_shortcut_run_plugin_command(ui: &mut Ui<'_>) -> Outcome {
    open_auth(ui)?;
    ui.click(BOTTOM_STATISTICS)?;
    ui.expect_selected(BOTTOM_STATISTICS, true)?;

    let mark = ui.trace_mark()?;
    ui.click("toolbar.logstats.summarize")?;
    // The command brings the Output panel forward and writes to it.
    ui.expect_selected(BOTTOM_OUTPUT, true)?;
    ui.expect_output_contains("Summary of auth.log: 12 of 12 records shown")?;
    ui.expect_output_contains("Levels: ERROR 3, WARN 2, INFO 6, DEBUG 1")?;
    ui.expect_output_contains("auth.session: 6")?;
    ui.expect_text("status.message", "Summarized 12 records of auth.log")?;
    ui.expect_trace(
        mark,
        &[
            ("ui.mouse_down", ""),
            ("command.executed", "logstats.summarize"),
        ],
    )?;
    ui.checkpoint("summary-by-toolbar")?;

    // Keyboard: clear the Output, then summarize again with the shortcut.
    let mark = ui.trace_mark()?;
    ui.press("ctrl-l")?;
    ui.expect_present("view.output.empty")?;
    ui.expect_text("status.message", "Output cleared")?;
    ui.press("ctrl-shift-m")?;
    ui.expect_output_contains("Summary of auth.log: 12 of 12 records shown")?;
    ui.expect_trace(
        mark,
        &[
            ("ui.key_down", "ctrl-l"),
            ("command.executed", "output.clear"),
            ("ui.key_down", "ctrl-shift-m"),
            ("command.executed", "logstats.summarize"),
        ],
    )?;
    ui.checkpoint("summary-by-shortcut")
}

fn filter_by_keyboard(ui: &mut Ui<'_>) -> Outcome {
    open_auth(ui)?;
    ui.click(BOTTOM_STATISTICS)?;

    ui.press("ctrl-f")?;
    ui.expect_focused("doc.filter")?;
    ui.type_text("level:error")?;
    ui.expect_text("doc.filter", "level:error")?;
    ui.expect_text("doc.summary", "3 of 12 records")?;
    let snapshot = ui.expect_count("doc.row.", 3)?;
    for row in snapshot.with_prefix("doc.row.") {
        let cells = row.value.as_deref().unwrap_or_default();
        if !cells.contains("\tERROR\t") {
            return Err(format!("row {} is not an error: {cells:?}", row.id));
        }
    }
    // Another plugin's view sees the same filtered records.
    ui.expect_text("view.logstats.statistics.prop.shown", "3 of 12")?;
    ui.checkpoint("errors-only")?;

    // Typing continues where the caret is.
    ui.type_text(" db")?;
    ui.expect_text("doc.filter", "level:error db")?;
    ui.expect_text("doc.summary", "2 of 12 records")?;
    ui.expect_count("doc.row.", 2)?;

    // Backspace edits the field like any text field.
    ui.press("backspace")?;
    ui.press("backspace")?;
    ui.press("backspace")?;
    ui.expect_text("doc.filter", "level:error")?;
    ui.expect_text("doc.summary", "3 of 12 records")?;

    // A shortcut clears the filter while the field has focus.
    ui.press("ctrl-shift-f")?;
    ui.expect_text("doc.filter", "")?;
    ui.expect_text("doc.summary", "12 records")?;
    ui.expect_count("doc.row.", 12)?;

    // A malformed query shows nothing and says why.
    ui.press("ctrl-f")?;
    ui.type_text("level:eror")?;
    ui.expect_text_contains("doc.summary", "Filter problem: unknown level `eror`")?;
    ui.expect_count("doc.row.", 0)?;
    ui.expect_present("doc.empty")?;
    ui.checkpoint("filter-problem")
}

fn declarative_package_contributions(ui: &mut Ui<'_>) -> Outcome {
    ui.click(SOURCE_API)?;
    ui.expect_text("doc.summary", "15 records")?;

    let mark = ui.trace_mark()?;
    ui.click("menu.filters")?;
    ui.expect_present("menu.filters.errors-only")?;
    ui.checkpoint("filters-menu")?;
    ui.click("menu.filters.errors-only")?;
    ui.expect_text("doc.filter", "level:error")?;
    ui.expect_text("doc.summary", "2 of 15 records")?;
    ui.expect_count("doc.row.", 2)?;
    ui.expect_trace(mark, &[("command.executed", "logs.filter.set")])?;

    ui.press("ctrl-2")?;
    ui.expect_text("doc.filter", "level:warn level:error")?;
    ui.expect_text("doc.summary", "4 of 15 records")?;
    ui.press("ctrl-1")?;
    ui.expect_text("doc.summary", "2 of 15 records")?;
    ui.checkpoint("end")
}

fn record_detail(ui: &mut Ui<'_>) -> Outcome {
    open_auth(ui)?;
    ui.expect_selected(BOTTOM_OUTPUT, true)?;
    ui.click("doc.row.4")?;
    ui.expect_selected("doc.row.4", true)?;
    ui.expect_selected(BOTTOM_RECORD, true)?;
    ui.expect_text("view.logs.record.prop.level", "ERROR")?;
    ui.expect_text("view.logs.record.prop.component", "auth.db")?;
    ui.expect_text(
        "view.logs.record.prop.message",
        "connection refused (attempt 1)",
    )?;
    ui.checkpoint("row-selected")?;

    ui.click("doc.row.0")?;
    ui.expect_selected("doc.row.4", false)?;
    ui.expect_text("view.logs.record.prop.message", "user 42 signed in")?;

    // A selection that the filter removes is dropped, not left dangling.
    ui.press("ctrl-1")?;
    ui.expect_text("doc.summary", "3 of 12 records")?;
    ui.expect_present("view.logs.record.empty")
}

fn command_error_is_visible(ui: &mut Ui<'_>) -> Outcome {
    let mark = ui.trace_mark()?;
    ui.press("ctrl-shift-m")?;
    let message =
        "Command `logstats.summarize` failed: no log is open; select one in Sources first";
    ui.expect_text("status.message", message)?;
    ui.expect_output_contains(message)?;
    ui.expect_trace(
        mark,
        &[
            ("ui.key_down", "ctrl-shift-m"),
            ("command.failed", "logstats.summarize"),
        ],
    )?;
    ui.checkpoint("error-shown")
}

/// Clicks item `item` of menu `menu`.
fn choose(ui: &mut Ui<'_>, menu: &str, item: &str) -> Outcome {
    ui.click(&format!("menu.{menu}"))?;
    ui.click(&format!("menu.{menu}.{item}"))?;
    ui.expect_absent(&format!("menu.{menu}.{item}"))
}

/// Covers the contributed controls the workflow scenarios above do not
/// touch. The end-to-end runner fails when a control is operated by no
/// scenario, so a new menu item, button, shortcut or panel needs a line here
/// or in a workflow scenario, with an assertion on what it visibly does.
fn contributed_controls_reach_their_commands(ui: &mut Ui<'_>) -> Outcome {
    ui.click("dock.left.tab.logs.sources")?;
    ui.expect_selected("dock.left.tab.logs.sources", true)?;
    open_auth(ui)?;

    // View > Focus Filter, then the three ways to change or clear a filter.
    choose(ui, "view", "focus-filter")?;
    ui.expect_focused("doc.filter")?;
    ui.type_text("token")?;
    ui.expect_text("doc.summary", "3 of 12 records")?;
    choose(ui, "filters", "clear")?;
    ui.expect_text("doc.filter", "")?;
    ui.expect_text("doc.summary", "12 records")?;
    choose(ui, "filters", "warnings-and-errors")?;
    ui.expect_text("doc.filter", "level:warn level:error")?;
    ui.expect_text("doc.summary", "5 of 12 records")?;
    ui.click("toolbar.logs.clear-filter")?;
    ui.expect_text("doc.filter", "")?;
    ui.expect_text("doc.summary", "12 records")?;

    // Reload from the menu and with F5; each reports in the Output.
    choose(ui, "view", "clear-output")?;
    ui.expect_present("view.output.empty")?;
    choose(ui, "view", "reload-log")?;
    ui.expect_text("status.message", "Reloaded auth.log (12 records)")?;
    ui.expect_count("view.output.line.", 1)?;
    ui.press("f5")?;
    ui.expect_count("view.output.line.", 2)?;
    ui.expect_text("view.output.line.1", "Reloaded auth.log (12 records)")?;

    // Tools > Summarize Levels is the menu route to the plugin command.
    choose(ui, "tools", "summarize-levels")?;
    ui.expect_output_contains("Summary of auth.log: 12 of 12 records shown")?;

    // Three ways to bring the Output panel forward.
    ui.click(BOTTOM_STATISTICS)?;
    ui.expect_selected(BOTTOM_OUTPUT, false)?;
    choose(ui, "view", "output")?;
    ui.expect_selected(BOTTOM_OUTPUT, true)?;
    ui.click(BOTTOM_RECORD)?;
    ui.expect_selected(BOTTOM_OUTPUT, false)?;
    ui.press("ctrl-4")?;
    ui.expect_selected(BOTTOM_OUTPUT, true)?;
    ui.click(BOTTOM_STATISTICS)?;
    ui.click(BOTTOM_OUTPUT)?;
    ui.expect_selected(BOTTOM_OUTPUT, true)?;
    ui.expect_selected(BOTTOM_STATISTICS, false)?;

    // Two ways to close the active document.
    ui.click(SOURCE_API)?;
    ui.expect_selected(TAB_API, true)?;
    ui.press("ctrl-w")?;
    ui.expect_absent(TAB_API)?;
    ui.expect_selected(TAB_AUTH, true)?;
    ui.expect_text("status.message", "Closed api.log")?;
    choose(ui, "file", "close-document")?;
    ui.expect_absent(TAB_AUTH)?;
    ui.expect_present("doc.empty")?;
    ui.checkpoint("end")
}

fn process_plugin_action(ui: &mut Ui<'_>) -> Outcome {
    open_auth(ui)?;
    ui.press("ctrl-1")?;
    ui.expect_text("doc.summary", "3 of 12 records")?;

    let mark = ui.trace_mark()?;
    ui.click("menu.tools")?;
    ui.click("menu.tools.top-components")?;
    // The plugin sees what the user sees: the filtered records.
    ui.expect_output_contains("Top components in auth.log (3 records shown):")?;
    ui.expect_output_contains("2  auth.db")?;
    ui.expect_output_contains("1  auth.session")?;
    ui.expect_text("status.message", "Top components: 2 found")?;
    ui.expect_trace(
        mark,
        &[
            ("plugin.invoked", "sample.top-components"),
            ("plugin.completed", "sample.top-components"),
        ],
    )?;
    ui.checkpoint("plugin-output")
}

/// Exercises target selection with local dependencies and a build script.
fn project_workspace_dependencies(ui: &mut Ui<'_>) -> Outcome {
    ui.click("menu.file")?;
    ui.click("menu.file.open-project")?;
    type_project(ui, "dependencies", "support")?;
    ui.click("toolbar.projects.build")?;
    expect_task(ui, "Build dependencies: succeeded")?;
    ui.click(&project_node("target:workspace-app:bin:workspace-app"))?;
    ui.press("f6")?;
    expect_task(ui, "Run bin workspace-app: succeeded")?;
    expect_build_line(ui, "local dependency ready")?;
    ui.checkpoint("workspace-built-and-ran")?;
    ui.click(&project_node("target:workspace-app:example:greeting"))?;
    ui.press("f6")?;
    expect_task(ui, "Run example greeting: succeeded")?;
    expect_build_line(ui, "example local dependency")?;
    ui.checkpoint("example-ran")
}

/// Tests an independently rendered plugin control through real pointer input.
fn native_plugin_view(ui: &mut Ui<'_>) -> Outcome {
    ui.click("dock.bottom.tab.sample.counter")?;
    ui.expect_text("view.sample.counter.value", "Count: 0")?;
    ui.click("view.sample.counter.increment")?;
    ui.expect_text("view.sample.counter.value", "Count: 1")?;
    ui.expect_focused("view.sample.counter.increment")?;
    ui.press("enter")?;
    ui.expect_text("view.sample.counter.value", "Count: 2")?;
    ui.press("space")?;
    ui.expect_text("view.sample.counter.value", "Count: 3")?;
    ui.checkpoint("custom-control-updated")
}

/// Confirms availability changes with project selection and gates shortcuts.
fn project_command_availability(ui: &mut Ui<'_>) -> Outcome {
    ui.wait_for("Build unavailable without a project", |snapshot| {
        snapshot
            .find("toolbar.projects.build")
            .is_some_and(|node| node.disabled_reason.is_some())
    })?;
    ui.press("f11")?;
    ui.expect_text_contains("status.message", "no project is open")?;
    ui.click("menu.file")?;
    ui.click("menu.file.open-project")?;
    type_project(ui, "dependencies", "support")?;
    ui.click(&project_node("target:support:lib:support"))?;
    ui.wait_for("Run unavailable for a library", |snapshot| {
        snapshot.find("toolbar.projects.run").is_some_and(|node| {
            node.disabled_reason
                .as_ref()
                .is_some_and(|reason| reason.contains("cannot be run"))
        })
    })?;
    ui.press("f6")?;
    ui.expect_text_contains("status.message", "cannot be run")?;
    ui.click(&project_node("target:workspace-app:bin:workspace-app"))?;
    ui.wait_for("Run available for a binary", |snapshot| {
        snapshot
            .find("toolbar.projects.run")
            .is_some_and(|node| node.disabled_reason.is_none())
    })?;
    ui.checkpoint("availability-follows-selection")
}

/// Exercises common and custom project actions without knowing the provider's process details.
fn ninja_project_actions(ui: &mut Ui<'_>) -> Outcome {
    ui.click("menu.file")?;
    ui.click("menu.file.open-project")?;
    type_project(ui, "ninja", "build")?;
    ui.click("toolbar.projects.build")?;
    expect_task(ui, "Build ninja: succeeded")?;
    ui.press("f6")?;
    expect_task(ui, "Run ninja: succeeded")?;
    expect_build_line(ui, "Hello from Ninja")?;
    ui.press("alt-f6")?;
    expect_task(ui, "Test ninja: succeeded")?;
    expect_build_line(ui, "Ninja tests passed")?;
    ui.click("menu.run")?;
    ui.click("menu.run.ninja.clean")?;
    expect_task(ui, "Clean Ninja Outputs ninja: succeeded")?;
    ui.checkpoint("custom-action-completed")
}

/// Exercises import and project membership entirely through visible controls.
fn project_import_and_multiple_roots(ui: &mut Ui<'_>) -> Outcome {
    ui.click("menu.file")?;
    ui.click("menu.file.open-project")?;
    ui.choose_folder(None)?;
    ui.press("ctrl-o")?;
    choose_directory(ui, "fixtures/import-folder")?;
    ui.expect_text_contains("status.message", "use Import Project")?;
    ui.click("menu.file")?;
    ui.click("menu.file.import-project")?;
    choose_directory(ui, "fixtures/import-folder")?;
    ui.expect_text_contains("status.message", "Opened import-folder")?;
    ui.expect_present(&project_node("group:Sources"))?;
    ui.click(&project_node("file:src"))?;
    ui.double_click(&project_node("file:src/example.txt"))?;
    ui.expect_text("doc.source", "A read-only document.")?;
    ui.click("doc.source")?;
    ui.press("ctrl-home")?;
    ui.type_text("unexpected")?;
    ui.expect_text_contains("doc.source", "unexpected")?;
    ui.press("ctrl-z")?;
    ui.expect_text("doc.source", "A read-only document.")?;
    ui.press("shift-end")?;
    ui.expect_text("doc.selection", "A read-only document.")?;
    ui.press("ctrl-c")?;
    ui.checkpoint("source-selection")?;
    ui.click("dock.left.tab.logs.sources")?;
    ui.click(SOURCE_AUTH)?;
    ui.click("doc.filter")?;
    ui.press("ctrl-a")?;
    ui.press("ctrl-v")?;
    ui.expect_text("doc.filter", "A read-only document.")?;
    ui.click(PROJECTS_TAB)?;
    ui.click("menu.file")?;
    ui.click("menu.file.close-project")?;
    ui.expect_absent(&project_node("project"))?;
    ui.press("ctrl-o")?;
    choose_directory(ui, "fixtures/import-folder")?;
    ui.expect_present("view.projects.tree.node.p2/project")?;
    ui.click("view.projects.tree.node.p2/project")?;
    ui.press("ctrl-o")?;
    choose_directory(ui, "fixtures/ninja")?;
    ui.expect_present("view.projects.tree.node.p3/project")?;
    ui.expect_present("view.projects.tree.node.p2/project")?;
    ui.click("view.projects.tree.node.p2/project")?;
    ui.expect_selected("view.projects.tree.node.p2/project", true)?;
    ui.press("f11")?;
    ui.expect_text_contains("status.message", "no build-tool provider")?;
    ui.checkpoint("multiple-projects")?;
    ui.click("menu.file")?;
    ui.click("menu.file.close-project")?;
    ui.expect_absent("view.projects.tree.node.p2/project")?;
    ui.expect_present("view.projects.tree.node.p3/project")
}

/// Browses to a directory and confirms the dialog's current operation.
fn choose_directory(ui: &mut Ui<'_>, directory: &str) -> Outcome {
    ui.choose_folder(Some(directory))
}
