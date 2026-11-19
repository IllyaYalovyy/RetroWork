//! What a project provider describes and how it is asked to act.
//!
//! A build-tool provider turns a directory into
//! a [`Project`], plans the standard actions as tool invocations, and reads
//! the tool's output. It never runs anything itself: the Projects module runs
//! what the provider plans, with the platform's task runner, so every provider
//! gets the same streaming, bounds, cancellation and state reporting.

use std::{
    borrow::Cow,
    path::{Path, PathBuf},
};

use retrowork_platform::{
    Tone,
    task::{Captured, OutputLine, ProcessSpec},
};

/// A project as its build tool reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Project {
    /// Id of the provider that discovered it, e.g. `cargo`.
    pub provider: String,
    pub name: String,
    /// Directory paths in diagnostics are relative to.
    pub root: PathBuf,
    pub units: Vec<Unit>,
}

impl Project {
    /// Rejects ambiguous or missing identities before a session becomes visible.
    pub fn validate(&self, provider: &str) -> Result<(), String> {
        if self.provider != provider {
            return Err("discovered project belongs to a different provider".into());
        }
        let mut units = std::collections::BTreeSet::new();
        for unit in &self.units {
            if unit.id.is_empty() || !units.insert(&unit.id) {
                return Err("duplicate or empty project unit ID".into());
            }
            let mut targets = std::collections::BTreeSet::new();
            for target in &unit.targets {
                if target.id.is_empty() || !targets.insert(&target.id) {
                    return Err("duplicate or empty project target ID".into());
                }
            }
        }
        Ok(())
    }

    /// The unit with id `id`.
    pub fn unit(&self, id: &str) -> Option<&Unit> {
        self.units.iter().find(|unit| unit.id == id)
    }
}

/// A buildable part of a project: a Cargo package, a Maven module, a Go module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unit {
    /// Stable within the project; what the provider uses to select the unit.
    pub id: String,
    pub name: String,
    pub version: Option<String>,
    pub manifest: PathBuf,
    pub targets: Vec<Target>,
}

/// Something a unit produces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// Provider-owned identity, independent of the displayed name or kind label.
    pub id: String,
    pub name: String,
    pub kind: TargetKind,
    /// The main source file, opened when the target is activated.
    pub source: Option<PathBuf>,
}

/// What kind of artifact a target is.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum TargetKind {
    Library,
    Binary,
    Test,
    Example,
    Benchmark,
    BuildScript,
    /// A kind the provider knows but the workbench has no name for.
    Other(String),
}

impl TargetKind {
    /// Short name shown next to the target, e.g. `bin`.
    pub fn label(&self) -> &str {
        match self {
            TargetKind::Library => "lib",
            TargetKind::Binary => "bin",
            TargetKind::Test => "test",
            TargetKind::Example => "example",
            TargetKind::Benchmark => "bench",
            TargetKind::BuildScript => "build script",
            TargetKind::Other(name) => name,
        }
    }
}

/// A validated provider action, including shared IDE actions.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ActionId(Cow<'static, str>);
impl ActionId {
    /// The shared build action.
    pub const BUILD: Self = Self(Cow::Borrowed("build"));
    /// The shared run action.
    pub const RUN: Self = Self(Cow::Borrowed("run"));
    /// The shared test action.
    pub const TEST: Self = Self(Cow::Borrowed("test"));
    /// Validates an action name supplied by a provider or command.
    pub fn new(id: &str) -> Result<Self, retrowork_platform::IdError> {
        retrowork_platform::CommandId::new(id)?;
        Ok(Self(Cow::Owned(id.to_owned())))
    }
    /// The stable name passed between project commands and their provider.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A provider-declared action shown through the shared project command infrastructure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActionDefinition {
    pub id: &'static str,
    pub title: &'static str,
}

/// Shared action names; providers explicitly declare the ones they support.
pub const STANDARD_ACTIONS: &[ActionDefinition] = &[
    ActionDefinition {
        id: "build",
        title: "Build",
    },
    ActionDefinition {
        id: "run",
        title: "Run",
    },
    ActionDefinition {
        id: "test",
        title: "Test",
    },
];

