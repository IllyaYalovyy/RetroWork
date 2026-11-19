//! Adversarial tests of the process-plugin host against real child processes.
//!
//! Each plugin here is a small shell script, so every failure mode is a real
//! OS process misbehaving, not a mock. Request numbers count from 1 per host,
//! which lets a script answer without parsing JSON.
#![cfg(unix)]

use std::{
    fs,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};

use retrowork_platform::{
    CommandId, ModuleId, ModuleState, Package, Platform,
    external::{Effect, MAX_QUEUED_REQUESTS, PluginError, ProcessHost},
};
use serde_json::{Value, json};

const READY: &str = r#"echo '{"type":"ready","protocol":0,"plugin":"test.plugin"}'"#;

/// Answers every request with one output line `reply <n>`.
fn answering(extra_setup: &str, before_reply: &str) -> String {
    format!(
        r#"{extra_setup}
read hello
{READY}
while read line; do
  n=$(printf '%s' "$line" | sed 's/.*"request":\([0-9]*\).*/\1/')
  {before_reply}
  echo "{{\"type\":\"result\",\"request\":$n,\"effects\":[{{\"type\":\"output\",\"lines\":[\"reply $n\"]}}]}}"
done
"#
    )
}

/// Creates an executable script.
///
/// The file is written by a helper process, never by this one. A test binary
/// runs tests on several threads; if this process held the script open for
/// writing while another thread forked a child, that child would inherit the
/// descriptor and executing the script would fail with "Text file busy".
fn script(directory: &Path, name: &str, body: &str) -> PathBuf {
    use std::{
        io::Write as _,
        process::{Command, Stdio},
    };
    let path = directory.join(name);
    let mut writer = Command::new("/bin/sh")
        .args(["-c", r#"cat > "$0" && chmod 755 "$0""#])
        .arg(&path)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    writer
        .stdin
        .take()
        .unwrap()
        .write_all(format!("#!/bin/sh\n{body}").as_bytes())
        .unwrap();
    assert!(writer.wait().unwrap().success());
    path
}

fn host(program: PathBuf, timeout_ms: u64) -> ProcessHost {
    ProcessHost::new(
        ModuleId::literal("test.plugin"),
        program,
        Duration::from_millis(timeout_ms),
    )
}

type Outcome = Result<Vec<Effect>, PluginError>;

fn submit(host: &ProcessHost, context: Value) -> Receiver<Outcome> {
    let (sender, receiver) = mpsc::channel();
    host.submit(CommandId::literal("test.run"), context, move |result| {
        let _ = sender.send(result);
    });
    receiver
}

fn outcome(receiver: &Receiver<Outcome>) -> Outcome {
    receiver
        .recv_timeout(Duration::from_secs(10))
        .expect("the host must always report an outcome")
}

fn reply(n: u32) -> Outcome {
    Ok(vec![Effect::Output {
        lines: vec![format!("reply {n}")],
    }])
}

/// Polls `condition` until it holds; fails the test after `limit`.
fn wait_until(what: &str, limit: Duration, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + limit;
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn pids(file: &Path) -> Vec<u32> {
    fs::read_to_string(file)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.trim().parse().ok())
        .collect()
}

fn is_running(pid: u32) -> bool {
    // A killed but unreaped process still has a /proc entry, so this also
    // proves the host reaped it.
    Path::new(&format!("/proc/{pid}")).exists()
}

fn assert_stopped(pid_file: &Path) {
    let pids = pids(pid_file);
    assert!(!pids.is_empty(), "the plugin never started");
    for pid in pids {
        wait_until(
            &format!("plugin process {pid} is gone"),
            Duration::from_secs(3),
            || !is_running(pid),
        );
    }
}

#[test]
fn requests_are_answered_in_order_by_one_process() {
    let directory = tempfile::tempdir().unwrap();
    let pid_file = directory.path().join("pids");
    let program = script(
        directory.path(),
        "plugin",
        &answering(&format!("echo $$ >> {}", pid_file.display()), ""),
    );
    let host = host(program, 5000);
    for n in 1..=3 {
        assert_eq!(outcome(&submit(&host, json!({}))), reply(n));
    }
    assert_eq!(pids(&pid_file).len(), 1, "the process is reused");
    drop(host);
    assert_stopped(&pid_file);
}

#[test]
fn plugin_that_never_replies_times_out_and_is_killed() {
    let directory = tempfile::tempdir().unwrap();
    let pid_file = directory.path().join("pids");
    let program = script(
        directory.path(),
        "plugin",
        &format!(
            "echo $$ >> {}\nread hello\n{READY}\nexec sleep 60\n",
            pid_file.display()
        ),
    );
    let host = host(program, 300);
    let started = Instant::now();
    assert_eq!(
        outcome(&submit(&host, json!({}))),
        Err(PluginError::Timeout(Duration::from_millis(300)))
    );
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_stopped(&pid_file);
}

#[test]
fn plugin_that_stops_reading_its_input_cannot_hang_the_host() {
    let directory = tempfile::tempdir().unwrap();
    let pid_file = directory.path().join("pids");
    let program = script(
        directory.path(),
        "plugin",
        &format!(
            "echo $$ >> {}\nread hello\n{READY}\nexec sleep 60\n",
            pid_file.display()
        ),
    );
    let host = host(program, 400);
    // Far larger than a pipe buffer: the write blocks until someone reads.
    let context = json!({ "padding": "x".repeat(4 * 1024 * 1024) });
    let started = Instant::now();
    assert_eq!(
        outcome(&submit(&host, context)),
        Err(PluginError::Timeout(Duration::from_millis(400)))
    );
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "a blocked write must respect the deadline, took {:?}",
        started.elapsed()
    );
    assert_stopped(&pid_file);

    // The host is usable again with a fresh process.
    let second = outcome(&submit(&host, json!({})));
    assert_eq!(
        second,
        Err(PluginError::Timeout(Duration::from_millis(400)))
    );
    assert_eq!(pids(&pid_file).len(), 2);
}

#[test]
fn overload_is_refused_at_once_and_time_in_the_queue_counts_against_the_deadline() {
    let directory = tempfile::tempdir().unwrap();
    let seen = directory.path().join("seen");
    let pid_file = directory.path().join("pids");
    let program = script(
        directory.path(),
        "plugin",
        &answering(
            &format!("echo $$ >> {}", pid_file.display()),
            &format!("echo $n >> {}; sleep 0.5", seen.display()),
        ),
    );
    let host = host(program, 1300);

    let first = submit(&host, json!({}));
    // Wait until the first request is in flight so the queue is empty.
    wait_until(
        "the plugin received request 1",
        Duration::from_secs(5),
        || fs::read_to_string(&seen).is_ok_and(|text| text.contains('1')),
    );
    let queued: Vec<_> = (0..MAX_QUEUED_REQUESTS)
        .map(|_| submit(&host, json!({})))
        .collect();
    let refused_at = Instant::now();
    let refused: Vec<_> = (0..3).map(|_| submit(&host, json!({}))).collect();
    for receiver in &refused {
        // Refusal is synchronous: the outcome is already there.
        assert_eq!(receiver.try_recv(), Ok(Err(PluginError::Busy)));
    }
    assert!(refused_at.elapsed() < Duration::from_millis(200));

    assert_eq!(outcome(&first), reply(1));
    let outcomes: Vec<Outcome> = queued.iter().map(outcome).collect();
    // 0.5 s per request against a 1.3 s budget: the queue cannot drain.
    assert_eq!(outcomes[0], reply(2), "{outcomes:?}");
    let timeout = Err(PluginError::Timeout(Duration::from_millis(1300)));
    assert_eq!(outcomes[2..], [timeout.clone(), timeout], "{outcomes:?}");
    // Expired requests were never handed to the plugin.
    let handed_over = fs::read_to_string(&seen).unwrap().lines().count();
    assert!(handed_over <= 3, "plugin saw {handed_over} requests");
}

#[test]
fn flood_of_unsolicited_output_is_fatal_and_does_not_accumulate() {
    let directory = tempfile::tempdir().unwrap();
    let pid_file = directory.path().join("pids");
    let program = script(
        directory.path(),
        "plugin",
        &format!(
            "echo $$ >> {}\nread hello\n{READY}\nwhile :; do echo '{{\"type\":\"result\",\"request\":999,\"effects\":[]}}'; done\n",
            pid_file.display()
        ),
    );
    let host = host(program, 2000);
    let started = Instant::now();
    let result = outcome(&submit(&host, json!({})));
    assert!(
        matches!(&result, Err(PluginError::Protocol(_))),
        "{result:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_stopped(&pid_file);
}

#[test]
fn endless_line_without_a_newline_is_cut_off() {
    let directory = tempfile::tempdir().unwrap();
    let pid_file = directory.path().join("pids");
    let program = script(
        directory.path(),
        "plugin",
        &format!(
            "echo $$ >> {}\nread hello\n{READY}\nread request\nexec tr '\\0' 'x' < /dev/zero\n",
            pid_file.display()
        ),
    );
    let host = host(program, 5000);
    let result = outcome(&submit(&host, json!({})));
    assert!(
        matches!(&result, Err(PluginError::Protocol(message)) if message.contains("longer than")),
        "{result:?}"
    );
    assert_stopped(&pid_file);
}

#[test]
fn dropping_the_host_stops_a_plugin_that_is_mid_request() {
    let directory = tempfile::tempdir().unwrap();
    let pid_file = directory.path().join("pids");
    let program = script(
        directory.path(),
        "plugin",
        &format!(
            "read hello\n{READY}\nread request\necho $$ >> {}\nexec sleep 60\n",
            pid_file.display()
        ),
    );
    let host = host(program, 60_000);
    let in_flight = submit(&host, json!({}));
    let queued = submit(&host, json!({}));
    wait_until("the request is in flight", Duration::from_secs(5), || {
        !pids(&pid_file).is_empty()
    });

    let started = Instant::now();
    drop(host);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "shutdown must not wait for the plugin, took {:?}",
        started.elapsed()
    );
    assert_stopped(&pid_file);
    assert_eq!(outcome(&in_flight), Err(PluginError::ShutDown));
    assert_eq!(outcome(&queued), Err(PluginError::ShutDown));
    assert_eq!(
        pids(&pid_file).len(),
        1,
        "no process is started after shutdown"
    );
}

#[test]
fn crash_is_reported_and_the_next_request_gets_a_fresh_process() {
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("crashed-once");
    let program = script(
        directory.path(),
        "plugin",
        &format!(
            "if [ ! -e {marker} ]; then\n  touch {marker}\n  read hello\n  {READY}\n  read request\n  exit 3\nfi\n{}",
            answering("", ""),
            marker = marker.display()
        ),
    );
    let host = host(program, 5000);
    assert_eq!(outcome(&submit(&host, json!({}))), Err(PluginError::Exited));
    assert_eq!(outcome(&submit(&host, json!({}))), reply(2));
}

#[test]
fn error_reported_by_the_plugin_keeps_the_process() {
    let directory = tempfile::tempdir().unwrap();
    let pid_file = directory.path().join("pids");
    let program = script(
        directory.path(),
        "plugin",
        &format!(
            "echo $$ >> {}\nread hello\n{READY}\nn=0\nwhile read line; do n=$((n+1)); echo \"{{\\\"type\\\":\\\"error\\\",\\\"request\\\":$n,\\\"message\\\":\\\"no log open\\\"}}\"; done\n",
            pid_file.display()
        ),
    );
    let host = host(program, 5000);
    for _ in 0..2 {
        assert_eq!(
            outcome(&submit(&host, json!({}))),
            Err(PluginError::Reported("no log open".into()))
        );
    }
    assert_eq!(pids(&pid_file).len(), 1);
}

#[test]
fn wrong_identity_and_missing_executable_are_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let program = script(
        directory.path(),
        "plugin",
        "read hello\necho '{\"type\":\"ready\",\"protocol\":0,\"plugin\":\"someone.else\"}'\nexec sleep 60\n",
    );
    let result = outcome(&submit(&host(program, 5000), json!({})));
    assert!(
        matches!(&result, Err(PluginError::Protocol(message)) if message.contains("someone.else")),
        "{result:?}"
    );

    let missing = host(directory.path().join("not-there"), 5000);
    let result = outcome(&submit(&missing, json!({})));
    assert!(
        matches!(result, Err(PluginError::Spawn { .. })),
        "{result:?}"
    );
}

