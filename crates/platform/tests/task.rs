//! The task runner against real processes: streaming, bounds, backpressure,
//! cancellation of a whole process group, and capture.
#![cfg(unix)]

use std::{
    fs,
    path::Path,
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};

use retrowork_platform::task::{
    BATCH_LINES, Batch, CANCEL_GRACE, EXIT_GRACE, MAX_LINE_BYTES, MAX_PENDING_BATCHES, OutputLine,
    ProcessSpec, Stream, TaskEvent, TaskHandle, TaskOutcome, capture, spawn,
};

fn shell(script: &str, cwd: &Path) -> ProcessSpec {
    ProcessSpec::new("/bin/sh", cwd).args(["-c", script])
}

fn run(spec: &ProcessSpec) -> (TaskHandle, Receiver<TaskEvent>) {
    let (sender, receiver) = mpsc::channel();
    let handle = spawn(spec, move |event| {
        let _ = sender.send(event);
    })
    .unwrap();
    (handle, receiver)
}

/// All lines and the outcome, consuming batches as they arrive.
fn collect(receiver: &Receiver<TaskEvent>) -> (Vec<OutputLine>, TaskOutcome, Duration) {
    let mut lines = Vec::new();
    loop {
        match receiver
            .recv_timeout(Duration::from_secs(20))
            .expect("the task finishes")
        {
            TaskEvent::Output(batch) => {
                assert!(batch.lines.len() <= BATCH_LINES);
                lines.extend(batch.lines);
            }
            TaskEvent::Finished { outcome, duration } => return (lines, outcome, duration),
        }
    }
}

fn texts(lines: &[OutputLine], stream: Stream) -> Vec<String> {
    lines
        .iter()
        .filter(|line| line.stream == stream)
        .map(|line| line.text.clone())
        .collect()
}

/// A process identified by its pid and its kernel start time, so that a
/// later check cannot mistake another process that reuses the pid for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Process {
    pid: u32,
    start: u64,
}

/// State letter and start time from `/proc/<pid>/stat`.
fn stat(pid: u32) -> Option<(char, u64)> {
    let text = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name may contain spaces and parentheses; fields after it
    // are separated by single spaces, starting with the state (field 3).
    let (_, rest) = text.rsplit_once(") ")?;
    let fields: Vec<&str> = rest.split(' ').collect();
    Some((
        fields.first()?.chars().next()?,
        fields.get(19)?.parse().ok()?,
    ))
}

impl Process {
    fn of(pid: u32) -> Self {
        let (_, start) = stat(pid).unwrap_or_else(|| panic!("process {pid} does not exist"));
        Self { pid, start }
    }

    /// Reads the pid a script wrote to `file` once it has been written.
    fn from_file(file: &Path) -> Self {
        wait_until("the pid file is written", Duration::from_secs(10), || {
            fs::read_to_string(file).is_ok_and(|text| text.ends_with('\n'))
        });
        Self::of(fs::read_to_string(file).unwrap().trim().parse().unwrap())
    }

    fn state(&self) -> Option<char> {
        stat(self.pid)
            .filter(|(_, start)| *start == self.start)
            .map(|(state, _)| state)
    }

    /// Running, as opposed to gone or exited and waiting to be reaped.
    fn running(&self) -> bool {
        self.state()
            .is_some_and(|state| state != 'Z' && state != 'X')
    }

    fn zombie(&self) -> bool {
        self.state() == Some('Z')
    }

    fn wait_gone(&self, what: &str, limit: Duration) {
        wait_until(what, limit, || !self.running());
    }
}

/// Stops a process the runner deliberately does not track, after checking
/// that the pid still names the same process.
fn kill_detached(process: Process) {
    if process.running() {
        let _ = std::process::Command::new("kill")
            .args(["-KILL", &process.pid.to_string()])
            .status();
    }
}

