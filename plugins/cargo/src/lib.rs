//! Cargo support: a bundled module that registers a [`ProjectProvider`] for
//! directories with a `Cargo.toml`.
//!
//! - Discovery runs `cargo metadata --no-deps`, which reads the workspace
//!   without resolving or downloading dependencies.
//! - Build, run and test run `cargo build|run|test --message-format=json`,
//!   narrowed to the selected package or target.
//! - Compiler messages in that JSON become diagnostics; their rendered text
//!   and every other line are shown as output.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

use retrowork_platform::{
    ActivationContext, ActivationError, Dependency, Module, ModuleManifest, Tone,
    task::{Captured, OutputLine, ProcessSpec, Stream, TaskOutcome},
};
use retrowork_projects::{
    ActionId, Diagnostic, Interpretation, Location, Project, ProjectProvider, ProjectRegistry,
    Scope, Severity, Target, TargetKind, Unit,
};
use semver::Version;
use serde_json::Value;

/// Module id of Cargo support.
pub const MODULE_ID: &str = "retrowork.cargo";

/// The program run for every Cargo action.
const CARGO: &str = "cargo";

/// Reads and drives Cargo projects.
#[derive(Debug, Default)]
pub struct CargoProvider;

fn target_kind(kinds: &[Value]) -> TargetKind {
    let first = kinds.first().and_then(Value::as_str).unwrap_or_default();
    match first {
        "lib" | "rlib" | "dylib" | "cdylib" | "staticlib" | "proc-macro" => TargetKind::Library,
        "bin" => TargetKind::Binary,
        "test" => TargetKind::Test,
        "example" => TargetKind::Example,
        "bench" => TargetKind::Benchmark,
        "custom-build" => TargetKind::BuildScript,
        other => TargetKind::Other(other.to_owned()),
    }
}

/// Cargo's selector flag for one target of `kind`, if it has one.
fn target_flag(kind: &TargetKind) -> Option<&'static str> {
    match kind {
        TargetKind::Binary => Some("--bin"),
        TargetKind::Test => Some("--test"),
        TargetKind::Example => Some("--example"),
        TargetKind::Benchmark => Some("--bench"),
        TargetKind::Library | TargetKind::BuildScript | TargetKind::Other(_) => None,
    }
}

fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("cargo metadata has no `{key}` string"))
}

fn parse_unit(package: &Value) -> Result<Unit, String> {
    let name = string(package, "name")?.to_owned();
    let mut targets: Vec<Target> = package
        .get("targets")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("package `{name}` has no targets"))?
        .iter()
        .map(|target| {
            let kind = target_kind(
                target
                    .get("kind")
                    .and_then(Value::as_array)
                    .map_or(&[][..], Vec::as_slice),
            );
            Ok(Target {
                id: format!("{}:{}", kind.label(), string(target, "name")?),
                name: string(target, "name")?.to_owned(),
                kind,
                source: Some(PathBuf::from(string(target, "src_path")?)),
            })
        })
        .collect::<Result<_, String>>()?;
    targets.sort_by(|a, b| (&a.kind, &a.name).cmp(&(&b.kind, &b.name)));
    Ok(Unit {
        id: name.clone(),
        version: package
            .get("version")
            .and_then(Value::as_str)
            .map(str::to_owned),
        manifest: PathBuf::from(string(package, "manifest_path")?),
        name,
        targets,
    })
}

fn tool_failure(output: &Captured) -> String {
    let lines: Vec<&str> = output
        .stderr
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let tail = lines[lines.len().saturating_sub(3)..].join(" ");
    let status = match &output.outcome {
        TaskOutcome::Exited(code) => format!("exit code {code}"),
        TaskOutcome::Signalled(signal) => format!("signal {signal}"),
        TaskOutcome::Cancelled => "cancelled".to_owned(),
    };
    if tail.is_empty() {
        format!("cargo metadata failed ({status})")
    } else {
        format!("cargo metadata failed ({status}): {tail}")
    }
}

