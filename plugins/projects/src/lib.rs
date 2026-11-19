//! Projects: a bundled module that opens build-tool projects and runs their
//! standard actions, independent of the build tool.
//!
//! It contributes
//!
//! - the **Projects** tree (left dock), showing logical folders, units and targets;
//! - **Build**, **Run**, **Test** and **Stop** (Run menu, toolbar, F11, F6,
//!   Alt+F6, Ctrl+F2) on the selected scope;
//! - **Build Output** (bottom dock): the running task's output, bounded, with
//!   its state on top;
//! - **Problems** (bottom dock): diagnostics from the last task; selecting one
//!   opens its file at the reported line in an editable **source document**;
//! - the [`ProjectRegistry`] service that build-tool plugins register a
//!   [`ProjectProvider`] with.
//!
//! One task runs at a time. Discovery and tasks run off the UI thread; their
//! results arrive through the platform inbox.

mod actions;
pub mod config;
mod discovery;
pub mod model;
mod navigation;
mod registry;
mod source;
mod views;
pub use registry::ProjectRegistry;
use views::{OutputView, ProblemsView, TreeView};
#[cfg(test)]
use {actions::summary, discovery::resolve_directory};

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};

use retrowork_platform::{
    ActivationContext, ActivationError, CommandContext, CommandDefinition, CommandError, CommandId,
    DocumentKind, MenuId, Module, ModuleId, ModuleManifest, Shell, TableColumn, TableModel,
    TableRow, TextLine, TextModel, Tone, TreeModel, TreeNode, ViewContent, ViewEvent, ViewId,
    ViewLocation, ViewProvider,
    core::MENU_FILE,
    task::{self, Batch, TaskEvent, TaskHandle, TaskOutcome},
};
use semver::Version;

pub use crate::model::{
    ActionDefinition, ActionId, Diagnostic, Interpretation, Location, Project, ProjectProvider,
    STANDARD_ACTIONS, Scope, Severity, Target, TargetKind, Unit,
};

/// Module id that build-tool plugins depend on.
pub const MODULE_ID: &str = "retrowork.projects";
/// Id of the Projects tree in the left dock.
pub const VIEW_TREE: &str = "projects.tree";
/// Id of the Problems view in the bottom dock.
pub const VIEW_PROBLEMS: &str = "projects.problems";
/// Id of the Build Output view in the bottom dock.
pub const VIEW_OUTPUT: &str = "projects.output";
/// Document kind of a source file; its resource is the path.
pub const DOCUMENT_SOURCE: &str = "projects.source";

/// Saves the active source document without blocking the UI.
pub const COMMAND_SAVE: &str = "projects.save";
/// Saves the active source under a new name through the system dialog.
pub const COMMAND_SAVE_AS: &str = "projects.save-as";
/// Saves dirty source documents across all open projects.
pub const COMMAND_SAVE_ALL: &str = "projects.save-all";
/// Reloads the active editable source after confirming discard of unsaved edits.
pub const COMMAND_RELOAD: &str = "projects.reload";
/// Id of the Run menu.
pub const MENU_RUN: &str = "run";

/// Opens the system picker or opens a configured project from a directory argument.
pub const COMMAND_OPEN: &str = "projects.open";
/// Imports a folder into a configured project.
pub const COMMAND_IMPORT: &str = "projects.import";
/// Closes the selected project without deleting files.
pub const COMMAND_CLOSE: &str = "projects.close";
/// Builds the selected scope.
pub const COMMAND_BUILD: &str = "projects.build";
/// Runs the selected scope.
pub const COMMAND_RUN: &str = "projects.run";
/// Runs the tests of the selected scope.
pub const COMMAND_TEST: &str = "projects.test";
/// Cancels the running task.
pub const COMMAND_STOP: &str = "projects.stop";

