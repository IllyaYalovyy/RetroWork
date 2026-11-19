//! Workbench state that is independent of how it is drawn: open documents,
//! active views, status text, output and the event trace.

use std::{
    cell::RefCell,
    collections::{BTreeMap, VecDeque},
    rc::Rc,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    contribution::ViewLocation,
    id::{CommandId, DocumentKind, ModuleId, ViewId},
    model::{TextLine, Tone, ViewEvent, ViewProvider},
};

/// Identity of one open document. Closing it never makes its ID refer to another tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DocumentId(pub(crate) u64);

/// A discard question bound to one document and its current edit revision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiscardRequest {
    sequence: u64,
    document: DocumentId,
    revision: u64,
}

impl DiscardRequest {
    /// The document whose edits the user is being asked to discard.
    pub fn document(&self) -> DocumentId {
        self.document
    }
}

/// Work authorized by a discard response; only the platform retains its payload.
enum DiscardAction {
    Close { quitting: bool },
    Event(ViewEvent),
}

/// A document open in the centre area.
pub struct Document {
    pub id: DocumentId,
    pub kind: DocumentKind,
    /// The string its factory opened it from; also what gets persisted.
    pub resource: String,
    pub title: String,
    pub provider: Rc<dyn ViewProvider>,
    pub owner: ModuleId,
}

/// Something a handler asks the platform to do once the handler has returned.
/// Deferring keeps handlers from re-entering the platform.
pub(crate) enum ShellRequest {
    DocumentEvent {
        document: DocumentId,
        event: ViewEvent,
    },
    OpenDocument {
        kind: DocumentKind,
        resource: String,
        event: Option<ViewEvent>,
    },
    ShowView(ViewId),
    RunCommand {
        command: CommandId,
        args: Option<Value>,
    },
}

/// A directory selection requested by a plugin command.
#[derive(Clone, Debug)]
pub struct FolderRequest {
    pub confirm: String,
    pub command: CommandId,
}

/// A system save dialog whose response belongs to a specific document.
#[derive(Clone, Debug)]
pub struct SavePathRequest {
    pub document: DocumentId,
    pub directory: std::path::PathBuf,
    pub name: String,
    pub command: CommandId,
}

/// A system path chooser requested by a plugin.
#[derive(Clone, Debug)]
pub enum PathRequest {
    Folder(FolderRequest),
    Save(SavePathRequest),
}

/// Exclusive ownership of a destination until a background save succeeds or fails.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceChange {
    sequence: u64,
    document: DocumentId,
    resource: String,
    title: String,
}

/// A place in the workbench that a command can ask to receive keyboard focus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FocusTarget {
    /// The filter field of the active document.
    DocumentFilter,
    /// The input field of a docked view.
    ViewInput(ViewId),
}

/// One entry of the semantic event trace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceEvent {
    pub seq: u64,
    /// Dotted category, e.g. `command.executed` or `ui.mouse_down`.
    pub kind: String,
    pub detail: String,
}

/// A bounded, ordered record of what happened: raw input the workbench
/// received, commands that ran, documents and views that changed.
///
/// Tests read it to prove that an outcome was caused by input travelling the
/// real route (input event, then command) rather than by a direct call.
pub struct Trace {
    events: VecDeque<TraceEvent>,
    next_seq: u64,
    capacity: usize,
}

impl Trace {
    /// Events kept before the oldest are dropped.
    pub const DEFAULT_CAPACITY: usize = 2000;

    fn new(capacity: usize) -> Self {
        Self {
            events: VecDeque::new(),
            next_seq: 1,
            capacity,
        }
    }

    /// Appends an event, dropping the oldest when the trace is full.
    pub fn record(&mut self, kind: &str, detail: impl Into<String>) {
        if self.events.len() == self.capacity {
            self.events.pop_front();
        }
        self.events.push_back(TraceEvent {
            seq: self.next_seq,
            kind: kind.to_owned(),
            detail: detail.into(),
        });
        self.next_seq += 1;
    }