fn severity(level: &str) -> Option<Severity> {
    match level {
        "error" | "error: internal compiler error" => Some(Severity::Error),
        "warning" => Some(Severity::Warning),
        "note" | "help" => Some(Severity::Note),
        _ => None,
    }
}

fn tone(severity: Option<Severity>) -> Tone {
    match severity {
        Some(Severity::Error) => Tone::Error,
        Some(Severity::Warning) => Tone::Warning,
        _ => Tone::Normal,
    }
}

/// Reads one `compiler-message` from Cargo's JSON output.
fn compiler_message(project: &Project, message: &Value) -> Vec<Interpretation> {
    let level = message
        .get("level")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let severity = severity(level);
    let mut items: Vec<Interpretation> = message
        .get("rendered")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .lines()
        .map(|line| Interpretation::Line {
            text: line.to_owned(),
            tone: tone(severity),
        })
        .collect();
    let primary = message
        .get("spans")
        .and_then(Value::as_array)
        .and_then(|spans| {
            spans
                .iter()
                .find(|span| span.get("is_primary").and_then(Value::as_bool) == Some(true))
        });
    // Summaries such as "aborting due to 2 previous errors" have no span and
    // are not problems of their own.
    if let (Some(severity), Some(span)) = (severity, primary) {
        let number = |key: &str| {
            span.get(key)
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok())
                .unwrap_or(1)
        };
        let location = span
            .get("file_name")
            .and_then(Value::as_str)
            .map(|file| Location {
                file: project.root.join(file),
                line: number("line_start"),
                column: number("column_start"),
            });
        items.push(Interpretation::Diagnostic(Diagnostic {
            severity,
            message: message
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            location,
        }));
    }
    items
}

/// The tone of a plain output line, from the conventions of Cargo and the
/// test harness.
fn line_tone(line: &OutputLine) -> Tone {
    let text = line.text.trim_start();
    if text.starts_with("error")
        || text.ends_with("... FAILED")
        || text.starts_with("test result: FAILED")
    {
        Tone::Error
    } else if text.starts_with("warning") {
        Tone::Warning
    } else if text.starts_with("test result: ok") {
        Tone::Info
    } else if line.stream == Stream::Stderr {
        Tone::Muted
    } else {
        Tone::Normal
    }
}

/// The target selected by its provider identity, never its visible label.
fn find_target<'a>(project: &'a Project, unit: &str, id: &str) -> Result<&'a Target, String> {
    project
        .unit(unit)
        .ok_or_else(|| format!("the project has no package `{unit}`"))?
        .targets
        .iter()
        .find(|target| target.id == id)
        .ok_or_else(|| format!("package `{unit}` has no target `{id}`"))
}

/// Cargo configuration decoded at the command boundary.
#[derive(Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
struct CargoOptions {
    release: bool,
    profile: Option<String>,
    workspace: bool,
    features: Vec<String>,
    arguments: Vec<String>,
}
impl CargoOptions {
    /// Validates supported options before constructing an invocation.
    fn parse(value: &Value, action: &ActionId) -> Result<Self, String> {
        let options: Self = if value.is_null() {
            Self::default()
        } else {
            serde_json::from_value(value.clone())
                .map_err(|error| format!("invalid Cargo options: {error}"))?
        };
        if options.release && options.profile.is_some() {
            return Err("choose release or profile, not both".into());
        }
        if options.workspace && *action == ActionId::RUN {
            return Err("Cargo Run requires one runnable target".into());
        }
        if !options.arguments.is_empty() && *action == ActionId::BUILD {
            return Err("Build does not accept program arguments".into());
        }
        let valid_name = |value: &str| {
            !value.is_empty()
                && value.len() <= 256
                && !value.starts_with('-')
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_-/?.".contains(&byte))
        };
        if options
            .profile
            .as_ref()
            .is_some_and(|profile| !valid_name(profile))
            || options.features.len() > 128
            || options.features.iter().any(|feature| !valid_name(feature))
        {
            return Err("invalid Cargo profile or feature name".into());
        }
        if options.arguments.len() > 128
            || options
                .arguments
                .iter()
                .any(|argument| argument.len() > 8192 || argument.contains('\0'))
        {
            return Err("too many or oversized program arguments".into());
        }
        Ok(options)
    }
}

