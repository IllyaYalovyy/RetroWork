//! Bounded file work and editing sessions owned by open source documents.

use crate::MAX_SOURCE_BYTES;
use retrowork_platform::{
    CommandContext, CommandError, DocumentFactory, DocumentId, Inbox, ModuleResource,
    OpenedDocument, ResourceChange, Shell, SourceModel, ViewContent, ViewEvent, ViewProvider,
    document_file::DocumentFile,
    document_session::{DocumentSession, SaveTask, SourceEvent},
};
use std::{
    cell::{Cell, RefCell},
    io,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};

type Loaded = Result<DocumentFile, String>;
type Delivery = Mutex<Option<Loaded>>;

/// One pending load. A closed document makes its delivery unreachable.
struct Load {
    document: Option<DocumentId>,
    path: PathBuf,
    delivery: Weak<Delivery>,
    inbox: Inbox,
}

/// File work shared by all source documents; only the worker performs I/O.
enum Job {
    Load(Load),
    Prepare {
        document: DocumentId,
        path: PathBuf,
        inbox: Inbox,
    },
    Save {
        task: Box<SaveTask>,
        path: PathBuf,
        change: Option<ResourceChange>,
        inbox: Inbox,
    },
}

/// Reloads the owning document after the platform has checked discard consent.
#[derive(Clone, Debug, PartialEq)]
struct Reload(DocumentId);

/// Requests a disk reload without applying discard consent to another tab.
pub(crate) fn reload(cx: &mut CommandContext<'_>) -> Result<(), CommandError> {
    let (document, _) = save_target(cx.shell, cx.args).map_err(CommandError::failed)?;
    cx.shell
        .discard_then(
            document,
            ViewEvent::Native(retrowork_platform::NativeValue::new(Reload(document))),
        )
        .map_err(CommandError::failed)
}

/// Destination selection and worker normalization, addressed to a stable document.
#[derive(Clone, Debug, PartialEq)]
enum PathEvent {
    Selected {
        document: DocumentId,
        path: PathBuf,
    },
    Prepared {
        document: DocumentId,
        result: Result<PathBuf, String>,
    },
}

/// Routes an asynchronous path result only to the document that requested it.
fn deliver_path(document: DocumentId, event: PathEvent, cx: &mut CommandContext<'_>) {
    let Some(provider) = cx.shell.document(document).map(|doc| doc.provider.clone()) else {
        return;
    };
    if let Err(error) = provider.handle(
        ViewEvent::Native(retrowork_platform::NativeValue::new(event)),
        cx,
    ) {
        cx.shell.set_status(error.to_string());
        cx.shell
            .output()
            .append(error.to_string(), retrowork_platform::Tone::Error);
    }
}

/// Validates a source target, including a captured system-picker response.
pub(crate) fn save_target(
    shell: &Shell,
    args: Option<&serde_json::Value>,
) -> Result<(DocumentId, DocumentSession), String> {
    let document = if let Some(id) = args.and_then(|args| args.get("document")) {
        let id = serde_json::from_value(id.clone()).map_err(|_| "Invalid document identity")?;
        shell.document(id)
    } else {
        shell.active_document()
    }
    .ok_or("Document is closed")?;
    if document.kind.as_str() != crate::DOCUMENT_SOURCE {
        return Err("This document is not a source file".into());
    }
    if let Err(retrowork_platform::CloseError::Blocked(reason)) = document.provider.can_close() {
        return Err(reason);
    }
    let ViewContent::Source(model) = document.provider.content(shell) else {
        return Err("Source is not loaded".into());
    };
    let session = model.editing.ok_or_else(|| {
        if model.message.is_empty() {
            "Source is not editable".into()
        } else {
            model.message
        }
    })?;
    Ok((document.id, session))
}

/// Finds dirty sources without an outstanding write, including background tabs.
fn pending_sources(shell: &Shell) -> impl Iterator<Item = &retrowork_platform::Document> {
    shell.documents().iter().filter(|document| {
        if document.kind.as_str() != crate::DOCUMENT_SOURCE {
            return false;
        }
        let ViewContent::Source(model) = document.provider.content(shell) else {
            return false;
        };
        model.editing.is_some_and(|session| {
            let status = session.status();
            status.dirty && !status.saving
        })
    })
}

/// Save All is available even when the active tab is not an editable source.
pub(crate) fn can_save_all(shell: &Shell) -> Result<(), String> {
    pending_sources(shell)
        .next()
        .map(|_| ())
        .ok_or_else(|| "No changes waiting to be saved".into())
}

