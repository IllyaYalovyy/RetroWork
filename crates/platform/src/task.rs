//! Running external tools such as compilers and test runners.
//!
//! [`spawn`] starts a process and streams its output; [`capture`] runs one to
//! completion and returns what it printed. Both are for tools the user asked
//! to run, not for plugins (see [`crate::external`]).
//!
//! # What the runner guarantees
//!
//! - **Bounded memory.** Each output line is cut at [`MAX_LINE_BYTES`].
//!   Lines travel in batches of at most [`BATCH_LINES`], and at most
//!   [`MAX_PENDING_BATCHES`] batches wait to be consumed. When the consumer
//!   falls behind, the runner stops reading and the tool blocks on its own
//!   output pipe, so a tool that floods its output cannot exhaust memory.
//! - **Prompt delivery.** A partial batch is delivered after
//!   [`BATCH_INTERVAL`], so slow output still appears as it is written.
//! - **A task owns its process group.** The tool runs as the leader of a new
//!   process group, and everything it starts stays in that group unless it
//!   leaves it. When the task ends, by itself or by cancellation, every
//!   process still in the group is killed before the task is reported
//!   finished.
//! - **Cancellation reaches the whole group.** Cancelling sends `SIGTERM` to
//!   the group and `SIGKILL` after [`CANCEL_GRACE`] if the task has not
//!   finished by then, whether or not the leader has already exited.
//!   Dropping a [`TaskHandle`] of an unfinished task kills the group at once.
//! - **The task always finishes.** Exactly one [`TaskEvent::Finished`] is
//!   delivered, after all output. If the leader exits but a process in the
//!   group keeps the output pipes open, that process is killed
//!   [`EXIT_GRACE`] after the exit.
//! - **No signal reaches another program.** The leader is not reaped until
//!   its group has been killed, so its process id, which is the group id,
//!   cannot be given to another process while signals may still be sent.
//! - **Errors clean up.** If starting a task fails part way, for example
//!   because a thread cannot be created, the group is killed and the leader
//!   reaped before the error is returned.
//!
//! Not provided: a process that leaves the group (`setsid`, `setpgid`) is
//! neither stopped nor waited for; if it keeps a pipe open, the thread
//! reading that pipe lives until it exits. Unix only.

use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read},
    os::unix::process::CommandExt as _,
    path::PathBuf,
    process::{Child, ChildStderr, ChildStdout, Command, Stdio},
    sync::{
        Arc, Condvar, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, RecvTimeoutError, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};

use rustix::process::{Pid, Signal, WaitId, WaitIdOptions, kill_process_group, waitid};

/// Longest output line kept, in bytes; the rest of the line is dropped.
pub const MAX_LINE_BYTES: usize = 8 * 1024;
/// Most lines in one [`Batch`].
pub const BATCH_LINES: usize = 512;
/// Most batches delivered but not yet consumed.
pub const MAX_PENDING_BATCHES: usize = 4;
/// Longest a line waits before a partial batch is delivered.
pub const BATCH_INTERVAL: Duration = Duration::from_millis(30);
/// Time between `SIGTERM` and `SIGKILL` when a task is cancelled.
pub const CANCEL_GRACE: Duration = Duration::from_secs(2);
/// How long output is still collected after the tool itself has exited.
pub const EXIT_GRACE: Duration = Duration::from_secs(2);

/// A tool invocation: what to run, where, and with which environment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessSpec {
    pub program: String,
    pub args: Vec<String>,
    /// Working directory.
    pub cwd: PathBuf,
    /// Variables set on top of the inherited environment.
    pub env: BTreeMap<String, String>,
    /// Variables removed from the inherited environment.
    pub env_remove: Vec<String>,
}

