//! Temporary workbench adapter; the normal application does not load this module.

use crate::{Candidate, Finder, Limits, ProjectRoot};
use retrowork_platform::{
    ActivationContext, ActivationError, CommandContext, CommandError, CommandId, Dependency,
    DocumentFactory, DocumentKind, FilterModel, FocusTarget, MenuId, Module, ModuleManifest,
    ModuleResource, OpenedDocument, Shell, TableColumn, TableModel, TableRow, Tone, ViewContent,
    ViewEvent, ViewProvider, core::MENU_TOOLS,
};
use std::{cell::RefCell, rc::Rc, sync::Arc};

const DOCUMENT: &str = "experiment.finder";

/// Starts the isolated experiment with explicitly supplied project directories.
pub struct PrototypeModule(pub Vec<ProjectRoot>);
impl Module for PrototypeModule {
    fn manifest(&self) -> ModuleManifest {
        ModuleManifest::bundled(
            "experiment.finder",
            "File Finder Experiment",
            semver::Version::new(0, 1, 0),
        )
        .depends_on(Dependency::literal(retrowork_projects::MODULE_ID, "^0.1"))
    }
    fn activate(&self, cx: &mut ActivationContext<'_>) -> Result<(), ActivationError> {
        let search = Rc::new(Search(Rc::new(RefCell::new(State {
            finder: None,
            projects: self.0.clone(),
            generated: false,
            query: String::new(),
            shown: vec![],
            selected: 0,
        }))));
        cx.document_factory(DocumentKind::literal(DOCUMENT), search.clone())?;
        cx.manage(search.clone());
        for (id, title, key) in [
            ("find", "Find File", "ctrl-p"),
            ("next", "Next Result", "alt-down"),
            ("previous", "Previous Result", "alt-up"),
            ("open", "Open Selected File", "ctrl-enter"),
            ("refresh", "Refresh File List", "ctrl-shift-r"),
            ("generated", "Toggle Generated Sources", "ctrl-shift-g"),
            ("close", "Close Last Search Project", "ctrl-shift-w"),
        ] {
            let command = CommandId::new(&format!("finder.{id}")).unwrap();
            let search = search.clone();
            cx.command(command.clone(), title, move |cx| search.command(id, cx))?;
            cx.key_binding(key, command.clone(), None)?;
            cx.menu_item(MenuId::literal(MENU_TOOLS), id, title, command, None, 10)?;
        }
        Ok(())
    }
}

struct State {
    finder: Option<Finder>,
    projects: Vec<ProjectRoot>,
    generated: bool,
    query: String,
    shown: Vec<Candidate>,
    selected: usize,
}
#[derive(Clone)]
struct Search(Rc<RefCell<State>>);
impl Search {
    /// Handles only experiment controls; all matching remains in the library-backed finder.
    fn command(&self, id: &str, cx: &mut CommandContext<'_>) -> Result<(), CommandError> {
        if id == "find" {
            cx.shell
                .open_document(DocumentKind::literal(DOCUMENT), "files");
            cx.shell.request_focus(FocusTarget::DocumentFilter);
            return Ok(());
        }
        let mut state = self.0.borrow_mut();
        match id {
            "next" => {
                state.selected = (state.selected + 1).min(state.shown.len().saturating_sub(1))
            }
            "previous" => state.selected = state.selected.saturating_sub(1),
            "open" => {
                let candidate = state
                    .shown
                    .get(state.selected)
                    .cloned()
                    .ok_or_else(|| CommandError::failed("No result selected"))?;
                let path = state
                    .finder
                    .as_mut()
                    .ok_or_else(|| CommandError::failed("Open Find File first"))?
                    .resolve(&candidate)
                    .map_err(CommandError::failed)?;
                cx.shell.open_document(
                    DocumentKind::literal(retrowork_projects::DOCUMENT_SOURCE),
                    path.display().to_string(),
                );
            }
            "refresh" => {
                if let Some(finder) = &mut state.finder {
                    finder.reconcile();
                }
            }
            "close" | "generated" => {
                if id == "close" {
                    state.projects.pop();
                } else {
                    state.generated = !state.generated;
                }
                let projects = state.projects.clone();
                let generated = state.generated;
                state.shown.clear();
                if let Some(finder) = &mut state.finder {
                    finder.set_projects(projects, generated);
                }
            }
            _ => {}
        }
        Ok(())
    }
}
impl ModuleResource for Search {
    fn shutdown(&self) {
        self.0.borrow_mut().finder.take();
    }
}
impl DocumentFactory for Search {
    fn open(&self, _: &str, cx: &mut CommandContext<'_>) -> Result<OpenedDocument, CommandError> {
        let mut state = self.0.borrow_mut();
        if state.finder.is_none() {
            let inbox = cx.services.inbox();
            let mut finder = Finder::new(
                Limits::default(),
                Arc::new(move || {
                    let _ = inbox.try_post(Box::new(|_| {}));
                }),
            )
            .map_err(|error| CommandError::failed(error.to_string()))?;
            finder.set_projects(state.projects.clone(), state.generated);
            state.finder = Some(finder);
        }
        Ok(OpenedDocument {
            title: "Find File".into(),
            provider: Rc::new(self.clone()),
        })
    }
}