impl ProjectProvider for CargoProvider {
    fn actions(&self) -> &'static [retrowork_projects::ActionDefinition] {
        retrowork_projects::STANDARD_ACTIONS
    }

    fn id(&self) -> &'static str {
        "cargo"
    }

    fn name(&self) -> &'static str {
        "Cargo"
    }

    fn detect(&self, directory: &Path) -> bool {
        directory.join("Cargo.toml").is_file()
    }

    fn discovery(&self, directory: &Path) -> ProcessSpec {
        ProcessSpec::new(CARGO, directory).args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--color",
            "never",
        ])
    }

    fn parse_discovery(&self, directory: &Path, output: &Captured) -> Result<Project, String> {
        if !output.outcome.succeeded() {
            return Err(tool_failure(output));
        }
        let metadata: Value = serde_json::from_str(&output.stdout)
            .map_err(|error| format!("cargo metadata printed invalid JSON: {error}"))?;
        let root = PathBuf::from(string(&metadata, "workspace_root")?);
        let members: BTreeSet<&str> = metadata
            .get("workspace_members")
            .and_then(Value::as_array)
            .ok_or("cargo metadata has no `workspace_members` list")?
            .iter()
            .map(|id| id.as_str().ok_or("workspace member id is not a string"))
            .collect::<Result<_, _>>()?;
        let mut units: Vec<Unit> = metadata
            .get("packages")
            .and_then(Value::as_array)
            .ok_or("cargo metadata has no `packages` list")?
            .iter()
            .filter(|package| {
                package
                    .get("id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| members.contains(id))
            })
            .map(parse_unit)
            .collect::<Result<_, _>>()?;
        if units.len() != members.len() {
            return Err("cargo metadata does not describe every workspace member".to_owned());
        }
        units.sort_by(|a, b| a.name.cmp(&b.name));
        if units.windows(2).any(|pair| pair[0].id == pair[1].id) {
            return Err("cargo metadata has duplicate workspace package names".to_owned());
        }
        let name = match units.as_slice() {
            [single] if single.manifest.parent() == Some(root.as_path()) => single.name.clone(),
            _ => root
                .file_name()
                .or_else(|| directory.file_name())
                .map_or_else(
                    || "workspace".to_owned(),
                    |name| name.to_string_lossy().into_owned(),
                ),
        };
        Ok(Project {
            provider: self.id().to_owned(),
            name,
            root,
            units,
        })
    }

    fn logical_roots(&self, project: &Project) -> Vec<retrowork_projects::config::LogicalRoot> {
        use retrowork_projects::config::{Group, LogicalRoot, conventional_roots};
        let mut roots = conventional_roots(project);
        roots.extend(["Cargo.toml", "Cargo.lock"].map(|path| LogicalRoot {
            group: Group::ProjectFiles,
            path: path.into(),
        }));
        roots
    }

    fn availability(
        &self,
        project: &Project,
        action: &ActionId,
        scope: &Scope,
    ) -> Result<(), String> {
        if !self
            .actions()
            .iter()
            .any(|definition| definition.id == action.as_str())
        {
            return Err(format!("unsupported Cargo action `{}`", action.as_str()));
        }
        match scope {
            Scope::Project => {}
            Scope::Unit(unit) => {
                let package = project
                    .unit(unit)
                    .ok_or_else(|| format!("the project has no package `{unit}`"))?;
                if *action == ActionId::RUN
                    && !package
                        .targets
                        .iter()
                        .any(|target| target.kind == TargetKind::Binary)
                {
                    return Err(format!(
                        "package `{unit}` has no binary to run; select an example explicitly"
                    ));
                }
            }
            Scope::Target { unit, target } => {
                let target = find_target(project, unit, target)?;
                let (kind, name) = (&target.kind, &target.name);
                if matches!(kind, TargetKind::BuildScript | TargetKind::Other(_)) {
                    return Err(format!(
                        "{} {name} cannot be selected independently; select its package",
                        kind.label()
                    ));
                }
                if *action == ActionId::RUN
                    && !matches!(kind, TargetKind::Binary | TargetKind::Example)
                {
                    return Err(format!(
                        "{} {name} cannot be run; select a binary or an example",
                        kind.label()
                    ));
                }
            }
        }
        Ok(())
    }

    fn plan(
        &self,
        project: &Project,
        action: &ActionId,
        scope: &Scope,
        options: &Value,
    ) -> Result<ProcessSpec, String> {
        self.availability(project, action, scope)?;
        let options = CargoOptions::parse(options, action)?;
        if options.workspace && *scope != Scope::Project {
            return Err("whole-workspace configuration requires project scope".into());
        }
        let subcommand = action.as_str();
        let mut args = vec![subcommand.to_owned()];
        match scope {
            Scope::Project => {}
            Scope::Unit(unit) => {
                args.extend(["-p".to_owned(), unit.clone()]);
            }
            Scope::Target { unit, target } => {
                let target = find_target(project, unit, target)?;
                let (kind, name) = (&target.kind, &target.name);
                args.extend(["-p".to_owned(), unit.clone()]);
                match (target_flag(kind), kind) {
                    (Some(flag), _) => args.extend([flag.to_owned(), name.clone()]),
                    (None, TargetKind::Library) => args.push("--lib".to_owned()),
                    (None, _) => {}
                }
            }
        }
        if options.workspace {
            args.push("--workspace".into());
        }
        if options.release {
            args.push("--release".into());
        }
        if let Some(profile) = options.profile {
            args.push(format!("--profile={profile}"));
        }
        if !options.features.is_empty() {
            args.push(format!("--features={}", options.features.join(",")));
        }
        args.extend([
            "--message-format=json".to_owned(),
            "--color".to_owned(),
            "never".to_owned(),
        ]);
        if !options.arguments.is_empty() {
            args.push("--".into());
            args.extend(options.arguments);
        }
        Ok(ProcessSpec::new(CARGO, &project.root).args(args))
    }

    fn interpret(&self, project: &Project, line: &OutputLine) -> Vec<Interpretation> {
        if line.stream == Stream::Stdout
            && line.text.starts_with('{')
            && let Ok(message) = serde_json::from_str::<Value>(&line.text)
        {
            return match message.get("reason").and_then(Value::as_str) {
                Some("compiler-message") => message
                    .get("message")
                    .map(|inner| compiler_message(project, inner))
                    .unwrap_or_default(),
                // Artifacts, build scripts and the final summary are reported
                // by Cargo's own status lines on stderr.
                Some("compiler-artifact" | "build-script-executed" | "build-finished") => {
                    Vec::new()
                }
                _ => vec![Interpretation::Line {
                    text: line.text.clone(),
                    tone: Tone::Normal,
                }],
            };
        }
        let mut text = line.text.clone();
        if line.truncated {
            text.push_str(" …");
        }
        vec![Interpretation::Line {
            tone: line_tone(line),
            text,
        }]
    }
}

