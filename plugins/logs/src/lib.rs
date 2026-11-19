//! Log Explorer: a bundled module contributing
//!
//! - the **Sources** tree (left dock), filled through the `logs.sources`
//!   extension point,
//! - the **log document** (centre): a records table with a filter,
//! - the **Record** detail view (bottom dock),
//! - commands to focus, set and clear the filter and to reload a log,
//! - the [`LogWorkspace`] service other modules use to read the active log.
//!
//! Everything here is toolkit-independent; the workbench draws the models.

pub mod query;
pub mod record;

use std::{
    cell::RefCell,
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    rc::Rc,
};

use retrowork_platform::{
    ActivationContext, ActivationError, CommandContext, CommandError, CommandId, DocumentFactory,
    DocumentKind, FilterModel, FocusTarget, MenuId, Module, ModuleId, ModuleManifest,
    OpenedDocument, PropertyModel, Shell, TableColumn, TableModel, TableRow, Tone, TreeModel,
    TreeNode, ViewContent, ViewEvent, ViewId, ViewLocation, ViewProvider, core::MENU_VIEW,
};
use semver::Version;
use serde_json::{Value, json};

use crate::{
    query::Query,
    record::{Level, LogRecord, parse_log},
};

/// Module id that packages and modules name to depend on the Log Explorer.
pub const MODULE_ID: &str = "retrowork.logs";
/// Document kind of an opened log; its resource is the source id.
pub const DOCUMENT_KIND: &str = "logs.source";
/// Id of the Sources tree in the left dock.
pub const VIEW_SOURCES: &str = "logs.sources";
/// Id of the Record view in the bottom dock.
pub const VIEW_RECORD: &str = "logs.record";
/// Id of the Filters menu, which packages may add saved filters to.
pub const MENU_FILTERS: &str = "filters";
/// Extension point: `{ id, title, group, path }` adds a log file to Sources.
pub const POINT_SOURCES: &str = "logs.sources";

/// Moves keyboard focus to the active log's filter field.
pub const COMMAND_FILTER_FOCUS: &str = "logs.filter.focus";
/// Sets the active log's filter to the `query` argument.
pub const COMMAND_FILTER_SET: &str = "logs.filter.set";
/// Clears the active log's filter.
pub const COMMAND_FILTER_CLEAR: &str = "logs.filter.clear";
/// Reads the active log from disk again, keeping its filter.
pub const COMMAND_RELOAD: &str = "logs.reload";

/// Largest log file opened, in bytes. Files are read into memory whole.
pub const MAX_LOG_BYTES: u64 = 64 * 1024 * 1024;

/// A log file contributed through the `logs.sources` extension point.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogSource {
    /// Stable id; also the document resource and what gets persisted.
    pub id: String,
    pub title: String,
    /// Folder the source is listed under.
    pub group: String,
    pub path: PathBuf,
    /// Module that contributed the source.
    pub owner: ModuleId,
}

struct DocumentState {
    records: Rc<Vec<LogRecord>>,
    filter: String,
    query: Query,
    /// Indices into `records` that pass the filter.
    visible: Rc<Vec<usize>>,
    rows: Rc<Vec<TableRow>>,
    /// Index into `visible`.
    selected: Option<usize>,
}

/// One opened log: its records, filter and selection.
pub struct LogDocument {
    source: LogSource,
    state: RefCell<DocumentState>,
}

/// The records of a log that pass its filter.
pub struct FilteredRecords {
    records: Rc<Vec<LogRecord>>,
    visible: Rc<Vec<usize>>,
}

impl FilteredRecords {
    /// The records that pass the filter, in file order.
    pub fn iter(&self) -> impl Iterator<Item = &LogRecord> {
        self.visible.iter().map(|&index| &self.records[index])
    }

    /// Number of records that pass the filter.
    pub fn len(&self) -> usize {
        self.visible.len()
    }