/// Attempts every pending source independently without changing the active tab.
/// Admission failures are reported individually; accepted writes finish asynchronously.
pub(crate) fn save_all(cx: &mut CommandContext<'_>) -> Result<(), CommandError> {
    let sources: Vec<_> = pending_sources(cx.shell)
        .map(|document| (document.resource.clone(), document.provider.clone()))
        .collect();
    let mut rejected = false;
    for (resource, provider) in sources {
        if let Err(error) = provider.handle(SourceEvent::Save.into(), cx) {
            rejected = true;
            cx.shell.output().append(
                format!("Cannot save {resource}: {error}"),
                retrowork_platform::Tone::Error,
            );
        }
    }
    if rejected {
        return Err(CommandError::failed(
            "Some saves could not start; see Output",
        ));
    }
    Ok(())
}

/// Opens the system Save As picker or handles its response for the original document.
pub(crate) fn save_as(cx: &mut CommandContext<'_>) -> Result<(), CommandError> {
    let (document, session) = save_target(cx.shell, cx.args).map_err(CommandError::failed)?;
    if let Some(path) = cx.args.and_then(|args| args.get("path")) {
        let path = serde_json::from_value(path.clone())
            .map_err(|_| CommandError::failed("Invalid destination path"))?;
        let provider = cx
            .shell
            .document(document)
            .expect("validated document")
            .provider
            .clone();
        provider.handle(
            ViewEvent::Native(retrowork_platform::NativeValue::new(PathEvent::Selected {
                document,
                path,
            })),
            cx,
        )
    } else {
        let path = session.path();
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        cx.shell
            .choose_save_path(retrowork_platform::SavePathRequest {
                document,
                directory: path.parent().unwrap_or(Path::new(".")).to_owned(),
                name,
                command: retrowork_platform::CommandId::literal(crate::COMMAND_SAVE_AS),
            });
        Ok(())
    }
}

/// Serializes reads and saves through a bounded queue without blocking the UI.
pub(crate) struct SourceFactory {
    sender: RefCell<Option<Arc<mpsc::SyncSender<Job>>>>,
    cancelled: Arc<AtomicBool>,
}
impl SourceFactory {
    pub(crate) fn new() -> io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel::<Job>(16);
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        std::thread::Builder::new()
            .name("source-files".into())
            .spawn(move || {
                while let Ok(job) = receiver.recv() {
                    if worker_cancelled.load(Ordering::Acquire) {
                        break;
                    }
                    let load = match job {
                        Job::Load(load) => load,
                        Job::Prepare {
                            document,
                            path,
                            inbox,
                        } => {
                            let result =
                                DocumentFile::new_path(&path).map_err(|error| error.to_string());
                            let _ = inbox.post_blocking(Box::new(move |cx| {
                                deliver_path(document, PathEvent::Prepared { document, result }, cx)
                            }));
                            continue;
                        }
                        Job::Save {
                            task,
                            path,
                            change,
                            inbox,
                        } => {
                            let completed = (*task).run();
                            let _ = inbox.post_blocking(Box::new(move |cx| {
                                if let Some(error) = completed.error() {
                                    let message =
                                        format!("Cannot save {}: {error}", path.display());
                                    cx.shell.set_status(&message);
                                    cx.shell
                                        .output()
                                        .append(message, retrowork_platform::Tone::Error);
                                }
                                if let Some(change) = change {
                                    cx.shell
                                        .finish_resource_change(change, completed.succeeded());
                                }
                                drop(completed);
                            }));
                            continue;
                        }
                    };
                    if load.delivery.strong_count() == 0 {
                        continue;
                    }
                    let result = read_file(&load.path);
                    if worker_cancelled.load(Ordering::Acquire) {
                        break;
                    }
                    if let Some(delivery) = load.delivery.upgrade() {
                        *delivery.lock().unwrap() = Some(result);
                        drop(delivery);
                        let _ = load.inbox.post_blocking(Box::new(move |cx| {
                            if let Some(document) =
                                load.document.and_then(|id| cx.shell.document(id))
                                && let ViewContent::Source(model) =
                                    document.provider.content(cx.shell)
                            {
                                let message = if model.message.is_empty() {
                                    format!("Reloaded {}", document.resource)
                                } else {
                                    model.message
                                };
                                cx.shell.set_status(message);
                            }
                        }));
                    }
                }
            })?;
        Ok(Self {
            sender: RefCell::new(Some(Arc::new(sender))),
            cancelled,
        })
    }
}
impl ModuleResource for SourceFactory {
    fn shutdown(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.sender.borrow_mut().take();
    }
}
impl Drop for SourceFactory {
    fn drop(&mut self) {
        self.shutdown();
    }
}
impl DocumentFactory for SourceFactory {
    fn open(
        &self,
        resource: &str,
        cx: &mut CommandContext<'_>,
    ) -> Result<OpenedDocument, CommandError> {
        let path = PathBuf::from(resource);
        let document = SourceDocument::new(
            self.sender
                .borrow()
                .as_ref()
                .map(Arc::downgrade)
                .unwrap_or_default(),
        );
        self.sender
            .borrow()
            .as_ref()
            .ok_or_else(|| CommandError::failed("source loader is closed"))?
            .try_send(Job::Load(Load {
                document: None,
                path: path.clone(),
                delivery: Arc::downgrade(&document.delivery),
                inbox: cx.services.inbox(),
            }))
            .map_err(|_| CommandError::failed("source loader is busy or unavailable; try again"))?;
        Ok(OpenedDocument {
            title: path.file_name().map_or_else(
                || resource.to_owned(),
                |name| name.to_string_lossy().into_owned(),
            ),
            provider: Rc::new(document),
        })
    }
}

