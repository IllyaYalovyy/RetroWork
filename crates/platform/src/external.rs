//! Process plugins: code that runs outside the application.
//!
//! **Status: experimental, protocol version 0.** This proves the route for
//! independently built plugins end to end; it is not a stable API and has no
//! installation, signing or permission story yet.
//!
//! The plugin is an executable started on first use. Host and plugin exchange
//! one JSON object per line over the plugin's stdin and stdout:
//!
//! ```text
//! host   -> {"type":"hello","protocol":0}
//! plugin -> {"type":"ready","protocol":0,"plugin":"<module id>"}
//! host   -> {"type":"invoke","request":1,"command":"<command id>","context":{...}}
//! plugin -> {"type":"result","request":1,"effects":[{"type":"output","lines":["..."]}]}
//!        or {"type":"error","request":1,"message":"..."}
//! ```
//!
//! # What the host guarantees
//!
//! - **The UI thread never waits for a plugin.** [`ProcessHost::submit`] only
//!   enqueues; one worker thread per plugin talks to the process.
//! - **Admission is bounded.** At most one request runs and
//!   [`MAX_QUEUED_REQUESTS`] wait. Further requests fail at once with
//!   [`PluginError::Busy`]; no thread is created per request.
//! - **One deadline covers the whole request**: time spent queued, starting
//!   the process, the handshake, writing the request and waiting for the
//!   reply. A request whose deadline passes while it is queued is never sent.
//! - **Writes cannot hang.** A plugin that stops reading its stdin makes the
//!   write miss the deadline; the process is then killed.
//! - **Plugin output is bounded.** Each line is limited to
//!   [`MAX_MESSAGE_BYTES`], and at most [`READ_BUFFER_MESSAGES`] lines are
//!   buffered; beyond that the plugin blocks on its own stdout. The only
//!   message accepted is the reply to the request in flight: output sent
//!   while no request is running, a reply with another request number, or an
//!   unreadable or oversized line is a protocol violation.
//! - **A misbehaving process does not survive.** After a timeout, an exit or
//!   a protocol violation the process is killed and reaped, the failure is
//!   reported, and the next request starts a fresh process.
//! - **Shutdown is bounded.** Dropping the [`ProcessHost`] kills and reaps the
//!   process immediately, also while a request is in flight, and no new
//!   process is started afterwards.
//! - A plugin can only produce the effects listed in [`Effect`], and those are
//!   size-limited, so it cannot flood the Output view.
//!
//! # What it does not do yet
//!
//! - Only the process the host started is killed. If a plugin starts further
//!   processes they are not tracked and can outlive it.
//! - No cancellation of a running request other than its deadline, no
//!   concurrent requests, no plugin-defined views, no sandbox: a process
//!   plugin runs with the user's privileges.
//! - The plugin's stderr is discarded.

use std::{
    cell::RefCell,
    io::{self, BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    rc::Rc,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError},
    },
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    activation::ActivationContext,
    command::CommandContext,
    core::VIEW_OUTPUT,
    id::{CommandId, ModuleId, ViewId},
    model::Tone,
    module::ActivationError,
};

/// Version of the wire protocol described in the module documentation.
pub const PROTOCOL_VERSION: u32 = 0;
/// Largest accepted message from a plugin, in bytes.
pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
/// Most output lines one result may add.
pub const MAX_OUTPUT_LINES: usize = 500;
/// Longest accepted output line or status text, in bytes.
pub const MAX_LINE_BYTES: usize = 4096;
/// Most rows of the active document handed to a plugin.
pub const MAX_EXPORTED_ROWS: usize = 5000;
/// Requests that may wait behind the one in flight.
pub const MAX_QUEUED_REQUESTS: usize = 4;
/// Plugin messages buffered before the plugin blocks on its stdout.
pub const READ_BUFFER_MESSAGES: usize = 8;

/// What a plugin may ask the host to do.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Effect {
    /// Append lines to the Output view.
    Output { lines: Vec<String> },
    /// Set the status bar text.
    Status { text: String },
}