impl ProcessSpec {
    /// Runs `program` in `cwd` with the inherited environment and no arguments.
    pub fn new(program: impl Into<String>, cwd: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: cwd.into(),
            env: BTreeMap::new(),
            env_remove: Vec::new(),
        }
    }

    /// Appends arguments.
    pub fn args<S: Into<String>>(mut self, args: impl IntoIterator<Item = S>) -> Self {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// The invocation as a user would type it, for display.
    pub fn command_line(&self) -> String {
        std::iter::once(self.program.as_str())
            .chain(self.args.iter().map(String::as_str))
            .map(|part| {
                if part.is_empty() || part.contains(char::is_whitespace) {
                    format!("'{part}'")
                } else {
                    part.to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command.args(&self.args).current_dir(&self.cwd);
        for name in &self.env_remove {
            command.env_remove(name);
        }
        command.envs(&self.env);
        command
    }
}

/// Which pipe a line came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

/// One line of tool output, without its line ending.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputLine {
    pub stream: Stream,
    pub text: String,
    /// Whether the line was longer than [`MAX_LINE_BYTES`] and was cut.
    pub truncated: bool,
}

/// How a task ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskOutcome {
    /// The tool exited by itself with this status code.
    Exited(i32),
    /// The tool was ended by this signal, not by a cancellation.
    Signalled(i32),
    /// [`TaskHandle::cancel`] was called before the tool exited.
    Cancelled,
}

impl TaskOutcome {
    /// Whether the tool exited by itself with status 0.
    pub fn succeeded(&self) -> bool {
        *self == TaskOutcome::Exited(0)
    }
}

/// Lines delivered together. Holding a batch counts against
/// [`MAX_PENDING_BATCHES`] until it is dropped, so a consumer that queues
/// batches for later processing must drop each one when it is done.
#[derive(Debug)]
pub struct Batch {
    pub lines: Vec<OutputLine>,
    _credit: Credit,
}

/// What a running task reports, in order: any number of `Output` events,
/// then exactly one `Finished`.
#[derive(Debug)]
pub enum TaskEvent {
    Output(Batch),
    Finished {
        outcome: TaskOutcome,
        /// From start to the tool's exit.
        duration: Duration,
    },
}

/// Counts batches that were delivered but not yet dropped.
#[derive(Debug, Default)]
struct Pending {
    count: Mutex<usize>,
    released: Condvar,
}

impl Pending {
    fn acquire(self: &Arc<Self>) -> Credit {
        let mut count = self.count.lock().unwrap_or_else(PoisonError::into_inner);
        while *count >= MAX_PENDING_BATCHES {
            count = self
                .released
                .wait(count)
                .unwrap_or_else(PoisonError::into_inner);
        }
        *count += 1;
        Credit(Arc::clone(self))
    }
}

#[derive(Debug)]
struct Credit(Arc<Pending>);

impl Drop for Credit {
    fn drop(&mut self) {
        *self.0.count.lock().unwrap_or_else(PoisonError::into_inner) -= 1;
        self.0.released.notify_all();
    }
}

/// How the leader of a group ended.
#[derive(Clone, Copy, Debug)]
struct Exit {
    code: Option<i32>,
    signal: Option<i32>,
    at: Instant,
}

/// A started tool: the leader of a new process group.
///
/// The leader is observed with `waitid(WNOWAIT)`, which leaves it a zombie,
/// and is reaped only by [`Group::finish`]. While it is unreaped its process
/// id, which is the group id, stays allocated, so a signal to the group can
/// only reach the tool and its descendants. Dropping a group finishes it.
#[derive(Debug)]
struct Group {
    pid: Pid,
    /// `None` once the leader has been reaped.
    leader: Mutex<Option<Child>>,
}

impl Group {
    /// Starts `spec` with piped output as the leader of a new group.
    fn start(spec: &ProcessSpec) -> Result<(Arc<Self>, ChildStdout, ChildStderr), String> {
        let mut command = spec.command();
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let mut child = command
            .spawn()
            .map_err(|error| format!("cannot start `{}`: {error}", spec.program))?;
        let Some(pid) = i32::try_from(child.id()).ok().and_then(Pid::from_raw) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("`{}` got an unusable process id", spec.program));
        };
        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");
        Ok((
            Arc::new(Self {
                pid,
                leader: Mutex::new(Some(child)),
            }),
            stdout,
            stderr,
        ))
    }

    fn leader(&self) -> std::sync::MutexGuard<'_, Option<Child>> {
        self.leader.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Sends `signal` to every process in the group, unless the group has
    /// been finished.
    fn signal(&self, signal: Signal) {
        let leader = self.leader();
        if leader.is_some() {
            // Fails only when no process is left in the group.
            let _ = kill_process_group(self.pid, signal);
        }
    }

    /// How the leader ended, without reaping it. With `block`, waits for it.
    /// `None` while it runs, or when it was already reaped.
    fn exit(&self, block: bool) -> Option<Exit> {
        let mut options = WaitIdOptions::EXITED | WaitIdOptions::NOWAIT;
        if !block {
            options |= WaitIdOptions::NOHANG;
        }
        loop {
            match waitid(WaitId::Pid(self.pid), options) {
                Ok(Some(status)) => {
                    return Some(Exit {
                        code: status.exit_status(),
                        signal: status.terminating_signal(),
                        at: Instant::now(),
                    });
                }
                Ok(None) => return None,
                Err(error) if error == rustix::io::Errno::INTR => {}
                Err(_) => return None,
            }
        }
    }

    /// Kills everything left in the group and reaps the leader. Idempotent.
    fn finish(&self) {
        let mut leader = self.leader();
        if let Some(mut child) = leader.take() {
            let _ = kill_process_group(self.pid, Signal::KILL);
            // Returns at once: the leader has exited or was just killed.
            let _ = child.wait();
        }
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        self.finish();
    }
}