/// Loads source through the shared bounded file reader and adds path context.
fn read_file(path: &Path) -> Loaded {
    DocumentFile::open(path, MAX_SOURCE_BYTES)
        .map_err(|error| format!("cannot open {}: {error}", path.display()))
}

/// Holds loaded text and a navigation request that may arrive before the read completes.
struct SourceDocument {
    pending: Cell<Option<&'static str>>,
    reload_revision: Cell<Option<u64>>,
    delivery: Arc<Delivery>,
    text: RefCell<Arc<str>>,
    hint: RefCell<String>,
    line: Cell<Option<usize>>,
    editing: RefCell<Option<DocumentSession>>,
    sender: Weak<mpsc::SyncSender<Job>>,
}
impl SourceDocument {
    fn new(sender: Weak<mpsc::SyncSender<Job>>) -> Self {
        Self {
            pending: Cell::new(None),
            reload_revision: Cell::new(None),
            delivery: Arc::new(Mutex::new(None)),
            text: RefCell::new(Arc::from("")),
            hint: RefCell::new("Loading source…".into()),
            line: Cell::new(None),
            editing: RefCell::new(None),
            sender,
        }
    }
}
impl ViewProvider for SourceDocument {
    fn content(&self, shell: &Shell) -> ViewContent {
        if let Some(result) = self.delivery.lock().unwrap().take() {
            if let Some(revision) = self.reload_revision.take() {
                self.pending.set(None);
                let session = self
                    .editing
                    .borrow()
                    .clone()
                    .expect("reload owns an editing session");
                let result = result.and_then(|file| {
                    let text = file.text();
                    session.reload(file, revision)?;
                    *self.text.borrow_mut() = text;
                    self.line.set(None);
                    Ok(())
                });
                match result {
                    Ok(()) => self.hint.borrow_mut().clear(),
                    Err(error) => {
                        let message =
                            format!("Cannot reload {}: {error}", session.path().display());
                        shell
                            .output()
                            .append(&message, retrowork_platform::Tone::Error);
                        *self.hint.borrow_mut() = message;
                    }
                }
            } else {
                match result {
                    Ok(file) => {
                        *self.text.borrow_mut() = file.text();
                        if file.is_read_only() {
                            *self.hint.borrow_mut() = "Read-only file".into();
                        } else {
                            *self.editing.borrow_mut() = Some(DocumentSession::new(file));
                            *self.hint.borrow_mut() = String::new();
                        }
                    }
                    Err(error) => *self.hint.borrow_mut() = error,
                }
            }
        }
        let lines = Arc::clone(&self.text.borrow());
        ViewContent::Source(SourceModel {
            text: lines,
            editing: self.editing.borrow().clone(),
            reveal_line: self.line.get(),
            message: self.hint.borrow().clone(),
        })
    }
    fn can_close(&self) -> Result<(), retrowork_platform::CloseError> {
        if let Some(reason) = self.pending.get() {
            return Err(retrowork_platform::CloseError::Blocked(reason.into()));
        }
        self.editing
            .borrow()
            .as_ref()
            .map_or(Ok(()), DocumentSession::can_close)
    }
    fn handle(&self, event: ViewEvent, cx: &mut CommandContext<'_>) -> Result<(), CommandError> {
        if let ViewEvent::RevealLine(line) = event {
            self.line.set(Some(line));
        } else if let ViewEvent::Native(event) = event {
            if let Some(Reload(document)) = event.get::<Reload>() {
                return self.reload(*document, cx);
            }
            if let Some(event) = event.get::<PathEvent>() {
                return self.handle_path(event, cx).inspect_err(|error| {
                    *self.hint.borrow_mut() = error.to_string();
                });
            }
            match event.get::<SourceEvent>() {
                Some(SourceEvent::Edited(text)) => {
                    if let Some(session) = self.editing.borrow().as_ref() {
                        session.edit(text.clone());
                    }
                }
                Some(SourceEvent::Save) => {
                    if let Some(reason) = self.pending.get() {
                        return Err(CommandError::failed(reason));
                    }
                    self.hint.borrow_mut().clear();
                    let editing = self.editing.borrow();
                    let session = editing
                        .as_ref()
                        .ok_or_else(|| CommandError::failed("Source is not loaded"))?;
                    let task = session.save().map_err(CommandError::failed)?;
                    self.sender
                        .upgrade()
                        .ok_or_else(|| CommandError::failed("Source worker is closed"))?
                        .try_send(Job::Save {
                            task: Box::new(task),
                            path: session.path(),
                            change: None,
                            inbox: cx.services.inbox(),
                        })
                        .map_err(|_| CommandError::failed("Source worker is busy; retry Save"))?;
                }
                None => {}
            }
        }
        Ok(())
    }
}

