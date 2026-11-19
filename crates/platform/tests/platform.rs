//! Contract tests of the platform through its public API only, the way a
//! module author and a front end use it.

use std::{cell::RefCell, fs, rc::Rc, time::Duration};

use retrowork_platform::{
    ActivationContext, ActivationError, CommandContext, CommandError, CommandId, Dependency,
    DocumentFactory, DocumentKind, Extension, MenuId, Module, ModuleId, ModuleManifest,
    ModuleState, OpenedDocument, Platform, RejectReason, Shell, StateStore, TextModel, ViewContent,
    ViewEvent, ViewId, ViewLocation, ViewProvider, ViewTarget,
    core::{
        COMMAND_CLEAR_OUTPUT, COMMAND_CLOSE_DOCUMENT, COMMAND_EXIT, COMMAND_LIST_MODULES,
        MENU_TOOLS, VIEW_OUTPUT,
    },
};
use semver::Version;
use serde_json::{Value, json};

type Activate = Box<dyn Fn(&mut ActivationContext<'_>) -> Result<(), ActivationError>>;

struct TestModule {
    manifest: ModuleManifest,
    activate: Activate,
}

fn module(
    id: &'static str,
    dependencies: &[&'static str],
    activate: impl Fn(&mut ActivationContext<'_>) -> Result<(), ActivationError> + 'static,
) -> TestModule {
    let mut manifest = ModuleManifest::bundled(id, id, Version::new(1, 0, 0));
    for dependency in dependencies {
        manifest = manifest.depends_on(Dependency::literal(dependency, "^1"));
    }
    TestModule {
        manifest,
        activate: Box::new(activate),
    }
}

impl Module for TestModule {
    fn manifest(&self) -> ModuleManifest {
        self.manifest.clone()
    }
    fn activate(&self, cx: &mut ActivationContext<'_>) -> Result<(), ActivationError> {
        (self.activate)(cx)
    }
}

fn command(id: &'static str) -> CommandId {
    CommandId::literal(id)
}

fn tools() -> MenuId {
    MenuId::literal(MENU_TOOLS)
}

fn state<'a>(platform: &'a Platform, id: &str) -> &'a ModuleState {
    &platform
        .modules()
        .iter()
        .find(|report| report.manifest.id.as_str() == id)
        .unwrap_or_else(|| panic!("module {id} is not registered"))
        .state
}

fn rejection(platform: &Platform, id: &str) -> String {
    match state(platform, id) {
        ModuleState::Rejected(reason) => reason.to_string(),
        ModuleState::Active => panic!("module {id} is active"),
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

fn trace_kinds(platform: &Platform, after: u64) -> Vec<String> {
    platform
        .shell()
        .trace()
        .since(after)
        .into_iter()
        .map(|event| format!("{} {}", event.kind, event.detail))
        .collect()
}

struct Blank;
impl ViewProvider for Blank {
    fn content(&self, _: &Shell) -> ViewContent {
        ViewContent::Text(TextModel::default())
    }
}

#[test]
fn failed_activation_leaves_nothing_behind_and_takes_dependents_with_it() {
    let platform = Platform::builder()
        .module(module("broken", &[], |cx| {
            cx.command(command("broken.run"), "Run", |_| Ok(()))?;
            cx.menu_item(tools(), "run", "Run", command("broken.run"), None, 0)?;
            cx.key_binding("ctrl-b", command("broken.run"), None)?;
            cx.view(
                ViewId::literal("broken.view"),
                "Broken",
                ViewLocation::Left,
                0,
                Rc::new(Blank),
            )?;
            cx.provide(Rc::new(7_u32))?;
            Err(ActivationError::new("license check failed"))
        }))
        .module(module("uses-broken", &["broken"], |cx| {
            cx.command(command("uses-broken.run"), "Run", |_| Ok(()))
        }))
        .module(module("independent", &[], |cx| {
            cx.command(command("independent.run"), "Run", |_| Ok(()))
        }))
        .build();

    assert_eq!(
        state(&platform, "broken"),
        &ModuleState::Rejected(RejectReason::ActivationFailed(
            "license check failed".into()
        ))
    );
    assert_eq!(
        state(&platform, "uses-broken"),
        &ModuleState::Rejected(RejectReason::DependencyUnavailable(ModuleId::literal(
            "broken"
        )))
    );
    assert_eq!(state(&platform, "independent"), &ModuleState::Active);

    let contributions = platform.contributions();
    assert!(contributions.command(&command("broken.run")).is_none());
    assert!(contributions.command(&command("uses-broken.run")).is_none());
    assert!(contributions.command(&command("independent.run")).is_some());
    assert!(contributions.menu_items(&tools()).is_empty());
    assert!(contributions.views(ViewLocation::Left).is_empty());
    assert!(
        contributions
            .key_bindings()
            .iter()
            .all(|binding| binding.chord.as_str() != "ctrl-b")
    );
    assert!(platform.services().get::<u32>().is_none());

    // The user is told, in the Output view, why features are missing.
    assert_eq!(
        output(&platform),
        [
            "Module `broken` was not loaded: activation failed: license check failed.",
            "Module `uses-broken` was not loaded: depends on `broken`, which was rejected or failed.",
        ]
    );
}

#[test]
fn contributions_may_only_refer_to_commands_of_declared_dependencies() {
    let platform = Platform::builder()
        .module(module("owner", &[], |cx| {
            cx.command(command("owner.run"), "Run", |_| Ok(()))
        }))
        .module(module("declared", &["owner"], |cx| {
            cx.menu_item(tools(), "run", "Run", command("owner.run"), None, 0)
        }))
        .module(module("undeclared", &[], |cx| {
            cx.menu_item(tools(), "sneaky", "Run", command("owner.run"), None, 0)
        }))
        .module(module("typo", &[], |cx| {
            cx.toolbar_item("typo", "Run", command("owner.rnu"), None, 0)
        }))
        .module(module("no-such-menu", &[], |cx| {
            cx.command(command("no-such-menu.run"), "Run", |_| Ok(()))?;
            cx.menu_item(
                MenuId::literal("nowhere"),
                "run",
                "Run",
                command("no-such-menu.run"),
                None,
                0,
            )
        }))
        .module(module("uses-core", &[], |cx| {
            // Core commands need no declared dependency.
            cx.toolbar_item("exit", "Exit", command(COMMAND_EXIT), None, 0)
        }))
        .build();

    assert_eq!(state(&platform, "declared"), &ModuleState::Active);
    assert_eq!(state(&platform, "uses-core"), &ModuleState::Active);
    assert!(rejection(&platform, "undeclared").contains("not a declared dependency"));
    assert!(rejection(&platform, "typo").contains("unknown command `owner.rnu`"));
    assert!(rejection(&platform, "no-such-menu").contains("unknown menu `nowhere`"));
    let items: Vec<&str> = platform
        .contributions()
        .menu_items(&tools())
        .iter()
        .map(|item| item.id.as_str())
        .collect();
    assert_eq!(items, ["run"]);
}

#[test]
fn conflicting_ids_and_keys_reject_the_module_activated_later() {
    let platform = Platform::builder()
        // Registered first, but `a-first` is activated first: order is by id.
        .module(module("b-second", &[], |cx| {
            cx.command(command("shared.run"), "Run", |_| Ok(()))
        }))
        .module(module("a-first", &[], |cx| {
            cx.command(command("shared.run"), "Run", |_| Ok(()))?;
            cx.key_binding("ctrl-shift-k", command("shared.run"), None)
        }))
        .module(module("c-key", &[], |cx| {
            cx.command(command("c-key.run"), "Run", |_| Ok(()))?;
            cx.key_binding("shift-ctrl-k", command("c-key.run"), None)
        }))
        .module(module("d-bad-key", &[], |cx| {
            cx.command(command("d-bad-key.run"), "Run", |_| Ok(()))?;
            cx.key_binding("a", command("d-bad-key.run"), None)
        }))
        .build();
    assert_eq!(state(&platform, "a-first"), &ModuleState::Active);
    assert!(rejection(&platform, "b-second").contains("already registered by `a-first`"));
    assert!(
        rejection(&platform, "c-key").contains("`ctrl-shift-k` is already bound to `shared.run`")
    );
    assert!(rejection(&platform, "d-bad-key").contains("would intercept ordinary typing"));
}

fn with_extensions(mut module: TestModule, values: &[Value]) -> TestModule {
    module.manifest.extensions = values
        .iter()
        .map(|data| Extension {
            point: "words".to_owned(),
            base_dir: None,
            data: data.clone(),
        })
        .collect();
    module
}

#[test]
fn extensions_of_a_module_are_applied_together_or_not_at_all() {
    let words = Rc::new(RefCell::new(Vec::<String>::new()));
    let owner = {
        let words = Rc::clone(&words);
        module("owner", &[], move |cx| {
            let words = Rc::clone(&words);
            cx.extension_point("words", move |extension| {
                let word = extension
                    .data
                    .as_str()
                    .ok_or_else(|| ActivationError::new("a word must be a string"))?
                    .to_owned();
                let words = Rc::clone(&words);
                let owner = extension.owner.clone();
                Ok(Box::new(move || {
                    words.borrow_mut().push(format!("{owner}:{word}"));
                }))
            })
        })
    };
    let nothing = |_: &mut ActivationContext<'_>| Ok(());
    let platform = Platform::builder()
        .module(owner)
        .module(with_extensions(
            module("good", &["owner"], nothing),
            &[json!("one"), json!("two")],
        ))
        .module(with_extensions(
            module("half-bad", &["owner"], nothing),
            &[json!("fine"), json!(42)],
        ))
        .module(with_extensions(
            module("undeclared", &[], nothing),
            &[json!("sneaky")],
        ))
        .build();

    assert_eq!(*words.borrow(), ["good:one", "good:two"]);
    assert!(rejection(&platform, "half-bad").contains("a word must be a string"));
    assert!(rejection(&platform, "undeclared").contains("not a declared dependency"));

    let unknown_point = Platform::builder()
        .module(with_extensions(module("lost", &[], nothing), &[json!("x")]))
        .build();
    assert!(rejection(&unknown_point, "lost").contains("unknown extension point `words`"));
}

#[test]
fn command_failure_is_visible_and_does_not_leak_into_the_next_action() {
    let mut platform = Platform::builder()
        .module(module("m", &[], |cx| {
            cx.command(command("m.fail"), "Fail", |cx| {
                // Queued work of a failed command must be discarded.
                cx.shell.run_command(command("m.mark"), None);
                Err(CommandError::failed("disk on fire"))
            })?;
            cx.command(command("m.mark"), "Mark", |cx| {
                cx.shell.output().append("marked", Default::default());
                Ok(())
            })?;
            cx.command(command("m.ok"), "Ok", |cx| {
                cx.shell.set_status("fine");
                Ok(())
            })?;
            cx.command(command("m.loop"), "Loop", |cx| {
                cx.shell.run_command(command("m.loop"), None);
                Ok(())
            })?;
            cx.command(command("m.args"), "Args", |cx| {
                let name = cx.string_arg("name")?.to_owned();
                cx.shell.set_status(format!("hello {name}"));
                Ok(())
            })
        }))
        .build();
    let before = platform.shell().trace().last_seq();

    assert_eq!(
        platform.execute(&command("m.fail"), None),
        Err(CommandError::Failed("disk on fire".into()))
    );
    let message = "Command `m.fail` failed: disk on fire";
    assert_eq!(platform.shell().status(), message);
    assert_eq!(output(&platform), [message]);
    assert_eq!(
        trace_kinds(&platform, before),
        [format!("command.failed {message}")]
    );

    platform.execute(&command("m.ok"), None).unwrap();
    assert_eq!(platform.shell().status(), "fine");
    assert_eq!(output(&platform), [message], "`m.mark` must not have run");

    assert_eq!(
        platform.execute(&command("m.loop"), None),
        Err(CommandError::RunawayChain(32))
    );
    assert_eq!(
        platform.execute(&command("m.nope"), None),
        Err(CommandError::UnknownCommand(command("m.nope")))
    );
    assert!(matches!(
        platform.execute(&command("m.args"), Some(&json!({ "nome": "x" }))),
        Err(CommandError::InvalidArguments(_))
    ));
    platform
        .execute(&command("m.args"), Some(&json!({ "name": "world" })))
        .unwrap();
    assert_eq!(platform.shell().status(), "hello world");
}

/// A document whose only state is a note; resource `missing` cannot be opened.
struct Note(RefCell<String>);

impl ViewProvider for Note {
    fn content(&self, _: &Shell) -> ViewContent {
        ViewContent::Text(TextModel::default())
    }
    fn handle(&self, event: ViewEvent, _: &mut CommandContext<'_>) -> Result<(), CommandError> {
        if let ViewEvent::FilterChanged(text) = event {
            *self.0.borrow_mut() = text;
        }
        Ok(())
    }
    fn persisted_state(&self) -> Option<Value> {
        Some(json!({ "note": *self.0.borrow() }))
    }
    fn restore_state(&self, state: &Value) {
        if let Some(note) = state.get("note").and_then(Value::as_str) {
            *self.0.borrow_mut() = note.to_owned();
        }
    }
}

struct NoteFactory;
impl DocumentFactory for NoteFactory {
    fn open(
        &self,
        resource: &str,
        _: &mut CommandContext<'_>,
    ) -> Result<OpenedDocument, CommandError> {
        if resource == "missing" {
            return Err(CommandError::failed("file is gone"));
        }
        Ok(OpenedDocument {
            title: resource.to_uppercase(),
            provider: Rc::new(Note(RefCell::default())),
        })
    }
}

fn notes_platform(store: Option<StateStore>) -> Platform {
    let mut builder = Platform::builder().module(module("notes", &[], |cx| {
        cx.document_factory(DocumentKind::literal("notes.note"), Rc::new(NoteFactory))?;
        cx.view(
            ViewId::literal("notes.extra"),
            "Extra",
            ViewLocation::Bottom,
            50,
            Rc::new(Blank),
        )?;
        cx.command(command("notes.open"), "Open", |cx| {
            let resource = cx.string_arg("resource")?.to_owned();
            cx.shell
                .open_document(DocumentKind::literal("notes.note"), resource);
            Ok(())
        })
    }));
    if let Some(store) = store {
        builder = builder.state_store(store);
    }
    builder.build()
}

fn open(platform: &mut Platform, resource: &str) -> Result<(), CommandError> {
    platform.execute(
        &command("notes.open"),
        Some(&json!({ "resource": resource })),
    )
}

fn open_documents(platform: &Platform) -> Vec<(String, String)> {
    platform
        .shell()
        .documents()
        .iter()
        .map(|document| {
            let note = document.provider.persisted_state().unwrap()["note"]
                .as_str()
                .unwrap()
                .to_owned();
            (document.resource.clone(), note)
        })
        .collect()
}

#[test]
fn documents_open_once_and_core_commands_act_on_them() {
    let mut platform = notes_platform(None);
    open(&mut platform, "a").unwrap();
    open(&mut platform, "b").unwrap();
    open(&mut platform, "a").unwrap();
    assert_eq!(platform.shell().documents().len(), 2, "`a` is opened once");
    assert_eq!(platform.shell().active_document().unwrap().title, "A");

    assert_eq!(
        open(&mut platform, "missing"),
        Err(CommandError::Failed("file is gone".into()))
    );
    assert_eq!(platform.shell().documents().len(), 2);

    platform
        .execute(&command(COMMAND_CLOSE_DOCUMENT), None)
        .unwrap();
    assert_eq!(platform.shell().active_document().unwrap().title, "B");
    assert_eq!(platform.shell().status(), "Closed A");

    platform
        .execute(&command(COMMAND_CLEAR_OUTPUT), None)
        .unwrap();
    assert!(output(&platform).is_empty());
    platform
        .execute(&command(COMMAND_LIST_MODULES), None)
        .unwrap();
    assert_eq!(
        output(&platform),
        [
            "Modules (2):",
            "  retrowork.core 0.1.0 [bundled] active",
            "  notes 1.0.0 [bundled] active",
        ]
    );

    assert!(!platform.shell().quit_requested());
    platform.execute(&command(COMMAND_EXIT), None).unwrap();
    assert!(platform.shell().quit_requested());
}

#[test]
fn session_is_restored_after_a_restart() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state/state.json");
    {
        let mut platform = notes_platform(Some(StateStore::new(&path)));
        assert!(!path.exists(), "nothing is written before anything changes");
        for resource in ["a", "b", "c"] {
            open(&mut platform, resource).unwrap();
        }
        platform
            .view_event(
                &ViewTarget::Document(platform.shell().documents()[1].id),
                ViewEvent::FilterChanged("remember me".into()),
            )
            .unwrap();
        platform.activate_document(platform.shell().documents()[1].id);
        assert!(platform.activate_view(&ViewId::literal("notes.extra")));
        assert!(!platform.activate_view(&ViewId::literal("no.such.view")));
    }

    // "Restart": a new platform over the same file. Nothing but the file
    // carries over.
    let mut platform = notes_platform(Some(StateStore::new(&path)));
    assert_eq!(
        open_documents(&platform),
        [
            ("a".to_owned(), String::new()),
            ("b".to_owned(), "remember me".to_owned()),
            ("c".to_owned(), String::new()),
        ]
    );
    assert_eq!(platform.shell().active_document().unwrap().resource, "b");
    assert_eq!(
        platform
            .shell()
            .active_view(ViewLocation::Bottom)
            .unwrap()
            .as_str(),
        "notes.extra"
    );
    assert_eq!(platform.shell().status(), "Restored 3 documents");

    // A document that can no longer be opened is reported and skipped; the
    // rest of the session still comes back.
    platform.close_document(platform.shell().documents()[2].id);
    platform
        .persistence()
        .unwrap()
        .flush(Duration::from_secs(5))
        .unwrap();
    let mut saved: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    saved["documents"][0]["resource"] = json!("missing");
    saved["active_views"]["bottom"] = json!("view.of.removed.module");
    fs::write(&path, serde_json::to_vec(&saved).unwrap()).unwrap();
    drop(platform);
    let platform = notes_platform(Some(StateStore::new(&path)));
    assert_eq!(
        open_documents(&platform),
        [("b".to_owned(), "remember me".to_owned())]
    );
    assert_eq!(
        output(&platform),
        ["Could not reopen notes.note `missing`: file is gone"]
    );
    assert_eq!(
        platform
            .shell()
            .active_view(ViewLocation::Bottom)
            .unwrap()
            .as_str(),
        VIEW_OUTPUT,
        "an unknown saved view falls back to the default"
    );
}

#[test]
fn unreadable_or_newer_state_is_never_destroyed() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.json");

    fs::write(&path, b"{ not json").unwrap();
    let mut platform = notes_platform(Some(StateStore::new(&path)));
    assert!(platform.shell().documents().is_empty());
    let message = &output(&platform)[0];
    assert!(message.contains("could not be read"), "{message}");
    assert_eq!(
        fs::read(directory.path().join("state.json.corrupt-1")).unwrap(),
        b"{ not json"
    );
    open(&mut platform, "a").unwrap();
    platform
        .persistence()
        .unwrap()
        .flush(Duration::from_secs(5))
        .unwrap();
    assert!(path.exists(), "the fresh session saves normally");
    drop(platform);

    let newer = br#"{"schema": 99, "documents": "from the future"}"#;
    fs::write(&path, newer).unwrap();
    let mut platform = notes_platform(Some(StateStore::new(&path)));
    assert!(output(&platform)[0].contains("newer version (schema 99)"));
    open(&mut platform, "a").unwrap();
    platform.close_document(platform.shell().documents()[0].id);
    assert_eq!(
        fs::read(&path).unwrap(),
        newer,
        "the newer file is untouched"
    );
}

#[test]
fn background_work_reaches_the_shell_only_through_pump() {
    let mut platform = notes_platform(None);
    let inbox = platform.services().inbox().clone();
    assert!(!platform.wait(Duration::from_millis(1)));

    let worker = std::thread::spawn(move || {
        let _ = inbox.try_post(Box::new(|cx| {
            cx.shell.set_status("from the worker");
            cx.shell
                .open_document(DocumentKind::literal("notes.note"), "async");
        }));
    });
    worker.join().unwrap();

    assert_eq!(platform.shell().status(), "Ready", "nothing ran yet");
    assert!(platform.wait(Duration::from_secs(5)));
    assert_eq!(platform.pump(), 1);
    assert_eq!(platform.shell().status(), "from the worker");
    assert_eq!(
        platform.shell().active_document().unwrap().resource,
        "async"
    );
    assert_eq!(platform.pump(), 0);
}

#[test]
fn waker_coalesces_posts_until_the_queue_is_drained() {
    let platform = notes_platform(None);
    let (sender, receiver) = std::sync::mpsc::channel();
    platform.set_waker(move || {
        let _ = sender.send(());
    });
    platform
        .services()
        .inbox()
        .try_post(Box::new(|_| {}))
        .unwrap();
    platform
        .services()
        .inbox()
        .try_post(Box::new(|_| {}))
        .unwrap();
    assert_eq!(receiver.try_iter().count(), 1);
}

#[test]
fn staged_service_registrations_follow_the_whole_activation_transaction() {
    let registry = Rc::new(retrowork_platform::Registry::<&'static str>::new(
        ModuleId::literal("test.owner"),
    ));
    let mut builder = Platform::builder().module(module("test.owner", &[], |_| Ok(())));
    for (id, failure) in [
        ("test.good", 0),
        ("test.failed", 1),
        ("test.invalid", 2),
        ("test.duplicate", 3),
    ] {
        let registry = registry.clone();
        builder = builder.module(module(id, &["test.owner"], move |cx| {
            registry.register(cx, id, id)?;
            match failure {
                1 => Err(ActivationError::new("rejected")),
                2 => cx.key_binding("ctrl-9", command("missing.command"), None),
                3 => registry.register(cx, id, id),
                _ => Ok(()),
            }
        }));
    }
    let undeclared = registry.clone();
    builder = builder.module(module("test.undeclared", &[], move |cx| {
        undeclared.register(cx, "bad", "bad")
    }));
    let platform = builder.build();
    assert_eq!(registry.values(), ["test.good"]);
    assert_eq!(
        registry.contributor("test.good"),
        Some(ModuleId::literal("test.good"))
    );
    for id in [
        "test.failed",
        "test.invalid",
        "test.duplicate",
        "test.undeclared",
    ] {
        assert!(matches!(state(&platform, id), ModuleState::Rejected(_)));
    }
    platform.begin_shutdown();
    assert!(registry.values().is_empty());
}

