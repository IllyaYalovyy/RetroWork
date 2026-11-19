//! The Log Explorer and Log Statistics modules through the platform's public
//! API: what a front end would see and send, without any UI toolkit.

use std::{fs, path::Path};

use retrowork_log_stats::{COMMAND_SUMMARIZE, LogStatsModule, VIEW_STATISTICS};
use retrowork_logs::{
    COMMAND_FILTER_CLEAR, COMMAND_FILTER_FOCUS, COMMAND_FILTER_SET, COMMAND_RELOAD, LogWorkspace,
    LogsModule, POINT_SOURCES, VIEW_RECORD, VIEW_SOURCES,
};
use retrowork_platform::{
    ActivationContext, ActivationError, CommandError, CommandId, Dependency, Extension,
    FocusTarget, Module, ModuleId, ModuleManifest, ModuleState, Platform, RejectReason, TableModel,
    ViewContent, ViewEvent, ViewId, ViewLocation, ViewTarget,
};
use semver::Version;
use serde_json::{Value, json};

const LOG: &str = "\
t1 INFO web.http: GET / 200
t2 ERROR web.db: timeout
t3 WARN web.http: slow GET /report
t4 ERROR web.http: GET /report 500
not a structured line
";

/// Contributes log sources the way a declarative package does.
struct Sources {
    id: &'static str,
    base: std::path::PathBuf,
    sources: Vec<Value>,
}

impl Module for Sources {
    fn manifest(&self) -> ModuleManifest {
        let mut manifest = ModuleManifest::bundled(self.id, self.id, Version::new(1, 0, 0))
            .depends_on(Dependency::literal(retrowork_logs::MODULE_ID, "^0.1"));
        manifest.extensions = self
            .sources
            .iter()
            .map(|data| Extension {
                point: POINT_SOURCES.to_owned(),
                base_dir: Some(self.base.clone()),
                data: data.clone(),
            })
            .collect();
        manifest
    }
    fn activate(&self, _: &mut ActivationContext<'_>) -> Result<(), ActivationError> {
        Ok(())
    }
}

fn sources(id: &'static str, base: &Path, sources: Vec<Value>) -> Sources {
    Sources {
        id,
        base: base.to_owned(),
        sources,
    }
}

fn web_source() -> Value {
    json!({ "id": "web", "title": "web.log", "group": "apps", "path": "web.log" })
}

fn platform(directory: &Path) -> Platform {
    fs::write(directory.join("web.log"), LOG).unwrap();
    fs::write(directory.join("empty.log"), "").unwrap();
    let platform = Platform::builder()
        .module(LogsModule)
        .module(LogStatsModule)
        .module(sources(
            "fixtures",
            directory,
            vec![
                web_source(),
                json!({ "id": "empty", "title": "empty.log", "group": "apps", "path": "empty.log" }),
            ],
        ))
        .build();
    for module in platform.modules() {
        assert_eq!(module.state, ModuleState::Active, "{}", module.manifest.id);
    }
    platform
}

fn command(id: &str) -> CommandId {
    CommandId::new(id).unwrap()
}

fn select_source(platform: &mut Platform, id: &str) {
    platform
        .view_event(
            &ViewTarget::View(ViewId::literal(VIEW_SOURCES)),
            ViewEvent::TreeSelect(format!("source:{id}")),
        )
        .unwrap();
}

fn table(platform: &Platform) -> TableModel {
    let index = platform
        .shell()
        .active_document_index()
        .expect("a document is open");
    match platform.document_content(platform.shell().documents()[index].id) {
        Some(ViewContent::Table(table)) => table,
        other => panic!("the active document is not a table: {other:?}"),
    }
}

fn messages(platform: &Platform) -> Vec<String> {
    table(platform)
        .rows
        .iter()
        .map(|row| row.cells[4].clone())
        .collect()
}