impl SourceDocument {
    /// Retains the current buffer until a bounded worker read succeeds for this revision.
    fn reload(
        &self,
        document: DocumentId,
        cx: &mut CommandContext<'_>,
    ) -> Result<(), CommandError> {
        if let Some(reason) = self.pending.get() {
            return Err(CommandError::failed(reason));
        }
        let session = self
            .editing
            .borrow()
            .clone()
            .ok_or_else(|| CommandError::failed("Source is not editable"))?;
        if session.status().saving {
            return Err(CommandError::failed("A save is still running"));
        }
        self.sender
            .upgrade()
            .ok_or_else(|| CommandError::failed("Source worker is closed"))?
            .try_send(Job::Load(Load {
                document: Some(document),
                path: session.path(),
                delivery: Arc::downgrade(&self.delivery),
                inbox: cx.services.inbox(),
            }))
            .map_err(|_| CommandError::failed("Source worker is busy; retry Reload"))?;
        self.reload_revision.set(Some(session.revision()));
        self.pending.set(Some("Reloading source"));
        *self.hint.borrow_mut() = "Reloading source…".into();
        Ok(())
    }

    /// Normalizes paths on the worker, then reserves ownership before admitting a write.
    fn handle_path(
        &self,
        event: &PathEvent,
        cx: &mut CommandContext<'_>,
    ) -> Result<(), CommandError> {
        let sender = self
            .sender
            .upgrade()
            .ok_or_else(|| CommandError::failed("Source worker is closed"))?;
        match event {
            PathEvent::Selected { document, path } => {
                if let Some(reason) = self.pending.get() {
                    return Err(CommandError::failed(reason));
                }
                self.pending.set(Some("Preparing Save As"));
                if sender
                    .try_send(Job::Prepare {
                        document: *document,
                        path: path.clone(),
                        inbox: cx.services.inbox(),
                    })
                    .is_err()
                {
                    self.pending.set(None);
                    return Err(CommandError::failed("Source worker is busy; retry Save As"));
                }
                self.hint.borrow_mut().clear();
            }
            PathEvent::Prepared { document, result } => {
                self.pending.set(None);
                let path = result
                    .as_ref()
                    .map_err(|error| CommandError::failed(error.clone()))?;
                for other in cx.shell.documents().iter().filter(|other| {
                    other.id != *document && other.kind.as_str() == crate::DOCUMENT_SOURCE
                }) {
                    if let ViewContent::Source(model) = other.provider.content(cx.shell)
                        && model.editing.is_some_and(|session| session.path() == *path)
                    {
                        return Err(CommandError::failed("Destination is already open"));
                    }
                }
                let resource = path
                    .to_str()
                    .ok_or_else(|| CommandError::failed("Destination path is not UTF-8"))?
                    .to_owned();
                let title = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                let session = self
                    .editing
                    .borrow()
                    .clone()
                    .ok_or_else(|| CommandError::failed("Source is not loaded"))?;
                let change = cx
                    .shell
                    .reserve_resource(*document, resource, title)
                    .map_err(CommandError::failed)?;
                let admitted = session
                    .save_as(path.clone())
                    .map_err(CommandError::failed)
                    .and_then(|task| {
                        sender
                            .try_send(Job::Save {
                                task: Box::new(task),
                                path: path.clone(),
                                change: Some(change.clone()),
                                inbox: cx.services.inbox(),
                            })
                            .map_err(|_| {
                                CommandError::failed("Source worker is busy; retry Save As")
                            })
                    });
                if admitted.is_err() {
                    cx.shell.finish_resource_change(change, false);
                }
                admitted?;
                self.hint.borrow_mut().clear();
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn load_errors_include_the_path() {
        let path = Path::new("/nonexistent-retrowork-source");
        assert!(
            read_file(path)
                .unwrap_err()
                .contains(&*path.to_string_lossy())
        );
    }
    #[test]
    fn closing_a_document_releases_its_buffer_and_pending_delivery() {
        let document = SourceDocument::new(Weak::new());
        let buffer = Arc::downgrade(&document.text.borrow());
        let delivery = Arc::downgrade(&document.delivery);
        drop(document);
        assert!(buffer.upgrade().is_none());
        assert!(delivery.upgrade().is_none());
    }
}