#[test]
fn inbox_bounds_admission_and_work_per_turn_and_releases_producers_on_shutdown() {
    use retrowork_platform::{INBOX_CAPACITY, PostError};
    let mut platform = notes_platform(None);
    let inbox = platform.services().inbox().clone();
    for _ in 0..INBOX_CAPACITY {
        inbox.try_post(Box::new(|_| {})).unwrap();
    }
    let rejected = inbox.try_post(Box::new(|_| {})).unwrap_err();
    assert_eq!(rejected.reason, PostError::Full);
    let pumped = platform.pump();
    assert!(pumped > 0 && pumped <= 32);
    assert!(platform.wait(Duration::ZERO));
    for _ in 0..pumped {
        inbox.try_post(Box::new(|_| {})).unwrap();
    }
    let worker_inbox = inbox.clone();
    let (sent, received) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        sent.send(
            worker_inbox
                .post_blocking(Box::new(|_| {}))
                .map_err(|error| error.reason),
        )
        .unwrap();
    });
    assert!(received.recv_timeout(Duration::from_millis(30)).is_err());
    drop(platform);
    assert_eq!(
        received.recv_timeout(Duration::from_secs(5)).unwrap(),
        Err(PostError::Closed)
    );
    worker.join().unwrap();
    assert_eq!(
        inbox.try_post(rejected.completion).unwrap_err().reason,
        PostError::Closed
    );
}