    /// Whether no record passes the filter.
    pub fn is_empty(&self) -> bool {
        self.visible.is_empty()
    }

    /// Number of records in the log before filtering.
    pub fn total(&self) -> usize {
        self.records.len()
    }
}

fn tone(level: Level) -> Tone {
    match level {
        Level::Error => Tone::Error,
        Level::Warn => Tone::Warning,
        Level::Info => Tone::Normal,
        Level::Debug | Level::Trace | Level::Unknown => Tone::Muted,
    }
}

impl LogDocument {
    fn new(source: LogSource, records: Vec<LogRecord>) -> Self {
        let document = Self {
            source,
            state: RefCell::new(DocumentState {
                records: Rc::new(records),
                filter: String::new(),
                query: Query::default(),
                visible: Rc::default(),
                rows: Rc::default(),
                selected: None,
            }),
        };
        document.refilter();
        document
    }

    /// Where the log came from.
    pub fn source(&self) -> &LogSource {
        &self.source
    }

    /// The filter text as typed.
    pub fn filter(&self) -> String {
        self.state.borrow().filter.clone()
    }

    /// The records that pass the filter now. Cheap: shares the lists.
    pub fn filtered(&self) -> FilteredRecords {
        let state = self.state.borrow();
        FilteredRecords {
            records: Rc::clone(&state.records),
            visible: Rc::clone(&state.visible),
        }
    }

    /// The record selected in the table, if it passes the filter.
    pub fn selected_record(&self) -> Option<LogRecord> {
        let state = self.state.borrow();
        state
            .selected
            .map(|row| state.records[state.visible[row]].clone())
    }

    /// Replaces the filter text. The selection is kept when the selected
    /// record still passes the filter and dropped otherwise.
    pub fn set_filter(&self, filter: &str) {
        {
            let mut state = self.state.borrow_mut();
            if state.filter == filter {
                return;
            }
            state.filter = filter.to_owned();
            state.query = Query::parse(filter);
        }
        self.refilter();
    }

    fn set_records(&self, records: Vec<LogRecord>) {
        {
            let mut state = self.state.borrow_mut();
            state.records = Rc::new(records);
            state.selected = None;
        }
        self.refilter();
    }

    fn refilter(&self) {
        let mut state = self.state.borrow_mut();
        let selected_record = state.selected.map(|row| state.visible[row]);
        let visible: Vec<usize> = state
            .records
            .iter()
            .enumerate()
            .filter(|(_, record)| state.query.matches(record))
            .map(|(index, _)| index)
            .collect();
        let rows = visible
            .iter()
            .map(|&index| {
                let record = &state.records[index];
                TableRow {
                    cells: vec![
                        record.line.to_string(),
                        record.timestamp.clone(),
                        record.level.to_string(),
                        record.component.clone(),
                        record.message.clone(),
                    ],
                    tone: tone(record.level),
                }
            })
            .collect();
        state.selected =
            selected_record.and_then(|record| visible.iter().position(|&index| index == record));
        state.visible = Rc::new(visible);
        state.rows = Rc::new(rows);
    }

    fn select_row(&self, row: usize) -> bool {
        let mut state = self.state.borrow_mut();
        let valid = row < state.visible.len();
        if valid {
            state.selected = Some(row);
        }
        valid
    }

    fn summary(&self) -> String {
        let state = self.state.borrow();
        match state.query.problem() {
            Some(problem) => format!("Filter problem: {problem}"),
            None if state.query.is_empty() => format!("{} records", state.records.len()),
            None => format!("{} of {} records", state.visible.len(), state.records.len()),
        }
    }
}

#[derive(Default)]
struct WorkspaceState {
    sources: Vec<LogSource>,
    collapsed_groups: BTreeSet<String>,
    selected_node: Option<String>,
    documents: Vec<Rc<LogDocument>>,
}

/// Service: the registered log sources and the logs opened from them.
#[derive(Default)]
pub struct LogWorkspace {
    state: RefCell<WorkspaceState>,
}