fn wait_until(what: &str, limit: Duration, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + limit;
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn output_streams_are_kept_apart_in_order_and_the_exit_code_is_reported() {
    let directory = tempfile::tempdir().unwrap();
    let mut spec = shell(
        "echo one; echo oops >&2; printf 'two\\r\\nthree'; echo \"$GREETING $PWD\"; exit 3",
        directory.path(),
    );
    spec.env.insert("GREETING".into(), "hi".into());
    let (_handle, events) = run(&spec);
    let (lines, outcome, _) = collect(&events);
    let pwd = directory.path().canonicalize().unwrap();
    assert_eq!(
        texts(&lines, Stream::Stdout),
        ["one", "two", &format!("threehi {}", pwd.display())]
    );
    assert_eq!(texts(&lines, Stream::Stderr), ["oops"]);
    assert_eq!(outcome, TaskOutcome::Exited(3));
}

#[test]
fn removed_variables_are_not_inherited() {
    let directory = tempfile::tempdir().unwrap();
    // Cargo sets this for every test process; the shell does not invent it.
    let name = "CARGO_MANIFEST_DIR";
    assert!(
        std::env::var_os(name).is_some(),
        "{name} must be inherited to begin with"
    );
    let script = format!("echo \"[${{{name}:+set}}]\"");
    let (_handle, events) = run(&shell(&script, directory.path()));
    assert_eq!(texts(&collect(&events).0, Stream::Stdout), ["[set]"]);

    let mut spec = shell(&script, directory.path());
    spec.env_remove.push(name.into());
    let (_handle, events) = run(&spec);
    let (lines, outcome, _) = collect(&events);
    assert_eq!(texts(&lines, Stream::Stdout), ["[]"]);
    assert_eq!(outcome, TaskOutcome::Exited(0));
}

#[test]
fn long_lines_are_cut_and_marked() {
    let directory = tempfile::tempdir().unwrap();
    let length = MAX_LINE_BYTES * 3;
    let (_handle, events) = run(&shell(
        &format!("head -c {length} /dev/zero | tr '\\0' x; echo; echo after"),
        directory.path(),
    ));
    let (lines, _, _) = collect(&events);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].text.len(), MAX_LINE_BYTES);
    assert!(lines[0].truncated);
    assert_eq!(lines[1].text, "after");
    assert!(!lines[1].truncated);
}

#[test]
fn slow_output_arrives_while_the_tool_is_still_running() {
    let directory = tempfile::tempdir().unwrap();
    let (handle, events) = run(&shell("echo first; sleep 5; echo second", directory.path()));
    let started = Instant::now();
    match events.recv_timeout(Duration::from_secs(3)).unwrap() {
        TaskEvent::Output(batch) => assert_eq!(texts(&batch.lines, Stream::Stdout), ["first"]),
        other => panic!("expected output, got {other:?}"),
    }
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(!handle.is_finished());
    handle.cancel();
    let (_, outcome, _) = collect(&events);
    assert_eq!(outcome, TaskOutcome::Cancelled);
}