#[test]
fn unavailable_commands_do_not_invoke_their_handlers() {
    let mut platform = Platform::builder()
        .module(module("test.actions", &[], |cx| {
            cx.register_command(
                retrowork_platform::CommandDefinition::new(
                    command("test.disabled"),
                    "Disabled",
                    |_| panic!("must not execute"),
                )
                .available_when(|_, _, _| Err("select a runnable target".into())),
            )
        }))
        .build();
    assert_eq!(
        platform
            .command_disabled_reason(&command("test.disabled"), None)
            .as_deref(),
        Some("select a runnable target")
    );
    assert!(
        platform
            .execute(&command("test.disabled"), None)
            .unwrap_err()
            .to_string()
            .contains("select a runnable target")
    );
}

#[test]
fn unchanged_state_is_retried_after_storage_recovers() {
    let directory = tempfile::tempdir().unwrap();
    let parent = directory.path().join("blocked");
    fs::write(&parent, "not a directory").unwrap();
    let path = parent.join("state.json");
    let mut platform = notes_platform(Some(StateStore::new(&path)));
    open(&mut platform, "a").unwrap();
    assert!(
        platform
            .persistence()
            .unwrap()
            .flush(Duration::from_secs(5))
            .is_err()
    );
    assert!(!path.exists());
    fs::remove_file(&parent).unwrap();
    fs::create_dir(&parent).unwrap();
    open(&mut platform, "a").unwrap();
    platform
        .persistence()
        .unwrap()
        .flush(Duration::from_secs(5))
        .unwrap();
    assert!(path.exists(), "the unchanged dirty state must be retried");
}

