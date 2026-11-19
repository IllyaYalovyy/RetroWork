//! Toolkit-independent view content.
//!
//! A module describes *what* a view shows with these models and reacts to
//! [`ViewEvent`]s; the workbench decides how to draw them. Because the models
//! carry no UI toolkit types, the same contract can later be served by a
//! process plugin, and module logic is testable without a window.
//!
//! Row and line collections are reference counted: a provider builds them when
//! its data changes and hands out cheap clones on every frame.

use std::{rc::Rc, sync::Arc};

use serde_json::Value;

use crate::{
    command::{CommandContext, CommandError},
    shell::Shell,
};

/// Semantic emphasis. The workbench maps tones to theme colours.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tone {
    #[default]
    Normal,
    Muted,
    Info,
    Warning,
    Error,
}

impl Tone {
    /// The tone's name, for reports and element values.
    pub fn as_str(self) -> &'static str {
        match self {
            Tone::Normal => "normal",
            Tone::Muted => "muted",
            Tone::Info => "info",
            Tone::Warning => "warning",
            Tone::Error => "error",
        }
    }
}

/// A node of a [`TreeModel`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeNode {
    /// Stable id, unique within the tree. Used for events and automation.
    pub id: String,
    pub label: String,
    pub children: Vec<TreeNode>,
    /// Folders can be expanded; leaves can be activated.
    pub is_folder: bool,
    pub expanded: bool,
}

/// Content of a tree view: folders and leaves.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TreeModel {
    pub roots: Vec<TreeNode>,
    pub selected: Option<String>,
    /// Shown instead of an empty tree.
    pub empty_hint: String,
    /// A single-line field above the tree. Pressing Enter in it reports
    /// [`ViewEvent::InputSubmitted`]. Only shown in docked views.
    pub input: Option<InputModel>,
}

/// A text field whose content is submitted with Enter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputModel {
    /// Text the field starts with when it is first shown. Later changes to
    /// it do not overwrite what the user has typed.
    pub initial_text: String,
    pub placeholder: String,
}

impl TreeModel {
    /// Visible nodes in display order with their depth, honouring `expanded`.
    pub fn visible(&self) -> Vec<(usize, &TreeNode)> {
        fn walk<'a>(nodes: &'a [TreeNode], depth: usize, out: &mut Vec<(usize, &'a TreeNode)>) {
            for node in nodes {
                out.push((depth, node));
                if node.is_folder && node.expanded {
                    walk(&node.children, depth + 1, out);
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.roots, 0, &mut out);
        out
    }
}

/// A column of a [`TableModel`].
#[derive(Clone, Debug, PartialEq)]
pub struct TableColumn {
    pub title: String,
    /// Width in logical pixels, or `None` to take the remaining space.
    pub width: Option<f32>,
}

/// A row of a [`TableModel`]: one cell per column.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableRow {
    pub cells: Vec<String>,
    pub tone: Tone,
}

/// A text filter shown above a table and owned by the provider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilterModel {
    pub text: String,
    pub placeholder: String,
}

/// Content of a table view, with an optional filter field.
#[derive(Clone, Debug, PartialEq)]
pub struct TableModel {
    pub columns: Vec<TableColumn>,
    pub rows: Rc<Vec<TableRow>>,
    pub selected: Option<usize>,
    pub filter: Option<FilterModel>,
    /// One-line description of what is shown, e.g. `12 of 340 records`.
    pub summary: String,
}

/// A line of a [`TextModel`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextLine {
    pub text: String,
    pub tone: Tone,
}

/// Content of a read-only text view, such as the Output.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextModel {
    pub lines: Rc<Vec<TextLine>>,
    pub empty_hint: String,
    /// A line shown above the text that does not scroll, such as the state
    /// of a running task.
    pub header: Option<TextLine>,
    /// Index into `lines` to highlight and scroll to. Without one, the view
    /// follows new lines as they are appended.
    pub selected: Option<usize>,
}

/// Initial text, optional editing session and navigation for a source document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceModel {
    pub text: Arc<str>,
    /// Present only when the provider accepts edits and saves for this document.
    pub editing: Option<crate::document_session::DocumentSession>,
    pub reveal_line: Option<usize>,
    pub message: String,
}

