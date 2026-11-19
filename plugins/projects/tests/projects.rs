//! The Projects module through the platform's public API, with a provider
//! that runs shell scripts, so every path is exercised with real processes
//! but without a build tool.

use std::{
    fs,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use retrowork_platform::{
    ActivationContext, ActivationError, CommandError, CommandId, Dependency, Module,
    ModuleManifest, Platform, TableModel, TextModel, Tone, TreeModel, ViewContent, ViewEvent,
    ViewId, ViewLocation, ViewTarget,
    task::{Captured, OutputLine, ProcessSpec},
};
use retrowork_projects::{
    ActionId, COMMAND_BUILD, COMMAND_OPEN, COMMAND_RUN, COMMAND_STOP, COMMAND_TEST, Diagnostic,
    Interpretation, Location, MAX_OUTPUT_LINES, Project, ProjectProvider, ProjectRegistry,
    ProjectWorkspace, ProjectsModule, Scope, Severity, Target, TargetKind, Unit, VIEW_OUTPUT,
    VIEW_PROBLEMS, VIEW_TREE,
};
use semver::Version;

/// A directory is a project when it has `fake.project`. Each line of that
/// file names a target as `<kind> <name>`. Actions run `<action>.sh` with the
/// scope as its argument. Output lines `E <file>:<line> <message>` and
/// `W …` are diagnostics.
struct FakeProvider;

impl ProjectProvider for FakeProvider {
    fn actions(&self) -> &'static [retrowork_projects::ActionDefinition] {
        retrowork_projects::STANDARD_ACTIONS
    }
    fn id(&self) -> &'static str {
        "fake"
    }
    fn name(&self) -> &'static str {
        "Fake"
    }
    fn detect(&self, directory: &Path) -> bool {
        directory.join("fake.project").is_file()
    }
    fn discovery(&self, directory: &Path) -> ProcessSpec {
        ProcessSpec::new("/bin/sh", directory).args([
            "-c",
            "if [ -f discovery.sh ]; then sh discovery.sh; else cat fake.project; fi",
        ])
    }
    fn parse_discovery(&self, directory: &Path, output: &Captured) -> Result<Project, String> {
        let targets = output
            .stdout
            .lines()
            .map(|line| {
                let (kind, name) = line.split_once(' ').ok_or(format!("bad line `{line}`"))?;
                let kind = match kind {
                    "bin" => TargetKind::Binary,
                    "lib" => TargetKind::Library,
                    other => return Err(format!("unknown kind `{other}`")),
                };
                Ok(Target {
                    id: format!("{}:{name}", kind.label()),
                    name: name.to_owned(),
                    kind,
                    source: Some(directory.join(format!("{name}.src"))),
                })
            })
            .collect::<Result<_, String>>()?;
        Ok(Project {
            provider: "fake".into(),
            name: "demo".into(),
            root: directory.to_owned(),
            units: vec![Unit {
                id: "core".into(),
                name: "core".into(),
                version: Some("1.0".into()),
                manifest: directory.join("fake.project"),
                targets,
            }],
        })
    }
    fn availability(
        &self,
        project: &Project,
        action: &ActionId,
        scope: &Scope,
    ) -> Result<(), String> {
        if let Scope::Target { unit, target } = scope {
            let target = project
                .unit(unit)
                .unwrap()
                .targets
                .iter()
                .find(|item| &item.id == target)
                .unwrap();
            if *action == ActionId::RUN
                && !matches!(target.kind, TargetKind::Binary | TargetKind::Example)
            {
                return Err(format!("{} cannot run", target.name));
            }
        }
        Ok(())
    }
    fn plan(
        &self,
        project: &Project,
        action: &ActionId,
        scope: &Scope,
        _options: &serde_json::Value,
    ) -> Result<ProcessSpec, String> {
        let scope = match scope {
            Scope::Project => "project".to_owned(),
            Scope::Unit(unit) => format!("unit-{unit}"),
            Scope::Target { unit, target } => {
                let selected = project
                    .unit(unit)
                    .unwrap()
                    .targets
                    .iter()
                    .find(|item| &item.id == target)
                    .unwrap();
                let (kind, name) = (&selected.kind, &selected.name);
                if *action == ActionId::RUN
                    && !matches!(kind, TargetKind::Binary | TargetKind::Example)
                {
                    return Err(format!("{name} cannot run"));
                }
                format!("target-{name}")
            }
        };
        let script = format!("{}.sh", action.as_str());
        Ok(ProcessSpec::new("/bin/sh", &project.root).args([script, scope]))
    }
    fn interpret(&self, project: &Project, line: &OutputLine) -> Vec<Interpretation> {
        let mut items = vec![Interpretation::Line {
            text: line.text.clone(),
            tone: Tone::Normal,
        }];
        let severity = match line.text.get(..2) {
            Some("E ") => Some(Severity::Error),
            Some("W ") => Some(Severity::Warning),
            _ => None,
        };
        if let Some(severity) = severity {
            let rest = &line.text[2..];
            let (place, message) = rest.split_once(' ').unwrap_or((rest, ""));
            let (file, number) = place.split_once(':').unwrap_or((place, "1"));
            items.push(Interpretation::Diagnostic(Diagnostic {
                severity,
                message: message.to_owned(),
                location: Some(Location {
                    file: project.root.join(file),
                    line: number.parse().unwrap_or(1),
                    column: 1,
                }),
            }));
        }
        items
    }
}

struct FakeModule;

impl Module for FakeModule {
    fn manifest(&self) -> ModuleManifest {
        ModuleManifest::bundled("test.fake", "Fake", Version::new(1, 0, 0))
            .depends_on(Dependency::literal(retrowork_projects::MODULE_ID, "^0.1"))
    }
    fn activate(&self, cx: &mut ActivationContext<'_>) -> Result<(), ActivationError> {
        cx.require_service::<ProjectRegistry>()?
            .register(cx, Arc::new(FakeProvider))?;
        Ok(())
    }
}

fn platform() -> Platform {
    let platform = Platform::builder()
        .module(ProjectsModule)
        .module(FakeModule)
        .build();
    assert!(platform.modules().iter().all(|module| module.is_active()));
    platform
}