#[test]
fn services_require_declared_dependencies_and_resources_stop_in_reverse_order() {
    struct Resource(&'static str, Rc<RefCell<Vec<&'static str>>>);
    impl retrowork_platform::ModuleResource for Resource {
        fn shutdown(&self) {
            self.1.borrow_mut().push(self.0);
        }
    }
    let order = Rc::new(RefCell::new(Vec::new()));
    let a = order.clone();
    let b = order.clone();
    let rejected = order.clone();
    let platform = Platform::builder()
        .module(module("test.a", &[], move |cx| {
            cx.provide(Rc::new(42usize))?;
            cx.manage(Rc::new(Resource("a", a.clone())));
            Ok(())
        }))
        .module(module("test.b", &["test.a"], move |cx| {
            assert_eq!(*cx.require_service::<usize>()?, 42);
            cx.manage(Rc::new(Resource("b", b.clone())));
            Ok(())
        }))
        .module(module("test.c", &[], move |cx| {
            cx.manage(Rc::new(Resource("rejected", rejected.clone())));
            cx.require_service::<usize>()?;
            Ok(())
        }))
        .build();
    assert!(rejection(&platform, "test.c").contains("not a declared dependency"));
    assert_eq!(
        platform.services().owner::<usize>().unwrap().as_str(),
        "test.a"
    );
    platform.begin_shutdown();
    platform.begin_shutdown();
    drop(platform);
    assert_eq!(*order.borrow(), ["rejected", "b", "a"]);
}

