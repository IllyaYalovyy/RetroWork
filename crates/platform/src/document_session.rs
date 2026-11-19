//! Editable snapshots with one outstanding save and an independently tracked baseline.

use crate::document_file::DocumentFile;
use ropey::Rope;
use std::{
    cell::RefCell,
    path::PathBuf,
    rc::Rc,
    sync::{Arc, Mutex, Weak},
};

/// Typed interactions shared by source providers and their editor views.
#[derive(Clone, Debug, PartialEq)]
pub enum SourceEvent {
    /// The exact post-edit snapshot, including undo and redo.
    Edited(Rope),
    /// Save the current edits through the provider's worker.
    Save,
}

impl From<SourceEvent> for crate::ViewEvent {
    fn from(event: SourceEvent) -> Self {
        Self::Native(crate::NativeValue::new(event))
    }
}

/// A document's current save state. Errors never clear unsaved changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveStatus {
    pub dirty: bool,
    pub saving: bool,
    pub error: Option<String>,
}

/// Shared document text and save state, independent of editor views and undo engines.
/// Rope snapshots share storage; editing does not serialize the entire document.
#[derive(Clone, Debug)]
pub struct DocumentSession(Rc<RefCell<State>>);

impl PartialEq for DocumentSession {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for DocumentSession {}

/// The file moves into a save task and returns through delivery; edits stay local.
#[derive(Debug)]
struct State {
    path: PathBuf,
    current: Rope,
    saved: Rope,
    file: Option<DocumentFile>,
    delivery: Arc<Mutex<Option<Saved>>>,
    error: Option<String>,
    dirty: bool,
    revision: u64,
}

/// Returns file ownership and the exact snapshot whose save was attempted.
#[derive(Debug)]
struct Saved {
    file: DocumentFile,
    text: Rope,
    result: Result<(), String>,
}

/// An owned save request. Run it on a worker, then wake the UI to collect its result.
/// Dropping an unexecuted request restores the file and reports cancellation.
pub struct SaveTask {
    destination: Option<PathBuf>,
    saved: Option<Saved>,
    delivery: Weak<Mutex<Option<Saved>>>,
}

/// A completed write held until its UI-side identity update is finished.
/// Dropping it delivers the result; completed writes cannot be executed again.
pub struct SaveCompletion(SaveTask);

impl SaveCompletion {
    /// The write failure, if any. The session retains it after delivery.
    pub fn error(&self) -> Option<&str> {
        self.0
            .saved
            .as_ref()
            .and_then(|saved| saved.result.as_ref().err().map(String::as_str))
    }

    /// Whether publication succeeded, before delivering its baseline to the session.
    pub fn succeeded(&self) -> bool {
        self.0
            .saved
            .as_ref()
            .is_some_and(|saved| saved.result.is_ok())
    }
}

impl DocumentSession {
    /// Creates an editing session from a file already loaded on a worker.
    pub fn new(file: DocumentFile) -> Self {
        let text = Rope::from(file.text().as_ref());
        Self(Rc::new(RefCell::new(State {
            path: file.path().to_owned(),
            current: text.clone(),
            saved: text,
            file: Some(file),
            delivery: Arc::new(Mutex::new(None)),
            error: None,
            dirty: false,
            revision: 0,
        })))
    }

    /// Returns an immutable snapshot of the current edits.
    pub fn text(&self) -> Rope {
        self.0.borrow().current.clone()
    }

    /// Identifies the current edits so background reads cannot replace newer work.
    pub fn revision(&self) -> u64 {
        self.0.borrow().revision
    }

    /// Installs a worker-loaded baseline only if the path and edit revision still match.
    /// Rejects concurrent saves and read-only replacements. Failure retains all state.
    /// The caller must obtain consent before replacing unsaved edits.
    pub fn reload(&self, file: DocumentFile, revision: u64) -> Result<(), String> {
        if self.status().saving {
            return Err("A save is still running".into());
        }
        let mut state = self.0.borrow_mut();
        if state.revision != revision {
            return Err("Document changed while reloading; reload again".into());
        }
        if file.path() != state.path {
            return Err("File target changed while reloading".into());
        }
        if file.is_read_only() {
            return Err("File became read-only; current edits were kept".into());
        }
        let text = Rope::from(file.text().as_ref());
        state.revision = state
            .revision
            .checked_add(1)
            .expect("document revision exhausted");
        state.current = text.clone();
        state.saved = text;
        state.file = Some(file);
        state.error = None;
        state.dirty = false;
        Ok(())
    }