/// A fake project whose build script prints `build_script`.
fn project(build_script: &str) -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    let write = |name: &str, text: &str| fs::write(directory.path().join(name), text).unwrap();
    retrowork_projects::config::ProjectConfig::new("demo".into(), "fake".into(), vec![])
        .create(directory.path())
        .unwrap();
    write("fake.project", "bin tool\nlib core\n");
    write("build.sh", build_script);
    write("run.sh", "echo \"running $1\"; sleep 30\n");
    write("test.sh", "echo \"testing $1\"\n");
    write("main.src", "first\nsecond\nthird\nfourth\n");
    write("tool.src", "tool source\n");
    directory
}

fn view(id: &str) -> ViewTarget {
    ViewTarget::View(ViewId::new(id).unwrap())
}

fn command(id: &str) -> CommandId {
    CommandId::new(id).unwrap()
}

/// Runs inbox completions until `condition` holds.
fn settle(platform: &mut Platform, what: &str, condition: impl Fn(&Platform) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !condition(platform) {
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        platform.wait(Duration::from_millis(50));
        platform.pump();
    }
}

fn workspace(platform: &Platform) -> Rc<ProjectWorkspace> {
    platform.services().get::<ProjectWorkspace>().unwrap()
}

fn open(platform: &mut Platform, directory: &Path) {
    platform
        .view_event(
            &view(VIEW_TREE),
            ViewEvent::InputSubmitted(directory.display().to_string()),
        )
        .unwrap();
    settle(platform, "the project is open", |platform| {
        workspace(platform).project().is_some() && !workspace(platform).is_discovering()
    });
}

fn tree(platform: &Platform) -> TreeModel {
    match platform.view_content(&ViewId::literal(VIEW_TREE)) {
        Some(ViewContent::Tree(tree)) => tree,
        other => panic!("not a tree: {other:?}"),
    }
}

fn output(platform: &Platform) -> TextModel {
    match platform.view_content(&ViewId::literal(VIEW_OUTPUT)) {
        Some(ViewContent::Text(text)) => text,
        other => panic!("not text: {other:?}"),
    }
}

fn header(platform: &Platform) -> String {
    output(platform).header.unwrap().text
}

fn lines(platform: &Platform) -> Vec<String> {
    output(platform)
        .lines
        .iter()
        .map(|line| line.text.clone())
        .collect()
}

fn problems(platform: &Platform) -> TableModel {
    match platform.view_content(&ViewId::literal(VIEW_PROBLEMS)) {
        Some(ViewContent::Table(table)) => table,
        other => panic!("not a table: {other:?}"),
    }
}

fn finish(platform: &mut Platform) {
    settle(platform, "the task finishes", |platform| {
        !workspace(platform).is_running()
    });
}

fn execute(platform: &mut Platform, id: &str) -> Result<(), CommandError> {
    platform.execute(&command(id), None)
}

#[test]
fn opening_a_directory_shows_its_units_and_targets() {
    let directory = project("echo ok\n");
    let mut platform = platform();
    assert_eq!(
        tree(&platform).empty_hint,
        "Use File > Import Project or Open Project."
    );
    assert!(tree(&platform).input.is_none());
    assert_eq!(header(&platform), "No task has run yet.");

    platform
        .view_event(
            &view(VIEW_TREE),
            ViewEvent::InputSubmitted(directory.path().display().to_string()),
        )
        .unwrap();
    assert!(workspace(&platform).is_discovering());
    settle(&mut platform, "the project is open", |platform| {
        workspace(platform).project().is_some() && !workspace(platform).is_discovering()
    });

    let tree = tree(&platform);
    let shown: Vec<(usize, String, String)> = tree
        .visible()
        .into_iter()
        .map(|(depth, node)| (depth, node.id.clone(), node.label.clone()))
        .collect();
    assert!(
        shown
            .iter()
            .any(|(_, id, label)| id == "p1/project" && label == "demo (Fake)")
    );
    assert!(
        shown
            .iter()
            .any(|(_, id, _)| id == "p1/target:core:bin:tool")
    );
    assert!(shown.iter().any(|(_, _, label)| label == "Build Targets"));
    assert_eq!(tree.selected.as_deref(), Some("p1/project"));
    assert_eq!(platform.shell().status(), "Opened demo (1 unit, Fake)");
    let trace: Vec<String> = platform
        .shell()
        .trace()
        .since(0)
        .into_iter()
        .map(|event| event.kind)
        .collect();
    assert!(trace.contains(&"project.discovering".to_owned()));
    assert!(trace.contains(&"project.opened".to_owned()));
}

#[test]
fn directories_that_cannot_be_opened_say_why() {
    let mut platform = platform();
    let submit = |platform: &mut Platform, text: &str| {
        platform.view_event(&view(VIEW_TREE), ViewEvent::InputSubmitted(text.into()))
    };
    assert_eq!(
        submit(&mut platform, "   "),
        Err(CommandError::Failed(
            "type the directory of a project".into()
        ))
    );
    submit(&mut platform, "/no/such/place").unwrap();
    settle(&mut platform, "missing folder", |p| {
        !workspace(p).is_discovering()
    });
    assert!(
        platform
            .shell()
            .status()
            .contains("/no/such/place is not a directory")
    );

    let plain = tempfile::tempdir().unwrap();
    submit(&mut platform, &plain.path().display().to_string()).unwrap();
    settle(
        &mut platform,
        "unconfigured folder is rejected",
        |platform| !workspace(platform).is_discovering(),
    );
    assert!(platform.shell().status().contains("Import Project"));

    let broken = tempfile::tempdir().unwrap();
    retrowork_projects::config::ProjectConfig::new("broken".into(), "fake".into(), vec![])
        .create(broken.path())
        .unwrap();
    fs::write(broken.path().join("fake.project"), "jar thing\n").unwrap();
    submit(&mut platform, &broken.path().display().to_string()).unwrap();
    settle(&mut platform, "discovery fails", |platform| {
        tree(platform).empty_hint.starts_with("Cannot open")
    });
    assert!(tree(&platform).empty_hint.ends_with(": unknown kind `jar`"));
    assert!(workspace(&platform).project().is_none());
    assert_eq!(
        execute(&mut platform, COMMAND_BUILD),
        Err(CommandError::Failed(
            "no project is open; use File > Open Project".into()
        ))
    );
}