impl LogWorkspace {
    /// All registered sources, in registration order.
    pub fn sources(&self) -> Vec<LogSource> {
        self.state.borrow().sources.clone()
    }

    /// The opened log for source `id`, if it was opened in this session.
    pub fn document(&self, id: &str) -> Option<Rc<LogDocument>> {
        self.state
            .borrow()
            .documents
            .iter()
            .find(|document| document.source.id == id)
            .cloned()
    }

    /// The log shown in the active centre tab, if that tab is a log.
    pub fn active_document(&self, shell: &Shell) -> Option<Rc<LogDocument>> {
        shell
            .active_document()
            .filter(|document| document.kind.as_str() == DOCUMENT_KIND)
            .and_then(|document| self.document(&document.resource))
    }

    fn require_active(&self, shell: &Shell) -> Result<Rc<LogDocument>, CommandError> {
        self.active_document(shell)
            .ok_or_else(|| CommandError::failed("no log is open; select one in Sources first"))
    }

    fn source(&self, id: &str) -> Option<LogSource> {
        self.state
            .borrow()
            .sources
            .iter()
            .find(|source| source.id == id)
            .cloned()
    }
}

fn read_records(source: &LogSource) -> Result<Vec<LogRecord>, CommandError> {
    let describe = |error: &dyn std::fmt::Display| {
        CommandError::failed(format!(
            "cannot read {} ({}): {error}",
            source.title,
            source.path.display()
        ))
    };
    let size = fs::metadata(&source.path)
        .map_err(|error| describe(&error))?
        .len();
    if size > MAX_LOG_BYTES {
        return Err(describe(&format!(
            "file is {size} bytes, the limit is {MAX_LOG_BYTES}"
        )));
    }
    let bytes = fs::read(&source.path).map_err(|error| describe(&error))?;
    // Logs are not always valid UTF-8; show what can be shown.
    Ok(parse_log(&String::from_utf8_lossy(&bytes)))
}

const GROUP_PREFIX: &str = "group:";
const SOURCE_PREFIX: &str = "source:";

struct SourcesView(Rc<LogWorkspace>);

impl ViewProvider for SourcesView {
    fn content(&self, _shell: &Shell) -> ViewContent {
        let state = self.0.state.borrow();
        let mut groups: Vec<&str> = state
            .sources
            .iter()
            .map(|source| source.group.as_str())
            .collect();
        groups.sort_unstable();
        groups.dedup();
        let roots = groups
            .into_iter()
            .map(|group| TreeNode {
                id: format!("{GROUP_PREFIX}{group}"),
                label: group.to_owned(),
                is_folder: true,
                expanded: !state.collapsed_groups.contains(group),
                children: state
                    .sources
                    .iter()
                    .filter(|source| source.group == group)
                    .map(|source| TreeNode {
                        id: format!("{SOURCE_PREFIX}{}", source.id),
                        label: source.title.clone(),
                        is_folder: false,
                        expanded: false,
                        children: Vec::new(),
                    })
                    .collect(),
            })
            .collect();
        ViewContent::Tree(TreeModel {
            roots,
            selected: state.selected_node.clone(),
            empty_hint: "No log sources. Install a package that contributes `logs.sources`."
                .to_owned(),
            input: None,
        })
    }

