//! Real-window acceptance of the isolated file finder with deliberate input failures.

use retrowork_testkit::{
    driver::{Outcome, Ui, UiDriver},
    e2e::{App, InputBackend, Setup, XServer},
    sabotage::{Fault, Sabotage},
};
use std::{fs, path::Path, time::Duration};

fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}
fn query(ui: &mut Ui<'_>, text: &str, count: usize) -> Outcome {
    ui.click("doc.filter")?;
    ui.press("ctrl-a")?;
    ui.type_text(text)?;
    let typed = ui.expect_text("doc.filter", text);
    if typed.is_err() {
        ui.checkpoint("query-failed")?;
    }
    typed?;
    ui.wait_for("completed file matching", |s| {
        s.find("doc.summary")
            .is_some_and(|n| n.label.contains("files |"))
    })?;
    ui.expect_count("doc.row.", count)?;
    Ok(())
}
fn start(ui: &mut Ui<'_>) -> Outcome {
    ui.wait_for("active window", |snapshot| snapshot.window_active)?;
    let mark = ui.trace_mark()?;
    ui.press("ctrl-p")?;
    let opened = ui.expect_focused("doc.filter");
    if opened.is_err() {
        ui.checkpoint("startup-failed")?;
    }
    opened?;
    ui.expect_trace(mark, &[("command.executed", "finder.find")])?;
    query(ui, "reader", 2)
}
fn pick(ui: &mut Ui<'_>) -> Outcome {
    ui.click("doc.row.1")?;
    ui.expect_selected("doc.row.1", true)
}

#[test]
#[ignore = "requires a native display; run explicitly through the full local gate"]
fn file_finder_through_operating_system_input() {
    run().unwrap();
}

fn run() -> Outcome {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let evidence = root.join("target/file-finder-e2e");
    fs::create_dir_all(&evidence).unwrap();
    fs::write(
        evidence.join("report.json"),
        r#"{"passed": false, "state": "running"}"#,
    )
    .unwrap();
    let fixtures = tempfile::tempdir().unwrap();
    write(fixtures.path(), "alpha/src/reader.rs", "alpha source\n");
    write(fixtures.path(), "beta/src/reader.rs", "beta source\n");
    write(
        fixtures.path(),
        "alpha/generated/schema.rs",
        "generated source\n",
    );
    write(fixtures.path(), "alpha/ignored.rs", "ignored source\n");
    write(fixtures.path(), "alpha/.gitignore", "ignored.rs\n");
    let sockets = tempfile::tempdir().unwrap();
    let empty = tempfile::tempdir().unwrap();
    let server = XServer::start(&evidence)?;
    let setup = Setup {
        demo: env!("CARGO_BIN_EXE_retrowork-file-finder-prototype").into(),
        packages: empty.path().into(),
        input: InputBackend::XTest,
        sockets: sockets.path().into(),
        fixtures: fixtures.path().into(),
    };
    let main = evidence.join("workflow");
    let mut app = App::launch(&server, &setup, empty.path(), &main, "app")?;
    let renderer = app.renderer();
    if let Err(error) = workflow(&mut app, &main) {
        fs::write(
            main.join("failure.snapshot.json"),
            serde_json::to_vec_pretty(&app.snapshot()?).unwrap(),
        )
        .unwrap();
        fs::write(
            main.join("failure.trace.json"),
            serde_json::to_vec_pretty(&app.trace_since(0)?).unwrap(),
        )
        .unwrap();
        let (width, height, pixels) = app.capture_pixels()?;
        image::save_buffer(
            main.join("failure-unmatched.png"),
            &pixels,
            width.into(),
            height.into(),
            image::ColorType::Rgba8,
        )
        .unwrap();
        return Err(error);
    }
    if !app.wait_for_exit(Duration::from_secs(10))?.success() {
        return Err("prototype failed to exit".into());
    }
    drop(app);
    let mut controls = vec![];
    for (name, fault) in [
        ("dropped-keyboard", Fault::DropKeyboard),
        ("dropped-pointer", Fault::DropPointer),
        (
            "misdirected-pointer",
            Fault::MisaimPointer { dx: 0., dy: -24. },
        ),
    ] {
        let path = evidence.join(name);
        let mut app = App::launch(&server, &setup, empty.path(), &path, "app")?;
        if fault != Fault::DropKeyboard {
            start(&mut Ui::new(&mut app))?;
        }
        let failure = {
            let mut broken = Sabotage::new(&mut app, fault, Duration::from_secs(1));
            let mut ui = Ui::new(&mut broken);
            if fault == Fault::DropKeyboard {
                start(&mut ui)
            } else {
                pick(&mut ui)
            }
        }
        .err()
        .ok_or_else(|| format!("{name} was not detected"))?;
        app.checkpoint("detected")?;
        controls.push(serde_json::json!({"name": name, "detected": true, "failure": failure}));
        app.key("ctrl-q")?;
        app.wait_for_exit(Duration::from_secs(10))?;
    }
    fs::write(evidence.join("report.json"), serde_json::to_vec_pretty(&serde_json::json!({
        "passed": true, "input": "xtest", "renderer": renderer,
        "display_kind": server.kind, "display": server.display,
        "workflow": "matching, selection, opening, generated sources, updates and project closure",
        "negative_controls": controls, "performance_measured": false
    })).unwrap()).unwrap();
    Ok(())
}

fn workflow(app: &mut App<'_>, main: &Path) -> Outcome {
    let mut ui = Ui::new(app);
    start(&mut ui)?;
    pick(&mut ui)?;
    ui.press("alt-up")?;
    ui.expect_selected("doc.row.0", true)?;
    ui.press("alt-down")?;
    ui.expect_selected("doc.row.1", true)?;
    ui.checkpoint("01-project-context-and-selection")?;
    query(&mut ui, "reader beta", 1)?;
    ui.press("enter")?;
    ui.expect_text_contains("doc.source", "beta source")?;
    ui.checkpoint("02-keyboard-open")?;
    ui.press("ctrl-p")?;
    query(&mut ui, "ignored", 0)?;
    query(&mut ui, "generated", 0)?;
    ui.press("ctrl-shift-g")?;
    ui.expect_count("doc.row.", 1)?;
    ui.expect_text_contains("doc.row.0", "Generated")?;
    ui.checkpoint("03-generated-inclusion")?;
    write(&main.join("fixtures"), "alpha/src/created.rs", "new file\n");
    query(&mut ui, "created", 1)?;
    ui.expect_text_contains("doc.row.0", "created.rs")?;
    fs::rename(
        main.join("fixtures/alpha/src/created.rs"),
        main.join("fixtures/alpha/src/renamed.rs"),
    )
    .unwrap();
    query(&mut ui, "renamed", 1)?;
    ui.expect_text_contains("doc.row.0", "renamed.rs")?;
    fs::remove_file(main.join("fixtures/alpha/src/renamed.rs")).unwrap();
    ui.expect_count("doc.row.", 0)?;
    ui.press("ctrl-shift-r")?;
    query(&mut ui, "reader", 2)?;
    ui.press("ctrl-shift-w")?;
    ui.expect_count("doc.row.", 1)?;
    ui.expect_text_contains("doc.row.0", "alpha")?;
    ui.checkpoint("04-project-closed")?;
    ui.press("ctrl-shift-w")?;
    ui.expect_count("doc.row.", 0)?;
    ui.checkpoint("05-before-empty-status")?;
    ui.expect_text_contains("doc.summary", "0 projects")?;
    ui.checkpoint("05-all-projects-closed")?;
    ui.press("ctrl-q")?;
    Ok(())
}