fn properties(platform: &Platform, view: &str) -> Vec<(String, String)> {
    match platform.view_content(&ViewId::new(view).unwrap()) {
        Some(ViewContent::Properties(model)) => model.rows,
        other => panic!("view {view} is not a property list: {other:?}"),
    }
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

fn set_filter(platform: &mut Platform, query: &str) -> Result<(), CommandError> {
    platform.execute(
        &command(COMMAND_FILTER_SET),
        Some(&json!({ "query": query })),
    )
}

#[test]
fn selecting_a_source_opens_it_once_and_shows_every_line() {
    let directory = tempfile::tempdir().unwrap();
    let mut platform = platform(directory.path());

    let Some(ViewContent::Tree(tree)) = platform.view_content(&ViewId::literal(VIEW_SOURCES))
    else {
        panic!("Sources is a tree");
    };
    let listed: Vec<(usize, &str, &str)> = tree
        .visible()
        .into_iter()
        .map(|(depth, node)| (depth, node.id.as_str(), node.label.as_str()))
        .collect();
    assert_eq!(
        listed,
        [
            (0, "group:apps", "apps"),
            (1, "source:web", "web.log"),
            (1, "source:empty", "empty.log"),
        ]
    );

    select_source(&mut platform, "web");
    select_source(&mut platform, "web");
    assert_eq!(platform.shell().documents().len(), 1);
    assert_eq!(platform.shell().status(), "Opened web.log (5 records)");
    let table = table(&platform);
    assert_eq!(table.summary, "5 records");
    assert_eq!(table.rows.len(), 5);
    assert_eq!(
        table.rows[1].cells,
        ["2", "t2", "ERROR", "web.db", "timeout"]
    );
    // A line that is not in the expected format is shown, not dropped.
    assert_eq!(
        table.rows[4].cells,
        ["5", "", "?", "", "not a structured line"]
    );

    select_source(&mut platform, "empty");
    assert_eq!(table_summary(&platform), "0 records");

    // Collapsing the folder hides its sources.
    platform
        .view_event(
            &ViewTarget::View(ViewId::literal(VIEW_SOURCES)),
            ViewEvent::TreeToggle("group:apps".into()),
        )
        .unwrap();
    let Some(ViewContent::Tree(tree)) = platform.view_content(&ViewId::literal(VIEW_SOURCES))
    else {
        panic!("Sources is a tree");
    };
    assert_eq!(tree.visible().len(), 1);
    assert_eq!(tree.selected.as_deref(), Some("source:empty"));
}

fn table_summary(platform: &Platform) -> String {
    table(platform).summary
}

#[test]
fn invalid_source_contributions_reject_the_contributor_and_add_nothing() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("web.log"), LOG).unwrap();
    let cases: Vec<(Vec<Value>, &str)> = vec![
        (
            vec![json!({ "id": "x", "title": "x", "group": "g", "path": "nope.log" })],
            "does not exist",
        ),
        (
            vec![json!({ "id": "Bad Id", "title": "x", "group": "g", "path": "web.log" })],
            "may only use",
        ),
        (
            vec![json!({ "id": "x", "group": "g", "path": "web.log" })],
            "`title` must be a non-empty string",
        ),
        (
            vec![
                json!({ "id": "x", "title": "x", "group": "g", "path": "web.log", "colour": "red" }),
            ],
            "unknown key `colour`",
        ),
        // The first entry is fine; the package still loads nothing.
        (vec![web_source(), web_source()], "already registered"),
    ];
    for (entries, expected) in cases {
        let platform = Platform::builder()
            .module(LogsModule)
            .module(sources("bad", directory.path(), entries))
            .build();
        let ModuleState::Rejected(reason) = &platform.modules()[2].state else {
            panic!("expected rejection for {expected}");
        };
        assert!(reason.to_string().contains(expected), "{reason}");
        let workspace = platform.services().get::<LogWorkspace>().unwrap();
        assert!(workspace.sources().is_empty(), "{expected}");
    }

    // Two packages may not claim the same source id either.
    let platform = Platform::builder()
        .module(LogsModule)
        .module(sources("first", directory.path(), vec![web_source()]))
        .module(sources("second", directory.path(), vec![web_source()]))
        .build();
    assert_eq!(platform.modules()[1].state, ModuleState::Active);
    assert!(matches!(
        &platform.modules()[3].state,
        ModuleState::Rejected(RejectReason::ActivationFailed(message)) if message.contains("already registered")
    ));
    let workspace = platform.services().get::<LogWorkspace>().unwrap();
    assert_eq!(workspace.sources().len(), 1);
    assert_eq!(workspace.sources()[0].owner, ModuleId::literal("first"));
}