/// Why a request to a process plugin produced no effects.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PluginError {
    #[error("could not start `{program}`: {message}")]
    Spawn { program: String, message: String },
    #[error("too many requests are waiting for this plugin; try again when it has caught up")]
    Busy,
    #[error("no reply within {0:?}; the plugin process was stopped")]
    Timeout(Duration),
    #[error("the plugin process exited unexpectedly")]
    Exited,
    #[error("protocol violation: {0}; the plugin process was stopped")]
    Protocol(String),
    #[error("the plugin was shut down")]
    ShutDown,
    #[error("{0}")]
    Reported(String),
}

impl PluginError {
    /// Whether the process can no longer be trusted to be in step.
    fn is_fatal(&self) -> bool {
        !matches!(self, PluginError::Reported(_) | PluginError::Busy)
    }
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
enum HostMessage<'a> {
    Hello {
        protocol: u32,
    },
    Invoke {
        request: u64,
        command: &'a str,
        context: &'a Value,
    },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
enum PluginMessage {
    Ready { protocol: u32, plugin: String },
    Result { request: u64, effects: Vec<Effect> },
    Error { request: u64, message: String },
}

type Done = Box<dyn FnOnce(Result<Vec<Effect>, PluginError>) + Send>;

struct Job {
    request: u64,
    command: CommandId,
    context: Value,
    deadline: Instant,
    done: Done,
}

/// The child process, shared between the worker and the host so that either
/// can stop it. The lock is only ever held to start or stop the process,
/// never across a wait on the plugin.
#[derive(Clone, Default)]
struct ChildSlot {
    child: Arc<Mutex<Option<Child>>>,
    closed: Arc<AtomicBool>,
}

impl ChildSlot {
    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Child>> {
        self.child.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Kills and reaps the process, if one is running.
    fn stop(&self) {
        if let Some(mut child) = self.lock().take() {
            // The process may already be gone; either way it must not linger
            // as a zombie. `wait` returns promptly after `kill`.
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

struct Settings {
    plugin: ModuleId,
    program: PathBuf,
    timeout: Duration,
}

/// One running plugin process with its reader and writer threads.
struct Session {
    slot: ChildSlot,
    outgoing: SyncSender<Vec<u8>>,
    written: Receiver<io::Result<()>>,
    incoming: Receiver<io::Result<String>>,
}

impl Drop for Session {
    fn drop(&mut self) {
        // Killing the process closes its pipes, which ends both threads: the
        // writer gets a broken pipe, the reader end-of-file or a closed channel.
        self.slot.stop();
    }
}

impl Session {
    fn start(settings: &Settings, slot: &ChildSlot) -> Result<Self, PluginError> {
        let spawn_error = |error: io::Error| PluginError::Spawn {
            program: settings.program.display().to_string(),
            message: error.to_string(),
        };
        let (mut stdin, stdout) = {
            let mut guard = slot.lock();
            // Checked under the lock so that a host being dropped either sees
            // this process and kills it, or prevents it from starting.
            if slot.closed.load(Ordering::SeqCst) {
                return Err(PluginError::ShutDown);
            }
            let mut child = Command::new(&settings.program)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .map_err(spawn_error)?;
            let pipes = (
                child.stdin.take().expect("stdin was piped"),
                child.stdout.take().expect("stdout was piped"),
            );
            *guard = Some(child);
            pipes
        };

        // From here on, dropping `session` stops the process.
        let (outgoing, to_write) = mpsc::sync_channel::<Vec<u8>>(1);
        let (wrote, written) = mpsc::sync_channel::<io::Result<()>>(1);
        let (received, incoming) = mpsc::sync_channel::<io::Result<String>>(READ_BUFFER_MESSAGES);
        let session = Session {
            slot: slot.clone(),
            outgoing,
            written,
            incoming,
        };

        let writer = thread::Builder::new()
            .name(format!("plugin-writer:{}", settings.plugin))
            .spawn(move || {
                for bytes in to_write {
                    let result = write_message(&mut stdin, &bytes);
                    let failed = result.is_err();
                    if wrote.send(result).is_err() || failed {
                        break;
                    }
                }
            });
        let reader = thread::Builder::new()
            .name(format!("plugin-reader:{}", settings.plugin))
            .spawn(move || {
                let mut reader = BufReader::new(stdout);
                loop {
                    let message = match read_message(&mut reader) {
                        Ok(Some(line)) => Ok(line),
                        Ok(None) => break,
                        Err(error) => Err(error),
                    };
                    let failed = message.is_err();
                    // Blocks while the buffer is full: a flooding plugin is
                    // held back by its own pipe instead of using host memory.
                    if received.send(message).is_err() || failed {
                        break;
                    }
                }
            });
        match (writer, reader) {
            (Ok(_), Ok(_)) => Ok(session),
            (Err(error), _) | (_, Err(error)) => Err(spawn_error(error)),
        }
    }

    fn send(
        &self,
        message: &HostMessage<'_>,
        settings: &Settings,
        deadline: Instant,
    ) -> Result<(), PluginError> {
        let mut line = serde_json::to_vec(message).expect("host messages serialise");
        line.push(b'\n');
        match self.outgoing.try_send(line) {
            Ok(()) => {}
            // A previous write is still stuck in the pipe.
            Err(TrySendError::Full(_)) => return Err(PluginError::Timeout(settings.timeout)),
            Err(TrySendError::Disconnected(_)) => return Err(PluginError::Exited),
        }
        match self.written.recv_timeout(remaining(deadline)) {
            Ok(Ok(())) => Ok(()),
            // A closed pipe means the process is gone.
            Ok(Err(_)) | Err(RecvTimeoutError::Disconnected) => Err(PluginError::Exited),
            Err(RecvTimeoutError::Timeout) => Err(PluginError::Timeout(settings.timeout)),
        }
    }

    fn receive(
        &self,
        settings: &Settings,
        deadline: Instant,
    ) -> Result<PluginMessage, PluginError> {
        match self.incoming.recv_timeout(remaining(deadline)) {
            Ok(Ok(line)) => parse_message(&line),
            Ok(Err(error)) => Err(PluginError::Protocol(error.to_string())),
            Err(RecvTimeoutError::Timeout) => Err(PluginError::Timeout(settings.timeout)),
            Err(RecvTimeoutError::Disconnected) => Err(PluginError::Exited),
        }
    }

    /// Fails if the plugin sent anything while no request was in flight.
    fn expect_silence(&self) -> Result<(), PluginError> {
        match self.incoming.try_recv() {
            Err(TryRecvError::Empty) => Ok(()),
            Err(TryRecvError::Disconnected) => Err(PluginError::Exited),
            Ok(_) => Err(PluginError::Protocol(
                "the plugin sent output that was not a reply to a request".to_owned(),
            )),
        }
    }

    fn handshake(&self, settings: &Settings, deadline: Instant) -> Result<(), PluginError> {
        self.send(
            &HostMessage::Hello {
                protocol: PROTOCOL_VERSION,
            },
            settings,
            deadline,
        )?;
        match self.receive(settings, deadline)? {
            PluginMessage::Ready { protocol, plugin }
                if protocol == PROTOCOL_VERSION && plugin == settings.plugin.as_str() =>
            {
                Ok(())
            }
            PluginMessage::Ready { protocol, plugin } => Err(PluginError::Protocol(format!(
                "expected plugin `{}` with protocol {PROTOCOL_VERSION}, got `{plugin}` with protocol {protocol}",
                settings.plugin
            ))),
            other => Err(PluginError::Protocol(format!(
                "expected `ready`, got {other:?}"
            ))),
        }
    }

    fn invoke(&self, job: &Job, settings: &Settings) -> Result<Vec<Effect>, PluginError> {
        self.expect_silence()?;
        self.send(
            &HostMessage::Invoke {
                request: job.request,
                command: job.command.as_str(),
                context: &job.context,
            },
            settings,
            job.deadline,
        )?;
        match self.receive(settings, job.deadline)? {
            PluginMessage::Result { request, effects } if request == job.request => {
                validate_effects(&effects)?;
                Ok(effects)
            }
            PluginMessage::Error { request, message } if request == job.request => {
                Err(PluginError::Reported(truncate(&message, MAX_LINE_BYTES)))
            }
            other => Err(PluginError::Protocol(format!(
                "expected a reply to request {}, got {}",
                job.request,
                truncate(&format!("{other:?}"), 200)
            ))),
        }
    }
}

fn remaining(deadline: Instant) -> Duration {
    deadline.saturating_duration_since(Instant::now())
}

fn parse_message(line: &str) -> Result<PluginMessage, PluginError> {
    serde_json::from_str(line).map_err(|error| {
        PluginError::Protocol(format!(
            "unreadable message `{}`: {error}",
            truncate(line.trim_end(), 120)
        ))
    })
}

fn write_message(stdin: &mut ChildStdin, bytes: &[u8]) -> io::Result<()> {
    stdin.write_all(bytes)?;
    stdin.flush()
}

fn run_worker(settings: Settings, jobs: Receiver<Job>, slot: ChildSlot) {
    let mut session: Option<Session> = None;
    // Ends when the host is dropped and the queue is empty.
    for job in jobs {
        let result = run_job(&settings, &slot, &mut session, &job);
        if result.as_ref().is_err_and(PluginError::is_fatal) {
            // Dropping the session kills and reaps the process.
            session = None;
        }
        (job.done)(result);
    }
}

fn run_job(
    settings: &Settings,
    slot: &ChildSlot,
    session: &mut Option<Session>,
    job: &Job,
) -> Result<Vec<Effect>, PluginError> {
    if slot.closed.load(Ordering::SeqCst) {
        return Err(PluginError::ShutDown);
    }
    if remaining(job.deadline).is_zero() {
        // Spent its whole budget waiting; the plugin never sees it.
        return Err(PluginError::Timeout(settings.timeout));
    }
    if session.is_none() {
        // Stored before the handshake so that a failed handshake still stops
        // the process when the caller drops the session.
        session
            .insert(Session::start(settings, slot)?)
            .handshake(settings, job.deadline)?;
    }
    let result = session
        .as_ref()
        .expect("a session was started above")
        .invoke(job, settings);
    // The host may have killed the process to shut down; say so instead of
    // reporting a crash.
    if result.is_err() && slot.closed.load(Ordering::SeqCst) {
        return Err(PluginError::ShutDown);
    }
    result
}

/// Owns one plugin process and the bounded queue of requests to it.
pub struct ProcessHost {
    plugin: ModuleId,
    program: PathBuf,
    timeout: Duration,
    slot: ChildSlot,
    /// Queue to the worker thread, started on first use.
    jobs: RefCell<Option<SyncSender<Job>>>,
    next_request: AtomicU64,
}

impl ProcessHost {
    /// A host for the plugin `plugin` implemented by `program`. Nothing starts
    /// until the first request; `timeout` limits each request from submission
    /// to reply.
    pub fn new(plugin: ModuleId, program: PathBuf, timeout: Duration) -> Self {
        Self {
            plugin,
            program,
            timeout,
            slot: ChildSlot::default(),
            jobs: RefCell::new(None),
            next_request: AtomicU64::new(1),
        }
    }

    fn queue(&self) -> Result<SyncSender<Job>, PluginError> {
        if let Some(jobs) = self.jobs.borrow().as_ref() {
            return Ok(jobs.clone());
        }
        let (sender, receiver) = mpsc::sync_channel(MAX_QUEUED_REQUESTS);
        let settings = Settings {
            plugin: self.plugin.clone(),
            program: self.program.clone(),
            timeout: self.timeout,
        };
        let slot = self.slot.clone();
        thread::Builder::new()
            .name(format!("plugin:{}", self.plugin))
            .spawn(move || run_worker(settings, receiver, slot))
            .map_err(|error| PluginError::Spawn {
                program: self.program.display().to_string(),
                message: format!("could not start a worker thread: {error}"),
            })?;
        *self.jobs.borrow_mut() = Some(sender.clone());
        Ok(sender)
    }

    /// Queues `command` for the plugin and returns immediately.
    ///
    /// `done` is called exactly once with the outcome, on the worker thread,
    /// or on the calling thread when the request is refused at once. The
    /// deadline starts now.
    pub fn submit(
        &self,
        command: CommandId,
        context: Value,
        done: impl FnOnce(Result<Vec<Effect>, PluginError>) + Send + 'static,
    ) {
        let job = Job {
            request: self.next_request.fetch_add(1, Ordering::Relaxed),
            command,
            context,
            deadline: Instant::now() + self.timeout,
            done: Box::new(done),
        };
        let refused = match self.queue() {
            Ok(queue) => match queue.try_send(job) {
                Ok(()) => return,
                Err(TrySendError::Full(job)) => (job, PluginError::Busy),
                Err(TrySendError::Disconnected(job)) => (job, PluginError::Exited),
            },
            Err(error) => (job, error),
        };
        let (job, error) = refused;
        (job.done)(Err(error));
    }
}

impl Drop for ProcessHost {
    fn drop(&mut self) {
        // Order matters: mark closed so the worker starts no new process,
        // then kill whatever is running. A request in flight fails at once
        // because its pipes close; queued requests are answered `ShutDown`.
        self.slot.closed.store(true, Ordering::SeqCst);
        self.jobs.borrow_mut().take();
        self.slot.stop();
    }
}

/// Reads one line of at most [`MAX_MESSAGE_BYTES`]. `Ok(None)` is end of input.
fn read_message(reader: &mut impl BufRead) -> io::Result<Option<String>> {
    let mut line = Vec::new();
    let limit = MAX_MESSAGE_BYTES as u64 + 1;
    if reader.take(limit).read_until(b'\n', &mut line)? == 0 {
        return Ok(None);
    }
    if line.last() != Some(&b'\n') && line.len() as u64 >= limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("message is longer than {MAX_MESSAGE_BYTES} bytes"),
        ));
    }
    String::from_utf8(line)
        .map(Some)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "message is not UTF-8"))
}