/// Output lines kept; older lines are dropped and counted.
pub const MAX_OUTPUT_LINES: usize = 10_000;
/// Largest source file shown, in bytes.
pub const MAX_SOURCE_BYTES: u64 = 4 * 1024 * 1024;
/// Largest discovery output accepted, in bytes.
pub const MAX_DISCOVERY_BYTES: usize = 32 * 1024 * 1024;
/// Longest a discovery may take.
pub const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(60);

/// Output lines of the current or last task, keeping the newest
/// [`MAX_OUTPUT_LINES`].
#[derive(Default)]
struct BoundedLog {
    lines: Rc<Vec<TextLine>>,
    dropped: usize,
}

impl BoundedLog {
    fn push(&mut self, text: String, tone: Tone) {
        let lines = Rc::make_mut(&mut self.lines);
        lines.push(TextLine { text, tone });
        if lines.len() > MAX_OUTPUT_LINES {
            let excess = lines.len() - MAX_OUTPUT_LINES;
            lines.drain(..excess);
            self.dropped += excess;
        }
    }

    fn clear(&mut self) {
        *self = Self::default();
    }
}

struct RunningTask {
    handle: TaskHandle,
    action: &'static ActionDefinition,
    target: String,
}

/// How the last task ended.
#[derive(Clone, Debug, PartialEq, Eq)]
struct TaskResult {
    action: &'static ActionDefinition,
    target: String,
    outcome: TaskOutcome,
    duration: Duration,
}

/// A successfully discovered project and the provider that owns its semantics.
struct ProjectSession {
    project: Arc<Project>,
    provider: Option<Arc<dyn ProjectProvider>>,
}

/// One admitted discovery operation, independent of the current project.
struct Discovery {
    generation: u64,
    cancellation: task::Cancellation,
    /// 0 prepares, 1 cancels, and 2 publishes configuration without further cancellation.
    publication: Arc<AtomicU8>,
}
impl Discovery {
    /// Cancels discovery unless the non-overwriting import publication has begun.
    fn cancel(&self) -> bool {
        if self
            .publication
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            self.cancellation.cancel();
        }
        self.publication.load(Ordering::Acquire) == 1
    }
}
impl Drop for Discovery {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Selection and task results owned by one open project.
struct Session {
    navigation: navigation::Navigation,
    id: u64,
    current: Option<ProjectSession>,
    /// Ids of collapsed tree folders.
    collapsed: BTreeSet<String>,
    selected: Option<String>,
    scope: Scope,
    task: Option<RunningTask>,
    last: Option<TaskResult>,
    /// Increases with every task so that late events of an earlier task are
    /// ignored.
    generation: u64,
    output: BoundedLog,
    problems: Vec<Diagnostic>,
    selected_problem: Option<usize>,
}

/// Open projects and the one pending discovery operation.
struct State {
    active: Session,
    others: BTreeMap<u64, Session>,
    next_id: u64,
    next_generation: u64,
    pending: Option<Discovery>,
    next_discovery: u64,
    open_error: Option<String>,
}
impl State {
    fn activate(&mut self, id: u64) -> Result<(), CommandError> {
        if self.active.id == id {
            return Ok(());
        }
        let next = self
            .others
            .remove(&id)
            .ok_or_else(|| CommandError::failed("project is no longer open"))?;
        let previous = std::mem::replace(&mut self.active, next);
        if previous.current.is_some() {
            self.others.insert(previous.id, previous);
        }
        Ok(())
    }
}
impl Default for Session {
    fn default() -> Self {
        Self {
            navigation: navigation::Navigation::default(),
            id: 0,
            current: None,
            collapsed: BTreeSet::new(),
            selected: None,
            scope: Scope::Project,
            task: None,
            last: None,
            generation: 0,
            output: BoundedLog::default(),
            problems: Vec::new(),
            selected_problem: None,
        }
    }
}

/// Service: the open project, the running task and their results.
pub struct ProjectWorkspace {
    registry: Rc<ProjectRegistry>,
    state: RefCell<State>,
}

const PROJECT_NODE: &str = "project";
const UNIT_PREFIX: &str = "unit:";
const TARGET_PREFIX: &str = "target:";

/// Escapes the unit portion so a separator inside its identity cannot alias another target.
fn unit_key(unit: &str) -> String {
    unit.replace('%', "%25").replace(':', "%3A")
}

/// The tree selector for a provider-owned unit identity.
fn unit_node(unit: &str) -> String {
    format!("{UNIT_PREFIX}{}", unit_key(unit))
}

fn target_node(unit: &str, target: &Target) -> String {
    format!("{TARGET_PREFIX}{}:{}", unit_key(unit), target.id)
}

impl ProjectWorkspace {
    fn new(registry: Rc<ProjectRegistry>) -> Self {
        Self {
            registry,
            state: RefCell::new(State {
                active: Session::default(),
                others: BTreeMap::new(),
                next_id: 0,
                next_generation: 0,
                pending: None,
                next_discovery: 0,
                open_error: None,
            }),
        }
    }