#[derive(Debug)]
struct Shared {
    group: Arc<Group>,
    cancelled: AtomicBool,
    exit: Mutex<Option<Exit>>,
    /// Notified when the leader exits and when the task finishes.
    changed: Condvar,
    finished: AtomicBool,
}

impl Shared {
    fn exit(&self) -> Option<Exit> {
        *self.exit.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn mark_finished(&self) {
        let _exit = self.exit.lock().unwrap_or_else(PoisonError::into_inner);
        self.finished.store(true, Ordering::SeqCst);
        self.changed.notify_all();
    }

    /// Waits until the task has finished or `limit` has passed.
    fn wait_finished(&self, limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        let mut exit = self.exit.lock().unwrap_or_else(PoisonError::into_inner);
        while !self.finished.load(Ordering::SeqCst) {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            exit = self
                .changed
                .wait_timeout(exit, remaining)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        true
    }
}

/// Controls a task started by [`spawn`].
#[derive(Debug)]
pub struct TaskHandle {
    shared: Arc<Shared>,
}

impl TaskHandle {
    /// Stops the tool and everything in its process group: `SIGTERM` now,
    /// `SIGKILL` after [`CANCEL_GRACE`] unless the task has finished by
    /// then. The task finishes as [`TaskOutcome::Cancelled`] unless the
    /// leader had already exited by itself. Does nothing once the task has
    /// finished.
    pub fn cancel(&self) {
        if self.is_finished() || self.shared.cancelled.swap(true, Ordering::SeqCst) {
            return;
        }
        self.shared.group.signal(Signal::TERM);
        let shared = Arc::clone(&self.shared);
        let escalation = thread::Builder::new()
            .name("task-cancel".into())
            .spawn(move || {
                if !shared.wait_finished(CANCEL_GRACE) {
                    shared.group.signal(Signal::KILL);
                }
            });
        if escalation.is_err() {
            // Without a timer there is no grace period.
            self.shared.group.signal(Signal::KILL);
        }
    }

    /// Whether `Finished` has been delivered.
    pub fn is_finished(&self) -> bool {
        self.shared.finished.load(Ordering::SeqCst)
    }

    /// Process id of the tool, which is also its process group id.
    pub fn pid(&self) -> u32 {
        self.shared.group.pid.as_raw_nonzero().get().unsigned_abs()
    }
}

impl Drop for TaskHandle {
    fn drop(&mut self) {
        if !self.is_finished() {
            self.shared.cancelled.store(true, Ordering::SeqCst);
            self.shared.group.signal(Signal::KILL);
        }
    }
}

fn outcome(exit: Exit, cancelled: bool) -> TaskOutcome {
    match (exit.code, exit.signal) {
        _ if cancelled => TaskOutcome::Cancelled,
        (Some(code), _) => TaskOutcome::Exited(code),
        (None, signal) => TaskOutcome::Signalled(signal.unwrap_or_default()),
    }
}

/// Starts `spec` and delivers its output and outcome to `sink` on a runner
/// thread. Fails, without calling `sink`, if the process or one of the
/// runner's threads cannot be started; the process is then stopped.
pub fn spawn(
    spec: &ProcessSpec,
    mut sink: impl FnMut(TaskEvent) + Send + 'static,
) -> Result<TaskHandle, String> {
    let started = Instant::now();
    // From here on, any early return drops the group, which kills and reaps.
    let (group, stdout, stderr) = Group::start(spec)?;
    let shared = Arc::new(Shared {
        group: Arc::clone(&group),
        cancelled: AtomicBool::new(false),
        exit: Mutex::new(None),
        changed: Condvar::new(),
        finished: AtomicBool::new(false),
    });
    let fail = |what: &str, error: std::io::Error| {
        group.finish();
        format!(
            "cannot start a {what} thread for `{}`: {error}",
            spec.program
        )
    };

    let (lines, incoming) = mpsc::sync_channel::<OutputLine>(BATCH_LINES * 2);
    start_reader(stdout, Stream::Stdout, lines.clone()).map_err(|error| fail("reader", error))?;
    start_reader(stderr, Stream::Stderr, lines).map_err(|error| fail("reader", error))?;

    let waiter_shared = Arc::clone(&shared);
    thread::Builder::new()
        .name("task-wait".into())
        .spawn(move || {
            // `None` only if the leader was reaped because starting failed.
            if let Some(exit) = waiter_shared.group.exit(true) {
                *waiter_shared
                    .exit
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) = Some(exit);
                waiter_shared.changed.notify_all();
            }
        })
        .map_err(|error| fail("waiter", error))?;

    let runner_shared = Arc::clone(&shared);
    thread::Builder::new()
        .name("task-output".into())
        .spawn(move || {
            let pending = Arc::new(Pending::default());
            let mut batch = Vec::new();
            let mut oldest: Option<Instant> = None;
            loop {
                let past_grace = runner_shared
                    .exit()
                    .is_some_and(|exit| exit.at.elapsed() >= EXIT_GRACE);
                if past_grace {
                    break;
                }
                match incoming.recv_timeout(BATCH_INTERVAL) {
                    Ok(line) => {
                        oldest.get_or_insert_with(Instant::now);
                        batch.push(line);
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                }
                let due = oldest.is_some_and(|at| at.elapsed() >= BATCH_INTERVAL);
                if batch.len() >= BATCH_LINES || (due && !batch.is_empty()) {
                    let credit = pending.acquire();
                    sink(TaskEvent::Output(Batch {
                        lines: std::mem::take(&mut batch),
                        _credit: credit,
                    }));
                    oldest = None;
                }
            }
            if !batch.is_empty() {
                let credit = pending.acquire();
                sink(TaskEvent::Output(Batch {
                    lines: batch,
                    _credit: credit,
                }));
            }
            // The pipes are closed or past their grace; the leader may still
            // run if it closed its output early.
            let exit = {
                let mut exit = runner_shared
                    .exit
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                while exit.is_none() {
                    exit = runner_shared
                        .changed
                        .wait(exit)
                        .unwrap_or_else(PoisonError::into_inner);
                }
                exit.expect("checked above")
            };
            runner_shared.group.finish();
            let outcome = outcome(exit, runner_shared.cancelled.load(Ordering::SeqCst));
            runner_shared.mark_finished();
            sink(TaskEvent::Finished {
                outcome,
                duration: exit.at.saturating_duration_since(started),
            });
        })
        .map_err(|error| fail("runner", error))?;
    Ok(TaskHandle { shared })
}

fn start_reader(
    pipe: impl Read + Send + 'static,
    stream: Stream,
    lines: SyncSender<OutputLine>,
) -> std::io::Result<()> {
    thread::Builder::new()
        .name("task-reader".into())
        .spawn(move || {
            let mut reader = BufReader::new(pipe);
            while let Ok(Some(line)) = read_line(&mut reader, stream) {
                // Blocks while the runner is behind: backpressure.
                if lines.send(line).is_err() {
                    break;
                }
            }
        })
        .map(drop)
}

/// Reads one line, keeping at most [`MAX_LINE_BYTES`] of it.
fn read_line(reader: &mut impl BufRead, stream: Stream) -> std::io::Result<Option<OutputLine>> {
    let mut kept = Vec::new();
    let mut truncated = false;
    let mut any = false;
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            break;
        }
        any = true;
        let (chunk, ends) = match buffer.iter().position(|&byte| byte == b'\n') {
            Some(at) => (&buffer[..at], Some(at + 1)),
            None => (buffer, None),
        };
        let room = MAX_LINE_BYTES.saturating_sub(kept.len());
        if chunk.len() > room {
            truncated = true;
        }
        kept.extend_from_slice(&chunk[..chunk.len().min(room)]);
        let consumed = ends.unwrap_or(buffer.len());
        reader.consume(consumed);
        if ends.is_some() {
            break;
        }
    }
    if !any {
        return Ok(None);
    }
    if kept.last() == Some(&b'\r') {
        kept.pop();
    }
    Ok(Some(OutputLine {
        stream,
        text: String::from_utf8_lossy(&kept).into_owned(),
        truncated,
    }))
}