#[test]
fn a_consumer_that_falls_behind_holds_the_tool_back() {
    let directory = tempfile::tempdir().unwrap();
    let counter = directory.path().join("written");
    // Counts lines as it writes them; blocks when the pipe is full.
    let script = format!(
        "i=0; while [ $i -lt 200000 ]; do echo line-$i; i=$((i+1)); echo $i > {}; done",
        counter.display()
    );
    let (handle, events) = run(&shell(&script, directory.path()));
    let mut held: Vec<Batch> = Vec::new();
    while held.len() < MAX_PENDING_BATCHES {
        if let TaskEvent::Output(batch) = events.recv_timeout(Duration::from_secs(10)).unwrap() {
            held.push(batch);
        }
    }
    // No further batch is delivered while all credits are held ...
    assert!(events.recv_timeout(Duration::from_millis(500)).is_err());
    // ... and the tool comes to a stop once the runner's buffer and the pipe
    // are full, long before it has written all 200,000 lines.
    let written = || {
        fs::read_to_string(&counter)
            .ok()
            .and_then(|text| text.trim().parse::<usize>().ok())
            .unwrap_or(0)
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut previous = written();
    let stalled_at = loop {
        std::thread::sleep(Duration::from_millis(400));
        let now = written();
        if now == previous {
            break now;
        }
        assert!(
            Instant::now() < deadline,
            "the tool never stopped writing ({now} lines)"
        );
        previous = now;
    };
    assert!(
        stalled_at < 100_000,
        "{stalled_at} lines were written without being consumed"
    );

    drop(held);
    assert!(matches!(
        events.recv_timeout(Duration::from_secs(5)).unwrap(),
        TaskEvent::Output(_)
    ));
    handle.cancel();
    let (_, outcome, _) = collect(&events);
    assert_eq!(outcome, TaskOutcome::Cancelled);
}

#[test]
fn cancelling_stops_the_whole_process_group() {
    let directory = tempfile::tempdir().unwrap();
    let pid_file = directory.path().join("child.pid");
    let script = format!(
        "sleep 60 & echo $! > {}; echo started; wait",
        pid_file.display()
    );
    let (handle, events) = run(&shell(&script, directory.path()));
    let grandchild = Process::from_file(&pid_file);
    let leader = Process::of(handle.pid());
    assert!(grandchild.running() && leader.running());

    let cancelled_at = Instant::now();
    handle.cancel();
    handle.cancel();
    let (lines, outcome, _) = collect(&events);
    assert_eq!(outcome, TaskOutcome::Cancelled);
    assert_eq!(texts(&lines, Stream::Stdout), ["started"]);
    assert!(cancelled_at.elapsed() < CANCEL_GRACE, "SIGTERM was enough");
    grandchild.wait_gone("the grandchild is gone", Duration::from_secs(3));
    assert!(
        !leader.running() && !leader.zombie(),
        "the leader was reaped"
    );
    assert!(handle.is_finished());
}

#[test]
fn a_tool_that_ignores_sigterm_is_killed_after_the_grace_period() {
    let directory = tempfile::tempdir().unwrap();
    let (handle, events) = run(&shell(
        "trap '' TERM; echo ready; while :; do sleep 1; done",
        directory.path(),
    ));
    assert!(matches!(
        events.recv_timeout(Duration::from_secs(5)).unwrap(),
        TaskEvent::Output(_)
    ));
    let cancelled_at = Instant::now();
    handle.cancel();
    let (_, outcome, _) = collect(&events);
    let took = cancelled_at.elapsed();
    assert_eq!(outcome, TaskOutcome::Cancelled);
    assert!(took >= CANCEL_GRACE && took < CANCEL_GRACE * 3, "{took:?}");
}

#[test]
fn a_signal_from_outside_is_not_a_cancellation() {
    let directory = tempfile::tempdir().unwrap();
    let (_handle, events) = run(&shell("kill -9 $$", directory.path()));
    let (_, outcome, _) = collect(&events);
    assert_eq!(outcome, TaskOutcome::Signalled(9));
}

#[test]
fn dropping_the_handle_kills_a_running_tool() {
    let directory = tempfile::tempdir().unwrap();
    let (handle, events) = run(&shell("echo up; sleep 60", directory.path()));
    assert!(matches!(
        events.recv_timeout(Duration::from_secs(5)).unwrap(),
        TaskEvent::Output(_)
    ));
    let leader = Process::of(handle.pid());
    drop(handle);
    let (_, outcome, _) = collect(&events);
    assert_eq!(outcome, TaskOutcome::Cancelled);
    leader.wait_gone("the tool is gone", Duration::from_secs(3));
}

#[test]
fn a_leftover_process_holding_the_pipes_is_killed_after_the_exit_grace() {
    let directory = tempfile::tempdir().unwrap();
    let pid_file = directory.path().join("leftover.pid");
    let started = Instant::now();
    let (_handle, events) = run(&shell(
        &format!("sleep 60 & echo $! > {}; echo done", pid_file.display()),
        directory.path(),
    ));
    let leftover = Process::from_file(&pid_file);
    let (lines, outcome, duration) = collect(&events);
    assert_eq!(texts(&lines, Stream::Stdout), ["done"]);
    assert_eq!(outcome, TaskOutcome::Exited(0));
    assert!(duration < Duration::from_secs(1), "{duration:?}");
    let took = started.elapsed();
    assert!(took >= EXIT_GRACE && took < EXIT_GRACE * 3, "{took:?}");
    leftover.wait_gone("the leftover is killed", Duration::from_secs(2));
}

#[test]
fn a_leftover_process_without_the_pipes_is_killed_when_the_task_ends() {
    let directory = tempfile::tempdir().unwrap();
    let pid_file = directory.path().join("leftover.pid");
    let release = directory.path().join("release");
    let (_handle, events) = run(&shell(
        &format!(
            "(exec >/dev/null 2>&1; sleep 60) & echo $! > {}; while [ ! -f {} ]; do sleep 0.01; done; echo done",
            pid_file.display(),
            release.display()
        ),
        directory.path(),
    ));
    let leftover = Process::from_file(&pid_file);
    assert!(leftover.running());
    let started = Instant::now();
    fs::write(&release, "").unwrap();
    let (_, outcome, _) = collect(&events);
    assert_eq!(outcome, TaskOutcome::Exited(0));
    assert!(
        started.elapsed() < EXIT_GRACE,
        "no grace is needed when the pipes close"
    );
    leftover.wait_gone("the leftover is killed", Duration::from_secs(2));
}

#[test]
fn cancelling_kills_a_descendant_that_ignores_sigterm_after_the_leader_exits() {
    let directory = tempfile::tempdir().unwrap();
    let pid_file = directory.path().join("stubborn.pid");
    // The leader exits on SIGTERM at once; its child ignores SIGTERM and
    // keeps the output pipes open. The child writes its pid only after its
    // trap is in place.
    let script = format!(
        "sh -c 'trap \"\" TERM; echo $$ > {}; while :; do sleep 0.1; done' & echo started; wait",
        pid_file.display()
    );
    let (handle, events) = run(&shell(&script, directory.path()));
    let stubborn = Process::from_file(&pid_file);
    let leader = Process::of(handle.pid());
    let cancelled_at = Instant::now();
    handle.cancel();
    wait_until(
        "the leader exits on SIGTERM",
        Duration::from_secs(2),
        || !leader.running(),
    );
    assert!(stubborn.running(), "the child ignores SIGTERM");
    let (_, outcome, _) = collect(&events);
    let took = cancelled_at.elapsed();
    assert_eq!(outcome, TaskOutcome::Cancelled);
    assert!(took < CANCEL_GRACE * 2, "{took:?}");
    stubborn.wait_gone("the stubborn child is killed", Duration::from_secs(2));
    assert!(!leader.zombie(), "the leader was reaped");
}

#[test]
fn cancelling_kills_a_descendant_that_ignores_sigterm_and_closed_its_pipes() {
    let directory = tempfile::tempdir().unwrap();
    let pid_file = directory.path().join("stubborn.pid");
    let script = format!(
        "sh -c 'trap \"\" TERM; exec >/dev/null 2>&1; echo $$ > {}; while :; do sleep 0.1; done' & echo started; wait",
        pid_file.display()
    );
    let (handle, events) = run(&shell(&script, directory.path()));
    let stubborn = Process::from_file(&pid_file);
    handle.cancel();
    let (_, outcome, _) = collect(&events);
    assert_eq!(outcome, TaskOutcome::Cancelled);
    stubborn.wait_gone("the stubborn child is killed", Duration::from_secs(2));
}

#[test]
fn dropping_the_handle_after_the_leader_exited_kills_what_is_left() {
    let directory = tempfile::tempdir().unwrap();
    let pid_file = directory.path().join("leftover.pid");
    let (handle, events) = run(&shell(
        &format!("sleep 60 & echo $! > {}; echo done", pid_file.display()),
        directory.path(),
    ));
    let leftover = Process::from_file(&pid_file);
    let leader = Process::of(handle.pid());
    // The leader has exited but is not reaped while the task still runs.
    wait_until("the leader has exited", Duration::from_secs(5), || {
        leader.zombie()
    });
    assert!(leftover.running());
    let dropped_at = Instant::now();
    drop(handle);
    leftover.wait_gone(
        "the leftover is killed at once",
        Duration::from_millis(1500),
    );
    assert!(
        dropped_at.elapsed() < EXIT_GRACE,
        "it did not wait for the grace period"
    );
    let (_, outcome, _) = collect(&events);
    assert_eq!(outcome, TaskOutcome::Cancelled);
    assert!(!leader.zombie(), "the leader was reaped");
}

#[test]
fn a_missing_program_fails_to_start_without_events() {
    let directory = tempfile::tempdir().unwrap();
    let error = spawn(
        &ProcessSpec::new("/no/such/program", directory.path()),
        |_| panic!("no events for a task that never started"),
    )
    .unwrap_err();
    assert!(
        error.starts_with("cannot start `/no/such/program`"),
        "{error}"
    );
    assert!(
        capture(
            &ProcessSpec::new("/no/such/program", directory.path()),
            10,
            Duration::from_secs(1)
        )
        .is_err()
    );
}

#[test]
fn capture_keeps_long_lines_whole() {
    let directory = tempfile::tempdir().unwrap();
    let length = MAX_LINE_BYTES * 10;
    let captured = capture(
        &shell(
            &format!("head -c {length} /dev/zero | tr '\\0' y; echo; echo warn >&2; exit 4"),
            directory.path(),
        ),
        1024 * 1024,
        Duration::from_secs(10),
    )
    .unwrap();
    assert_eq!(captured.stdout.trim_end().len(), length);
    assert_eq!(captured.stderr, "warn\n");
    assert_eq!(captured.outcome, TaskOutcome::Exited(4));
}

#[test]
fn capture_ends_when_the_leader_exits_but_a_child_keeps_the_pipes() {
    let directory = tempfile::tempdir().unwrap();
    let pid_file = directory.path().join("leftover.pid");
    let started = Instant::now();
    let captured = capture(
        &shell(
            &format!("sleep 60 & echo $! > {}; echo done", pid_file.display()),
            directory.path(),
        ),
        1024,
        Duration::from_secs(20),
    )
    .unwrap();
    let took = started.elapsed();
    assert_eq!(captured.stdout, "done\n");
    assert_eq!(captured.outcome, TaskOutcome::Exited(0));
    assert!(took >= EXIT_GRACE && took < EXIT_GRACE * 3, "{took:?}");
    // The kill was sent before the pipes closed; a dying process closes its
    // files slightly before it shows as exited, so allow it a moment.
    let pid: u32 = fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    wait_until(
        "the child holding the pipes is gone",
        Duration::from_secs(1),
        || stat(pid).is_none_or(|(state, _)| state == 'Z' || state == 'X'),
    );
}

#[test]
fn capture_keeps_its_deadline_when_a_detached_process_holds_the_pipes() {
    let directory = tempfile::tempdir().unwrap();
    let pid_file = directory.path().join("detached.pid");
    // `setsid` moves the child out of the group, beyond the runner's reach.
    let script = format!(
        "setsid sh -c 'echo $$ > {}; exec sleep 60' & while [ ! -s {} ]; do sleep 0.05; done; echo done",
        pid_file.display(),
        pid_file.display()
    );
    let started = Instant::now();
    let result = capture(
        &shell(&script, directory.path()),
        1024,
        Duration::from_secs(4),
    );
    let took = started.elapsed();
    let detached = Process::from_file(&pid_file);
    kill_detached(detached);
    let error = result.unwrap_err();
    assert!(error.contains("did not finish within 4s"), "{error}");
    assert!(took < Duration::from_secs(6), "{took:?}");
}

#[test]
fn capture_enforces_its_size_and_time_limits() {
    let directory = tempfile::tempdir().unwrap();
    let too_much = capture(
        &shell("head -c 5000 /dev/zero", directory.path()),
        1000,
        Duration::from_secs(10),
    )
    .unwrap_err();
    assert!(too_much.contains("more than 1000 bytes"), "{too_much}");

    let pid_file = directory.path().join("pid");
    let started = Instant::now();
    let too_slow = capture(
        &shell(
            &format!("echo $$ > {}; sleep 30", pid_file.display()),
            directory.path(),
        ),
        1000,
        Duration::from_millis(300),
    )
    .unwrap_err();
    assert!(
        too_slow.contains("did not finish within 300ms"),
        "{too_slow}"
    );
    assert!(started.elapsed() < Duration::from_secs(3));
    let pid: u32 = fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    wait_until("the slow tool is gone", Duration::from_secs(3), || {
        stat(pid).is_none_or(|(state, _)| state == 'Z' || state == 'X')
    });
}

#[test]
fn cancelling_capture_reaps_its_process_and_pre_cancelled_work_never_starts() {
    use retrowork_platform::task::{Cancellation, capture_with_cancel};
    let directory = tempfile::tempdir().unwrap();
    let pid_file = directory.path().join("capture.pid");
    let spec = shell(
        &format!("echo $$ > {}; sleep 60", pid_file.display()),
        directory.path(),
    );
    let cancellation = Cancellation::default();
    let worker_cancel = cancellation.clone();
    let worker = std::thread::spawn(move || {
        capture_with_cancel(&spec, 1024, Duration::from_secs(10), &worker_cancel)
    });
    let process = Process::from_file(&pid_file);
    cancellation.cancel();
    assert!(worker.join().unwrap().unwrap_err().contains("cancelled"));
    assert!(!process.running());
    assert!(!process.zombie());
    let marker = directory.path().join("unexpected");
    let spec = shell(&format!("touch {}", marker.display()), directory.path());
    assert!(capture_with_cancel(&spec, 1024, Duration::from_secs(1), &cancellation).is_err());
    assert!(!marker.exists());
}