    /// The open project, if discovery has finished.
    pub fn project(&self) -> Option<Arc<Project>> {
        self.state
            .borrow()
            .active
            .current
            .as_ref()
            .map(|session| Arc::clone(&session.project))
    }

    /// Whether discovery is in progress, including cancellation cleanup.
    pub fn is_discovering(&self) -> bool {
        self.state.borrow().pending.is_some()
    }

    /// Whether a task is running.
    pub fn is_running(&self) -> bool {
        self.state.borrow().active.task.is_some()
            || self
                .state
                .borrow()
                .others
                .values()
                .any(|session| session.task.is_some())
    }

    /// Diagnostics of the last task, in the order the tool reported them.
    pub fn problems(&self) -> Vec<Diagnostic> {
        self.state.borrow().active.problems.clone()
    }

    /// Resolves a tree selection once, rejecting stale nodes instead of widening scope.
    fn select(&self, node: &str) -> Result<(), CommandError> {
        let (project_id, local) = node
            .split_once('/')
            .ok_or_else(|| CommandError::failed("invalid project item"))?;
        let project_id = project_id
            .strip_prefix('p')
            .and_then(|id| id.parse().ok())
            .ok_or_else(|| CommandError::failed("invalid project identity"))?;
        let mut state = self.state.borrow_mut();
        let session = if state.active.id == project_id {
            &state.active
        } else {
            state
                .others
                .get(&project_id)
                .ok_or_else(|| CommandError::failed("project is no longer open"))?
        };
        let project = &session
            .current
            .as_ref()
            .ok_or_else(|| CommandError::failed("no project is open"))?
            .project;
        let scope = if local == PROJECT_NODE || session.navigation.contains(local) {
            Some(Scope::Project)
        } else if local.starts_with(UNIT_PREFIX) {
            project
                .units
                .iter()
                .find(|unit| unit_node(&unit.id) == local)
                .map(|unit| Scope::Unit(unit.id.clone()))
        } else {
            project.units.iter().find_map(|unit| {
                unit.targets
                    .iter()
                    .find(|target| target_node(&unit.id, target) == local)
                    .map(|target| Scope::Target {
                        unit: unit.id.clone(),
                        target: target.id.clone(),
                    })
            })
        }
        .ok_or_else(|| CommandError::failed("the selected project item no longer exists"))?;
        state.activate(project_id)?;
        state.active.scope = scope;
        state.active.selected = Some(node.to_owned());
        Ok(())
    }