/// Registers [`CargoProvider`] with the Projects module.
#[derive(Default)]
pub struct CargoModule;

impl Module for CargoModule {
    fn manifest(&self) -> ModuleManifest {
        ModuleManifest::bundled(
            MODULE_ID,
            "Cargo",
            Version::parse(env!("CARGO_PKG_VERSION")).expect("crate version is semver"),
        )
        .depends_on(Dependency::literal(retrowork_projects::MODULE_ID, "^0.1"))
    }

    fn activate(&self, cx: &mut ActivationContext<'_>) -> Result<(), ActivationError> {
        let registry: Rc<ProjectRegistry> = cx.require_service()?;
        registry.register(cx, Arc::new(CargoProvider))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn project() -> Project {
        Project {
            provider: "cargo".into(),
            name: "workspace".into(),
            root: PathBuf::from("/work"),
            units: vec![Unit {
                id: "app".into(),
                name: "app".into(),
                version: Some("0.1.0".into()),
                manifest: PathBuf::from("/work/app/Cargo.toml"),
                targets: [
                    (TargetKind::Binary, "tool"),
                    (TargetKind::Example, "demo"),
                    (TargetKind::Test, "it"),
                    (TargetKind::Library, "app"),
                    (TargetKind::Benchmark, "speed"),
                    (TargetKind::BuildScript, "build"),
                    (TargetKind::Other("future".into()), "unknown"),
                ]
                .into_iter()
                .map(|(kind, name)| Target {
                    id: format!("{}:{name}", kind.label()),
                    kind,
                    name: name.into(),
                    source: Some(PathBuf::from("/work/app/src/lib.rs")),
                })
                .collect(),
            }],
        }
    }

    fn captured(stdout: &str, outcome: TaskOutcome, stderr: &str) -> Captured {
        Captured {
            stdout: stdout.into(),
            stderr: stderr.into(),
            outcome,
        }
    }

    fn args(action: ActionId, scope: Scope) -> Result<Vec<String>, String> {
        CargoProvider
            .plan(&project(), &action, &scope, &Value::Null)
            .map(|spec| spec.args)
    }

    fn target(kind: TargetKind, name: &str) -> Scope {
        Scope::Target {
            unit: "app".into(),
            target: format!("{}:{name}", kind.label()),
        }
    }

    const TAIL: [&str; 3] = ["--message-format=json", "--color", "never"];

    fn expect(parts: &[&str]) -> Vec<String> {
        parts
            .iter()
            .chain(TAIL.iter())
            .map(|part| (*part).to_owned())
            .collect()
    }

    #[test]
    fn configuration_is_validated_before_process_construction() {
        let spec = CargoProvider.plan(&project(), &ActionId::RUN, &Scope::Project, &json!({"release": true, "features": ["app/fast"], "arguments": ["--flag", "two words"]})).unwrap();
        assert_eq!(
            spec.args,
            [
                "run",
                "--release",
                "--features=app/fast",
                "--message-format=json",
                "--color",
                "never",
                "--",
                "--flag",
                "two words"
            ]
        );
        for options in [
            json!({"unknown": true}),
            json!({"release": true, "profile": "dev"}),
            json!({"features": ["--bad"]}),
            json!({"profile": ""}),
            json!({"arguments": ["arg"]}),
        ] {
            assert!(
                CargoProvider
                    .plan(&project(), &ActionId::BUILD, &Scope::Project, &options)
                    .is_err()
            );
        }
        assert!(
            CargoProvider
                .plan(
                    &project(),
                    &ActionId::RUN,
                    &Scope::Project,
                    &json!({"workspace": true})
                )
                .is_err()
        );
        let spec = CargoProvider
            .plan(
                &project(),
                &ActionId::BUILD,
                &Scope::Project,
                &json!({"workspace": true, "profile": "dev"}),
            )
            .unwrap();
        assert!(spec.args.contains(&"--workspace".to_owned()));
        assert!(spec.args.contains(&"--profile=dev".to_owned()));
        assert!(
            CargoProvider
                .availability(
                    &project(),
                    &ActionId::new("cargo.unknown").unwrap(),
                    &Scope::Project
                )
                .is_err()
        );
    }

    #[test]
    fn metadata_becomes_a_sorted_project() {
        let metadata = json!({
            "workspace_root": "/work",
            "workspace_members": ["id:zeta", "id:alpha"],
            "packages": [
                { "id": "non-member" },
                {
                    "id": "id:zeta", "name": "zeta", "version": "1.0.0",
                    "manifest_path": "/work/zeta/Cargo.toml",
                    "targets": [
                        { "name": "zeta", "kind": ["bin"], "src_path": "/work/zeta/src/main.rs" },
                        { "name": "zeta", "kind": ["lib"], "src_path": "/work/zeta/src/lib.rs" },
                        { "name": "it", "kind": ["test"], "src_path": "/work/zeta/tests/it.rs" },
                        { "name": "build-script-build", "kind": ["custom-build"], "src_path": "/work/zeta/build.rs" },
                        { "name": "odd", "kind": ["weird"], "src_path": "/work/zeta/odd.rs" }
                    ]
                },
                {
                    "id": "id:alpha", "name": "alpha", "version": "0.2.0",
                    "manifest_path": "/work/alpha/Cargo.toml",
                    "targets": [
                        { "name": "alpha", "kind": ["proc-macro"], "src_path": "/work/alpha/src/lib.rs" }
                    ]
                }
            ]
        });
        let project = CargoProvider
            .parse_discovery(
                Path::new("/work"),
                &captured(&metadata.to_string(), TaskOutcome::Exited(0), ""),
            )
            .unwrap();
        assert_eq!(project.name, "work");
        assert_eq!(project.root, PathBuf::from("/work"));
        let names: Vec<&str> = project
            .units
            .iter()
            .map(|unit| unit.name.as_str())
            .collect();
        assert_eq!(names, ["alpha", "zeta"]);
        let kinds: Vec<String> = project.units[1]
            .targets
            .iter()
            .map(|target| format!("{} {}", target.kind.label(), target.name))
            .collect();
        assert_eq!(
            kinds,
            [
                "lib zeta",
                "bin zeta",
                "test it",
                "build script build-script-build",
                "weird odd"
            ]
        );
        assert_eq!(project.units[0].targets[0].kind, TargetKind::Library);
    }

    #[test]
    fn a_single_package_at_the_root_names_the_project() {
        let metadata = json!({
            "workspace_root": "/work/hello",
            "workspace_members": ["id:hello"],
            "packages": [{
                "id": "id:hello", "name": "hello", "version": "0.1.0",
                "manifest_path": "/work/hello/Cargo.toml",
                "targets": [{ "name": "hello", "kind": ["bin"], "src_path": "/work/hello/src/main.rs" }]
            }]
        });
        let project = CargoProvider
            .parse_discovery(
                Path::new("/work/hello"),
                &captured(&metadata.to_string(), TaskOutcome::Exited(0), ""),
            )
            .unwrap();
        assert_eq!(project.name, "hello");
    }

    #[test]
    fn discovery_failures_say_why() {
        let failed = CargoProvider
            .parse_discovery(
                Path::new("/work"),
                &captured(
                    "",
                    TaskOutcome::Exited(101),
                    "error: failed to parse manifest at `/work/Cargo.toml`\n\nCaused by:\n  missing field `name`\n",
                ),
            )
            .unwrap_err();
        assert_eq!(
            failed,
            "cargo metadata failed (exit code 101): error: failed to parse manifest at `/work/Cargo.toml` Caused by:   missing field `name`"
        );
        let silent = CargoProvider
            .parse_discovery(
                Path::new("/w"),
                &captured("", TaskOutcome::Signalled(9), ""),
            )
            .unwrap_err();
        assert_eq!(silent, "cargo metadata failed (signal 9)");
        let garbage = CargoProvider
            .parse_discovery(
                Path::new("/w"),
                &captured("not json", TaskOutcome::Exited(0), ""),
            )
            .unwrap_err();
        assert!(garbage.contains("invalid JSON"));
        let incomplete = CargoProvider
            .parse_discovery(Path::new("/w"), &captured("{}", TaskOutcome::Exited(0), ""))
            .unwrap_err();
        assert!(incomplete.contains("workspace_root"));
        let no_targets = json!({ "workspace_root": "/w", "workspace_members": ["x"], "packages": [{ "id": "x", "name": "x", "manifest_path": "/w/Cargo.toml" }] });
        assert!(
            CargoProvider
                .parse_discovery(
                    Path::new("/w"),
                    &captured(&no_targets.to_string(), TaskOutcome::Exited(0), "")
                )
                .unwrap_err()
                .contains("has no targets")
        );
    }

    #[test]
    fn actions_are_narrowed_to_the_selected_scope() {
        assert_eq!(
            args(ActionId::BUILD, Scope::Project).unwrap(),
            expect(&["build"])
        );
        assert_eq!(
            args(ActionId::TEST, Scope::Project).unwrap(),
            expect(&["test"])
        );
        assert_eq!(
            args(ActionId::RUN, Scope::Project).unwrap(),
            expect(&["run"])
        );
        assert_eq!(
            args(ActionId::BUILD, Scope::Unit("app".into())).unwrap(),
            expect(&["build", "-p", "app"])
        );
        assert_eq!(
            args(ActionId::RUN, target(TargetKind::Binary, "tool")).unwrap(),
            expect(&["run", "-p", "app", "--bin", "tool"])
        );
        assert_eq!(
            args(ActionId::RUN, target(TargetKind::Example, "demo")).unwrap(),
            expect(&["run", "-p", "app", "--example", "demo"])
        );
        assert_eq!(
            args(ActionId::TEST, target(TargetKind::Test, "it")).unwrap(),
            expect(&["test", "-p", "app", "--test", "it"])
        );
        assert_eq!(
            args(ActionId::TEST, target(TargetKind::Library, "app")).unwrap(),
            expect(&["test", "-p", "app", "--lib"])
        );
        assert_eq!(
            args(ActionId::BUILD, target(TargetKind::Benchmark, "speed")).unwrap(),
            expect(&["build", "-p", "app", "--bench", "speed"])
        );
        assert!(
            args(ActionId::BUILD, target(TargetKind::BuildScript, "build"))
                .unwrap_err()
                .contains("cannot be selected independently")
        );
        assert!(
            args(
                ActionId::BUILD,
                target(TargetKind::Other("future".into()), "unknown")
            )
            .unwrap_err()
            .contains("cannot be selected independently")
        );
        assert!(
            args(ActionId::RUN, target(TargetKind::Binary, "removed"))
                .unwrap_err()
                .contains("has no target")
        );
        assert!(
            args(
                ActionId::BUILD,
                Scope::Target {
                    unit: "removed".into(),
                    target: "bin:tool".into(),
                }
            )
            .unwrap_err()
            .contains("has no package")
        );
        let spec = CargoProvider
            .plan(&project(), &ActionId::BUILD, &Scope::Project, &Value::Null)
            .unwrap();
        assert_eq!(
            (spec.program.as_str(), spec.cwd.as_path()),
            (CARGO, Path::new("/work"))
        );
    }

    #[test]
    fn impossible_actions_are_refused_with_a_reason() {
        assert_eq!(
            args(ActionId::RUN, target(TargetKind::Library, "app")).unwrap_err(),
            "lib app cannot be run; select a binary or an example"
        );
        assert_eq!(
            args(ActionId::BUILD, Scope::Unit("ghost".into())).unwrap_err(),
            "the project has no package `ghost`"
        );
    }

    fn stdout(text: &str) -> OutputLine {
        OutputLine {
            stream: Stream::Stdout,
            text: text.into(),
            truncated: false,
        }
    }

    fn stderr(text: &str) -> OutputLine {
        OutputLine {
            stream: Stream::Stderr,
            ..stdout(text)
        }
    }

    fn interpret(line: OutputLine) -> Vec<Interpretation> {
        CargoProvider.interpret(&project(), &line)
    }

    fn line(text: &str, tone: Tone) -> Interpretation {
        Interpretation::Line {
            text: text.into(),
            tone,
        }
    }

    #[test]
    fn compiler_messages_become_rendered_lines_and_a_located_diagnostic() {
        let message = json!({
            "reason": "compiler-message",
            "message": {
                "level": "error",
                "message": "mismatched types",
                "rendered": "error[E0308]: mismatched types\n --> app/src/main.rs:3:18\n",
                "spans": [
                    { "file_name": "app/src/other.rs", "line_start": 9, "column_start": 1, "is_primary": false },
                    { "file_name": "app/src/main.rs", "line_start": 3, "column_start": 18, "is_primary": true }
                ]
            }
        });
        assert_eq!(
            interpret(stdout(&message.to_string())),
            [
                line("error[E0308]: mismatched types", Tone::Error),
                line(" --> app/src/main.rs:3:18", Tone::Error),
                Interpretation::Diagnostic(Diagnostic {
                    severity: Severity::Error,
                    message: "mismatched types".into(),
                    location: Some(Location {
                        file: PathBuf::from("/work/app/src/main.rs"),
                        line: 3,
                        column: 18,
                    }),
                }),
            ]
        );
    }

    #[test]
    fn messages_without_a_primary_span_are_only_output() {
        let summary = json!({
            "reason": "compiler-message",
            "message": { "level": "error", "message": "aborting", "rendered": "error: aborting due to 1 previous error\n", "spans": [] }
        });
        assert_eq!(
            interpret(stdout(&summary.to_string())),
            [line("error: aborting due to 1 previous error", Tone::Error)]
        );
        let unknown_level = json!({
            "reason": "compiler-message",
            "message": { "level": "failure-note", "message": "x", "rendered": "For more information…\n",
                         "spans": [{ "file_name": "a.rs", "line_start": 1, "column_start": 1, "is_primary": true }] }
        });
        assert_eq!(
            interpret(stdout(&unknown_level.to_string())),
            [line("For more information…", Tone::Normal)]
        );
        let warning = json!({
            "reason": "compiler-message",
            "message": { "level": "warning", "message": "unused", "rendered": "",
                         "spans": [{ "file_name": "/abs/lib.rs", "is_primary": true }] }
        });
        assert_eq!(
            interpret(stdout(&warning.to_string())),
            [Interpretation::Diagnostic(Diagnostic {
                severity: Severity::Warning,
                message: "unused".into(),
                location: Some(Location {
                    file: PathBuf::from("/abs/lib.rs"),
                    line: 1,
                    column: 1
                }),
            })]
        );
    }

    #[test]
    fn cargo_events_are_hidden_and_application_output_is_preserved() {
        for reason in [
            "compiler-artifact",
            "build-script-executed",
            "build-finished",
        ] {
            assert!(interpret(stdout(&json!({ "reason": reason }).to_string())).is_empty());
        }
        assert_eq!(
            interpret(stdout(r#"{"reason":"application-event","value":42}"#)),
            [line(
                r#"{"reason":"application-event","value":42}"#,
                Tone::Normal
            )]
        );
        assert_eq!(
            interpret(stdout(r#"{"program":"output"}"#)),
            [line(r#"{"program":"output"}"#, Tone::Normal)]
        );
        assert_eq!(
            interpret(stdout("{not json")),
            [line("{not json", Tone::Normal)]
        );
        assert_eq!(interpret(stdout("Hello")), [line("Hello", Tone::Normal)]);
        assert_eq!(
            interpret(stderr("   Compiling app v0.1.0")),
            [line("   Compiling app v0.1.0", Tone::Muted)]
        );
        assert_eq!(
            interpret(stderr("error: could not compile")),
            [line("error: could not compile", Tone::Error)]
        );
        assert_eq!(
            interpret(stderr("warning: unused")),
            [line("warning: unused", Tone::Warning)]
        );
        assert_eq!(
            interpret(stdout("test fails ... FAILED")),
            [line("test fails ... FAILED", Tone::Error)]
        );
        assert_eq!(
            interpret(stdout("test result: FAILED. 1 passed; 1 failed")),
            [line("test result: FAILED. 1 passed; 1 failed", Tone::Error)]
        );
        assert_eq!(
            interpret(stdout("test result: ok. 2 passed")),
            [line("test result: ok. 2 passed", Tone::Info)]
        );
        let cut = OutputLine {
            truncated: true,
            ..stdout("long")
        };
        assert_eq!(interpret(cut), [line("long …", Tone::Normal)]);
    }

    #[test]
    fn detection_looks_for_a_manifest() {
        let directory = tempfile::tempdir().unwrap();
        assert!(!CargoProvider.detect(directory.path()));
        std::fs::write(directory.path().join("Cargo.toml"), "").unwrap();
        assert!(CargoProvider.detect(directory.path()));
        assert_eq!(
            CargoProvider.discovery(directory.path()).command_line(),
            "cargo metadata --format-version 1 --no-deps --color never"
        );
    }
}