impl ViewProvider for Search {
    fn content(&self, _: &Shell) -> ViewContent {
        let mut state = self.0.borrow_mut();
        let results = state
            .finder
            .as_mut()
            .map(|f| f.poll(200))
            .unwrap_or_default();
        if state.shown != results.files {
            state.selected = 0;
            state.shown = results.files;
        }
        let summary = if results.busy {
            "Updating…".into()
        } else {
            format!(
                "{} of {} files | {} projects | generated {}{}{}",
                results.matched,
                results.total,
                state.projects.len(),
                if state.generated {
                    "included"
                } else {
                    "excluded"
                },
                if results.truncated {
                    " | LIMIT REACHED"
                } else {
                    ""
                },
                if results.errors.is_empty() {
                    String::new()
                } else {
                    format!(" | {}", results.errors.join("; "))
                }
            )
        };
        ViewContent::Table(TableModel {
            columns: vec![
                TableColumn {
                    title: "File / path".into(),
                    width: None,
                },
                TableColumn {
                    title: "Project".into(),
                    width: Some(160.),
                },
                TableColumn {
                    title: "Kind".into(),
                    width: Some(100.),
                },
            ],
            rows: Rc::new(
                state
                    .shown
                    .iter()
                    .map(|file| TableRow {
                        cells: vec![
                            file.relative.display().to_string(),
                            file.project.clone(),
                            if file.generated {
                                "Generated"
                            } else {
                                "Source"
                            }
                            .into(),
                        ],
                        tone: if file.generated {
                            Tone::Muted
                        } else {
                            Tone::Normal
                        },
                    })
                    .collect(),
            ),
            selected: (!state.shown.is_empty()).then_some(state.selected),
            filter: Some(FilterModel {
                text: state.query.clone(),
                placeholder: "Find file · Alt+↓/↑ selects · Enter opens".into(),
            }),
            summary,
        })
    }
    fn handle(&self, event: ViewEvent, cx: &mut CommandContext<'_>) -> Result<(), CommandError> {
        match event {
            ViewEvent::FilterChanged(query) => {
                let mut state = self.0.borrow_mut();
                if let Some(finder) = &mut state.finder {
                    finder.query(&query);
                }
                state.query = query;
                state.shown.clear();
                state.selected = 0;
            }
            ViewEvent::RowSelect(index) => {
                self.0.borrow_mut().selected = index;
            }
            ViewEvent::InputSubmitted(_) => self.command("open", cx)?,
            _ => {}
        }
        Ok(())
    }
}