    /// Events with `seq > after`, oldest first.
    pub fn since(&self, after: u64) -> Vec<TraceEvent> {
        self.events
            .iter()
            .filter(|event| event.seq > after)
            .cloned()
            .collect()
    }

    /// Sequence number of the newest event, or 0 when nothing was recorded.
    pub fn last_seq(&self) -> u64 {
        self.next_seq - 1
    }
}

/// The Output panel's content. Shared between the shell (writers) and the
/// Output view (reader).
#[derive(Default)]
pub struct OutputLog {
    lines: RefCell<Rc<Vec<TextLine>>>,
}

impl OutputLog {
    /// Most lines kept; older lines are dropped first.
    pub const MAX_LINES: usize = 10_000;

    /// Adds a line, dropping the oldest beyond [`Self::MAX_LINES`].
    pub fn append(&self, text: impl Into<String>, tone: Tone) {
        let mut lines = self.lines.borrow_mut();
        let lines = Rc::make_mut(&mut lines);
        lines.push(TextLine {
            text: text.into(),
            tone,
        });
        if lines.len() > Self::MAX_LINES {
            let excess = lines.len() - Self::MAX_LINES;
            lines.drain(..excess);
        }
    }

    /// Removes every line. Lists returned earlier by [`Self::lines`] keep theirs.
    pub fn clear(&self) {
        *self.lines.borrow_mut() = Rc::default();
    }

    /// The current lines. Later changes do not affect the returned list.
    pub fn lines(&self) -> Rc<Vec<TextLine>> {
        Rc::clone(&self.lines.borrow())
    }
}

/// Workbench state that does not depend on how it is drawn. Handlers change
/// it directly or queue requests that the platform carries out after the
/// handler returns.
pub struct Shell {
    discard_request: Option<DiscardRequest>,
    discard_confirmation: Option<(DiscardRequest, DiscardAction)>,
    next_request_sequence: u64,
    path_request: Option<PathRequest>,
    resource_changes: BTreeMap<DocumentId, ResourceChange>,
    next_document_id: u64,
    documents: Vec<Document>,
    active_document: Option<usize>,
    active_views: BTreeMap<ViewLocation, ViewId>,
    status: String,
    output: Rc<OutputLog>,
    trace: Trace,
    quit_requested: bool,
    focus_request: Option<FocusTarget>,
    pub(crate) requests: VecDeque<ShellRequest>,
}

impl Default for Shell {
    fn default() -> Self {
        Self {
            discard_request: None,
            discard_confirmation: None,
            next_request_sequence: 0,
            path_request: None,
            resource_changes: BTreeMap::new(),
            next_document_id: 0,
            documents: Vec::new(),
            active_document: None,
            active_views: BTreeMap::new(),
            status: "Ready".to_owned(),
            output: Rc::default(),
            trace: Trace::new(Trace::DEFAULT_CAPACITY),
            quit_requested: false,
            focus_request: None,
            requests: VecDeque::new(),
        }
    }
}

impl Shell {
    /// Requests a directory picker; confirmation invokes the command with a `directory` argument.
    pub fn choose_folder(&mut self, request: FolderRequest) {
        self.path_request = Some(PathRequest::Folder(request));
    }
    /// Requests the system save dialog, returning `document` and `path` to its command.
    pub fn choose_save_path(&mut self, request: SavePathRequest) {
        self.path_request = Some(PathRequest::Save(request));
    }
    /// Takes the pending system picker request for the front end.
    pub fn take_path_request(&mut self) -> Option<PathRequest> {
        self.path_request.take()
    }

    /// Reserves a normalized resource key. Opening it activates this document while saving.
    /// The provider must normalize file aliases before requesting ownership.
    pub fn reserve_resource(
        &mut self,
        id: DocumentId,
        resource: String,
        title: String,
    ) -> Result<ResourceChange, String> {
        let document = self.document(id).ok_or("Document is closed")?;
        if self.resource_changes.contains_key(&id) {
            return Err("A destination change is already running".into());
        }
        if self.find_document(&document.kind, &resource).is_some() {
            return Err("Destination is already open".into());
        }
        self.next_request_sequence = self
            .next_request_sequence
            .checked_add(1)
            .expect("request sequence exhausted");
        let change = ResourceChange {
            sequence: self.next_request_sequence,
            document: id,
            resource,
            title,
        };
        self.resource_changes.insert(id, change.clone());
        Ok(change)
    }