/// What an action applies to, from the selection in the project tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    /// The whole project.
    Project,
    /// One unit, by id.
    Unit(String),
    /// One target of a unit.
    Target { unit: String, target: String },
}

/// How serious a diagnostic is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warning,
    Note,
}

impl Severity {
    /// Name shown in the Problems view.
    pub fn label(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Note => "note",
        }
    }
}

/// A place in a source file; line and column count from 1.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Location {
    pub file: PathBuf,
    pub line: usize,
    pub column: usize,
}

/// A problem a tool reported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    pub location: Option<Location>,
}

/// What one line of tool output means.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Interpretation {
    /// Text to show in the build output.
    Line { text: String, tone: Tone },
    /// A problem to list in the Problems view.
    Diagnostic(Diagnostic),
}

/// Knows one kind of project.
///
/// Implementations must be pure: they describe tool invocations and read
/// tool output, but do not start processes, touch the UI or keep state.
/// They are called from worker threads, so they are `Send + Sync`.
pub trait ProjectProvider: Send + Sync {
    /// Stable id, e.g. `cargo`.
    fn id(&self) -> &'static str;

    /// Name shown to users, e.g. `Cargo`.
    fn name(&self) -> &'static str;

    /// Actions this provider supports; custom IDs must start with the provider ID and a dot.
    fn actions(&self) -> &'static [ActionDefinition];

    /// Whether `directory` looks like a project of this kind. Must be cheap:
    /// it only looks at files.
    fn detect(&self, directory: &Path) -> bool;

    /// The tool invocation that describes the project in `directory`.
    fn discovery(&self, directory: &Path) -> ProcessSpec;

    /// Reads what the discovery invocation printed. Errors are shown to the
    /// user as the reason the project could not be opened.
    fn parse_discovery(&self, directory: &Path, output: &Captured) -> Result<Project, String>;

    /// Suggests logical files and directories relative to the project root. Missing paths are omitted.
    fn logical_roots(&self, project: &Project) -> Vec<crate::config::LogicalRoot> {
        crate::config::conventional_roots(project)
    }

    /// Checks support without constructing a process or performing I/O.
    fn availability(
        &self,
        project: &Project,
        action: &ActionId,
        scope: &Scope,
    ) -> Result<(), String>;

    /// The tool invocation for `action` on `scope`. Errors explain why the
    /// action does not apply, e.g. running a library. Called only when executing; validate the request again before planning.
    fn plan(
        &self,
        project: &Project,
        action: &ActionId,
        scope: &Scope,
        options: &serde_json::Value,
    ) -> Result<ProcessSpec, String>;

    /// What one line of output from a planned invocation means. May return
    /// several items, e.g. a diagnostic and the lines that render it.
    fn interpret(&self, project: &Project, line: &OutputLine) -> Vec<Interpretation>;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identities_are_unambiguous_and_actions_are_extensible() {
        let target = Target {
            id: "opaque".into(),
            name: "label".into(),
            kind: TargetKind::Other("rule".into()),
            source: None,
        };
        let unit = Unit {
            id: "unit".into(),
            name: "label".into(),
            version: None,
            manifest: "build".into(),
            targets: vec![target],
        };
        let mut project = Project {
            provider: "custom".into(),
            name: "project".into(),
            root: "/work".into(),
            units: vec![unit],
        };
        assert!(project.validate("custom").is_ok());
        assert!(project.validate("other").is_err());
        let duplicate = project.units[0].targets[0].clone();
        project.units[0].targets.push(duplicate);
        assert!(project.validate("custom").is_err());
        project.units[0].targets.pop();
        project.units.push(project.units[0].clone());
        assert!(project.validate("custom").is_err());
        assert_eq!(
            ActionId::new("custom.check").unwrap().as_str(),
            "custom.check"
        );
        assert!(ActionId::new("bad action").is_err());
    }
}