#[test]
fn open_project_command_brings_the_tree_forward_and_focuses_its_field() {
    let mut platform = platform();
    execute(&mut platform, COMMAND_OPEN).unwrap();
    assert_eq!(
        platform
            .shell()
            .active_view(ViewLocation::Left)
            .map(|view| view.as_str()),
        Some(VIEW_TREE)
    );
    assert_eq!(
        match platform.shell_mut().take_path_request().unwrap() {
            retrowork_platform::PathRequest::Folder(request) => request.command,
            _ => panic!("expected folder picker"),
        },
        CommandId::literal(COMMAND_OPEN)
    );
}

#[test]
fn a_successful_build_streams_output_and_reports_success() {
    let directory = project("echo \"building $1\"; echo progress >&2\n");
    let mut platform = platform();
    open(&mut platform, directory.path());
    execute(&mut platform, COMMAND_BUILD).unwrap();
    assert!(workspace(&platform).is_running());
    assert_eq!(header(&platform), "Build demo: running");
    assert_eq!(
        platform
            .shell()
            .active_view(ViewLocation::Bottom)
            .map(|view| view.as_str()),
        Some(VIEW_OUTPUT)
    );
    finish(&mut platform);
    // Lines of the two streams come through separate pipes, so only the
    // order within each stream is defined.
    let mut lines = lines(&platform);
    assert_eq!(lines.remove(0), "> /bin/sh build.sh project");
    lines.sort();
    assert_eq!(lines, ["building project", "progress"]);
    let header = header(&platform);
    assert!(
        header.starts_with("Build demo: succeeded after "),
        "{header}"
    );
    assert_eq!(platform.shell().status(), header);
    assert!(problems(&platform).rows.is_empty());
}

#[test]
fn diagnostics_are_listed_and_open_their_file_at_the_line() {
    let directory =
        project("echo 'E main.src:3 cannot compile'; echo 'W main.src:1 unused'; exit 2\n");
    let mut platform = platform();
    open(&mut platform, directory.path());
    execute(&mut platform, COMMAND_BUILD).unwrap();
    finish(&mut platform);

    let header = header(&platform);
    assert!(
        header.starts_with("Build demo: failed (exit code 2) after "),
        "{header}"
    );
    assert!(header.ends_with("; 1 error, 1 warning"), "{header}");
    let table = problems(&platform);
    let rows: Vec<Vec<String>> = table.rows.iter().map(|row| row.cells.clone()).collect();
    assert_eq!(
        rows,
        [
            ["error", "cannot compile", "main.src:3:1"],
            ["warning", "unused", "main.src:1:1"],
        ]
    );
    assert_eq!(table.rows[0].tone, Tone::Error);

    platform
        .view_event(&view(VIEW_PROBLEMS), ViewEvent::RowSelect(0))
        .unwrap();
    settle(
        &mut platform,
        "source is loaded",
        |platform| matches!(platform.document_content(platform.shell().documents()[0].id), Some(ViewContent::Source(text)) if !text.text.is_empty()),
    );
    let documents = platform.shell().documents();
    assert_eq!(documents.len(), 1);
    assert_eq!(documents[0].title, "main.src");
    assert_eq!(
        PathBuf::from(&documents[0].resource),
        directory.path().join("main.src")
    );
    let source =
        |platform: &Platform| match platform.document_content(platform.shell().documents()[0].id) {
            Some(ViewContent::Source(text)) => text,
            other => panic!("not source: {other:?}"),
        };
    assert_eq!(source(&platform).reveal_line, Some(3));
    assert_eq!(source(&platform).text.lines().nth(2).unwrap(), "third");
    assert_eq!(problems(&platform).selected, Some(0));

    // The same file is reused and moves to the other line.
    platform
        .view_event(&view(VIEW_PROBLEMS), ViewEvent::RowSelect(1))
        .unwrap();
    assert_eq!(platform.shell().documents().len(), 1);
    assert_eq!(source(&platform).reveal_line, Some(1));
    // A row that does not exist is ignored.
    platform
        .view_event(&view(VIEW_PROBLEMS), ViewEvent::RowSelect(9))
        .unwrap();
    assert_eq!(problems(&platform).selected, Some(1));
}

#[test]
fn the_selected_tree_node_decides_the_scope() {
    let directory = project("echo \"building $1\"\n");
    let mut platform = platform();
    open(&mut platform, directory.path());
    let tree_event = |platform: &mut Platform, event| platform.view_event(&view(VIEW_TREE), event);

    tree_event(
        &mut platform,
        ViewEvent::TreeSelect("p1/target:core:bin:tool".into()),
    )
    .unwrap();
    execute(&mut platform, COMMAND_TEST).unwrap();
    finish(&mut platform);
    assert_eq!(lines(&platform)[1], "testing target-tool");
    assert!(header(&platform).starts_with("Test bin tool: succeeded"));

    // Clicking a folder toggles it and also selects it.
    tree_event(&mut platform, ViewEvent::TreeToggle("p1/unit:core".into())).unwrap();
    assert!(
        !tree(&platform)
            .visible()
            .iter()
            .any(|(_, node)| node.id == "p1/target:core:bin:tool")
    );
    execute(&mut platform, COMMAND_BUILD).unwrap();
    finish(&mut platform);
    assert_eq!(lines(&platform)[1], "building unit-core");
    tree_event(&mut platform, ViewEvent::TreeToggle("p1/unit:core".into())).unwrap();
    assert!(
        tree(&platform)
            .visible()
            .iter()
            .any(|(_, node)| node.id == "p1/target:core:bin:tool")
    );

    tree_event(
        &mut platform,
        ViewEvent::TreeSelect("p1/target:core:lib:core".into()),
    )
    .unwrap();
    assert_eq!(
        execute(&mut platform, COMMAND_RUN),
        Err(CommandError::Failed("core cannot run".into()))
    );
    assert!(!workspace(&platform).is_running());

    // Activating a target opens its source.
    tree_event(
        &mut platform,
        ViewEvent::TreeActivate("p1/target:core:bin:tool".into()),
    )
    .unwrap();
    assert_eq!(platform.shell().documents()[0].title, "tool.src");
}