    /// Commits a successful publication or releases a failed reservation. Stale results do nothing.
    pub fn finish_resource_change(&mut self, change: ResourceChange, succeeded: bool) {
        if self.resource_changes.get(&change.document) != Some(&change) {
            return;
        }
        self.resource_changes.remove(&change.document);
        if succeeded && let Some(index) = self.document_index(change.document) {
            self.documents[index].resource = change.resource;
            self.documents[index].title = change.title;
            self.trace.record(
                "document.retargeted",
                self.documents[index].resource.clone(),
            );
            self.set_status(format!("Document: {}", self.documents[index].resource));
        }
    }

    /// Prevents closing while a worker owns the destination, even if the text is unchanged.
    fn document_can_close(&self, index: usize) -> Result<(), crate::CloseError> {
        if self
            .resource_changes
            .contains_key(&self.documents[index].id)
        {
            Err(crate::CloseError::Blocked(
                "A destination change is still running".into(),
            ))
        } else {
            self.documents[index].provider.can_close()
        }
    }

    /// The status bar text.
    pub fn status(&self) -> &str {
        &self.status
    }

    /// Replaces the status bar text.
    pub fn set_status(&mut self, status: impl Into<String>) {
        self.status = status.into();
    }

    /// The Output view's lines.
    pub fn output(&self) -> &Rc<OutputLog> {
        &self.output
    }

    /// The ordered record of input, commands and state changes, bounded to the
    /// newest [`Trace::DEFAULT_CAPACITY`] events.
    pub fn trace(&self) -> &Trace {
        &self.trace
    }

    /// For front ends that record raw input, and handlers that record their own
    /// events.
    pub fn trace_mut(&mut self) -> &mut Trace {
        &mut self.trace
    }

    /// Open documents, in tab order.
    pub fn documents(&self) -> &[Document] {
        &self.documents
    }

    /// Finds an open document by identity, independent of tab position.
    pub fn document(&self, id: DocumentId) -> Option<&Document> {
        self.documents.iter().find(|document| document.id == id)
    }

    pub(crate) fn document_index(&self, id: DocumentId) -> Option<usize> {
        self.documents.iter().position(|document| document.id == id)
    }

    /// Index of the active document in [`Self::documents`].
    pub fn active_document_index(&self) -> Option<usize> {
        self.active_document
    }

    /// The active document, if any is open.
    pub fn active_document(&self) -> Option<&Document> {
        self.active_document.map(|index| &self.documents[index])
    }

    /// Makes the document at `index` the active one. Out-of-range is ignored.
    pub(crate) fn activate_document(&mut self, index: usize) {
        if index < self.documents.len() && self.active_document != Some(index) {
            self.active_document = Some(index);
            let document = &self.documents[index];
            self.trace.record(
                "document.activated",
                format!("{}:{}", document.kind, document.resource),
            );
        }
    }

    /// Closes the document at `index`, activating a neighbour.
    pub(crate) fn close_document(&mut self, index: usize) -> bool {
        if index >= self.documents.len() {
            return false;
        }
        if let Err(reason) = self.document_can_close(index) {
            self.request_discard(index, &reason, DiscardAction::Close { quitting: false });
            self.set_status(format!("{}: {reason}", self.documents[index].title));
            return false;
        }
        self.remove_document(index);
        true
    }

    /// Removes an approved document and selects its neighbour.
    fn remove_document(&mut self, index: usize) {
        let document = self.documents.remove(index);
        self.trace.record(
            "document.closed",
            format!("{}:{}", document.kind, document.resource),
        );
        self.active_document = match self.active_document {
            _ if self.documents.is_empty() => None,
            Some(active) if active > index => Some(active - 1),
            Some(active) => Some(active.min(self.documents.len() - 1)),
            None => None,
        };
    }