    /// The last successfully saved path, without filesystem I/O.
    pub fn path(&self) -> PathBuf {
        self.status();
        self.0.borrow().path.clone()
    }

    /// Accepts the editor's exact post-edit snapshot, including undo and redo.
    pub fn edit(&self, text: Rope) {
        let mut state = self.0.borrow_mut();
        state.revision = state
            .revision
            .checked_add(1)
            .expect("document revision exhausted");
        state.dirty = text != state.saved;
        state.current = text;
    }

    /// Collects completed saves without filesystem I/O and reports the current state.
    pub fn status(&self) -> SaveStatus {
        let mut state = self.0.borrow_mut();
        let completed = state.delivery.lock().unwrap().take();
        if let Some(completed) = completed {
            if completed.result.is_ok() {
                state.path = completed.file.path().to_owned();
                state.saved = completed.text;
                state.dirty = state.current != state.saved;
            }
            state.error = completed.result.err();
            state.file = Some(completed.file);
        }
        SaveStatus {
            dirty: state.dirty,
            saving: state.file.is_none(),
            error: state.error.clone(),
        }
    }

    /// Takes a snapshot for saving. Rejects overlapping saves and unchanged documents.
    /// A later edit remains dirty when this request completes.
    pub fn save(&self) -> Result<SaveTask, String> {
        self.prepare_save(None)
    }

    /// Saves even unchanged text under a new name. The worker rejects existing files.
    pub fn save_as(&self, path: PathBuf) -> Result<SaveTask, String> {
        self.prepare_save(Some(path))
    }

    /// Moves file ownership into one pending save while retaining editable snapshots.
    fn prepare_save(&self, destination: Option<PathBuf>) -> Result<SaveTask, String> {
        let status = self.status();
        if status.saving {
            return Err("A save is already running".into());
        }
        if !status.dirty && destination.is_none() {
            return Err("No changes to save".into());
        }
        let mut state = self.0.borrow_mut();
        let file = state.file.take().expect("idle sessions own their file");
        state.error = None;
        Ok(SaveTask {
            destination,
            saved: Some(Saved {
                file,
                text: state.current.clone(),
                result: Err("Save cancelled; retry to save your changes".into()),
            }),
            delivery: Arc::downgrade(&state.delivery),
        })
    }

    /// Explains why closing would lose work or interrupt a save.
    pub fn can_close(&self) -> Result<(), crate::CloseError> {
        let status = self.status();
        if status.saving {
            Err(crate::CloseError::Blocked("A save is still running".into()))
        } else if status.dirty {
            Err(crate::CloseError::Unsaved {
                revision: self.0.borrow().revision,
            })
        } else {
            Ok(())
        }
    }
}

impl SaveTask {
    /// Skips requests whose document was dropped before execution. Once started,
    /// I/O cannot be cancelled. Completion never replaces current text or undo history.
    /// Hold the returned completion until UI-side identity updates are done; dropping it
    /// delivers the saved baseline and makes the session available for another save.
    pub fn run(mut self) -> SaveCompletion {
        if self.delivery.strong_count() == 0 {
            return SaveCompletion(self);
        }
        let saved = self.saved.as_mut().expect("a save task runs once");
        let text = Arc::from(saved.text.to_string());
        saved.result = match &self.destination {
            Some(path) => saved.file.save_as(path, text),
            None => saved.file.save(text),
        }
        .map_err(|error| error.to_string());
        SaveCompletion(self)
    }
}

impl Drop for SaveTask {
    fn drop(&mut self) {
        if let Some(delivery) = self.delivery.upgrade() {
            *delivery.lock().unwrap() = self.saved.take();
        }
    }
}