#[test]
fn document_identity_survives_tab_shifts_and_rejects_closed_targets() {
    let mut platform = notes_platform(None);
    open(&mut platform, "a").unwrap();
    open(&mut platform, "b").unwrap();
    let first = platform.shell().documents()[0].id;
    let second = platform.shell().documents()[1].id;
    platform.close_document(first);
    assert!(
        platform
            .view_event(
                &ViewTarget::Document(first),
                ViewEvent::FilterChanged("wrong".into())
            )
            .is_err()
    );
    platform
        .view_event(
            &ViewTarget::Document(second),
            ViewEvent::FilterChanged("right".into()),
        )
        .unwrap();
    assert_eq!(
        open_documents(&platform),
        [("b".to_owned(), "right".to_owned())]
    );
    platform.close_document(first);
    assert!(platform.shell().document(second).is_some());
    open(&mut platform, "a").unwrap();
    assert_ne!(platform.shell().active_document().unwrap().id, first);
}

#[test]
fn rejected_completions_can_be_retried_and_blocking_submission_is_never_allowed_on_the_ui_thread() {
    use retrowork_platform::{INBOX_CAPACITY, PostError};
    let mut platform = notes_platform(None);
    let inbox = platform.services().inbox().clone();
    assert_eq!(
        inbox.post_blocking(Box::new(|_| {})).unwrap_err().reason,
        PostError::UiThread
    );
    for _ in 0..INBOX_CAPACITY {
        inbox.try_post(Box::new(|_| {})).unwrap();
    }
    let rejected = inbox
        .try_post(Box::new(|cx| cx.shell.set_status("retried")))
        .unwrap_err();
    assert_eq!(rejected.reason, PostError::Full);
    platform.pump();
    inbox.try_post(rejected.completion).unwrap();
    while platform.pump() != 0 {}
    assert_eq!(platform.shell().status(), "retried");
}