    fn handle(&self, event: ViewEvent, cx: &mut CommandContext<'_>) -> Result<(), CommandError> {
        match event {
            ViewEvent::TreeToggle(node) => {
                if let Some(group) = node.strip_prefix(GROUP_PREFIX) {
                    let mut state = self.0.state.borrow_mut();
                    if !state.collapsed_groups.remove(group) {
                        state.collapsed_groups.insert(group.to_owned());
                    }
                }
            }
            // Selecting a source shows it: browsing logs should not need a
            // second gesture. Folders are only selected.
            ViewEvent::TreeSelect(node) | ViewEvent::TreeActivate(node) => {
                if let Some(source) = node.strip_prefix(SOURCE_PREFIX) {
                    cx.shell
                        .open_document(DocumentKind::literal(DOCUMENT_KIND), source);
                }
                self.0.state.borrow_mut().selected_node = Some(node);
            }
            ViewEvent::RowSelect(_)
            | ViewEvent::FilterChanged(_)
            | ViewEvent::RevealLine(_)
            | ViewEvent::Native(_)
            | ViewEvent::InputSubmitted(_) => {}
        }
        Ok(())
    }
}

struct LogDocumentView(Rc<LogDocument>);

impl ViewProvider for LogDocumentView {
    fn content(&self, _shell: &Shell) -> ViewContent {
        let state = self.0.state.borrow();
        ViewContent::Table(TableModel {
            columns: vec![
                TableColumn {
                    title: "Line".into(),
                    width: Some(56.),
                },
                TableColumn {
                    title: "Time".into(),
                    width: Some(176.),
                },
                TableColumn {
                    title: "Level".into(),
                    width: Some(64.),
                },
                TableColumn {
                    title: "Component".into(),
                    width: Some(150.),
                },
                TableColumn {
                    title: "Message".into(),
                    width: None,
                },
            ],
            rows: Rc::clone(&state.rows),
            selected: state.selected,
            filter: Some(FilterModel {
                text: state.filter.clone(),
                placeholder: "Filter: text, level:error, component:auth".into(),
            }),
            summary: {
                drop(state);
                self.0.summary()
            },
        })
    }

    fn handle(&self, event: ViewEvent, cx: &mut CommandContext<'_>) -> Result<(), CommandError> {
        match event {
            ViewEvent::FilterChanged(text) => {
                self.0.set_filter(&text);
                cx.shell.set_status(self.0.summary());
            }
            ViewEvent::RowSelect(row) => {
                if self.0.select_row(row) {
                    cx.shell.show_view(ViewId::literal(VIEW_RECORD));
                }
            }
            ViewEvent::TreeSelect(_)
            | ViewEvent::TreeActivate(_)
            | ViewEvent::TreeToggle(_)
            | ViewEvent::RevealLine(_)
            | ViewEvent::Native(_)
            | ViewEvent::InputSubmitted(_) => {}
        }
        Ok(())
    }

    fn persisted_state(&self) -> Option<Value> {
        Some(json!({ "filter": self.0.filter() }))
    }

    fn restore_state(&self, state: &Value) {
        if let Some(filter) = state.get("filter").and_then(Value::as_str) {
            self.0.set_filter(filter);
        }
    }

    fn export(&self, max_rows: usize) -> Option<Value> {
        let filtered = self.0.filtered();
        let rows: Vec<Value> = filtered
            .iter()
            .take(max_rows)
            .map(|record| {
                json!({
                    "line": record.line,
                    "timestamp": record.timestamp,
                    "level": record.level.as_str(),
                    "component": record.component,
                    "message": record.message,
                })
            })
            .collect();
        Some(json!({
            "filter": self.0.filter(),
            "total": filtered.total(),
            "matching": filtered.len(),
            "truncated": filtered.len() > rows.len(),
            "records": rows,
        }))
    }
}

struct RecordView(Rc<LogWorkspace>);

impl ViewProvider for RecordView {
    fn content(&self, shell: &Shell) -> ViewContent {
        let record = self
            .0
            .active_document(shell)
            .and_then(|document| document.selected_record());
        ViewContent::Properties(PropertyModel {
            rows: record
                .map(|record| {
                    vec![
                        ("Line".to_owned(), record.line.to_string()),
                        ("Time".to_owned(), record.timestamp),
                        ("Level".to_owned(), record.level.to_string()),
                        ("Component".to_owned(), record.component),
                        ("Message".to_owned(), record.message),
                    ]
                })
                .unwrap_or_default(),
            empty_hint: "Select a record to see its fields.".to_owned(),
        })
    }
}