#[test]
fn a_running_task_can_be_cancelled_and_blocks_other_work_until_then() {
    let directory = project("echo ok\n");
    let mut platform = platform();
    open(&mut platform, directory.path());
    assert_eq!(
        execute(&mut platform, COMMAND_STOP),
        Err(CommandError::Failed("nothing is running".into()))
    );
    platform
        .view_event(
            &view(VIEW_TREE),
            ViewEvent::TreeSelect("p1/target:core:bin:tool".into()),
        )
        .unwrap();
    execute(&mut platform, COMMAND_RUN).unwrap();
    settle(&mut platform, "the program prints", |platform| {
        lines(platform)
            .iter()
            .any(|line| line == "running target-tool")
    });

    assert_eq!(
        execute(&mut platform, COMMAND_BUILD),
        Err(CommandError::Failed(
            "a task is already running; stop it first".into()
        ))
    );
    let reopen = platform.view_event(
        &view(VIEW_TREE),
        ViewEvent::InputSubmitted(directory.path().display().to_string()),
    );
    reopen.unwrap();
    settle(&mut platform, "duplicate open completes", |p| {
        !workspace(p).is_discovering()
    });

    let stopped_at = Instant::now();
    execute(&mut platform, COMMAND_STOP).unwrap();
    assert_eq!(platform.shell().status(), "Stopping Run bin tool…");
    finish(&mut platform);
    assert!(stopped_at.elapsed() < Duration::from_secs(5));
    let header = header(&platform);
    assert!(
        header.starts_with("Run bin tool: cancelled after "),
        "{header}"
    );
    assert_eq!(output(&platform).header.unwrap().tone, Tone::Warning);
    let finished: Vec<String> = platform
        .shell()
        .trace()
        .since(0)
        .into_iter()
        .filter(|event| event.kind == "task.finished")
        .map(|event| event.detail)
        .collect();
    assert_eq!(finished.len(), 1);
    assert!(finished[0].contains("cancelled"));
}

#[test]
fn output_keeps_only_the_newest_lines() {
    let directory = project("i=0; while [ $i -lt 25000 ]; do echo line-$i; i=$((i+1)); done\n");
    let mut platform = platform();
    open(&mut platform, directory.path());
    execute(&mut platform, COMMAND_BUILD).unwrap();
    finish(&mut platform);
    let lines = lines(&platform);
    assert_eq!(lines.len(), MAX_OUTPUT_LINES);
    assert_eq!(lines.last().unwrap(), "line-24999");
    assert_eq!(lines[0], "line-15000");
    let header = header(&platform);
    assert!(
        header.ends_with(" (15001 earlier lines not kept)"),
        "{header}"
    );
}

#[test]
fn a_new_task_starts_with_empty_output_and_problems() {
    let directory = project("echo 'E main.src:2 bad'; exit 1\n");
    let mut platform = platform();
    open(&mut platform, directory.path());
    execute(&mut platform, COMMAND_BUILD).unwrap();
    finish(&mut platform);
    assert_eq!(workspace(&platform).problems().len(), 1);
    execute(&mut platform, COMMAND_TEST).unwrap();
    finish(&mut platform);
    assert!(workspace(&platform).problems().is_empty());
    assert_eq!(
        lines(&platform),
        ["> /bin/sh test.sh project", "testing project"]
    );
}

#[test]
fn another_open_is_rejected_until_discovery_finishes() {
    let directory = project("echo built");
    let mut platform = platform();
    let event = || ViewEvent::InputSubmitted(directory.path().display().to_string());
    platform.view_event(&view(VIEW_TREE), event()).unwrap();
    let error = platform.view_event(&view(VIEW_TREE), event()).unwrap_err();
    assert!(
        error.to_string().contains("discovery is already running"),
        "{error}"
    );
    settle(&mut platform, "the first discovery completes", |platform| {
        workspace(platform).project().is_some() && !workspace(platform).is_discovering()
    });
    assert_eq!(
        workspace(&platform).project().unwrap().root,
        directory.path()
    );
    open(&mut platform, directory.path());
}

#[test]
fn failed_replacement_keeps_the_current_project_and_selection() {
    let original = project("echo original");
    let broken = project("echo broken");
    fs::write(broken.path().join("fake.project"), "invalid metadata").unwrap();
    let mut platform = platform();
    open(&mut platform, original.path());
    platform
        .view_event(
            &view(VIEW_TREE),
            ViewEvent::TreeSelect("p1/unit:core".into()),
        )
        .unwrap();
    platform
        .view_event(
            &view(VIEW_TREE),
            ViewEvent::InputSubmitted(broken.path().display().to_string()),
        )
        .unwrap();
    settle(&mut platform, "replacement fails", |platform| {
        platform.shell().status().starts_with("Cannot open")
    });
    assert_eq!(
        workspace(&platform).project().unwrap().root,
        original.path()
    );
    assert_eq!(tree(&platform).selected.as_deref(), Some("p1/unit:core"));
}

#[test]
fn reopening_a_diagnostic_after_closing_its_source_preserves_the_location() {
    let directory = project("echo 'E main.src:3 cannot compile'; exit 2");
    let mut platform = platform();
    open(&mut platform, directory.path());
    execute(&mut platform, COMMAND_BUILD).unwrap();
    finish(&mut platform);
    for _ in 0..3 {
        platform
            .view_event(&view(VIEW_PROBLEMS), ViewEvent::RowSelect(0))
            .unwrap();
        settle(
            &mut platform,
            "source is loaded",
            |platform| matches!(platform.document_content(platform.shell().documents()[0].id), Some(ViewContent::Source(text)) if !text.text.is_empty()),
        );
        match platform.document_content(platform.shell().documents()[0].id) {
            Some(ViewContent::Source(text)) => assert_eq!(text.reveal_line, Some(3)),
            other => panic!("unexpected source: {other:?}"),
        }
        let buffer = match platform.document_content(platform.shell().documents()[0].id) {
            Some(ViewContent::Source(text)) => Arc::downgrade(&text.text),
            _ => unreachable!(),
        };
        platform.close_document(platform.shell().documents()[0].id);
        assert!(
            buffer.upgrade().is_none(),
            "closed buffers must be released"
        );
    }
}

