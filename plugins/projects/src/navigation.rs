//! Logical roots and bounded, lazy file enumeration.

use super::*;
use config::{Group, LogicalRoot, ProjectConfig};

/// Cached navigation for one project; directories are read only when expanded.
#[derive(Default)]
pub(super) struct Navigation {
    pub(super) roots: Vec<TreeNode>,
    paths: BTreeMap<String, PathBuf>,
    loaded: BTreeSet<String>,
    pending: BTreeSet<String>,
}
impl Navigation {
    /// Builds logical roots from explicit mappings or common build-project conventions.
    pub(super) fn new(
        project: &Project,
        config: &ProjectConfig,
        suggested: Vec<LogicalRoot>,
    ) -> Self {
        let roots = if config.roots.is_empty() {
            suggested
        } else {
            config.roots.clone()
        };
        let mut navigation = Self::default();
        let mut groups = BTreeMap::<Group, Vec<TreeNode>>::new();
        for root in roots {
            let path = project.root.join(&root.path);
            let Ok(canonical) = path.canonicalize() else {
                continue;
            };
            if !canonical.starts_with(&project.root) {
                continue;
            }
            let id = format!("file:{}", root.path.display());
            if navigation
                .paths
                .insert(id.clone(), canonical.clone())
                .is_some()
            {
                continue;
            }
            groups.entry(root.group).or_default().push(TreeNode {
                id,
                label: root.path.display().to_string(),
                is_folder: canonical.is_dir(),
                expanded: false,
                children: vec![],
            });
        }
        navigation.roots = groups
            .into_iter()
            .map(|(group, children)| TreeNode {
                id: format!("group:{}", group.title()),
                label: group.title().into(),
                children,
                is_folder: true,
                expanded: true,
            })
            .collect();
        navigation
    }
    pub(super) fn contains(&self, id: &str) -> bool {
        self.paths.contains_key(id)
            || self.roots.iter().any(|node| node.id == id)
            || id == "targets"
    }
    pub(super) fn file(&self, id: &str) -> Option<PathBuf> {
        self.paths.get(id).cloned()
    }
    fn insert_children(nodes: &mut [TreeNode], id: &str, children: &[TreeNode]) {
        for node in nodes {
            if node.id == id {
                node.children = children.to_vec();
                return;
            }
            Self::insert_children(&mut node.children, id, children);
        }
    }
}
impl ProjectWorkspace {
    /// Starts at most one read per directory and routes its result to its owning project.
    pub(super) fn expand(&self, id: &str, cx: &mut CommandContext<'_>) {
        let (project_id, root, path) = {
            let mut state = self.state.borrow_mut();
            let session = &mut state.active;
            if session.navigation.pending.len() >= 4 || session.navigation.paths.len() >= 20_000 {
                cx.shell.set_status(
                    "Project navigation limit reached; finish pending reads or reopen the project",
                );
                return;
            }
            if session.navigation.loaded.contains(id)
                || !session.navigation.pending.insert(id.into())
            {
                return;
            }
            let Some(path) = session.navigation.paths.get(id).cloned() else {
                session.navigation.pending.remove(id);
                return;
            };
            (
                session.id,
                session.current.as_ref().unwrap().project.root.clone(),
                path,
            )
        };
        let id = id.to_owned();
        let inbox = cx.services.inbox();
        let failed_id = id.clone();
        let spawned = std::thread::Builder::new()
            .name("project-files".into())
            .spawn(move || {
                let result = (|| -> Result<Vec<(TreeNode, PathBuf)>, String> {
                    let mut children = Vec::new();
                    for (index, entry) in std::fs::read_dir(path)
                        .map_err(|e| e.to_string())?
                        .enumerate()
                    {
                        if index == 2000 {
                            return Err("directory exceeds the 2000-entry navigation limit".into());
                        }
                        let entry = entry.map_err(|e| e.to_string())?;
                        let kind = entry.file_type().map_err(|e| e.to_string())?;
                        if kind.is_symlink() {
                            continue;
                        }
                        let path = entry.path();
                        let relative = path.strip_prefix(&root).map_err(|e| e.to_string())?;
                        let node = TreeNode {
                            id: format!("file:{}", relative.display()),
                            label: entry.file_name().to_string_lossy().into(),
                            children: vec![],
                            is_folder: kind.is_dir(),
                            expanded: false,
                        };
                        children.push((node, path));
                    }
                    children.sort_by(|a, b| {
                        (!a.0.is_folder, &a.0.label).cmp(&(!b.0.is_folder, &b.0.label))
                    });
                    Ok(children)
                })();
                let _ = inbox.post_blocking(Box::new(move |cx| {
                    let Some(workspace) = cx.services.get::<ProjectWorkspace>() else {
                        return;
                    };
                    let mut state = workspace.state.borrow_mut();
                    let session = if state.active.id == project_id {
                        &mut state.active
                    } else {
                        let Some(session) = state.others.get_mut(&project_id) else {
                            return;
                        };
                        session
                    };
                    session.navigation.pending.remove(&id);
                    match result {
                        Ok(children) => {
                            let nodes: Vec<_> =
                                children.iter().map(|(node, _)| node.clone()).collect();
                            for (node, path) in children {
                                session.navigation.paths.insert(node.id, path);
                            }
                            Navigation::insert_children(&mut session.navigation.roots, &id, &nodes);
                            session.navigation.loaded.insert(id);
                        }
                        Err(error) => cx.shell.set_status(error),
                    }
                }));
            });
        if let Err(error) = spawned {
            self.state
                .borrow_mut()
                .active
                .navigation
                .pending
                .remove(&failed_id);
            cx.shell.set_status(error.to_string());
        }
    }
}