fn package(directory: &Path, plugin_body: &str) -> Package {
    script(directory, "plugin.sh", plugin_body);
    fs::write(
        directory.join("package.toml"),
        r#"
            id = "test.plugin"
            name = "Test Plugin"
            version = "0.1.0"
            kind = "process"
            requires-api = "^0.1"

            [process]
            command = "plugin.sh"
            timeout-ms = 400

            [[command]]
            id = "test.run"
            title = "Run Test"

            [[menu-item]]
            menu = "tools"
            id = "run-test"
            title = "Run Test"
            command = "test.run"
        "#,
    )
    .unwrap();
    Package::load(directory).unwrap()
}

fn output(platform: &Platform) -> Vec<String> {
    platform
        .shell()
        .output()
        .lines()
        .iter()
        .map(|line| line.text.clone())
        .collect()
}

#[test]
fn package_command_runs_off_the_ui_thread_and_its_effects_arrive_through_the_inbox() {
    let directory = tempfile::tempdir().unwrap();
    let mut platform = Platform::builder()
        .module(package(directory.path(), &answering("", "")))
        .build();
    assert_eq!(platform.modules()[1].state, ModuleState::Active);

    let command = CommandId::literal("test.run");
    platform.execute(&command, None).unwrap();
    // Executing returns before the plugin has answered.
    assert_eq!(platform.shell().status(), "Running Run Test…");
    assert!(output(&platform).is_empty());

    assert!(platform.wait(Duration::from_secs(5)));
    assert_eq!(platform.pump(), 1);
    assert_eq!(output(&platform), ["reply 1"]);
    assert_eq!(
        platform
            .shell()
            .active_view(retrowork_platform::ViewLocation::Bottom)
            .map(|view| view.as_str()),
        Some("output")
    );
}