#[test]
fn discovery_keeps_the_current_project_and_can_be_cancelled() {
    let original = project("echo original");
    let replacement = project("echo replacement");
    fs::write(
        replacement.path().join("discovery.sh"),
        "echo ready > discovery.ready; sleep 60; cat fake.project",
    )
    .unwrap();
    let mut platform = platform();
    open(&mut platform, original.path());
    platform
        .view_event(
            &view(VIEW_TREE),
            ViewEvent::InputSubmitted(replacement.path().display().to_string()),
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !replacement.path().join("discovery.ready").exists() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(workspace(&platform).is_discovering());
    assert_eq!(
        workspace(&platform).project().unwrap().root,
        original.path()
    );
    assert!(!tree(&platform).roots.is_empty());
    execute(&mut platform, COMMAND_STOP).unwrap();
    settle(
        &mut platform,
        "discovery cancellation completes",
        |platform| !workspace(platform).is_discovering(),
    );
    assert_eq!(
        workspace(&platform).project().unwrap().root,
        original.path()
    );
    assert_eq!(platform.shell().status(), "Project discovery cancelled");
    fs::remove_file(replacement.path().join("discovery.sh")).unwrap();
    open(&mut platform, replacement.path());
    assert_eq!(
        workspace(&platform).project().unwrap().root,
        replacement.path()
    );
}

#[test]
fn stale_tree_selection_never_becomes_project_scope() {
    let directory = project("echo built");
    let mut platform = platform();
    open(&mut platform, directory.path());
    platform
        .view_event(
            &view(VIEW_TREE),
            ViewEvent::TreeSelect("p1/unit:core".into()),
        )
        .unwrap();
    assert!(
        platform
            .view_event(
                &view(VIEW_TREE),
                ViewEvent::TreeSelect("p1/target:gone:bin:missing".into())
            )
            .is_err()
    );
    assert_eq!(tree(&platform).selected.as_deref(), Some("p1/unit:core"));
}

#[test]
fn import_and_open_are_distinct_and_import_never_overwrites_configuration() {
    use retrowork_projects::{
        COMMAND_CLOSE, COMMAND_IMPORT,
        config::{CONFIG_PATH, ProjectConfig},
    };
    let directory = project("echo imported");
    fs::remove_file(directory.path().join(CONFIG_PATH)).unwrap();
    let mut platform = platform();
    let args = serde_json::json!({"directory": directory.path()});
    platform
        .execute(&command(COMMAND_OPEN), Some(&args))
        .unwrap();
    settle(
        &mut platform,
        "Open rejects a folder without configuration",
        |p| !workspace(p).is_discovering(),
    );
    assert!(workspace(&platform).project().is_none());
    assert!(!directory.path().join(CONFIG_PATH).exists());
    platform
        .execute(&command(COMMAND_IMPORT), Some(&args))
        .unwrap();
    settle(&mut platform, "import completes", |p| {
        !workspace(p).is_discovering()
    });
    assert_eq!(
        ProjectConfig::load(directory.path()).unwrap().provider,
        "fake"
    );
    assert!(workspace(&platform).project().is_some());
    platform.execute(&command(COMMAND_CLOSE), None).unwrap();
    let original = fs::read(directory.path().join(CONFIG_PATH)).unwrap();
    platform
        .execute(&command(COMMAND_IMPORT), Some(&args))
        .unwrap();
    settle(&mut platform, "repeat import fails", |p| {
        !workspace(p).is_discovering()
    });
    assert!(workspace(&platform).project().is_none());
    assert_eq!(
        fs::read(directory.path().join(CONFIG_PATH)).unwrap(),
        original
    );
    open(&mut platform, directory.path());
    open(&mut platform, directory.path());
    assert_eq!(tree(&platform).roots.len(), 1);
}

#[test]
fn two_projects_keep_task_output_and_selection_with_their_owner() {
    let first = project("sleep 0.2; echo first-project");
    let second = project("echo second-project");
    let mut platform = platform();
    open(&mut platform, first.path());
    execute(&mut platform, COMMAND_BUILD).unwrap();
    open(&mut platform, second.path());
    assert_eq!(tree(&platform).roots.len(), 2);
    assert!(lines(&platform).is_empty());
    assert!(execute(&mut platform, COMMAND_BUILD).is_err());
    finish(&mut platform);
    assert!(lines(&platform).is_empty());
    platform
        .view_event(&view(VIEW_TREE), ViewEvent::TreeSelect("p1/project".into()))
        .unwrap();
    assert!(lines(&platform).iter().any(|line| line == "first-project"));
    platform
        .view_event(&view(VIEW_TREE), ViewEvent::TreeSelect("p2/project".into()))
        .unwrap();
    execute(&mut platform, COMMAND_BUILD).unwrap();
    finish(&mut platform);
    assert!(lines(&platform).iter().any(|line| line == "second-project"));
    execute(&mut platform, retrowork_projects::COMMAND_CLOSE).unwrap();
    assert_eq!(tree(&platform).roots.len(), 1);
    assert_eq!(workspace(&platform).project().unwrap().root, first.path());
    assert!(lines(&platform).iter().any(|line| line == "first-project"));
}

#[test]
fn logical_directories_load_lazily_and_files_open_as_documents() {
    let directory = project("echo ok");
    fs::create_dir(directory.path().join("src")).unwrap();
    fs::write(directory.path().join("src/lib.rs"), "pub fn example() {}\n").unwrap();
    let mut platform = platform();
    open(&mut platform, directory.path());
    assert!(
        !tree(&platform)
            .visible()
            .iter()
            .any(|(_, node)| node.label == "lib.rs")
    );
    platform
        .view_event(
            &view(VIEW_TREE),
            ViewEvent::TreeToggle("p1/file:src".into()),
        )
        .unwrap();
    settle(&mut platform, "source directory is listed", |p| {
        tree(p)
            .visible()
            .iter()
            .any(|(_, node)| node.label == "lib.rs")
    });
    platform
        .view_event(
            &view(VIEW_TREE),
            ViewEvent::TreeActivate("p1/file:src/lib.rs".into()),
        )
        .unwrap();
    let id = platform.shell().documents()[0].id;
    settle(
        &mut platform,
        "source is loaded",
        |p| matches!(p.document_content(id), Some(ViewContent::Source(source)) if source.text.contains("example")),
    );
    assert_eq!(platform.shell().documents()[0].title, "lib.rs");
}

#[test]
fn a_plain_folder_imports_without_a_tool_and_reopens_with_its_configuration() {
    use retrowork_projects::{COMMAND_CLOSE, COMMAND_IMPORT, config::ProjectConfig};
    let directory = tempfile::tempdir().unwrap();
    fs::create_dir(directory.path().join("docs")).unwrap();
    let mut platform = platform();
    let args = serde_json::json!({"directory": directory.path()});
    platform
        .execute(&command(COMMAND_IMPORT), Some(&args))
        .unwrap();
    settle(&mut platform, "plain import", |p| {
        !workspace(p).is_discovering()
    });
    assert_eq!(
        ProjectConfig::load(directory.path()).unwrap().provider,
        "projects.files"
    );
    assert_eq!(tree(&platform).roots[0].children[0].label, "Documentation");
    assert!(
        execute(&mut platform, COMMAND_BUILD)
            .unwrap_err()
            .to_string()
            .contains("no build-tool provider")
    );
    execute(&mut platform, COMMAND_CLOSE).unwrap();
    open(&mut platform, directory.path());
    assert_eq!(
        workspace(&platform).project().unwrap().root,
        directory.path()
    );
}

#[test]
fn cancelled_import_leaves_no_configuration() {
    use retrowork_projects::{COMMAND_IMPORT, config::CONFIG_PATH};
    let directory = project("echo ok");
    fs::remove_file(directory.path().join(CONFIG_PATH)).unwrap();
    fs::write(
        directory.path().join("discovery.sh"),
        "sleep 1; cat fake.project",
    )
    .unwrap();
    let mut platform = platform();
    platform
        .execute(
            &command(COMMAND_IMPORT),
            Some(&serde_json::json!({"directory": directory.path()})),
        )
        .unwrap();
    execute(&mut platform, COMMAND_STOP).unwrap();
    settle(&mut platform, "cancelled import", |p| {
        !workspace(p).is_discovering()
    });
    assert!(!directory.path().join(CONFIG_PATH).exists());
    assert!(workspace(&platform).project().is_none());
}

#[test]
fn explicit_logical_roots_override_conventions_and_cannot_escape_the_project() {
    use retrowork_projects::config::{CONFIG_PATH, Group, LogicalRoot, ProjectConfig};
    let directory = project("echo ok");
    let outside = tempfile::tempdir().unwrap();
    fs::create_dir(directory.path().join("manual")).unwrap();
    fs::write(directory.path().join("manual/guide.txt"), "guide").unwrap();
    std::os::unix::fs::symlink(outside.path(), directory.path().join("escape")).unwrap();
    fs::remove_file(directory.path().join(CONFIG_PATH)).unwrap();
    ProjectConfig::new(
        "My Project".into(),
        "fake".into(),
        vec![
            LogicalRoot {
                group: Group::Documentation,
                path: "manual".into(),
            },
            LogicalRoot {
                group: Group::Other,
                path: "escape".into(),
            },
            LogicalRoot {
                group: Group::Other,
                path: "missing".into(),
            },
        ],
    )
    .create(directory.path())
    .unwrap();
    let mut platform = platform();
    open(&mut platform, directory.path());
    assert!(tree(&platform).roots[0].label.starts_with("My Project"));
    let visible = tree(&platform);
    let ids: Vec<_> = visible
        .visible()
        .iter()
        .map(|(_, node)| node.id.as_str())
        .collect();
    assert!(ids.contains(&"p1/file:manual"));
    assert!(!ids.contains(&"p1/file:escape"));
    assert!(!ids.contains(&"p1/file:missing"));
}

#[test]
fn stale_selection_cannot_change_the_active_project() {
    let first = project("echo first");
    let second = project("echo second");
    let mut platform = platform();
    open(&mut platform, first.path());
    open(&mut platform, second.path());
    assert!(
        platform
            .view_event(
                &view(VIEW_TREE),
                ViewEvent::TreeSelect("p1/unit:removed".into())
            )
            .is_err()
    );
    assert_eq!(workspace(&platform).project().unwrap().root, second.path());
}

#[test]
fn edited_sources_save_on_the_worker_and_block_close_until_saved() {
    use retrowork_platform::{
        core::{COMMAND_CLOSE_DOCUMENT, COMMAND_EXIT},
        document_session::SourceEvent,
    };
    let directory = project("echo ready");
    let mut platform = platform();
    open(&mut platform, directory.path());
    platform
        .view_event(
            &view(VIEW_TREE),
            ViewEvent::TreeActivate("p1/target:core:bin:tool".into()),
        )
        .unwrap();
    settle(&mut platform, "editable source", |p| {
        p.shell().active_document().is_some_and(|doc|
        matches!(p.document_content(doc.id), Some(ViewContent::Source(ref m)) if m.editing.is_some()))
    });
    let id = platform.shell().active_document().unwrap().id;
    let Some(ViewContent::Source(model)) = platform.document_content(id) else {
        panic!("source expected")
    };
    let session = model.editing.unwrap();
    let original = session.text();
    let mut edited = original.clone();
    edited.insert(0, "edited ");
    platform
        .view_event(
            &ViewTarget::Document(id),
            SourceEvent::Edited(edited.clone()).into(),
        )
        .unwrap();
    platform
        .execute(&command(COMMAND_CLOSE_DOCUMENT), None)
        .unwrap();
    assert!(platform.shell().document(id).is_some());
    platform.close_document(id);
    assert!(platform.shell().document(id).is_some());
    platform.execute(&command(COMMAND_EXIT), None).unwrap();
    assert!(!platform.shell().quit_requested());
    assert!(platform.shell().status().contains("Unsaved changes"));
    platform
        .execute(&command(retrowork_projects::COMMAND_SAVE), None)
        .unwrap();
    settle(&mut platform, "source saved", |_| !session.status().saving);
    assert_eq!(session.status().error, None);
    assert!(!session.status().dirty);
    assert_eq!(
        fs::read_to_string(directory.path().join("tool.src")).unwrap(),
        edited.to_string()
    );
    platform
        .view_event(
            &ViewTarget::Document(id),
            SourceEvent::Edited(original).into(),
        )
        .unwrap();
    assert!(session.status().dirty);
    platform
        .view_event(
            &ViewTarget::Document(id),
            SourceEvent::Edited(edited).into(),
        )
        .unwrap();
    platform.close_document(id);
    assert!(platform.shell().document(id).is_none());
}

#[test]
fn save_as_retargets_the_same_document_and_keeps_failures_at_the_old_path() {
    use retrowork_platform::document_session::SourceEvent;
    use retrowork_projects::{COMMAND_SAVE, COMMAND_SAVE_AS};
    let directory = project("echo ready");
    let mut platform = platform();
    open(&mut platform, directory.path());
    platform
        .view_event(
            &view(VIEW_TREE),
            ViewEvent::TreeActivate("p1/target:core:bin:tool".into()),
        )
        .unwrap();
    settle(&mut platform, "editable source", |p| {
        p.shell().active_document().is_some_and(|doc|
            matches!(p.document_content(doc.id), Some(ViewContent::Source(ref m)) if m.editing.is_some()))
    });
    let id = platform.shell().active_document().unwrap().id;
    let original_resource = platform.shell().document(id).unwrap().resource.clone();
    let Some(ViewContent::Source(model)) = platform.document_content(id) else {
        panic!("source")
    };
    let initial_text = model.text.clone();
    let session = model.editing.unwrap();
    execute(&mut platform, COMMAND_SAVE_AS).unwrap();
    let retrowork_platform::PathRequest::Save(request) =
        platform.shell_mut().take_path_request().unwrap()
    else {
        panic!("save picker")
    };
    assert_eq!(request.document, id);
    assert_eq!(request.name, "tool.src");
    let destination = directory.path().join("copy.src");
    let args = serde_json::json!({"document": id, "path": destination});
    platform
        .execute(&command(COMMAND_SAVE_AS), Some(&args))
        .unwrap();
    platform.close_document(id);
    assert!(platform.shell().document(id).is_some());
    settle(&mut platform, "Save As", |p| {
        p.shell().document(id).unwrap().resource == destination.to_str().unwrap()
    });
    assert_eq!(session.path(), destination);
    assert_eq!(platform.shell().document(id).unwrap().title, "copy.src");
    let Some(ViewContent::Source(after)) = platform.document_content(id) else {
        panic!("source")
    };
    assert!(
        std::sync::Arc::ptr_eq(&initial_text, &after.text),
        "Save As must not reset the editor"
    );
    assert_eq!(
        platform.persisted_state().documents[0].resource,
        destination.to_str().unwrap()
    );
    platform
        .view_event(
            &ViewTarget::Document(id),
            SourceEvent::Edited("later".into()).into(),
        )
        .unwrap();
    execute(&mut platform, COMMAND_SAVE).unwrap();
    settle(&mut platform, "save new path", |_| !session.status().saving);
    assert_eq!(fs::read_to_string(&destination).unwrap(), "later");
    assert_eq!(
        fs::read_to_string(&original_resource).unwrap(),
        &*initial_text
    );
    let existing = directory.path().join("existing.src");
    fs::write(&existing, "other").unwrap();
    platform
        .execute(
            &command(COMMAND_SAVE_AS),
            Some(&serde_json::json!({"document": id, "path": existing})),
        )
        .unwrap();
    settle(&mut platform, "existing destination refused", |p| {
        p.shell().status().contains("already exists")
    });
    assert_eq!(session.path(), destination);
    assert_eq!(fs::read_to_string(existing).unwrap(), "other");
    platform.close_document(id);
    assert!(
        platform
            .execute(&command(COMMAND_SAVE_AS), Some(&args))
            .is_err()
    );
}

#[test]
fn save_all_keeps_other_projects_saving_when_a_background_file_conflicts() {
    use retrowork_platform::document_session::SourceEvent;
    use retrowork_projects::{COMMAND_SAVE, COMMAND_SAVE_ALL};
    let first = project("echo first");
    let second = project("echo second");
    let mut platform = platform();
    assert!(
        platform
            .command_disabled_reason(&command(COMMAND_SAVE_ALL), None)
            .is_some()
    );
    let mut documents = Vec::new();
    for (number, directory) in [(1, &first), (2, &second)] {
        open(&mut platform, directory.path());
        platform
            .view_event(
                &view(VIEW_TREE),
                ViewEvent::TreeActivate(format!("p{number}/target:core:bin:tool")),
            )
            .unwrap();
        settle(&mut platform, "source loaded", |p| {
            p.shell().active_document().is_some_and(|doc|
                matches!(p.document_content(doc.id), Some(ViewContent::Source(ref m)) if m.editing.is_some()))
        });
        let id = platform.shell().active_document().unwrap().id;
        let Some(ViewContent::Source(model)) = platform.document_content(id) else {
            panic!("source")
        };
        documents.push((id, model.editing.unwrap()));
        platform
            .view_event(
                &ViewTarget::Document(id),
                SourceEvent::Edited(format!("edited {number}").as_str().into()).into(),
            )
            .unwrap();
    }
    fs::write(first.path().join("tool.src"), "external changes").unwrap();
    platform
        .view_event(
            &view(VIEW_TREE),
            ViewEvent::TreeActivate("p2/target:core:lib:core".into()),
        )
        .unwrap();
    let active = platform.shell().active_document().unwrap().id;
    platform.execute(&command(COMMAND_SAVE_ALL), None).unwrap();
    assert!(
        platform
            .command_disabled_reason(&command(COMMAND_SAVE_ALL), None)
            .is_some()
    );
    settle(&mut platform, "all writes completed", |_| {
        documents
            .iter()
            .all(|(_, session)| !session.status().saving)
    });
    assert_eq!(platform.shell().active_document().unwrap().id, active);
    assert!(documents[0].1.status().dirty);
    assert!(
        documents[0]
            .1
            .status()
            .error
            .as_ref()
            .unwrap()
            .contains("changed on disk")
    );
    assert!(!documents[1].1.status().dirty);
    assert_eq!(
        fs::read_to_string(first.path().join("tool.src")).unwrap(),
        "external changes"
    );
    assert_eq!(
        fs::read_to_string(second.path().join("tool.src")).unwrap(),
        "edited 2"
    );
    assert!(
        platform
            .shell()
            .output()
            .lines()
            .iter()
            .any(|line| line.text.contains(first.path().to_str().unwrap())
                && line.text.contains("changed on disk"))
    );
    fs::write(first.path().join("tool.src"), "tool source\n").unwrap();
    platform.activate_document(documents[1].0);
    platform
        .execute(
            &command(COMMAND_SAVE),
            Some(&serde_json::json!({"document": documents[0].0})),
        )
        .unwrap();
    settle(&mut platform, "retry saved", |_| {
        !documents[0].1.status().saving
    });
    assert!(!documents[0].1.status().dirty);
    assert_eq!(
        platform.shell().active_document().unwrap().id,
        documents[1].0
    );
    assert!(
        platform
            .command_disabled_reason(&command(COMMAND_SAVE_ALL), None)
            .is_some()
    );
    assert!(platform.execute(&command(COMMAND_SAVE_ALL), None).is_err());
}

#[test]
fn save_all_reports_busy_destinations_and_still_admits_other_documents() {
    use retrowork_platform::{
        DocumentKind, core::COMMAND_CLEAR_OUTPUT, document_session::SourceEvent,
    };
    use retrowork_projects::{COMMAND_SAVE_ALL, COMMAND_SAVE_AS, DOCUMENT_SOURCE};
    let directory = project("echo ready");
    let mut platform = platform();
    let mut documents = Vec::new();
    for name in ["tool.src", "main.src"] {
        platform.shell_mut().open_document(
            DocumentKind::literal(DOCUMENT_SOURCE),
            directory.path().join(name).to_str().unwrap(),
        );
        execute(&mut platform, COMMAND_CLEAR_OUTPUT).unwrap();
        let id = platform.shell().active_document().unwrap().id;
        settle(
            &mut platform,
            "editable source",
            |p| matches!(p.document_content(id), Some(ViewContent::Source(ref model)) if model.editing.is_some()),
        );
        let Some(ViewContent::Source(model)) = platform.document_content(id) else {
            panic!("source")
        };
        documents.push((id, model.editing.unwrap()));
        platform
            .view_event(
                &ViewTarget::Document(id),
                SourceEvent::Edited("edited".into()).into(),
            )
            .unwrap();
    }
    let destination = directory.path().join("copy.src");
    platform
        .execute(
            &command(COMMAND_SAVE_AS),
            Some(&serde_json::json!({"document": documents[0].0, "path": destination})),
        )
        .unwrap();
    let error = execute(&mut platform, COMMAND_SAVE_ALL).unwrap_err();
    assert!(error.to_string().contains("Some saves could not start"));
    assert!(
        platform
            .shell()
            .output()
            .lines()
            .iter()
            .any(|line| line.text.contains("tool.src") && line.text.contains("Preparing Save As"))
    );
    assert!(documents[1].1.status().saving);
    settle(
        &mut platform,
        "Save As and admitted Save All completed",
        |_| documents[0].1.path() == destination && !documents[1].1.status().saving,
    );
    assert_eq!(
        fs::read_to_string(directory.path().join("main.src")).unwrap(),
        "edited"
    );
    assert_eq!(fs::read_to_string(destination).unwrap(), "edited");
    assert_eq!(
        fs::read_to_string(directory.path().join("tool.src")).unwrap(),
        "tool source\n"
    );
}

#[test]
fn reload_requires_current_consent_and_keeps_edits_after_failed_or_stale_reads() {
    use retrowork_platform::{
        DocumentKind, core::COMMAND_CLEAR_OUTPUT, document_session::SourceEvent,
    };
    use retrowork_projects::{COMMAND_RELOAD, COMMAND_SAVE, DOCUMENT_SOURCE};
    let directory = project("echo ready");
    let path = directory.path().join("tool.src");
    let mut platform = platform();
    platform.shell_mut().open_document(
        DocumentKind::literal(DOCUMENT_SOURCE),
        path.to_str().unwrap(),
    );
    execute(&mut platform, COMMAND_CLEAR_OUTPUT).unwrap();
    let id = platform.shell().active_document().unwrap().id;
    settle(
        &mut platform,
        "editable source",
        |p| matches!(p.document_content(id), Some(ViewContent::Source(ref m)) if m.editing.is_some()),
    );
    let Some(ViewContent::Source(model)) = platform.document_content(id) else {
        panic!("source")
    };
    let session = model.editing.unwrap();
    let edit = |platform: &mut Platform, text: &str| {
        platform
            .view_event(
                &ViewTarget::Document(id),
                SourceEvent::Edited(text.into()).into(),
            )
            .unwrap()
    };
    edit(&mut platform, "mine");
    fs::write(&path, "external").unwrap();
    execute(&mut platform, COMMAND_RELOAD).unwrap();
    let request = platform.shell_mut().take_discard_request().unwrap();
    platform.answer_discard(request, false);
    assert_eq!(session.text(), "mine");
    execute(&mut platform, COMMAND_RELOAD).unwrap();
    let request = platform.shell_mut().take_discard_request().unwrap();
    edit(&mut platform, "newer");
    platform.answer_discard(request, true);
    assert_eq!(session.text(), "newer");
    assert!(platform.shell().status().contains("Document changed"));
    execute(&mut platform, COMMAND_RELOAD).unwrap();
    let request = platform.shell_mut().take_discard_request().unwrap();
    platform.answer_discard(request, true);
    assert!(
        platform
            .command_disabled_reason(&command(COMMAND_SAVE), None)
            .is_some()
    );
    edit(&mut platform, "during read");
    settle(
        &mut platform,
        "stale read rejected",
        |p| matches!(p.document_content(id), Some(ViewContent::Source(ref m)) if m.message.contains("changed while reloading")),
    );
    assert_eq!(session.text(), "during read");
    assert!(session.status().dirty);
    execute(&mut platform, COMMAND_RELOAD).unwrap();
    let request = platform.shell_mut().take_discard_request().unwrap();
    platform.shell_mut().open_document(
        DocumentKind::literal(DOCUMENT_SOURCE),
        directory.path().join("main.src").to_str().unwrap(),
    );
    execute(&mut platform, COMMAND_CLEAR_OUTPUT).unwrap();
    let other = platform.shell().active_document().unwrap().id;
    assert_ne!(other, id);
    platform.answer_discard(request, true);
    settle(&mut platform, "background reload", |_| {
        session.text() == "external"
    });
    assert!(platform.shell().status().starts_with("Reloaded"));
    assert!(!session.status().dirty);
    assert_eq!(platform.shell().active_document().unwrap().id, other);
    platform.activate_document(id);
    fs::remove_file(&path).unwrap();
    execute(&mut platform, COMMAND_RELOAD).unwrap();
    assert!(platform.shell_mut().take_discard_request().is_none());
    settle(
        &mut platform,
        "missing file error",
        |p| matches!(p.document_content(id), Some(ViewContent::Source(ref m)) if m.message.contains("Cannot reload")),
    );
    assert_eq!(session.text(), "external");
    fs::write(&path, "restored").unwrap();
    execute(&mut platform, COMMAND_RELOAD).unwrap();
    settle(&mut platform, "clean reload", |p| {
        p.document_content(id);
        session.text() == "restored"
    });
    edit(&mut platform, "saved after reload");
    execute(&mut platform, COMMAND_SAVE).unwrap();
    settle(&mut platform, "saved after reload", |_| {
        !session.status().saving
    });
    assert_eq!(session.status().error, None);
    assert_eq!(fs::read_to_string(path).unwrap(), "saved after reload");
}
