//! Cargo support against real `cargo` on copies of the fixture projects in
//! `fixtures/projects`. The fixtures use only local dependencies, so nothing is
//! downloaded; Cargo runs offline.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use retrowork_cargo::CargoProvider;
use retrowork_platform::{
    ActivationContext, ActivationError, CommandId, Dependency, Module, ModuleManifest, Platform,
    ViewContent, ViewEvent, ViewId, ViewTarget,
    task::{Captured, OutputLine, ProcessSpec},
};
use retrowork_projects::{
    ActionId, COMMAND_BUILD, COMMAND_RUN, COMMAND_STOP, COMMAND_TEST, Interpretation, Project,
    ProjectProvider, ProjectRegistry, ProjectWorkspace, ProjectsModule, Scope, Severity,
    TargetKind, VIEW_OUTPUT, VIEW_TREE,
};
use semver::Version;

/// Variables of the test run that must not reach the Cargo under test: the
/// coverage tool instruments this process, and a shared target directory
/// would mix fixture builds with this workspace's.
const ISOLATED: &[&str] = &[
    "RUSTFLAGS",
    "CARGO_ENCODED_RUSTFLAGS",
    "RUSTDOCFLAGS",
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "CARGO_TARGET_DIR",
    "CARGO_BUILD_TARGET_DIR",
    "LLVM_PROFILE_FILE",
    "CARGO_LLVM_COV",
    "CARGO_LLVM_COV_TARGET_DIR",
];

fn isolate(mut spec: ProcessSpec) -> ProcessSpec {
    spec.env_remove
        .extend(ISOLATED.iter().map(|name| (*name).to_owned()));
    spec.env.insert("CARGO_NET_OFFLINE".into(), "true".into());
    spec
}

/// The real provider, with the test's environment isolation added.
struct Isolated(CargoProvider);

impl ProjectProvider for Isolated {
    fn actions(&self) -> &'static [retrowork_projects::ActionDefinition] {
        retrowork_projects::STANDARD_ACTIONS
    }
    fn id(&self) -> &'static str {
        self.0.id()
    }
    fn name(&self) -> &'static str {
        self.0.name()
    }
    fn detect(&self, directory: &Path) -> bool {
        self.0.detect(directory)
    }
    fn discovery(&self, directory: &Path) -> ProcessSpec {
        isolate(self.0.discovery(directory))
    }
    fn parse_discovery(&self, directory: &Path, output: &Captured) -> Result<Project, String> {
        self.0.parse_discovery(directory, output)
    }
    fn logical_roots(&self, project: &Project) -> Vec<retrowork_projects::config::LogicalRoot> {
        self.0.logical_roots(project)
    }
    fn availability(
        &self,
        project: &Project,
        action: &ActionId,
        scope: &Scope,
    ) -> Result<(), String> {
        self.0.availability(project, action, scope)
    }
    fn plan(
        &self,
        project: &Project,
        action: &ActionId,
        scope: &Scope,
        options: &serde_json::Value,
    ) -> Result<ProcessSpec, String> {
        self.0.plan(project, action, scope, options).map(isolate)
    }
    fn interpret(&self, project: &Project, line: &OutputLine) -> Vec<Interpretation> {
        self.0.interpret(project, line)
    }
}

struct IsolatedCargo;