/// What a finished [`capture`] produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Captured {
    pub stdout: String,
    pub stderr: String,
    pub outcome: TaskOutcome,
}

/// Shared cancellation for bounded blocking operations running on workers.
#[derive(Clone, Default)]
pub struct Cancellation(std::sync::Arc<std::sync::atomic::AtomicBool>);
impl Cancellation {
    /// Requests cancellation; the worker cleans up its owned process group.
    pub fn cancel(&self) {
        self.0.store(true, std::sync::atomic::Ordering::Release);
    }
    /// Whether cancellation was requested.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Acquire)
    }
}

/// Runs `spec` to completion and returns everything it printed, without a
/// line length limit. Blocks the calling thread, so call it from a worker.
///
/// One deadline, `timeout`, covers the whole run: the leader's exit and
/// reading both pipes to their end. If the leader exits but a process in its
/// group keeps a pipe open, that process is killed [`EXIT_GRACE`] after the
/// exit and the output read so far is used. Fails if the tool cannot start,
/// if either stream exceeds `max_bytes`, or if the deadline passes; in every
/// case the group is killed and the leader reaped before returning.
pub fn capture(
    spec: &ProcessSpec,
    max_bytes: usize,
    timeout: Duration,
) -> Result<Captured, String> {
    capture_with_cancel(spec, max_bytes, timeout, &Cancellation::default())
}