fn validate_effects(effects: &[Effect]) -> Result<(), PluginError> {
    let mut total_lines = 0;
    for effect in effects {
        match effect {
            Effect::Output { lines } => {
                total_lines += lines.len();
                if total_lines > MAX_OUTPUT_LINES {
                    return Err(PluginError::Protocol(format!(
                        "more than {MAX_OUTPUT_LINES} output lines in one result"
                    )));
                }
                if lines.iter().any(|line| line.len() > MAX_LINE_BYTES) {
                    return Err(PluginError::Protocol(format!(
                        "an output line is longer than {MAX_LINE_BYTES} bytes"
                    )));
                }
            }
            Effect::Status { text } if text.len() > MAX_LINE_BYTES => {
                return Err(PluginError::Protocol(format!(
                    "status text is longer than {MAX_LINE_BYTES} bytes"
                )));
            }
            Effect::Status { .. } => {}
        }
    }
    Ok(())
}

fn truncate(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// Finds the executable a process package names.
pub(crate) fn resolve_program(package_dir: &Path, command: &str) -> Result<PathBuf, String> {
    let direct = Path::new(command);
    if direct.is_absolute() {
        return direct
            .is_file()
            .then(|| direct.to_owned())
            .ok_or_else(|| format!("executable `{command}` does not exist"));
    }
    let in_package = package_dir.join(command);
    if in_package.is_file() {
        return Ok(in_package);
    }
    let beside_host = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|parent| parent.join(command)));
    match beside_host {
        Some(path) if path.is_file() => Ok(path),
        _ => Err(format!(
            "executable `{command}` was not found in {} or next to the application",
            package_dir.display()
        )),
    }
}