    /// Delivers a document event after consent to discard its current edits.
    /// Clean documents need no question. Busy or closed documents reject the request.
    pub fn discard_then(&mut self, document: DocumentId, event: ViewEvent) -> Result<(), String> {
        let index = self.document_index(document).ok_or("Document is closed")?;
        match self.document_can_close(index) {
            Ok(()) => self
                .requests
                .push_back(ShellRequest::DocumentEvent { document, event }),
            Err(reason @ crate::CloseError::Unsaved { .. }) => {
                self.request_discard(index, &reason, DiscardAction::Event(event));
            }
            Err(reason) => return Err(reason.to_string()),
        }
        Ok(())
    }

    /// Queues a question only for edits; busy providers cannot authorize discard.
    fn request_discard(&mut self, index: usize, reason: &crate::CloseError, action: DiscardAction) {
        if let crate::CloseError::Unsaved { revision } = reason {
            self.next_request_sequence = self
                .next_request_sequence
                .checked_add(1)
                .expect("discard sequence exhausted");
            self.discard_request = Some(DiscardRequest {
                sequence: self.next_request_sequence,
                document: self.documents[index].id,
                revision: *revision,
            });
            self.discard_confirmation = self.discard_request.map(|request| (request, action));
        }
    }

    /// Takes the pending question for presentation by the front end.
    pub fn take_discard_request(&mut self) -> Option<DiscardRequest> {
        self.discard_request.take()
    }

    /// Applies an explicit response. Newer edits or an in-flight save invalidate consent.
    pub(crate) fn answer_discard(&mut self, request: DiscardRequest, discard: bool) {
        if self
            .discard_confirmation
            .as_ref()
            .map(|(request, _)| *request)
            != Some(request)
        {
            return;
        }
        let (_, action) = self
            .discard_confirmation
            .take()
            .expect("matching confirmation");
        if !discard {
            self.set_status("Discard cancelled");
            return;
        }
        let Some(index) = self.document_index(request.document) else {
            return;
        };
        if self.document_can_close(index)
            != Err(crate::CloseError::Unsaved {
                revision: request.revision,
            })
        {
            self.set_status(
                "Document changed while confirmation was open; request the action again",
            );
            return;
        }
        self.trace.record(
            match &action {
                DiscardAction::Close { .. } => "document.discarded",
                DiscardAction::Event(_) => "document.discard-approved",
            },
            self.documents[index].resource.clone(),
        );
        match action {
            DiscardAction::Close { quitting } => {
                self.remove_document(index);
                if quitting {
                    self.request_quit();
                }
            }
            DiscardAction::Event(event) => self.requests.push_back(ShellRequest::DocumentEvent {
                document: request.document,
                event,
            }),
        }
    }

    pub(crate) fn find_document(&self, kind: &DocumentKind, resource: &str) -> Option<usize> {
        self.documents.iter().position(|document| {
            &document.kind == kind
                && (document.resource == resource
                    || self
                        .resource_changes
                        .get(&document.id)
                        .is_some_and(|change| change.resource == resource))
        })
    }

    /// Adds `document` as the last tab without activating it, records
    /// `document.opened`, and returns its index.
    pub(crate) fn push_document(&mut self, mut document: Document) -> usize {
        self.next_document_id += 1;
        document.id = DocumentId(self.next_document_id);
        self.trace.record(
            "document.opened",
            format!("{}:{}", document.kind, document.resource),
        );
        self.documents.push(document);
        self.documents.len() - 1
    }

    /// Opens (or activates, if already open) a document once the current
    /// handler returns.
    pub fn open_document(&mut self, kind: DocumentKind, resource: impl Into<String>) {
        self.requests.push_back(ShellRequest::OpenDocument {
            kind,
            resource: resource.into(),
            event: None,
        });
    }

    /// Opens or activates a document, then delivers its initial navigation event.
    pub fn open_document_with_event(
        &mut self,
        kind: DocumentKind,
        resource: impl Into<String>,
        event: ViewEvent,
    ) {
        self.requests.push_back(ShellRequest::OpenDocument {
            kind,
            resource: resource.into(),
            event: Some(event),
        });
    }