/// Captures output with the same bounds as `capture`, stopping on cancellation.
/// Cancellation kills the owned process group and reaps its leader.
pub fn capture_with_cancel(
    spec: &ProcessSpec,
    max_bytes: usize,
    timeout: Duration,
    cancellation: &Cancellation,
) -> Result<Captured, String> {
    if cancellation.is_cancelled() {
        return Err("capture cancelled".into());
    }
    let deadline = Instant::now() + timeout;
    let (group, stdout, stderr) = Group::start(spec)?;
    let (sender, results) = mpsc::channel::<(Stream, std::io::Result<Vec<u8>>)>();
    let limit = u64::try_from(max_bytes)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let pipes: [(Stream, Box<dyn Read + Send>); 2] = [
        (Stream::Stdout, Box::new(stdout)),
        (Stream::Stderr, Box::new(stderr)),
    ];
    for (stream, pipe) in pipes {
        let sender = sender.clone();
        thread::Builder::new()
            .name("capture-reader".into())
            .spawn(move || {
                let mut bytes = Vec::new();
                let result = pipe.take(limit).read_to_end(&mut bytes).map(|_| bytes);
                let _ = sender.send((stream, result));
            })
            .map_err(|error| format!("cannot start a reader thread: {error}"))?;
    }
    drop(sender);

    let timed_out = || {
        format!(
            "`{}` did not finish within {timeout:?} and was stopped",
            spec.command_line()
        )
    };
    let mut collected: [Option<Vec<u8>>; 2] = [None, None];
    let mut exit: Option<Exit> = None;
    let mut killed_leftovers = false;
    while collected.iter().any(Option::is_none) || exit.is_none() {
        if cancellation.is_cancelled() {
            return Err("capture cancelled".into());
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(timed_out());
        }
        if exit.is_none() {
            exit = group.exit(false);
        }
        if let Some(exit) = exit
            && !killed_leftovers
            && exit.at.elapsed() >= EXIT_GRACE
        {
            // The leader is gone; whatever still holds a pipe is stopped so
            // that reading ends.
            group.signal(Signal::KILL);
            killed_leftovers = true;
        }
        if collected.iter().all(Option::is_some) {
            // Both pipes closed; only the leader's exit is missing.
            thread::sleep(Duration::from_millis(5).min(deadline - now));
            continue;
        }
        match results.recv_timeout(Duration::from_millis(20).min(deadline - now)) {
            Ok((stream, result)) => {
                let name = match stream {
                    Stream::Stdout => "stdout",
                    Stream::Stderr => "stderr",
                };
                let bytes = result.map_err(|error| format!("reading {name} failed: {error}"))?;
                if bytes.len() > max_bytes {
                    return Err(format!(
                        "`{}` printed more than {max_bytes} bytes on {name}",
                        spec.command_line()
                    ));
                }
                collected[usize::from(stream == Stream::Stderr)] = Some(bytes);
            }
            Err(RecvTimeoutError::Timeout) => {}
            // A reader ended without reporting; nothing more will arrive.
            Err(RecvTimeoutError::Disconnected) => return Err(timed_out()),
        }
    }
    group.finish();
    let [stdout, stderr] =
        collected.map(|bytes| String::from_utf8_lossy(&bytes.unwrap_or_default()).into_owned());
    Ok(Captured {
        stdout,
        stderr,
        outcome: outcome(exit.expect("the loop waits for the exit"), false),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whether `pid` is a process that has not exited, judged by `/proc`.
    fn running(pid: u32) -> bool {
        std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
            stat.rsplit_once(") ")
                .is_some_and(|(_, rest)| !rest.starts_with('Z') && !rest.starts_with('X'))
        })
    }

    fn wait_gone(pid: u32) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while running(pid) {
            assert!(Instant::now() < deadline, "process {pid} is still running");
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// A group whose leader started a child; returns both pids.
    fn group_with_child(directory: &std::path::Path) -> (Arc<Group>, u32, u32) {
        let pid_file = directory.join("child.pid");
        let spec = ProcessSpec::new("/bin/sh", directory).args([
            "-c",
            &format!("sleep 60 & echo $! > {}; sleep 60", pid_file.display()),
        ]);
        let (group, _stdout, _stderr) = Group::start(&spec).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let child = loop {
            if let Some(pid) = std::fs::read_to_string(&pid_file)
                .ok()
                .and_then(|text| text.trim().parse().ok())
            {
                break pid;
            }
            assert!(Instant::now() < deadline, "the child never started");
            thread::sleep(Duration::from_millis(10));
        };
        let leader = group.pid.as_raw_nonzero().get().unsigned_abs();
        (group, leader, child)
    }

    /// Every failed start ends by dropping the group; that must leave nothing
    /// running and nothing unreaped.
    #[test]
    fn dropping_a_group_kills_the_whole_group_and_reaps_the_leader() {
        let directory = tempfile::tempdir().unwrap();
        let (group, leader, child) = group_with_child(directory.path());
        assert!(running(leader) && running(child));
        drop(group);
        // Reaped: the leader's entry is gone, not left as a zombie.
        assert!(!std::path::Path::new(&format!("/proc/{leader}")).exists());
        wait_gone(child);
    }

    #[test]
    fn finishing_is_idempotent_and_releases_a_waiting_waiter() {
        let directory = tempfile::tempdir().unwrap();
        let (group, leader, child) = group_with_child(directory.path());
        let waiting = Arc::clone(&group);
        let waiter = thread::spawn(move || waiting.exit(true));
        thread::sleep(Duration::from_millis(50));
        group.finish();
        group.finish();
        // The killed leader is reported; a waiter is never left blocked.
        let exit = waiter.join().unwrap();
        assert!(exit.is_none_or(|exit| exit.signal == Some(9)), "{exit:?}");
        assert!(group.exit(false).is_none(), "the leader was reaped");
        wait_gone(child);
        assert!(!running(leader));
        // Signals after finishing go nowhere, whoever has the pid now.
        group.signal(Signal::KILL);
    }

    #[test]
    fn the_exit_of_a_leader_is_seen_without_reaping_it() {
        let directory = tempfile::tempdir().unwrap();
        let spec = ProcessSpec::new("/bin/sh", directory.path()).args(["-c", "exit 7"]);
        let (group, _stdout, _stderr) = Group::start(&spec).unwrap();
        let exit = group.exit(true).unwrap();
        assert_eq!((exit.code, exit.signal), (Some(7), None));
        // Still waitable: the pid stays reserved until the group finishes.
        assert_eq!(group.exit(false).map(|exit| exit.code), Some(Some(7)));
        let pid = group.pid.as_raw_nonzero().get();
        assert!(std::path::Path::new(&format!("/proc/{pid}")).exists());
        group.finish();
        assert!(group.exit(false).is_none());
    }

    fn line(text: &str) -> Option<OutputLine> {
        Some(OutputLine {
            stream: Stream::Stdout,
            text: text.into(),
            truncated: false,
        })
    }

    #[test]
    fn lines_are_split_and_cut_at_the_limit() {
        let mut long = vec![b'x'; MAX_LINE_BYTES + 100];
        long.extend_from_slice(b"\nnext\r\nlast");
        let mut reader = BufReader::with_capacity(64, &long[..]);
        let first = read_line(&mut reader, Stream::Stdout).unwrap().unwrap();
        assert_eq!(first.text.len(), MAX_LINE_BYTES);
        assert!(first.truncated);
        assert_eq!(
            read_line(&mut reader, Stream::Stdout).unwrap(),
            line("next")
        );
        assert_eq!(
            read_line(&mut reader, Stream::Stdout).unwrap(),
            line("last")
        );
        assert_eq!(read_line(&mut reader, Stream::Stdout).unwrap(), None);
        let mut invalid = BufReader::new(&[0xff, b'a', b'\n'][..]);
        assert_eq!(
            read_line(&mut invalid, Stream::Stdout).unwrap(),
            line("\u{fffd}a")
        );
    }

    #[test]
    fn command_line_quotes_only_what_needs_it() {
        let spec = ProcessSpec::new("cargo", "/tmp").args(["run", "--", "two words", ""]);
        assert_eq!(spec.command_line(), "cargo run -- 'two words' ''");
    }

    #[test]
    fn outcome_success_means_exit_code_zero() {
        assert!(TaskOutcome::Exited(0).succeeded());
        assert!(!TaskOutcome::Exited(1).succeeded());
        assert!(!TaskOutcome::Signalled(9).succeeded());
        assert!(!TaskOutcome::Cancelled.succeeded());
    }
}
