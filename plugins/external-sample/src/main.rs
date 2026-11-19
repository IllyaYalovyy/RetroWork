//! Sample process plugin: counts records per component in the active log.
//!
//! This program deliberately shares no code with RetroWork. It could be
//! written in any language; all it needs is to read and write the JSON lines
//! of plugin protocol version 0, which is experimental.

use std::{
    collections::BTreeMap,
    io::{self, BufRead, Write},
};

use serde_json::{Value, json};

const PLUGIN_ID: &str = "sample.top-components";
const COMMAND: &str = "sample.top-components.run";
const PROTOCOL: u64 = 0;

fn main() -> io::Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            // Not ours to interpret; the host treats silence as a timeout.
            continue;
        };
        let reply = match message["type"].as_str() {
            Some("hello") if message["protocol"].as_u64() == Some(PROTOCOL) => {
                json!({ "type": "ready", "protocol": PROTOCOL, "plugin": PLUGIN_ID })
            }
            Some("hello") => return Ok(()),
            Some("invoke") => invoke(&message),
            _ => continue,
        };
        writeln!(stdout, "{reply}")?;
        stdout.flush()?;
    }
    Ok(())
}

fn invoke(message: &Value) -> Value {
    let request = &message["request"];
    if message["command"].as_str() != Some(COMMAND) {
        return json!({
            "type": "error",
            "request": request,
            "message": format!("unknown command {}", message["command"]),
        });
    }
    let document = &message["context"]["document"];
    let Some(records) = document["data"]["records"].as_array() else {
        return json!({
            "type": "error",
            "request": request,
            "message": "open a log first: there is no active log document",
        });
    };

    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for record in records {
        if let Some(component) = record["component"].as_str().filter(|name| !name.is_empty()) {
            *counts.entry(component).or_default() += 1;
        }
    }
    let mut ranked: Vec<(&str, usize)> = counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));

    let title = document["title"].as_str().unwrap_or("log");
    let mut lines = vec![format!(
        "Top components in {title} ({} records shown):",
        records.len()
    )];
    lines.extend(
        ranked
            .iter()
            .take(5)
            .map(|(component, count)| format!("  {count:>4}  {component}")),
    );
    if document["data"]["truncated"].as_bool() == Some(true) {
        lines.push("  (only the first records were analysed)".to_owned());
    }
    json!({
        "type": "result",
        "request": request,
        "effects": [
            { "type": "output", "lines": lines },
            { "type": "status", "text": format!("Top components: {} found", ranked.len()) },
        ],
    })
}