/// Registers `command` so that invoking it asks the plugin behind `host`.
pub(crate) fn register_command(
    cx: &mut ActivationContext<'_>,
    host: &Rc<ProcessHost>,
    command: &CommandId,
    title: &str,
) -> Result<(), ActivationError> {
    let host = Rc::clone(host);
    let inbox = cx.inbox();
    let plugin = cx.module().clone();
    let id = command.clone();
    let title = title.to_owned();
    cx.command(command.clone(), &title.clone(), move |cx| {
        let document = cx.shell.active_document().map(|document| {
            json!({
                "kind": document.kind.as_str(),
                "resource": document.resource,
                "title": document.title,
                "data": document.provider.export(MAX_EXPORTED_ROWS),
            })
        });
        cx.shell.set_status(format!("Running {title}…"));
        cx.shell
            .trace_mut()
            .record("plugin.invoked", format!("{plugin} {id}"));
        let (plugin, title, inbox) = (plugin.clone(), title.clone(), inbox.clone());
        host.submit(id.clone(), json!({ "document": document }), move |result| {
            // Back to the UI thread: plugin results only ever touch the shell
            // through the inbox.
            let _ =
                inbox.post_blocking(Box::new(move |cx: &mut CommandContext<'_>| match result {
                    Ok(effects) => {
                        for effect in effects {
                            match effect {
                                Effect::Output { lines } => {
                                    for line in lines {
                                        cx.shell.output().append(line, Tone::Normal);
                                    }
                                    cx.shell.show_view(ViewId::literal(VIEW_OUTPUT));
                                }
                                Effect::Status { text } => cx.shell.set_status(text),
                            }
                        }
                        cx.shell
                            .trace_mut()
                            .record("plugin.completed", plugin.as_str());
                    }
                    Err(error) => {
                        let message = format!("{title} failed (plugin `{plugin}`): {error}");
                        cx.shell.trace_mut().record("plugin.failed", &message);
                        cx.shell.output().append(&message, Tone::Error);
                        cx.shell.show_view(ViewId::literal(VIEW_OUTPUT));
                        cx.shell.set_status(message);
                    }
                }));
        });
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_are_read_line_by_line_and_bounded() {
        let mut input = io::Cursor::new(b"{\"a\":1}\nlast".to_vec());
        assert_eq!(
            read_message(&mut input).unwrap().as_deref(),
            Some("{\"a\":1}\n")
        );
        assert_eq!(read_message(&mut input).unwrap().as_deref(), Some("last"));
        assert_eq!(read_message(&mut input).unwrap(), None);

        let mut exact = vec![b'x'; MAX_MESSAGE_BYTES - 1];
        exact.push(b'\n');
        assert!(read_message(&mut io::Cursor::new(exact)).unwrap().is_some());

        let oversized = vec![b'x'; MAX_MESSAGE_BYTES + 10];
        let error = read_message(&mut io::Cursor::new(oversized)).unwrap_err();
        assert!(error.to_string().contains("longer than"));

        let error = read_message(&mut io::Cursor::new(vec![0xff, b'\n'])).unwrap_err();
        assert!(error.to_string().contains("UTF-8"));
    }

    #[test]
    fn effects_are_limited_in_count_and_size() {
        let ok = [
            Effect::Output {
                lines: vec!["x".repeat(MAX_LINE_BYTES); MAX_OUTPUT_LINES],
            },
            Effect::Status {
                text: "done".into(),
            },
        ];
        assert_eq!(validate_effects(&ok), Ok(()));

        let too_many = [
            Effect::Output {
                lines: vec![String::new(); MAX_OUTPUT_LINES],
            },
            Effect::Output {
                lines: vec![String::new()],
            },
        ];
        assert!(matches!(
            validate_effects(&too_many),
            Err(PluginError::Protocol(message)) if message.contains("output lines")
        ));
        let too_long = [Effect::Output {
            lines: vec!["x".repeat(MAX_LINE_BYTES + 1)],
        }];
        assert!(validate_effects(&too_long).is_err());
        let long_status = [Effect::Status {
            text: "x".repeat(MAX_LINE_BYTES + 1),
        }];
        assert!(validate_effects(&long_status).is_err());
    }

    #[test]
    fn unknown_effects_and_fields_are_rejected() {
        assert!(parse_message(r#"{"type":"result","request":1,"effects":[]}"#).is_ok());
        for rejected in [
            r#"{"type":"result","request":1,"effects":[{"type":"open-url","url":"x"}]}"#,
            r#"{"type":"result","request":1,"effects":[],"extra":true}"#,
            r#"{"type":"shutdown-host"}"#,
            "not json",
        ] {
            assert!(
                matches!(parse_message(rejected), Err(PluginError::Protocol(_))),
                "{rejected}"
            );
        }
    }

    #[test]
    fn truncation_respects_character_boundaries() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("héllo", 2), "h…");
        assert_eq!(truncate("héllo", 3), "hé…");
    }
}