#[test]
fn package_command_failure_is_shown_to_the_user() {
    let directory = tempfile::tempdir().unwrap();
    let mut platform = Platform::builder()
        .module(package(
            directory.path(),
            &format!("read hello\n{READY}\nexec sleep 60\n"),
        ))
        .build();
    platform
        .execute(&CommandId::literal("test.run"), None)
        .unwrap();
    assert!(platform.wait(Duration::from_secs(5)));
    platform.pump();
    let output = output(&platform);
    assert_eq!(output.len(), 1);
    assert!(
        output[0].starts_with("Run Test failed (plugin `test.plugin`): no reply within 400ms"),
        "{output:?}"
    );
    assert_eq!(platform.shell().status(), output[0]);
    let failures = platform
        .shell()
        .trace()
        .since(0)
        .into_iter()
        .filter(|event| event.kind == "plugin.failed")
        .count();
    assert_eq!(failures, 1);
}

#[test]
fn package_with_a_missing_executable_is_rejected_with_the_reason() {
    let directory = tempfile::tempdir().unwrap();
    let package = package(directory.path(), "exit 0\n");
    fs::remove_file(directory.path().join("plugin.sh")).unwrap();
    let platform = Platform::builder().module(package).build();
    let ModuleState::Rejected(reason) = &platform.modules()[1].state else {
        panic!("the package must be rejected");
    };
    assert!(reason.to_string().contains("plugin.sh"), "{reason}");
    // Nothing of the package is visible.
    assert!(
        platform
            .contributions()
            .command(&CommandId::literal("test.run"))
            .is_none()
    );
    assert!(
        platform
            .contributions()
            .menu_items(&retrowork_platform::MenuId::literal("tools"))
            .is_empty()
    );
}