impl Module for IsolatedCargo {
    fn manifest(&self) -> ModuleManifest {
        ModuleManifest::bundled("test.cargo", "Cargo under test", Version::new(1, 0, 0))
            .depends_on(Dependency::literal(retrowork_projects::MODULE_ID, "^0.1"))
    }
    fn activate(&self, cx: &mut ActivationContext<'_>) -> Result<(), ActivationError> {
        cx.require_service::<ProjectRegistry>()?
            .register(cx, Arc::new(Isolated(CargoProvider)))?;
        Ok(())
    }
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            if entry.file_name() != "target" {
                copy_dir(&entry.path(), &target);
            }
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// A private copy of fixture `name`.
fn fixture(name: &str) -> (tempfile::TempDir, PathBuf) {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/projects")
        .join(name);
    let directory = tempfile::tempdir().unwrap();
    let copy = directory.path().join(name);
    copy_dir(&source, &copy);
    (directory, copy.canonicalize().unwrap())
}

fn settle(
    platform: &mut Platform,
    what: &str,
    limit: Duration,
    condition: impl Fn(&Platform) -> bool,
) {
    let deadline = Instant::now() + limit;
    while !condition(platform) {
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        platform.wait(Duration::from_millis(50));
        platform.pump();
    }
}

fn workspace(platform: &Platform) -> std::rc::Rc<ProjectWorkspace> {
    platform.services().get::<ProjectWorkspace>().unwrap()
}

fn opened(directory: &Path) -> Platform {
    let mut platform = Platform::builder()
        .module(ProjectsModule)
        .module(IsolatedCargo)
        .build();
    platform
        .view_event(
            &ViewTarget::View(ViewId::literal(VIEW_TREE)),
            ViewEvent::InputSubmitted(directory.display().to_string()),
        )
        .unwrap();
    settle(
        &mut platform,
        "cargo metadata finishes",
        Duration::from_secs(60),
        |platform| workspace(platform).project().is_some(),
    );
    platform
}

fn select(platform: &mut Platform, node: &str) {
    platform
        .view_event(
            &ViewTarget::View(ViewId::literal(VIEW_TREE)),
            ViewEvent::TreeSelect(format!("p1/{node}")),
        )
        .unwrap();
}

/// Runs `command` to completion and returns the output header and lines.
fn act(platform: &mut Platform, command: &str) -> (String, Vec<String>) {
    platform
        .execute(&CommandId::new(command).unwrap(), None)
        .unwrap();
    settle(
        platform,
        "cargo finishes",
        Duration::from_secs(180),
        |platform| !workspace(platform).is_running(),
    );
    match platform.view_content(&ViewId::literal(VIEW_OUTPUT)) {
        Some(ViewContent::Text(text)) => (
            text.header.unwrap().text,
            text.lines.iter().map(|line| line.text.clone()).collect(),
        ),
        other => panic!("not text: {other:?}"),
    }
}

#[test]
fn the_workspace_fixture_is_discovered_with_all_packages_and_targets() {
    let (_keep, root) = fixture("workspace");
    let platform = opened(&root);
    let project = workspace(&platform).project().unwrap();
    assert_eq!(project.name, "workspace");
    assert_eq!(project.root, root);
    let units: Vec<String> = project
        .units
        .iter()
        .map(|unit| {
            let targets: Vec<String> = unit
                .targets
                .iter()
                .map(|target| format!("{} {}", target.kind.label(), target.name))
                .collect();
            format!("{}: {}", unit.name, targets.join(", "))
        })
        .collect();
    assert_eq!(
        units,
        [
            "app: bin app",
            "checks: lib checks",
            "greeter: lib greeter, test greeting",
            "slow: bin slow",
        ]
    );
    let app = project.unit("app").unwrap();
    assert_eq!(app.targets[0].kind, TargetKind::Binary);
    assert_eq!(app.targets[0].source, Some(root.join("app/src/main.rs")));
}

#[test]
fn build_run_and_test_report_their_outcome() {
    let (_keep, root) = fixture("workspace");
    let mut platform = opened(&root);

    let (header, lines) = act(&mut platform, COMMAND_BUILD);
    assert!(
        header.starts_with("Build workspace: succeeded after"),
        "{header}\n{lines:#?}"
    );
    assert_eq!(
        lines[0],
        "> cargo build --message-format=json --color never"
    );
    assert!(
        lines.iter().any(|line| line.contains("Finished")),
        "{lines:#?}"
    );

    select(&mut platform, "target:app:bin:app");
    let (header, lines) = act(&mut platform, COMMAND_RUN);
    assert!(
        header.starts_with("Run bin app: succeeded after"),
        "{header}"
    );
    assert!(
        lines.contains(&"Hello, RetroWork!".to_owned()),
        "{lines:#?}"
    );

    select(&mut platform, "unit:greeter");
    let (header, lines) = act(&mut platform, COMMAND_TEST);
    assert!(
        header.starts_with("Test greeter: succeeded after"),
        "{header}\n{lines:#?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("test result: ok. 1 passed"))
    );

    select(&mut platform, "unit:checks");
    let (header, lines) = act(&mut platform, COMMAND_TEST);
    assert!(
        header.starts_with("Test checks: failed (exit code 101) after"),
        "{header}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("test result: FAILED. 1 passed; 1 failed")),
        "{lines:#?}"
    );
}

#[test]
fn compiler_errors_become_located_problems() {
    let (_keep, root) = fixture("broken");
    let mut platform = opened(&root);
    let (header, lines) = act(&mut platform, COMMAND_BUILD);
    assert!(
        header.starts_with("Build broken: failed (exit code 101) after"),
        "{header}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("error[E0308]: mismatched types")),
        "{lines:#?}"
    );
    let problems = workspace(&platform).problems();
    let errors: Vec<_> = problems
        .iter()
        .filter(|problem| problem.severity == Severity::Error)
        .collect();
    assert_eq!(errors.len(), 1, "{problems:#?}");
    assert_eq!(errors[0].message, "mismatched types");
    let location = errors[0].location.as_ref().unwrap();
    assert_eq!(location.file, root.join("src/main.rs"));
    assert_eq!(location.line, 3);
    assert!(header.contains("1 error"), "{header}");
}

#[test]
fn stopping_a_running_program_ends_it() {
    let (_keep, root) = fixture("workspace");
    let mut platform = opened(&root);
    select(&mut platform, "target:slow:bin:slow");
    platform
        .execute(&CommandId::literal(COMMAND_RUN), None)
        .unwrap();
    settle(
        &mut platform,
        "the program ticks",
        Duration::from_secs(180),
        |platform| match platform.view_content(&ViewId::literal(VIEW_OUTPUT)) {
            Some(ViewContent::Text(text)) => text.lines.iter().any(|line| line.text == "tick 2"),
            _ => false,
        },
    );
    platform
        .execute(&CommandId::literal(COMMAND_STOP), None)
        .unwrap();
    settle(
        &mut platform,
        "the run ends",
        Duration::from_secs(10),
        |platform| !workspace(platform).is_running(),
    );
    let header = match platform.view_content(&ViewId::literal(VIEW_OUTPUT)) {
        Some(ViewContent::Text(text)) => text.header.unwrap().text,
        other => panic!("{other:?}"),
    };
    assert!(
        header.starts_with("Run bin slow: cancelled after"),
        "{header}"
    );
}

#[test]
fn workspace_dependencies_build_scripts_and_target_actions_work_together() {
    let (_keep, root) = fixture("dependencies");
    let mut platform = opened(&root);
    let project = workspace(&platform).project().unwrap();
    assert_eq!(
        project
            .units
            .iter()
            .map(|unit| unit.name.as_str())
            .collect::<Vec<_>>(),
        ["nondefault", "support", "workspace-app"]
    );
    let (header, lines) = act(&mut platform, COMMAND_BUILD);
    assert!(header.contains("succeeded"), "{header} {lines:?}");
    select(&mut platform, "unit:nondefault");
    let (header, lines) = act(&mut platform, COMMAND_BUILD);
    assert!(header.contains("failed"), "{header} {lines:?}");
    for (node, expected) in [
        ("unit:workspace-app", "local dependency ready"),
        (
            "target:workspace-app:bin:alternate",
            "alternate local dependency",
        ),
        (
            "target:workspace-app:example:greeting",
            "example local dependency",
        ),
    ] {
        select(&mut platform, node);
        let (header, lines) = act(&mut platform, COMMAND_RUN);
        assert!(header.contains("succeeded"), "{header} {lines:?}");
        assert!(lines.iter().any(|line| line == expected), "{lines:?}");
    }
    select(&mut platform, "target:workspace-app:test:workspace");
    let (header, lines) = act(&mut platform, COMMAND_TEST);
    assert!(header.contains("succeeded"), "{header} {lines:?}");
    assert!(
        lines.iter().any(|line| line.contains("1 passed")),
        "{lines:?}"
    );
}