struct LogDocumentFactory(Rc<LogWorkspace>);

impl DocumentFactory for LogDocumentFactory {
    fn open(
        &self,
        resource: &str,
        cx: &mut CommandContext<'_>,
    ) -> Result<OpenedDocument, CommandError> {
        let source = self
            .0
            .source(resource)
            .ok_or_else(|| CommandError::failed(format!("unknown log source `{resource}`")))?;
        let records = read_records(&source)?;
        let message = format!("Opened {} ({} records)", source.title, records.len());
        cx.shell.output().append(&message, Tone::Normal);
        cx.shell.set_status(message);
        let title = source.title.clone();
        let document = Rc::new(LogDocument::new(source, records));
        let mut state = self.0.state.borrow_mut();
        state
            .documents
            .retain(|existing| existing.source.id != document.source.id);
        state.documents.push(Rc::clone(&document));
        Ok(OpenedDocument {
            title,
            provider: Rc::new(LogDocumentView(document)),
        })
    }
}

fn is_source_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'))
}

/// Validates a `logs.sources` contribution.
fn parse_source(
    data: &Value,
    base_dir: Option<&Path>,
    owner: &ModuleId,
) -> Result<LogSource, ActivationError> {
    let field = |name: &str| {
        data.get(name)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| ActivationError::new(format!("`{name}` must be a non-empty string")))
    };
    let known = ["id", "title", "group", "path"];
    if let Some(unknown) = data
        .as_object()
        .and_then(|object| object.keys().find(|key| !known.contains(&key.as_str())))
    {
        return Err(ActivationError::new(format!("unknown key `{unknown}`")));
    }
    let id = field("id")?;
    if !is_source_id(id) {
        return Err(ActivationError::new(format!(
            "source id `{id}` may only use a-z, 0-9, `-`, `_`, `.`"
        )));
    }
    let relative = Path::new(field("path")?);
    let path = match (relative.is_absolute(), base_dir) {
        (true, _) => relative.to_owned(),
        (false, Some(base_dir)) => base_dir.join(relative),
        (false, None) => {
            return Err(ActivationError::new(format!(
                "relative path `{}` needs a package directory",
                relative.display()
            )));
        }
    };
    if !path.is_file() {
        return Err(ActivationError::new(format!(
            "log file {} does not exist",
            path.display()
        )));
    }
    Ok(LogSource {
        id: id.to_owned(),
        title: field("title")?.to_owned(),
        group: field("group")?.to_owned(),
        path,
        owner: owner.clone(),
    })
}

/// Registers the Sources and Record views, the log document kind, the filter
/// and reload commands, and the `logs.sources` extension point.
#[derive(Default)]
pub struct LogsModule;

impl Module for LogsModule {
    fn manifest(&self) -> ModuleManifest {
        ModuleManifest::bundled(
            MODULE_ID,
            "Log Explorer",
            Version::parse(env!("CARGO_PKG_VERSION")).expect("crate version is semver"),
        )
    }