#[test]
fn native_providers_reject_wrong_event_types_without_changing_their_model() {
    use retrowork_platform::{NativeModel, NativeProvider, NativeValue};
    #[derive(Clone, Debug, PartialEq)]
    struct Model(u32);
    impl NativeModel for Model {
        type Event = u32;
    }
    struct Provider(std::cell::Cell<u32>);
    impl NativeProvider for Provider {
        fn can_close(&self) -> Result<(), retrowork_platform::CloseError> {
            if self.0.get() == 0 {
                Ok(())
            } else {
                Err(retrowork_platform::CloseError::Blocked(
                    "Unsaved native data".into(),
                ))
            }
        }
        type Model = Model;
        fn model(&self, _: &Shell) -> Model {
            Model(self.0.get())
        }
        fn handle_native(
            &self,
            event: &u32,
            _: &mut CommandContext<'_>,
        ) -> Result<(), CommandError> {
            self.0.set(self.0.get() + event);
            Ok(())
        }
    }
    let provider = Provider(std::cell::Cell::new(1));
    let view: &dyn ViewProvider = &provider;
    assert_eq!(
        view.can_close().unwrap_err().to_string(),
        "Unsaved native data"
    );
    provider.0.set(0);
    assert!(view.can_close().is_ok());
    let mut platform = Platform::builder()
        .module(module("test.native", &[], |cx| {
            cx.view(
                ViewId::literal("test.native"),
                "Native",
                ViewLocation::Bottom,
                0,
                Rc::new(Provider(std::cell::Cell::new(0))),
            )
        }))
        .build();
    let target = ViewTarget::View(ViewId::literal("test.native"));
    assert!(
        platform
            .view_event(&target, ViewEvent::Native(NativeValue::new("wrong")))
            .is_err()
    );
    assert!(
        platform
            .view_event(&target, ViewEvent::RowSelect(0))
            .is_err()
    );
    platform
        .view_event(&target, ViewEvent::Native(NativeValue::new(7u32)))
        .unwrap();
    let Some(ViewContent::Native(value)) = platform.view_content(&ViewId::literal("test.native"))
    else {
        panic!("native model missing");
    };
    assert_eq!(value.get::<Model>(), Some(&Model(7)));
    assert!(value.get::<String>().is_none());
    assert_eq!(value, NativeValue::new(Model(7)));
    assert_ne!(value, NativeValue::new(7u32));
    assert!(value.type_name().ends_with("Model"));
}