/// Content of a name/value list, such as a record's fields.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PropertyModel {
    pub rows: Vec<(String, String)>,
    pub empty_hint: String,
}

/// What a view or document shows.
#[derive(Clone, Debug, PartialEq)]
pub enum ViewContent {
    /// A plugin-owned model rendered by a registered front-end adapter.
    Native(crate::NativeValue),
    Tree(TreeModel),
    Table(TableModel),
    Text(TextModel),
    Source(SourceModel),
    Properties(PropertyModel),
}

/// A user interaction with a view, reported by the workbench.
#[derive(Clone, Debug, PartialEq)]
pub enum ViewEvent {
    /// Reveals a one-based line in a source document.
    RevealLine(usize),
    /// An interaction interpreted by the owning plugin.
    Native(crate::NativeValue),
    /// A tree node was selected (single click or keyboard).
    TreeSelect(String),
    /// A tree leaf was opened (double click or Enter).
    TreeActivate(String),
    /// A tree folder was expanded or collapsed.
    TreeToggle(String),
    /// A table row was selected.
    RowSelect(usize),
    /// The text of the view's filter changed.
    FilterChanged(String),
    /// Enter was pressed in the view's input field, with this text.
    InputSubmitted(String),
}

/// Why a document cannot close. Only unsaved edits permit explicit discard.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CloseError {
    /// A monotonically increasing edit revision, including undo and redo.
    /// Change it whenever the work that would be discarded changes.
    #[error("Unsaved changes: save or confirm discard before closing")]
    Unsaved { revision: u64 },
    /// Work that must finish or be resolved before closing; never bypassed by discard.
    #[error("{0}")]
    Blocked(String),
}

/// Supplies the content of a docked view or a document and handles its events.
pub trait ViewProvider {
    /// Rejects closure while owned work would be lost or a write is still running.
    fn can_close(&self) -> Result<(), crate::CloseError> {
        Ok(())
    }

    /// What to show now. Called for every frame the view is visible in, so it
    /// must be cheap; `shell` tells which document is active.
    fn content(&self, shell: &Shell) -> ViewContent;

    /// Reacts to a user interaction. Requests queued on the shell run after
    /// this returns; an error is shown and discards them. Ignores all events
    /// unless overridden.
    fn handle(&self, event: ViewEvent, cx: &mut CommandContext<'_>) -> Result<(), CommandError> {
        let _ = (event, cx);
        Ok(())
    }

    /// State to persist across restarts, for documents. `None` persists nothing.
    fn persisted_state(&self) -> Option<Value> {
        None
    }

    /// Restores what [`Self::persisted_state`] returned. Must tolerate any
    /// value: persisted state may come from an older or newer version.
    fn restore_state(&self, state: &Value) {
        let _ = state;
    }

    /// Data handed to process plugins when a command runs on this document,
    /// limited to `max_rows` rows.
    fn export(&self, max_rows: usize) -> Option<Value> {
        let _ = max_rows;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(id: &str) -> TreeNode {
        TreeNode {
            id: id.into(),
            label: id.into(),
            children: vec![],
            is_folder: false,
            expanded: false,
        }
    }

    fn folder(id: &str, expanded: bool, children: Vec<TreeNode>) -> TreeNode {
        TreeNode {
            id: id.into(),
            label: id.into(),
            children,
            is_folder: true,
            expanded,
        }
    }

    #[test]
    fn visible_nodes_skip_children_of_collapsed_folders() {
        let model = TreeModel {
            roots: vec![
                folder(
                    "open",
                    true,
                    vec![leaf("a"), folder("closed", false, vec![leaf("hidden")])],
                ),
                leaf("b"),
            ],
            ..Default::default()
        };
        let visible: Vec<(usize, &str)> = model
            .visible()
            .into_iter()
            .map(|(depth, node)| (depth, node.id.as_str()))
            .collect();
        assert_eq!(visible, [(0, "open"), (1, "a"), (1, "closed"), (0, "b")]);
    }
}