    fn activate(&self, cx: &mut ActivationContext<'_>) -> Result<(), ActivationError> {
        let workspace = Rc::new(LogWorkspace::default());
        cx.provide(Rc::clone(&workspace))?;

        cx.extension_point(POINT_SOURCES, {
            let workspace = Rc::clone(&workspace);
            // Ids accepted but not yet committed, by contributor: a package
            // must not list the same source twice either.
            let accepted = RefCell::new(BTreeSet::<(ModuleId, String)>::new());
            move |extension| {
                let source = parse_source(extension.data, extension.base_dir, extension.owner)?;
                let fresh = accepted
                    .borrow_mut()
                    .insert((extension.owner.clone(), source.id.clone()));
                if !fresh || workspace.source(&source.id).is_some() {
                    return Err(ActivationError::new(format!(
                        "log source `{}` is already registered",
                        source.id
                    )));
                }
                let workspace = Rc::clone(&workspace);
                Ok(Box::new(move || {
                    workspace.state.borrow_mut().sources.push(source);
                }))
            }
        })?;

        cx.view(
            ViewId::literal(VIEW_SOURCES),
            "Sources",
            ViewLocation::Left,
            0,
            Rc::new(SourcesView(Rc::clone(&workspace))),
        )?;
        cx.view(
            ViewId::literal(VIEW_RECORD),
            "Record",
            ViewLocation::Bottom,
            10,
            Rc::new(RecordView(Rc::clone(&workspace))),
        )?;
        cx.document_factory(
            DocumentKind::literal(DOCUMENT_KIND),
            Rc::new(LogDocumentFactory(Rc::clone(&workspace))),
        )?;

        cx.menu(MenuId::literal(MENU_FILTERS), "Filters", 150)?;

        cx.command(CommandId::literal(COMMAND_FILTER_FOCUS), "Focus Filter", {
            let workspace = Rc::clone(&workspace);
            move |cx| {
                workspace.require_active(cx.shell)?;
                cx.shell.request_focus(FocusTarget::DocumentFilter);
                cx.shell.set_status("Type to filter records");
                Ok(())
            }
        })?;
        cx.menu_item(
            MenuId::literal(MENU_VIEW),
            "focus-filter",
            "Focus Filter",
            CommandId::literal(COMMAND_FILTER_FOCUS),
            None,
            10,
        )?;
        cx.key_binding("ctrl-f", CommandId::literal(COMMAND_FILTER_FOCUS), None)?;

        cx.command(CommandId::literal(COMMAND_FILTER_SET), "Set Filter", {
            let workspace = Rc::clone(&workspace);
            move |cx| {
                let query = cx.string_arg("query")?.to_owned();
                let document = workspace.require_active(cx.shell)?;
                document.set_filter(&query);
                cx.shell.set_status(document.summary());
                Ok(())
            }
        })?;

        cx.command(CommandId::literal(COMMAND_FILTER_CLEAR), "Clear Filter", {
            let workspace = Rc::clone(&workspace);
            move |cx| {
                let document = workspace.require_active(cx.shell)?;
                document.set_filter("");
                cx.shell.set_status(document.summary());
                Ok(())
            }
        })?;
        cx.menu_item(
            MenuId::literal(MENU_FILTERS),
            "clear",
            "Clear Filter",
            CommandId::literal(COMMAND_FILTER_CLEAR),
            None,
            0,
        )?;
        cx.toolbar_item(
            "logs.clear-filter",
            "Clear Filter",
            CommandId::literal(COMMAND_FILTER_CLEAR),
            None,
            20,
        )?;
        cx.key_binding(
            "ctrl-shift-f",
            CommandId::literal(COMMAND_FILTER_CLEAR),
            None,
        )?;

        cx.command(CommandId::literal(COMMAND_RELOAD), "Reload Log", {
            let workspace = Rc::clone(&workspace);
            move |cx| {
                let document = workspace.require_active(cx.shell)?;
                let records = read_records(document.source())?;
                let message = format!(
                    "Reloaded {} ({} records)",
                    document.source().title,
                    records.len()
                );
                document.set_records(records);
                cx.shell.output().append(&message, Tone::Normal);
                cx.shell.set_status(message);
                Ok(())
            }
        })?;
        cx.menu_item(
            MenuId::literal(MENU_VIEW),
            "reload-log",
            "Reload Log",
            CommandId::literal(COMMAND_RELOAD),
            None,
            20,
        )?;
        cx.toolbar_item(
            "logs.reload",
            "Reload",
            CommandId::literal(COMMAND_RELOAD),
            None,
            10,
        )?;
        cx.key_binding("f5", CommandId::literal(COMMAND_RELOAD), None)?;
        Ok(())
    }
}
