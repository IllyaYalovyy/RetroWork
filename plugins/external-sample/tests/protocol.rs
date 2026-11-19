//! Speaks the plugin protocol to the real sample executable, the way the
//! host does: JSON lines over the child's stdin and stdout.

use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};

use serde_json::{Value, json};

struct Plugin {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

impl Plugin {
    fn start() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_retrowork-plugin-sample"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        Self {
            stdin: child.stdin.take(),
            stdout: BufReader::new(child.stdout.take().unwrap()),
            child,
        }
    }

    fn send(&mut self, message: &Value) {
        writeln!(self.stdin.as_mut().expect("stdin is open"), "{message}").unwrap();
    }

    fn receive(&mut self) -> Value {
        let mut line = String::new();
        assert!(
            self.stdout.read_line(&mut line).unwrap() > 0,
            "plugin closed its output"
        );
        serde_json::from_str(&line).unwrap()
    }

    fn handshake() -> Self {
        let mut plugin = Self::start();
        plugin.send(&json!({ "type": "hello", "protocol": 0 }));
        assert_eq!(
            plugin.receive(),
            json!({ "type": "ready", "protocol": 0, "plugin": "sample.top-components" })
        );
        plugin
    }
}

impl Drop for Plugin {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn record(component: &str) -> Value {
    json!({ "line": 1, "timestamp": "t", "level": "INFO", "component": component, "message": "m" })
}

#[test]
fn ranks_components_of_the_records_it_is_given() {
    let mut plugin = Plugin::handshake();
    let records: Vec<Value> = ["b", "a", "b", "c", "b", "a", ""]
        .into_iter()
        .map(record)
        .collect();
    plugin.send(&json!({
        "type": "invoke",
        "request": 7,
        "command": "sample.top-components.run",
        "context": { "document": {
            "kind": "logs.source", "resource": "x", "title": "x.log",
            "data": { "records": records, "truncated": true },
        } },
    }));
    assert_eq!(
        plugin.receive(),
        json!({
            "type": "result",
            "request": 7,
            "effects": [
                { "type": "output", "lines": [
                    "Top components in x.log (7 records shown):",
                    "     3  b",
                    "     2  a",
                    "     1  c",
                    "  (only the first records were analysed)",
                ] },
                { "type": "status", "text": "Top components: 3 found" },
            ],
        })
    );
}

#[test]
fn reports_errors_for_requests_it_cannot_serve_and_stays_usable() {
    let mut plugin = Plugin::handshake();
    plugin.send(&json!({
        "type": "invoke", "request": 1, "command": "sample.top-components.run",
        "context": { "document": null },
    }));
    let reply = plugin.receive();
    assert_eq!(reply["type"], "error");
    assert_eq!(reply["request"], 1);
    assert!(
        reply["message"]
            .as_str()
            .unwrap()
            .contains("open a log first")
    );

    plugin.send(&json!({
        "type": "invoke", "request": 2, "command": "something.else", "context": {},
    }));
    let reply = plugin.receive();
    assert_eq!(
        (&reply["type"], &reply["request"]),
        (&json!("error"), &json!(2))
    );

    // Lines that are not messages are ignored rather than answered.
    plugin.send(&json!("noise"));
    plugin.send(&json!({
        "type": "invoke", "request": 3, "command": "sample.top-components.run",
        "context": { "document": { "title": "y.log", "data": { "records": [] } } },
    }));
    assert_eq!(plugin.receive()["request"], 3);
}

#[test]
fn exits_when_the_host_speaks_another_protocol_version_or_closes_the_pipe() {
    let mut plugin = Plugin::start();
    plugin.send(&json!({ "type": "hello", "protocol": 1 }));
    assert!(plugin.child.wait().unwrap().success());

    let mut plugin = Plugin::handshake();
    // End of input is how the host says goodbye.
    plugin.stdin.take();
    assert!(plugin.child.wait().unwrap().success());
}