#[test]
fn filter_narrows_rows_keeps_a_surviving_selection_and_drives_the_record_view() {
    let directory = tempfile::tempdir().unwrap();
    let mut platform = platform(directory.path());
    select_source(&mut platform, "web");
    let document = ViewTarget::Document(platform.shell().documents()[0].id);

    assert!(properties(&platform, VIEW_RECORD).is_empty());
    platform
        .view_event(&document, ViewEvent::RowSelect(3))
        .unwrap();
    assert_eq!(
        platform
            .shell()
            .active_view(ViewLocation::Bottom)
            .unwrap()
            .as_str(),
        VIEW_RECORD,
        "selecting a record brings its detail forward"
    );
    assert_eq!(
        properties(&platform, VIEW_RECORD)[4],
        ("Message".to_owned(), "GET /report 500".to_owned())
    );
    // An out-of-range row is ignored.
    platform
        .view_event(&document, ViewEvent::RowSelect(99))
        .unwrap();
    assert_eq!(table(&platform).selected, Some(3));

    platform
        .view_event(&document, ViewEvent::FilterChanged("level:error".into()))
        .unwrap();
    assert_eq!(messages(&platform), ["timeout", "GET /report 500"]);
    assert_eq!(
        table(&platform).selected,
        Some(1),
        "the same record, at its new row"
    );
    assert_eq!(platform.shell().status(), "2 of 5 records");

    platform
        .view_event(&document, ViewEvent::FilterChanged("level:warn".into()))
        .unwrap();
    assert_eq!(messages(&platform), ["slow GET /report"]);
    assert_eq!(
        table(&platform).selected,
        None,
        "a filtered-out record is deselected"
    );
    assert!(properties(&platform, VIEW_RECORD).is_empty());

    platform
        .execute(&command(COMMAND_FILTER_CLEAR), None)
        .unwrap();
    assert_eq!(table(&platform).filter.unwrap().text, "");
    assert_eq!(table_summary(&platform), "5 records");

    set_filter(&mut platform, "level:nope").unwrap();
    assert!(messages(&platform).is_empty());
    assert!(table_summary(&platform).starts_with("Filter problem: unknown level `nope`"));
}

#[test]
fn commands_need_an_open_log_and_valid_arguments() {
    let directory = tempfile::tempdir().unwrap();
    let mut platform = platform(directory.path());
    let no_log = CommandError::Failed("no log is open; select one in Sources first".into());
    for id in [
        COMMAND_FILTER_FOCUS,
        COMMAND_FILTER_CLEAR,
        COMMAND_RELOAD,
        COMMAND_SUMMARIZE,
    ] {
        assert_eq!(
            platform.execute(&command(id), None),
            Err(no_log.clone()),
            "{id}"
        );
    }
    assert_eq!(set_filter(&mut platform, "x"), Err(no_log));

    select_source(&mut platform, "web");
    assert!(matches!(
        platform.execute(&command(COMMAND_FILTER_SET), None),
        Err(CommandError::InvalidArguments(_))
    ));
    assert_eq!(
        table_summary(&platform),
        "5 records",
        "a rejected command changes nothing"
    );

    assert_eq!(platform.shell_mut().take_focus_request(), None);
    platform
        .execute(&command(COMMAND_FILTER_FOCUS), None)
        .unwrap();
    assert_eq!(
        platform.shell_mut().take_focus_request(),
        Some(FocusTarget::DocumentFilter)
    );
}

#[test]
fn reload_shows_what_is_on_disk_now_and_a_vanished_file_is_reported() {
    let directory = tempfile::tempdir().unwrap();
    let mut platform = platform(directory.path());
    select_source(&mut platform, "web");
    set_filter(&mut platform, "level:error").unwrap();
    assert_eq!(messages(&platform).len(), 2);

    let path = directory.path().join("web.log");
    fs::write(&path, format!("{LOG}t6 ERROR web.db: deadlock\n")).unwrap();
    assert_eq!(messages(&platform).len(), 2, "nothing changes until reload");
    platform.execute(&command(COMMAND_RELOAD), None).unwrap();
    assert_eq!(
        messages(&platform),
        ["timeout", "GET /report 500", "deadlock"]
    );
    assert_eq!(
        table_summary(&platform),
        "3 of 6 records",
        "the filter stays"
    );
    assert_eq!(
        output(&platform).last().unwrap(),
        "Reloaded web.log (6 records)"
    );

    // Bytes that are not UTF-8 do not make the log unreadable.
    fs::write(&path, b"t1 INFO a.b: caf\xe9\n").unwrap();
    set_filter(&mut platform, "").unwrap();
    platform.execute(&command(COMMAND_RELOAD), None).unwrap();
    assert_eq!(messages(&platform), ["caf\u{fffd}"]);

    fs::remove_file(&path).unwrap();
    let error = platform
        .execute(&command(COMMAND_RELOAD), None)
        .unwrap_err();
    assert!(
        error.to_string().starts_with("cannot read web.log ("),
        "{error}"
    );
    assert_eq!(
        messages(&platform).len(),
        1,
        "the loaded records stay on screen"
    );
    assert!(platform.shell().status().contains("cannot read web.log"));
}