    /// Brings a docked view to the front once the current handler returns.
    pub fn show_view(&mut self, view: ViewId) {
        self.requests.push_back(ShellRequest::ShowView(view));
    }

    /// Runs another command once the current handler returns.
    pub fn run_command(&mut self, command: CommandId, args: Option<Value>) {
        self.requests
            .push_back(ShellRequest::RunCommand { command, args });
    }

    /// The view in front at `location`.
    pub fn active_view(&self, location: ViewLocation) -> Option<&ViewId> {
        self.active_views.get(&location)
    }

    /// Brings `view` to the front at `location`. Records `view.activated`
    /// only when the front view changes. The caller checks that `view` is
    /// docked at `location`.
    pub(crate) fn set_active_view(&mut self, location: ViewLocation, view: ViewId) {
        if self.active_views.get(&location) != Some(&view) {
            self.trace.record("view.activated", view.as_str());
            self.active_views.insert(location, view);
        }
    }

    /// Asks the front end to quit after the current action.
    pub fn request_quit(&mut self) {
        for index in 0..self.documents.len() {
            if let Err(reason) = self.document_can_close(index) {
                self.request_discard(index, &reason, DiscardAction::Close { quitting: true });
                self.set_status(format!("{}: {reason}", self.documents[index].title));
                return;
            }
        }
        self.quit_requested = true;
    }

    /// The first document preventing normal shutdown, with its title and reason.
    pub fn close_blocker(&self) -> Option<String> {
        self.documents
            .iter()
            .enumerate()
            .find_map(|(index, document)| {
                self.document_can_close(index)
                    .err()
                    .map(|reason| format!("{}: {reason}", document.title))
            })
    }

    /// Whether quitting was requested.
    pub fn quit_requested(&self) -> bool {
        self.quit_requested
    }

    /// Asks the front end to move keyboard focus to `target`.
    pub fn request_focus(&mut self, target: FocusTarget) {
        self.focus_request = Some(target);
    }