#[test]
fn command_predicates_handlers_and_queued_work_keep_their_module_service_scope() {
    let mut platform = Platform::builder()
        .module(module("test.owner", &[], |cx| cx.provide(Rc::new(42usize))))
        .module(module("test.stranger", &[], |cx| {
            cx.register_command(
                retrowork_platform::CommandDefinition::new(
                    command("test.stranger.run"),
                    "Stranger",
                    |cx| {
                        assert!(cx.services.get::<usize>().is_none());
                        cx.services
                            .inbox()
                            .try_post(Box::new(|cx| {
                                assert!(cx.services.get::<usize>().is_none());
                                cx.shell.set_status("stranger completed");
                            }))
                            .unwrap();
                        Ok(())
                    },
                )
                .available_when(|_, services, _| {
                    assert!(services.get::<usize>().is_none());
                    Ok(())
                }),
            )
        }))
        .module(module("test.dependent", &["test.owner"], |cx| {
            cx.command(command("test.dependent.run"), "Dependent", |cx| {
                assert_eq!(*cx.services.get::<usize>().unwrap(), 42);
                cx.services
                    .inbox()
                    .try_post(Box::new(|cx| {
                        assert_eq!(*cx.services.get::<usize>().unwrap(), 42);
                        cx.shell.set_status("dependent completed");
                    }))
                    .unwrap();
                Ok(())
            })
        }))
        .build();
    for (id, status) in [
        ("test.stranger.run", "stranger completed"),
        ("test.dependent.run", "dependent completed"),
    ] {
        platform
            .execute(&CommandId::new(id).unwrap(), None)
            .unwrap();
        platform.pump();
        assert_eq!(platform.shell().status(), status);
    }
    platform.begin_shutdown();
    assert!(
        platform
            .execute(&command("test.dependent.run"), None)
            .unwrap_err()
            .to_string()
            .contains("shutting down")
    );
    assert!(
        platform
            .view_event(
                &ViewTarget::View(ViewId::literal(VIEW_OUTPUT)),
                ViewEvent::RowSelect(0)
            )
            .is_err()
    );
}