    /// Opens `location`'s file and selects its line.
    fn reveal(&self, location: &Location, cx: &mut CommandContext<'_>) {
        cx.shell.set_status(format!(
            "Source: {}:{}",
            location.file.display(),
            location.line
        ));
        cx.shell.open_document_with_event(
            DocumentKind::literal(DOCUMENT_SOURCE),
            location.file.display().to_string(),
            ViewEvent::RevealLine(location.line),
        );
    }
}

/// What the runner thread sends to the UI thread.
enum Update {
    Output {
        items: Vec<Interpretation>,
        /// Held until applied, which keeps the runner from running ahead.
        batch: Batch,
    },
    Finished {
        outcome: TaskOutcome,
        duration: Duration,
    },
}

/// The Projects module. Build-tool plugins depend on it and register their
/// provider with the [`ProjectRegistry`] service it offers.
#[derive(Default)]
pub struct ProjectsModule;

impl Module for ProjectsModule {
    fn manifest(&self) -> ModuleManifest {
        ModuleManifest::bundled(
            MODULE_ID,
            "Projects",
            Version::parse(env!("CARGO_PKG_VERSION")).expect("crate version is semver"),
        )
    }

    fn activate(&self, cx: &mut ActivationContext<'_>) -> Result<(), ActivationError> {
        let registry = Rc::new(ProjectRegistry::default());
        cx.provide(Rc::clone(&registry))?;
        let workspace = Rc::new(ProjectWorkspace::new(registry));
        cx.provide(Rc::clone(&workspace))?;
        cx.manage(workspace.clone());

        cx.view(
            ViewId::literal(VIEW_TREE),
            "Projects",
            ViewLocation::Left,
            10,
            Rc::new(TreeView(Rc::clone(&workspace))),
        )?;
        cx.view(
            ViewId::literal(VIEW_PROBLEMS),
            "Problems",
            ViewLocation::Bottom,
            30,
            Rc::new(ProblemsView(Rc::clone(&workspace))),
        )?;
        cx.view(
            ViewId::literal(VIEW_OUTPUT),
            "Build Output",
            ViewLocation::Bottom,
            40,
            Rc::new(OutputView(Rc::clone(&workspace))),
        )?;
        let source_loader = Rc::new(
            source::SourceFactory::new()
                .map_err(|error| ActivationError::new(error.to_string()))?,
        );
        cx.manage(source_loader.clone());
        cx.document_factory(DocumentKind::literal(DOCUMENT_SOURCE), source_loader)?;
        cx.register_command(
            CommandDefinition::new(CommandId::literal(COMMAND_SAVE), "Save", |cx| {
                let (document, _) =
                    source::save_target(cx.shell, cx.args).map_err(CommandError::failed)?;
                let provider = cx
                    .shell
                    .document(document)
                    .expect("validated document")
                    .provider
                    .clone();
                provider.handle(
                    retrowork_platform::document_session::SourceEvent::Save.into(),
                    cx,
                )
            })
            .available_when(|shell, _, args| {
                let (_, session) = source::save_target(shell, args)?;
                if session.status().dirty {
                    Ok(())
                } else {
                    Err("No changes to save".into())
                }
            }),
        )?;
        cx.menu_item(
            MenuId::literal(MENU_FILE),
            "save",
            "Save",
            CommandId::literal(COMMAND_SAVE),
            None,
            90,
        )?;
        cx.key_binding("ctrl-s", CommandId::literal(COMMAND_SAVE), None)?;
        cx.register_command(
            CommandDefinition::new(
                CommandId::literal(COMMAND_SAVE_AS),
                "Save As",
                source::save_as,
            )
            .available_when(|shell, _, args| source::save_target(shell, args).map(|_| ())),
        )?;
        cx.menu_item(
            MenuId::literal(MENU_FILE),
            "save-as",
            "Save As…",
            CommandId::literal(COMMAND_SAVE_AS),
            None,
            91,
        )?;
        cx.key_binding("ctrl-shift-s", CommandId::literal(COMMAND_SAVE_AS), None)?;
        cx.register_command(
            CommandDefinition::new(
                CommandId::literal(COMMAND_SAVE_ALL),
                "Save All",
                source::save_all,
            )
            .available_when(|shell, _, _| source::can_save_all(shell)),
        )?;
        cx.menu_item(
            MenuId::literal(MENU_FILE),
            "save-all",
            "Save All",
            CommandId::literal(COMMAND_SAVE_ALL),
            None,
            92,
        )?;
        cx.key_binding("ctrl-alt-s", CommandId::literal(COMMAND_SAVE_ALL), None)?;

        cx.register_command(
            CommandDefinition::new(
                CommandId::literal(COMMAND_RELOAD),
                "Reload from Disk",
                source::reload,
            )
            .available_when(|shell, _, args| source::save_target(shell, args).map(|_| ())),
        )?;
        cx.menu_item(
            MenuId::literal(MENU_FILE),
            "reload",
            "Reload from Disk",
            CommandId::literal(COMMAND_RELOAD),
            None,
            93,
        )?;

        for (id, title, item, import) in [
            (COMMAND_OPEN, "Open Project", "open-project", false),
            (COMMAND_IMPORT, "Import Project", "import-project", true),
        ] {
            let workspace = workspace.clone();
            cx.command(CommandId::literal(id), title, move |cx| {
                cx.shell.show_view(ViewId::literal(VIEW_TREE));
                if let Some(directory) = cx
                    .args
                    .and_then(|args| args.get("directory"))
                    .and_then(serde_json::Value::as_str)
                {
                    workspace.open(directory, import, cx)
                } else {
                    cx.shell.choose_folder(retrowork_platform::FolderRequest {
                        confirm: if import { "Import" } else { "Open" }.into(),
                        command: CommandId::literal(id),
                    });
                    Ok(())
                }
            })?;
            cx.menu_item(
                MenuId::literal(MENU_FILE),
                item,
                &format!("{title}…"),
                CommandId::literal(id),
                None,
                if import { 1 } else { 0 },
            )?;
        }
        cx.key_binding("ctrl-o", CommandId::literal(COMMAND_OPEN), None)?;
        cx.command(CommandId::literal(COMMAND_CLOSE), "Close Project", {
            let workspace = workspace.clone();
            move |cx| {
                let mut state = workspace.state.borrow_mut();
                if state.active.task.is_some() {
                    return Err(CommandError::failed(
                        "stop this project's task before closing it",
                    ));
                }
                state.active = state
                    .others
                    .pop_first()
                    .map(|(_, session)| session)
                    .unwrap_or_default();
                cx.shell.set_status("Project closed");
                Ok(())
            }
        })?;
        cx.menu_item(
            MenuId::literal(MENU_FILE),
            "close-project",
            "Close Project",
            CommandId::literal(COMMAND_CLOSE),
            None,
            2,
        )?;

        cx.menu(MenuId::literal(MENU_RUN), "Run", 160)?;
        let actions = [
            (ActionId::BUILD, "Build", COMMAND_BUILD, "build", "f11", 0),
            (ActionId::RUN, "Run", COMMAND_RUN, "run", "f6", 10),
            (ActionId::TEST, "Test", COMMAND_TEST, "test", "alt-f6", 20),
        ];
        for (action, title, command, item, key, order) in actions {
            let workspace = Rc::clone(&workspace);
            let availability = action.clone();
            cx.register_command(
                CommandDefinition::new(CommandId::literal(command), title, move |cx| {
                    workspace.start(&action, cx)
                })
                .available_when(move |_, services, _| {
                    services
                        .get::<ProjectWorkspace>()
                        .ok_or("project service unavailable")?
                        .action_available(&availability)
                }),
            )?;
            cx.menu_item(
                MenuId::literal(MENU_RUN),
                item,
                title,
                CommandId::literal(command),
                None,
                order,
            )?;
            cx.toolbar_item(
                &format!("projects.{item}"),
                title,
                CommandId::literal(command),
                None,
                40 + order,
            )?;
            cx.key_binding(key, CommandId::literal(command), None)?;
        }
        cx.register_command(
            CommandDefinition::new(CommandId::literal(COMMAND_STOP), "Stop", {
                let workspace = Rc::clone(&workspace);
                move |cx| workspace.stop(cx)
            })
            .available_when(|_, services, _| {
                if services
                    .get::<ProjectWorkspace>()
                    .is_some_and(|workspace| workspace.is_running() || workspace.is_discovering())
                {
                    Ok(())
                } else {
                    Err("nothing is running".into())
                }
            }),
        )?;
        cx.menu_item(
            MenuId::literal(MENU_RUN),
            "stop",
            "Stop",
            CommandId::literal(COMMAND_STOP),
            None,
            30,
        )?;
        cx.toolbar_item(
            "projects.stop",
            "Stop",
            CommandId::literal(COMMAND_STOP),
            None,
            70,
        )?;
        cx.key_binding("ctrl-f2", CommandId::literal(COMMAND_STOP), None)?;
        Ok(())
    }
}

impl retrowork_platform::ModuleResource for ProjectWorkspace {
    fn shutdown(&self) {
        let mut state = self.state.borrow_mut();
        state.pending = None;
        state.active.task = None;
        state.others.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn result(outcome: TaskOutcome) -> TaskResult {
        TaskResult {
            action: &STANDARD_ACTIONS[0],
            target: "app".into(),
            outcome,
            duration: Duration::from_millis(1250),
        }
    }

    fn problem(severity: Severity) -> Diagnostic {
        Diagnostic {
            severity,
            message: "m".into(),
            location: None,
        }
    }

    #[test]
    fn summary_names_the_outcome_and_counts_problems() {
        assert_eq!(
            summary(&result(TaskOutcome::Exited(0)), &[]),
            "Build app: succeeded after 1.2 s"
        );
        assert_eq!(
            summary(
                &result(TaskOutcome::Exited(101)),
                &[
                    problem(Severity::Error),
                    problem(Severity::Note),
                    problem(Severity::Warning),
                    problem(Severity::Error)
                ]
            ),
            "Build app: failed (exit code 101) after 1.2 s; 2 errors, 1 warning"
        );
        assert_eq!(
            summary(
                &result(TaskOutcome::Signalled(9)),
                &[problem(Severity::Warning)]
            ),
            "Build app: failed (signal 9) after 1.2 s; 0 errors, 1 warning"
        );
        assert_eq!(
            summary(&result(TaskOutcome::Cancelled), &[]),
            "Build app: cancelled after 1.2 s"
        );
    }

    #[test]
    fn output_keeps_the_newest_lines_and_counts_the_rest() {
        let mut log = BoundedLog::default();
        for n in 0..MAX_OUTPUT_LINES + 25 {
            log.push(n.to_string(), Tone::Normal);
        }
        assert_eq!(log.lines.len(), MAX_OUTPUT_LINES);
        assert_eq!(log.lines[0].text, "25");
        assert_eq!(log.dropped, 25);
        log.clear();
        assert!(log.lines.is_empty());
        assert_eq!(log.dropped, 0);
    }

    #[test]
    fn typed_directories_are_resolved_and_checked() {
        let directory = tempfile::tempdir().unwrap();
        let canonical = directory.path().canonicalize().unwrap();
        assert_eq!(
            resolve_directory(&format!("  {}  ", directory.path().display())).unwrap(),
            canonical
        );
        assert!(
            resolve_directory("")
                .unwrap_err()
                .contains("type the directory")
        );
        let file = directory.path().join("file");
        fs::write(&file, "").unwrap();
        assert!(
            resolve_directory(&file.display().to_string())
                .unwrap_err()
                .contains("is not a directory")
        );
        if let Some(home) = std::env::var_os("HOME") {
            assert_eq!(
                resolve_directory("~").unwrap(),
                PathBuf::from(home).canonicalize().unwrap()
            );
        }
        assert!(resolve_directory("~nobody-in-particular").is_err());
    }

    #[test]
    fn target_kinds_and_severities_have_labels() {
        assert_eq!(TargetKind::BuildScript.label(), "build script");
        assert_eq!(TargetKind::Other("custom".into()).label(), "custom");
        assert_eq!(Severity::Note.label(), "note");
    }
}