    /// Returns and clears the pending focus request. Called by the front end.
    pub fn take_focus_request(&mut self) -> Option<FocusTarget> {
        self.focus_request.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{TextModel, ViewContent};

    struct Blank;
    impl ViewProvider for Blank {
        fn content(&self, _shell: &Shell) -> ViewContent {
            ViewContent::Text(TextModel::default())
        }
    }

    fn shell_with(resources: &[&str]) -> Shell {
        let mut shell = Shell::default();
        for resource in resources {
            shell.push_document(Document {
                id: DocumentId(0),
                kind: DocumentKind::literal("test.doc"),
                resource: (*resource).to_owned(),
                title: (*resource).to_owned(),
                provider: Rc::new(Blank),
                owner: ModuleId::literal("test"),
            });
        }
        shell
    }

    fn active(shell: &Shell) -> Option<&str> {
        shell
            .active_document()
            .map(|document| document.resource.as_str())
    }

    #[test]
    fn resource_reservations_keep_identity_and_exclude_competing_opens_and_saves() {
        let mut shell = shell_with(&["a", "b"]);
        let a = shell.documents[0].id;
        let b = shell.documents[1].id;
        assert!(shell.reserve_resource(a, "b".into(), "B".into()).is_err());
        let change = shell
            .reserve_resource(a, "new".into(), "New".into())
            .unwrap();
        assert_eq!(
            shell.find_document(&DocumentKind::literal("test.doc"), "new"),
            Some(0)
        );
        assert!(
            shell
                .reserve_resource(b, "new".into(), "New".into())
                .is_err()
        );
        assert!(
            shell
                .reserve_resource(a, "another".into(), "Another".into())
                .is_err()
        );
        assert!(!shell.close_document(0));
        shell.request_quit();
        assert!(!shell.quit_requested());
        shell.finish_resource_change(change.clone(), false);
        assert_eq!(shell.document(a).unwrap().resource, "a");
        let retry = shell
            .reserve_resource(a, "new".into(), "New".into())
            .unwrap();
        shell.finish_resource_change(change, true);
        assert_eq!(shell.document(a).unwrap().resource, "a");
        shell.finish_resource_change(retry, true);
        assert_eq!(shell.document(a).unwrap().resource, "new");
        assert_eq!(shell.document(a).unwrap().title, "New");
        assert_eq!(
            shell.find_document(&DocumentKind::literal("test.doc"), "a"),
            None
        );
        assert!(shell.close_document(0));
        assert!(
            shell
                .reserve_resource(a, "other".into(), "Other".into())
                .is_err()
        );
    }

    struct Guard(RefCell<Result<(), crate::CloseError>>);
    impl ViewProvider for Guard {
        fn content(&self, _: &Shell) -> ViewContent {
            ViewContent::Text(TextModel::default())
        }
        fn can_close(&self) -> Result<(), crate::CloseError> {
            self.0.borrow().clone()
        }
    }

    #[test]
    fn discard_events_keep_identity_and_cannot_replay_or_replace_newer_consent() {
        let mut shell = shell_with(&["a", "b"]);
        let id = shell.documents[0].id;
        let guard = Rc::new(Guard(RefCell::new(Err(crate::CloseError::Unsaved {
            revision: 1,
        }))));
        shell.documents[0].provider = guard.clone();
        shell.discard_then(id, ViewEvent::RevealLine(3)).unwrap();
        let cancelled = shell.take_discard_request().unwrap();
        shell.answer_discard(cancelled, false);
        shell.answer_discard(cancelled, true);
        assert!(shell.requests.is_empty());
        shell.discard_then(id, ViewEvent::RevealLine(4)).unwrap();
        let stale = shell.take_discard_request().unwrap();
        *guard.0.borrow_mut() = Err(crate::CloseError::Unsaved { revision: 2 });
        shell.answer_discard(stale, true);
        assert!(shell.requests.is_empty());
        shell.discard_then(id, ViewEvent::RevealLine(5)).unwrap();
        let current = shell.take_discard_request().unwrap();
        shell.answer_discard(stale, true);
        shell.activate_document(1);
        shell.answer_discard(current, true);
        assert!(
            matches!(shell.requests.pop_front(), Some(ShellRequest::DocumentEvent { document, event: ViewEvent::RevealLine(5) }) if document == id)
        );
        assert_eq!(shell.documents.len(), 2);
        shell.answer_discard(current, true);
        assert!(shell.requests.is_empty());
        *guard.0.borrow_mut() = Err(crate::CloseError::Blocked("Working".into()));
        assert!(shell.discard_then(id, ViewEvent::RevealLine(6)).is_err());
        *guard.0.borrow_mut() = Ok(());
        shell.discard_then(id, ViewEvent::RevealLine(7)).unwrap();
        assert!(
            matches!(shell.requests.pop_front(), Some(ShellRequest::DocumentEvent { document, event: ViewEvent::RevealLine(7) }) if document == id)
        );
        shell.close_document(0);
        assert!(shell.discard_then(id, ViewEvent::RevealLine(8)).is_err());
    }

    #[test]
    fn discard_requires_the_same_document_revision_and_never_bypasses_busy_work() {
        use crate::CloseError;
        let mut shell = shell_with(&["a", "b"]);
        let guard = Rc::new(Guard(RefCell::new(Err(CloseError::Unsaved {
            revision: 1,
        }))));
        shell.documents[0].provider = guard.clone();
        assert!(!shell.close_document(0));
        let request = shell.take_discard_request().unwrap();
        shell.answer_discard(request, false);
        assert_eq!(shell.documents.len(), 2);
        shell.answer_discard(request, true);
        assert_eq!(
            shell.documents.len(),
            2,
            "cancelled consent cannot be replayed"
        );
        for changed in [
            Err(CloseError::Unsaved { revision: 2 }),
            Err(CloseError::Blocked("Saving".into())),
            Ok(()),
        ] {
            *guard.0.borrow_mut() = Err(CloseError::Unsaved { revision: 1 });
            assert!(!shell.close_document(0));
            let fresh = shell.take_discard_request().unwrap();
            shell.answer_discard(request, true);
            assert_eq!(
                shell.documents.len(),
                2,
                "an older question cannot answer a newer one"
            );
            *guard.0.borrow_mut() = changed;
            shell.answer_discard(fresh, true);
            assert_eq!(shell.documents.len(), 2);
        }
        *guard.0.borrow_mut() = Err(CloseError::Blocked("Saving".into()));
        assert!(!shell.close_document(0));
        assert!(shell.take_discard_request().is_none());
        *guard.0.borrow_mut() = Err(CloseError::Unsaved { revision: 3 });
        assert!(!shell.close_document(0));
        let request = shell.take_discard_request().unwrap();
        shell.activate_document(1);
        shell.answer_discard(request, true);
        assert_eq!(active(&shell), Some("b"));
        shell.answer_discard(request, true);
        assert_eq!(shell.documents.len(), 1);
    }

    #[test]
    fn quitting_confirms_each_dirty_document_and_can_stop_without_discarding_the_rest() {
        let mut shell = shell_with(&["a", "b"]);
        for document in &mut shell.documents {
            document.provider = Rc::new(Guard(RefCell::new(Err(crate::CloseError::Unsaved {
                revision: 0,
            }))));
        }
        shell.request_quit();
        assert!(!shell.quit_requested());
        let first = shell.take_discard_request().unwrap();
        shell.answer_discard(first, true);
        assert_eq!(shell.documents.len(), 1);
        assert!(!shell.quit_requested());
        let second = shell.take_discard_request().unwrap();
        shell.answer_discard(second, false);
        assert_eq!(shell.documents.len(), 1);
        shell.request_quit();
        let second = shell.take_discard_request().unwrap();
        shell.answer_discard(second, true);
        assert!(shell.quit_requested());
        assert!(shell.documents.is_empty());
    }

    #[test]
    fn closing_keeps_the_same_document_active_or_moves_to_a_neighbour() {
        let mut shell = shell_with(&["a", "b", "c"]);
        shell.activate_document(1);

        shell.close_document(0);
        assert_eq!(active(&shell), Some("b"), "closing before the active one");

        shell.close_document(1);
        assert_eq!(active(&shell), Some("b"), "closing after the active one");

        shell.close_document(0);
        assert_eq!(active(&shell), None, "closing the last document");
        shell.close_document(0);
        assert!(shell.documents().is_empty());
    }

    #[test]
    fn closing_the_active_last_tab_activates_the_previous_one() {
        let mut shell = shell_with(&["a", "b", "c"]);
        shell.activate_document(2);
        shell.close_document(2);
        assert_eq!(active(&shell), Some("b"));
    }

    #[test]
    fn trace_is_bounded_and_keeps_sequence_numbers() {
        let mut trace = Trace::new(3);
        for n in 1..=5 {
            trace.record("test", n.to_string());
        }
        let kept: Vec<(u64, String)> = trace
            .since(0)
            .into_iter()
            .map(|event| (event.seq, event.detail))
            .collect();
        assert_eq!(
            kept,
            [
                (3, "3".to_owned()),
                (4, "4".to_owned()),
                (5, "5".to_owned())
            ]
        );
        assert_eq!(trace.last_seq(), 5);
        assert_eq!(trace.since(4).len(), 1);
    }

    #[test]
    fn output_drops_oldest_lines_beyond_its_limit_and_snapshots_are_stable() {
        let output = OutputLog::default();
        output.append("first", Tone::Normal);
        let snapshot = output.lines();
        for n in 0..OutputLog::MAX_LINES {
            output.append(n.to_string(), Tone::Normal);
        }
        assert_eq!(snapshot.len(), 1, "earlier snapshot is unaffected");
        let lines = output.lines();
        assert_eq!(lines.len(), OutputLog::MAX_LINES);
        assert_eq!(lines[0].text, "0");
        output.clear();
        assert!(output.lines().is_empty());
    }
}