#[test]
fn filter_is_persisted_per_document_and_export_matches_what_is_shown() {
    let directory = tempfile::tempdir().unwrap();
    let mut platform = platform(directory.path());
    select_source(&mut platform, "web");
    set_filter(&mut platform, "component:http").unwrap();

    let provider = &platform.shell().documents()[0].provider;
    assert_eq!(
        provider.persisted_state(),
        Some(json!({ "filter": "component:http" }))
    );

    let export = provider.export(2).unwrap();
    assert_eq!(export["total"], 5);
    assert_eq!(export["matching"], 3);
    assert_eq!(export["truncated"], true);
    assert_eq!(export["records"].as_array().unwrap().len(), 2);
    assert_eq!(
        export["records"][1],
        json!({
            "line": 3, "timestamp": "t3", "level": "WARN",
            "component": "web.http", "message": "slow GET /report",
        })
    );
    assert_eq!(provider.export(100).unwrap()["truncated"], false);

    // State from another version is tolerated.
    provider.restore_state(&json!({ "filter": 7, "unknown": true }));
    assert_eq!(table(&platform).filter.unwrap().text, "component:http");
    provider.restore_state(&json!({ "filter": "level:warn" }));
    assert_eq!(messages(&platform), ["slow GET /report"]);
}

#[test]
fn statistics_follow_the_active_log_and_summarize_reports_what_is_shown() {
    let directory = tempfile::tempdir().unwrap();
    let mut platform = platform(directory.path());
    assert!(properties(&platform, VIEW_STATISTICS).is_empty());

    select_source(&mut platform, "web");
    let rows = |platform: &Platform| -> Vec<String> {
        properties(platform, VIEW_STATISTICS)
            .into_iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect()
    };
    assert_eq!(
        rows(&platform),
        [
            "Log=web.log",
            "Shown=5 of 5",
            "ERROR=2",
            "WARN=1",
            "INFO=1",
            "?=1",
            "Top 1=web.http (3)",
            "Top 2=web.db (1)",
        ]
    );
    set_filter(&mut platform, "level:error").unwrap();
    assert_eq!(rows(&platform)[1..3], ["Shown=2 of 5", "ERROR=2"]);

    platform.shell().output().clear();
    assert!(platform.activate_view(&ViewId::literal(VIEW_STATISTICS)));
    platform.execute(&command(COMMAND_SUMMARIZE), None).unwrap();
    assert_eq!(
        output(&platform),
        [
            "Summary of web.log: 2 of 5 records shown",
            "  Levels: ERROR 2",
            "  web.db: 1",
            "  web.http: 1",
        ]
    );
    assert_eq!(platform.shell().status(), "Summarized 2 records of web.log");
    assert_eq!(
        platform
            .shell()
            .active_view(ViewLocation::Bottom)
            .unwrap()
            .as_str(),
        "output"
    );

    select_source(&mut platform, "empty");
    assert_eq!(rows(&platform), ["Log=empty.log", "Shown=0 of 0"]);
    platform.execute(&command(COMMAND_SUMMARIZE), None).unwrap();
    assert_eq!(output(&platform).last().unwrap(), "  Levels: no records");
}

#[test]
fn statistics_module_is_rejected_when_the_log_explorer_is_absent() {
    let platform = Platform::builder().module(LogStatsModule).build();
    assert_eq!(
        platform.modules()[1].state,
        ModuleState::Rejected(RejectReason::MissingDependency(ModuleId::literal(
            retrowork_logs::MODULE_ID
        )))
    );
    assert!(
        platform
            .contributions()
            .command(&command(COMMAND_SUMMARIZE))
            .is_none()
    );
    assert!(platform.contributions().toolbar().is_empty());
}
