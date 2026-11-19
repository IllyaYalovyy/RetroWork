//! Project tree, task output and diagnostics views.

use super::*;

pub(super) struct TreeView(pub(super) Rc<ProjectWorkspace>);

impl ViewProvider for TreeView {
    fn content(&self, _shell: &Shell) -> ViewContent {
        let state = self.0.state.borrow();
        fn qualify(node: &mut TreeNode, session: &Session) {
            node.expanded = if node.id.starts_with("file:") {
                session.collapsed.contains(&node.id)
            } else {
                !session.collapsed.contains(&node.id) && node.expanded
            };
            for child in &mut node.children {
                qualify(child, session);
            }
            node.id = format!("p{}/{}", session.id, node.id);
        }
        let mut sessions: Vec<_> = std::iter::once(&state.active)
            .chain(state.others.values())
            .filter(|session| session.current.is_some())
            .collect();
        sessions.sort_by_key(|session| session.id);
        let roots = sessions
            .into_iter()
            .map(|session| {
                let ProjectSession { project, provider } = session.current.as_ref().unwrap();
                let units = project
                    .units
                    .iter()
                    .map(|unit| TreeNode {
                        id: unit_node(&unit.id),
                        label: unit.name.clone(),
                        is_folder: true,
                        expanded: true,
                        children: unit
                            .targets
                            .iter()
                            .map(|target| TreeNode {
                                id: target_node(&unit.id, target),
                                label: format!("{} {}", target.kind.label(), target.name),
                                is_folder: false,
                                expanded: false,
                                children: vec![],
                            })
                            .collect(),
                    })
                    .collect();
                let mut children = session.navigation.roots.clone();
                if !project.units.is_empty() {
                    children.push(TreeNode {
                        id: "targets".into(),
                        label: "Build Targets".into(),
                        is_folder: true,
                        expanded: true,
                        children: units,
                    });
                }
                let mut root = TreeNode {
                    id: PROJECT_NODE.into(),
                    label: format!(
                        "{} ({})",
                        project.name,
                        provider
                            .as_ref()
                            .map_or("Files", |provider| provider.name())
                    ),
                    is_folder: true,
                    expanded: true,
                    children,
                };
                qualify(&mut root, session);
                root
            })
            .collect();
        ViewContent::Tree(TreeModel {
            roots,
            selected: state.active.selected.clone(),
            input: None,
            empty_hint: state
                .open_error
                .clone()
                .unwrap_or_else(|| "Use File > Import Project or Open Project.".into()),
        })
    }

    fn handle(&self, event: ViewEvent, cx: &mut CommandContext<'_>) -> Result<(), CommandError> {
        match event {
            ViewEvent::InputSubmitted(text) => self.0.open(&text, false, cx),
            // Folders toggle on click; they are also what the user selects to
            // build a unit or the whole project.
            ViewEvent::TreeToggle(node) => {
                self.0.select(&node)?;
                let local = node.split_once('/').unwrap().1.to_owned();
                self.0.expand(&local, cx);
                let mut state = self.0.state.borrow_mut();
                if !state.active.collapsed.remove(&local) {
                    state.active.collapsed.insert(local);
                }
                state.active.selected = Some(node);
                Ok(())
            }
            ViewEvent::TreeSelect(node) => self.0.select(&node),
            ViewEvent::TreeActivate(node) => {
                self.0.select(&node)?;
                let Some(project) = self.0.project() else {
                    return Ok(());
                };
                let local = node.split_once('/').unwrap().1;
                let source = self
                    .0
                    .state
                    .borrow()
                    .active
                    .navigation
                    .file(local)
                    .or_else(|| {
                        project.units.iter().find_map(|unit| {
                            unit.targets
                                .iter()
                                .find(|target| target_node(&unit.id, target) == local)
                                .and_then(|target| target.source.clone())
                        })
                    });
                if let Some(file) = source {
                    self.0.reveal(
                        &Location {
                            file,
                            line: 1,
                            column: 1,
                        },
                        cx,
                    );
                }
                Ok(())
            }
            ViewEvent::RevealLine(_)
            | ViewEvent::Native(_)
            | ViewEvent::RowSelect(_)
            | ViewEvent::FilterChanged(_) => Ok(()),
        }
    }
}

pub(super) struct OutputView(pub(super) Rc<ProjectWorkspace>);

impl ViewProvider for OutputView {
    fn content(&self, _shell: &Shell) -> ViewContent {
        ViewContent::Text(TextModel {
            lines: Rc::clone(&self.0.state.borrow().active.output.lines),
            empty_hint: "Build, run or test a project to see its output here.".to_owned(),
            header: Some(self.0.header()),
            selected: None,
        })
    }
}

pub(super) struct ProblemsView(pub(super) Rc<ProjectWorkspace>);

impl ProblemsView {
    fn location_text(root: &Path, location: &Location) -> String {
        let file = location.file.strip_prefix(root).unwrap_or(&location.file);
        format!("{}:{}:{}", file.display(), location.line, location.column)
    }
}

impl ViewProvider for ProblemsView {
    fn content(&self, _shell: &Shell) -> ViewContent {
        let state = self.0.state.borrow();
        let root = match &state.active.current {
            Some(ProjectSession { project, .. }) => project.root.clone(),
            _ => PathBuf::new(),
        };
        let rows = state
            .active
            .problems
            .iter()
            .map(|problem| TableRow {
                cells: vec![
                    problem.severity.label().to_owned(),
                    problem.message.clone(),
                    problem
                        .location
                        .as_ref()
                        .map(|location| Self::location_text(&root, location))
                        .unwrap_or_default(),
                ],
                tone: match problem.severity {
                    Severity::Error => Tone::Error,
                    Severity::Warning => Tone::Warning,
                    Severity::Note => Tone::Muted,
                },
            })
            .collect();
        ViewContent::Table(TableModel {
            columns: vec![
                TableColumn {
                    title: "Severity".into(),
                    width: Some(80.),
                },
                TableColumn {
                    title: "Message".into(),
                    width: None,
                },
                TableColumn {
                    title: "Location".into(),
                    width: Some(260.),
                },
            ],
            rows: Rc::new(rows),
            selected: state.active.selected_problem,
            filter: None,
            summary: String::new(),
        })
    }

    fn handle(&self, event: ViewEvent, cx: &mut CommandContext<'_>) -> Result<(), CommandError> {
        if let ViewEvent::RowSelect(row) = event {
            let location = {
                let mut state = self.0.state.borrow_mut();
                let Some(problem) = state.active.problems.get(row) else {
                    return Ok(());
                };
                let location = problem.location.clone();
                state.active.selected_problem = Some(row);
                location
            };
            if let Some(location) = location {
                self.0.reveal(&location, cx);
            }
        }
        Ok(())
    }
}
