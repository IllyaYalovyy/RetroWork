//! Headless GPUI interaction tests.
//!
//! Tier: **headless GPUI**. The real `Workbench` view is laid out in a window
//! of GPUI's test platform. Input enters as platform events (pointer down/up
//! at coordinates, key down/up) and goes through GPUI's real hit testing,
//! focus handling and key dispatch. Nothing is drawn to a screen and there is
//! no operating system input here; `retrowork-e2e` covers that.
//!
//! The scenarios are the same functions the end-to-end runner executes.

use std::{fs, path::Path, rc::Rc, time::Duration};

use gpui::{
    AnyWindowHandle, AppContext as _, Bounds, Entity, KeyUpEvent, Keystroke, Modifiers,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, TestAppContext, VisualTestContext,
    WindowBounds, WindowOptions, point, px, size,
};
use retrowork_demo::{Options, build_platform};
use retrowork_logs::LogsModule;
use retrowork_platform::{
    ActivationContext, ActivationError, CommandId, Dependency, MenuId, Module, ModuleManifest,
    Package, Platform, PropertyModel, Shell, Tone, TraceEvent, ViewContent, ViewId, ViewLocation,
    ViewProvider,
    core::{COMMAND_CLEAR_OUTPUT, MENU_TOOLS, VIEW_OUTPUT},
};
use retrowork_testkit::{
    driver::{Outcome, Ui, UiDriver},
    sabotage::{Fault, Sabotage},
    scenarios::{self, SOURCE_AUTH, TAB_API, TAB_AUTH},
};
use retrowork_workbench::{Workbench, open_workbench, semantics::UiSnapshot};

struct Headless {
    cx: TestAppContext,
    window: AnyWindowHandle,
    workbench: Entity<Workbench>,
}

impl Headless {
    fn launch(cx: &mut TestAppContext, platform: Platform) -> Self {
        let (window, workbench) = cx.update(|cx| {
            retrowork_workbench::init(cx);
            open_workbench(
                platform,
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: Default::default(),
                        size: size(px(1200.), px(780.)),
                    })),
                    ..Default::default()
                },
                cx,
            )
            .expect("open the workbench window")
        });
        cx.run_until_parked();
        Self {
            cx: cx.clone(),
            window,
            workbench,
        }
    }

    fn frame(&mut self) {
        self.cx.run_until_parked();
        self.cx
            .update_window(self.window, |_, window, cx| {
                window.simulate_next_frame(cx);
                window.refresh();
                window.draw(cx).clear(cx);
            })
            .expect("the test window is open");
    }

    fn events(&mut self) -> VisualTestContext {
        VisualTestContext::from_window(self.window, &self.cx)
    }
}

impl UiDriver for Headless {
    fn input_route(&self) -> &'static str {
        "gpui-test-platform"
    }

    fn snapshot(&mut self) -> Outcome<UiSnapshot> {
        self.frame();
        let workbench = self.workbench.clone();
        let snapshot = self
            .cx
            .update_window(self.window, |_, window, cx| {
                workbench.read(cx).snapshot(window)
            })
            .map_err(|error| error.to_string())?;
        snapshot.validate()?;
        Ok(snapshot)
    }

    fn trace_since(&mut self, after: u64) -> Outcome<Vec<TraceEvent>> {
        let workbench = self.workbench.clone();
        Ok(self
            .cx
            .read(|cx| workbench.read(cx).platform().shell().trace().since(after)))
    }

    fn pointer_click(&mut self, x: f32, y: f32, count: usize) -> Outcome {
        let position = point(px(x), px(y));
        let modifiers = Modifiers::default();
        self.frame();
        self.events().simulate_event(MouseMoveEvent {
            position,
            pressed_button: None,
            modifiers,
        });
        for click_count in 1..=count {
            self.frame();
            self.events().simulate_event(MouseDownEvent {
                button: MouseButton::Left,
                position,
                modifiers,
                click_count,
                first_mouse: false,
            });
            self.frame();
            self.events().simulate_event(MouseUpEvent {
                button: MouseButton::Left,
                position,
                modifiers,
                click_count,
            });
        }
        self.frame();
        Ok(())
    }

    fn key(&mut self, chord: &str) -> Outcome {
        self.frame();
        let os = self.cx.read(|cx| cx.platform_info().os);
        let keystroke = Keystroke::parse_with_os(chord, os).map_err(|error| error.to_string())?;
        self.cx.dispatch_keystroke(self.window, keystroke.clone());
        self.events().simulate_event(KeyUpEvent { keystroke });
        self.frame();
        Ok(())
    }

    fn text(&mut self, text: &str) -> Outcome {
        self.frame();
        for character in text.chars() {
            let key = match character {
                ' ' => "space".to_owned(),
                other => other.to_string(),
            };
            self.cx.dispatch_keystroke(
                self.window,
                Keystroke {
                    modifiers: Modifiers::default(),
                    key,
                    key_char: Some(character.to_string()),
                },
            );
            self.frame();
        }
        Ok(())
    }

    fn pause(&mut self) {
        self.cx.run_until_parked();
    }

    fn patience(&self) -> Duration {
        // Nothing here depends on wall-clock time: if a condition does not
        // hold once the executor is idle, it never will.
        Duration::from_millis(50)
    }
}

fn packages_dir() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../packages"))
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// A packages directory with the sample workspace only. The process-plugin
/// package is left out: its executable is not built for headless tests.
fn sample_packages() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    copy_dir(
        &packages_dir().join("sample-workspace"),
        &directory.path().join("sample-workspace"),
    );
    directory
}

fn demo_platform(packages: &Path, state_dir: Option<&Path>) -> Platform {
    let platform = build_platform(&Options {
        samples: true,
        packages: Some(packages.to_owned()),
        state_dir: state_dir.map(Path::to_owned),
        ..Options::default()
    });
    for module in platform.modules() {
        assert!(module.is_active(), "{:?}", module.state);
    }
    platform
}

fn run(name: &str, driver: &mut dyn UiDriver) -> Outcome {
    let scenario = scenarios::find(name).unwrap_or_else(|| panic!("no scenario `{name}`"));
    (scenario.run)(&mut Ui::new(driver))
}

fn pass(name: &str, cx: &mut TestAppContext) {
    let packages = sample_packages();
    let mut driver = Headless::launch(cx, demo_platform(packages.path(), None));
    if let Err(error) = run(name, &mut driver) {
        panic!("scenario `{name}` failed: {error}");
    }
}

macro_rules! scenario_tests {
    ($($test:ident => $name:literal),* $(,)?) => {
        $(
            #[gpui::test]
            fn $test(cx: &mut TestAppContext) {
                pass($name, cx);
            }
        )*

        /// Every scenario that can run headless has a test above.
        #[test]
        fn all_headless_scenarios_are_run() {
            let run: &[&str] = &[$($name),*];
            for scenario in scenarios::SCENARIOS {
                assert_eq!(
                    run.contains(&scenario.name),
                    !scenario.native_only,
                    "scenario `{}`",
                    scenario.name
                );
            }
        }
    };
}

scenario_tests! {
    native_plugin_view => "native-plugin-view",
    tree_selection_opens_log => "tree-selection-opens-log",
    tabs_and_panels_switch => "tabs-and-panels-switch",
    menu_by_pointer => "menu-by-pointer",
    toolbar_and_shortcut_run_plugin_command => "toolbar-and-shortcut-run-plugin-command",
    filter_by_keyboard => "filter-by-keyboard",
    declarative_package_contributions => "declarative-package-contributions",
    record_detail => "record-detail",
    command_error_is_visible => "command-error-is-visible",
    contributed_controls_reach_their_commands => "contributed-controls-reach-their-commands",
}

#[gpui::test]
fn system_folder_prompt_survives_background_work_before_layout(cx: &mut TestAppContext) {
    let packages = sample_packages();
    let platform = demo_platform(packages.path(), None);
    let inbox = platform.services().inbox().clone();
    let mut driver = Headless::launch(cx, platform);
    Ui::new(&mut driver).click("menu.file").unwrap();
    let snapshot = driver.snapshot().unwrap();
    let (x, y) = snapshot
        .find("menu.file.open-project")
        .unwrap()
        .visible
        .unwrap()
        .center();
    let position = point(px(x), px(y));
    driver.events().simulate_event(MouseMoveEvent {
        position,
        pressed_button: None,
        modifiers: Modifiers::default(),
    });
    driver.events().simulate_event(MouseDownEvent {
        button: MouseButton::Left,
        position,
        modifiers: Modifiers::default(),
        click_count: 1,
        first_mouse: false,
    });
    inbox.try_post(Box::new(|_| {})).unwrap();
    driver.events().simulate_event(MouseUpEvent {
        button: MouseButton::Left,
        position,
        modifiers: Modifiers::default(),
        click_count: 1,
    });
    driver.frame();
    assert!(driver.cx.did_prompt_for_paths());
    driver.cx.simulate_path_prompt_response(|options| {
        assert!(options.directories && !options.files && !options.multiple);
        None
    });
    driver.frame();
}

#[gpui::test]
fn queued_work_yields_to_pointer_and_keyboard_input(cx: &mut TestAppContext) {
    use retrowork_platform::INBOX_CAPACITY;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    let packages = sample_packages();
    let platform = demo_platform(packages.path(), None);
    let inbox = platform.services().inbox().clone();
    let mut driver = Headless::launch(cx, platform);
    let completed = Arc::new(AtomicUsize::new(0));
    let enqueue = |count| {
        for _ in 0..count {
            let completed = completed.clone();
            inbox
                .try_post(Box::new(move |_| {
                    completed.fetch_add(1, Ordering::SeqCst);
                }))
                .unwrap();
        }
    };
    enqueue(INBOX_CAPACITY);
    driver.cx.run_until_parked();
    assert!((1..=32).contains(&completed.load(Ordering::SeqCst)));
    Ui::new(&mut driver).click(SOURCE_AUTH).unwrap();
    assert!(completed.load(Ordering::SeqCst) < INBOX_CAPACITY);
    assert!(driver.cx.read(|cx| {
        driver
            .workbench
            .read(cx)
            .platform()
            .shell()
            .active_document()
            .is_some()
    }));

    let before = completed.load(Ordering::SeqCst);
    enqueue(before);
    Ui::new(&mut driver).press("ctrl-f").unwrap();
    assert!(completed.load(Ordering::SeqCst) < INBOX_CAPACITY + before);
    Ui::new(&mut driver).type_text("level:warn").unwrap();
    Ui::new(&mut driver)
        .expect_text("doc.summary", "2 of 12 records")
        .unwrap();
    for _ in 0..INBOX_CAPACITY {
        if completed.load(Ordering::SeqCst) == INBOX_CAPACITY + before {
            break;
        }
        driver.frame();
    }
    assert_eq!(completed.load(Ordering::SeqCst), INBOX_CAPACITY + before);
}

#[gpui::test]
fn session_is_restored_into_the_window_after_a_restart(cx: &mut TestAppContext) {
    // This test includes real filesystem I/O and its worker-thread wakeups.
    cx.background_executor.allow_parking();
    let packages = sample_packages();
    let state = tempfile::tempdir().unwrap();
    {
        let mut first = Headless::launch(cx, demo_platform(packages.path(), Some(state.path())));
        let mut ui = Ui::new(&mut first);
        ui.click(SOURCE_AUTH).unwrap();
        ui.click(scenarios::SOURCE_API).unwrap();
        ui.click(TAB_AUTH).unwrap();
        ui.press("ctrl-f").unwrap();
        ui.type_text("level:warn").unwrap();
        ui.expect_text("doc.summary", "2 of 12 records").unwrap();
        ui.click(scenarios::BOTTOM_STATISTICS).unwrap();
        ui.expect_selected(scenarios::BOTTOM_STATISTICS, true)
            .unwrap();
        let persistence = first.cx.read(|cx| {
            first
                .workbench
                .read(cx)
                .platform()
                .begin_shutdown()
                .unwrap()
        });
        persistence.shutdown(Duration::from_secs(2)).unwrap();
        first
            .cx
            .update_window(first.window, |_, window, _| window.remove_window())
            .unwrap();
    }

    // A second application instance over the same state directory.
    let mut second = Headless::launch(cx, demo_platform(packages.path(), Some(state.path())));
    let mut ui = Ui::new(&mut second);
    ui.expect_selected(TAB_AUTH, true).unwrap();
    ui.expect_selected(TAB_API, false).unwrap();
    ui.expect_text("doc.filter", "level:warn").unwrap();
    ui.expect_text("doc.summary", "2 of 12 records").unwrap();
    ui.expect_count("doc.row.", 2).unwrap();
    ui.expect_selected(scenarios::BOTTOM_STATISTICS, true)
        .unwrap();
    ui.expect_text("status.message", "Restored 2 documents")
        .unwrap();
    // The other document kept its own, empty filter.
    ui.click(TAB_API).unwrap();
    ui.expect_text("doc.filter", "").unwrap();
    ui.expect_text("doc.summary", "15 records").unwrap();
}

#[gpui::test]
fn exit_shortcut_requests_quit(cx: &mut TestAppContext) {
    let packages = sample_packages();
    let mut driver = Headless::launch(cx, demo_platform(packages.path(), None));
    let quit_requested = |driver: &Headless| {
        let workbench = driver.workbench.clone();
        driver
            .cx
            .read(|cx| workbench.read(cx).platform().shell().quit_requested())
    };
    assert!(!quit_requested(&driver));
    driver.key("ctrl-q").unwrap();
    assert!(quit_requested(&driver));
}

// ---------------------------------------------------------------------------
// Negative controls: the scenarios must fail when something is broken.
// ---------------------------------------------------------------------------

fn expect_failure(result: Outcome, expected: &str) {
    match result {
        Ok(()) => panic!("the scenario passed although the wiring was broken"),
        Err(error) => assert!(
            error.contains(expected),
            "the scenario failed, but not where expected ({expected:?}):\n{error}"
        ),
    }
}

#[gpui::test]
fn broken_pointer_delivery_fails_the_pointer_scenarios(cx: &mut TestAppContext) {
    let packages = sample_packages();
    let patience = Duration::from_millis(20);
    for (fault, scenario, expected) in [
        (
            Fault::DropPointer,
            "tree-selection-opens-log",
            "`doc.tab.logs.source:auth` selected = true",
        ),
        (
            // 40 px lower is another tree row: the wrong log opens.
            Fault::MisaimPointer { dx: 0., dy: 40. },
            "tree-selection-opens-log",
            "`doc.tab.logs.source:auth` selected = true",
        ),
        (
            Fault::DropPointer,
            "menu-by-pointer",
            "`menu.help` selected = true",
        ),
        (Fault::DropPointer, "record-detail", "selected = true"),
    ] {
        let mut driver = Headless::launch(cx, demo_platform(packages.path(), None));
        let mut broken = Sabotage::new(&mut driver, fault, patience);
        expect_failure(run(scenario, &mut broken), expected);
    }
}

#[gpui::test]
fn broken_keyboard_delivery_fails_the_keyboard_scenarios(cx: &mut TestAppContext) {
    let packages = sample_packages();
    let patience = Duration::from_millis(20);
    for (scenario, expected) in [
        ("filter-by-keyboard", "`doc.filter` to have keyboard focus"),
        (
            "toolbar-and-shortcut-run-plugin-command",
            "`view.output.empty` to be present",
        ),
        ("command-error-is-visible", "`status.message` to read"),
        (
            "declarative-package-contributions",
            "level:warn level:error",
        ),
    ] {
        let mut driver = Headless::launch(cx, demo_platform(packages.path(), None));
        let mut broken = Sabotage::new(&mut driver, Fault::DropKeyboard, patience);
        expect_failure(run(scenario, &mut broken), expected);
    }
}

/// Stands in for the Log Statistics module with selectable defects.
struct Impostor {
    handler_does_nothing: bool,
    shortcut_runs_other_command: bool,
}

struct NoStatistics;
impl ViewProvider for NoStatistics {
    fn content(&self, _: &Shell) -> ViewContent {
        ViewContent::Properties(PropertyModel::default())
    }
}

impl Module for Impostor {
    fn manifest(&self) -> ModuleManifest {
        ModuleManifest::bundled(
            retrowork_log_stats::MODULE_ID,
            "Impostor",
            semver::Version::new(0, 1, 0),
        )
        .depends_on(Dependency::literal(retrowork_logs::MODULE_ID, "^0.1"))
    }

    fn activate(&self, cx: &mut ActivationContext<'_>) -> Result<(), ActivationError> {
        let command = CommandId::literal(retrowork_log_stats::COMMAND_SUMMARIZE);
        cx.view(
            ViewId::literal(retrowork_log_stats::VIEW_STATISTICS),
            "Statistics",
            ViewLocation::Bottom,
            20,
            Rc::new(NoStatistics),
        )?;
        let handler_does_nothing = self.handler_does_nothing;
        cx.command(command.clone(), "Summarize Levels", move |cx| {
            if handler_does_nothing {
                return Ok(());
            }
            let output = cx.shell.output();
            for line in [
                "Summary of auth.log: 12 of 12 records shown",
                "  Levels: ERROR 3, WARN 2, INFO 6, DEBUG 1",
                "  auth.session: 6",
            ] {
                output.append(line, Tone::Normal);
            }
            cx.shell.show_view(ViewId::literal(VIEW_OUTPUT));
            cx.shell.set_status("Summarized 12 records of auth.log");
            Ok(())
        })?;
        cx.menu_item(
            MenuId::literal(MENU_TOOLS),
            "summarize-levels",
            "Summarize Levels",
            command.clone(),
            None,
            10,
        )?;
        cx.toolbar_item("logstats.summarize", "Summarize", command.clone(), None, 30)?;
        let bound = if self.shortcut_runs_other_command {
            CommandId::literal(COMMAND_CLEAR_OUTPUT)
        } else {
            command
        };
        cx.key_binding("ctrl-shift-m", bound, None)
    }
}

fn platform_with(stats: Option<Impostor>, packages: &Path) -> Platform {
    let mut builder = Platform::builder().module(LogsModule);
    if let Some(stats) = stats {
        builder = builder.module(stats);
    }
    builder
        .module(Package::load(&packages.join("sample-workspace")).unwrap())
        .build()
}

#[gpui::test]
fn miswired_plugin_command_fails_the_plugin_scenario(cx: &mut TestAppContext) {
    let packages = sample_packages();
    let scenario = "toolbar-and-shortcut-run-plugin-command";

    // Control of the control: a faithful stand-in passes, so the failures
    // below are caused by the defects and nothing else.
    let faithful = Impostor {
        handler_does_nothing: false,
        shortcut_runs_other_command: false,
    };
    let mut driver = Headless::launch(cx, platform_with(Some(faithful), packages.path()));
    run(scenario, &mut driver).expect("a faithful stand-in passes");

    for (stats, expected) in [
        // The plugin is not installed at all.
        (None, "`dock.bottom.tab.logstats.statistics` to be visible"),
        // The toolbar button is wired to a handler that does nothing.
        (
            Some(Impostor {
                handler_does_nothing: true,
                shortcut_runs_other_command: false,
            }),
            "`dock.bottom.tab.output` selected = true",
        ),
        // The shortcut is bound to a different command.
        (
            Some(Impostor {
                handler_does_nothing: false,
                shortcut_runs_other_command: true,
            }),
            "an Output line containing \"Summary of auth.log",
        ),
    ] {
        let mut driver = Headless::launch(cx, platform_with(stats, packages.path()));
        expect_failure(run(scenario, &mut driver), expected);
    }
}

#[gpui::test]
fn package_without_its_menu_items_fails_the_package_scenario(cx: &mut TestAppContext) {
    let packages = sample_packages();
    let manifest = packages.path().join("sample-workspace/package.toml");
    let text = fs::read_to_string(&manifest).unwrap();
    let cut = text.find("[[menu-item]]").unwrap();
    fs::write(&manifest, &text[..cut]).unwrap();

    let mut driver = Headless::launch(cx, demo_platform(packages.path(), None));
    expect_failure(
        run("declarative-package-contributions", &mut driver),
        "`menu.filters.errors-only` to be present",
    );
}

#[gpui::test]
fn native_views_must_observe_declared_controls_without_duplicate_ids(cx: &mut TestAppContext) {
    use gpui::{IntoElement, ParentElement, Styled};
    use retrowork_platform::{NativeModel, NativeProvider};
    use retrowork_workbench::{
        semantics::{Control, Role},
        views::{ViewContext, ViewRenderer, ViewRenderers, WorkbenchModule},
    };
    #[derive(Clone, Debug, PartialEq)]
    struct Model;
    impl NativeModel for Model {
        type Event = ();
    }
    struct Provider;
    impl NativeProvider for Provider {
        type Model = Model;
        fn model(&self, _: &Shell) -> Model {
            Model
        }
        fn handle_native(
            &self,
            _: &(),
            _: &mut retrowork_platform::CommandContext<'_>,
        ) -> Result<(), retrowork_platform::CommandError> {
            Ok(())
        }
    }
    struct Renderer(bool);
    impl ViewRenderer for Renderer {
        type Model = Model;
        fn controls(&self) -> &'static [&'static str] {
            &["button"]
        }
        fn render(&self, _: &Model, context: &mut ViewContext<'_, ()>) -> gpui::AnyElement {
            let mut element = gpui::div().relative().child("Button");
            if self.0 {
                element = element
                    .child(context.observe(Control::new("button", Role::Button, "Button")))
                    .child(context.observe(Control::new("button", Role::Button, "Button")));
            }
            element.into_any_element()
        }
    }
    struct Plugin(bool);
    impl Module for Plugin {
        fn manifest(&self) -> ModuleManifest {
            ModuleManifest::bundled(
                "test.observation",
                "Observation",
                semver::Version::new(1, 0, 0),
            )
            .depends_on(Dependency::literal(
                retrowork_workbench::views::MODULE_ID,
                "^0.1",
            ))
        }
        fn activate(&self, cx: &mut ActivationContext<'_>) -> Result<(), ActivationError> {
            cx.require_service::<ViewRenderers>()?
                .register(cx, Renderer(self.0))?;
            cx.view(
                ViewId::literal("test.observation"),
                "Observation",
                ViewLocation::Bottom,
                -100,
                Rc::new(Provider),
            )
        }
    }
    for (duplicate, expected) in [
        (false, "missing observed control"),
        (true, "duplicate element id"),
    ] {
        let platform = Platform::builder()
            .module(WorkbenchModule)
            .module(Plugin(duplicate))
            .build();
        let mut driver = Headless::launch(cx, platform);
        let error = driver.snapshot().unwrap_err();
        assert!(error.contains(expected), "{error}");
    }
}

#[gpui::test]
fn system_folder_cancellation_restores_shortcuts(cx: &mut TestAppContext) {
    let packages = sample_packages();
    let mut driver = Headless::launch(cx, demo_platform(packages.path(), None));
    for _ in 0..2 {
        Ui::new(&mut driver).press("ctrl-o").unwrap();
        assert!(driver.cx.did_prompt_for_paths());
        driver.cx.simulate_path_prompt_response(|options| {
            assert!(options.directories && !options.files && !options.multiple);
            assert_eq!(options.prompt.as_deref(), Some("Open"));
            None
        });
        driver.frame();
    }
}

#[gpui::test]
fn document_filter_submission_keeps_workbench_shortcuts_working(cx: &mut TestAppContext) {
    use retrowork_platform::{
        CommandContext, CommandError, DocumentFactory, DocumentKind, FilterModel, FocusTarget,
        OpenedDocument, TableModel, ViewEvent,
    };
    use std::cell::RefCell;

    #[derive(Clone, Default)]
    struct Filter(Rc<RefCell<String>>, bool);
    impl DocumentFactory for Filter {
        fn open(
            &self,
            resource: &str,
            _: &mut CommandContext<'_>,
        ) -> Result<OpenedDocument, CommandError> {
            Ok(OpenedDocument {
                title: "Filter".into(),
                provider: Rc::new(Self(self.0.clone(), resource == "result")),
            })
        }
    }
    impl ViewProvider for Filter {
        fn content(&self, _: &Shell) -> ViewContent {
            if self.1 {
                return ViewContent::Source(retrowork_platform::SourceModel {
                    editing: None,
                    text: std::sync::Arc::from(self.0.borrow().as_str()),
                    reveal_line: None,
                    message: String::new(),
                });
            }
            ViewContent::Table(TableModel {
                columns: vec![],
                rows: Rc::default(),
                selected: None,
                filter: Some(FilterModel {
                    text: self.0.borrow().clone(),
                    placeholder: "Query".into(),
                }),
                summary: String::new(),
            })
        }
        fn handle(
            &self,
            event: ViewEvent,
            cx: &mut CommandContext<'_>,
        ) -> Result<(), CommandError> {
            match event {
                ViewEvent::FilterChanged(text) => *self.0.borrow_mut() = text,
                ViewEvent::InputSubmitted(text) => {
                    cx.shell.set_status(format!("Submitted: {text}"));
                    cx.shell
                        .open_document(DocumentKind::literal("test.filter"), "result");
                }
                _ => {}
            }
            Ok(())
        }
    }
    struct Plugin;
    impl Module for Plugin {
        fn manifest(&self) -> ModuleManifest {
            ModuleManifest::bundled(
                "test.filter-submit",
                "Filter",
                semver::Version::new(0, 1, 0),
            )
        }
        fn activate(&self, cx: &mut ActivationContext<'_>) -> Result<(), ActivationError> {
            cx.document_factory(
                DocumentKind::literal("test.filter"),
                Rc::new(Filter::default()),
            )?;
            cx.command(CommandId::literal("test.open-filter"), "Filter", |cx| {
                cx.shell
                    .open_document(DocumentKind::literal("test.filter"), "filter");
                cx.shell.request_focus(FocusTarget::DocumentFilter);
                Ok(())
            })?;
            cx.key_binding("ctrl-p", CommandId::literal("test.open-filter"), None)
        }
    }
    let mut driver = Headless::launch(cx, Platform::builder().module(Plugin).build());
    let mut ui = Ui::new(&mut driver);
    ui.press("ctrl-p").unwrap();
    ui.expect_focused("doc.filter").unwrap();
    ui.type_text("query").unwrap();
    ui.press("enter").unwrap();
    ui.expect_text_contains("status.message", "Submitted: query")
        .unwrap();
    ui.expect_text("doc.source", "query").unwrap();
    for _ in 0..2 {
        ui.press("ctrl-p").unwrap();
        ui.expect_focused("doc.filter").unwrap();
        ui.expect_text("doc.filter", "query").unwrap();
        ui.press("enter").unwrap();
        ui.expect_text("doc.source", "query").unwrap();
    }
}

#[gpui::test]
fn editing_preserves_undo_and_prevents_dirty_close(cx: &mut TestAppContext) {
    use retrowork_platform::{
        DocumentFactory, DocumentKind, OpenedDocument, SourceModel, ViewEvent,
        document_file::DocumentFile,
        document_session::{DocumentSession, SourceEvent},
    };
    #[derive(Clone)]
    struct Source(SourceModel);
    impl ViewProvider for Source {
        fn content(&self, _: &Shell) -> ViewContent {
            ViewContent::Source(self.0.clone())
        }
        fn can_close(&self) -> Result<(), retrowork_platform::CloseError> {
            self.0.editing.as_ref().unwrap().can_close()
        }
        fn handle(
            &self,
            event: ViewEvent,
            _: &mut retrowork_platform::CommandContext<'_>,
        ) -> Result<(), retrowork_platform::CommandError> {
            if let ViewEvent::Native(event) = event
                && let Some(SourceEvent::Edited(text)) = event.get::<SourceEvent>()
            {
                self.0.editing.as_ref().unwrap().edit(text.clone());
            }
            Ok(())
        }
    }
    impl DocumentFactory for Source {
        fn open(
            &self,
            _: &str,
            _: &mut retrowork_platform::CommandContext<'_>,
        ) -> Result<OpenedDocument, retrowork_platform::CommandError> {
            Ok(OpenedDocument {
                title: "Source".into(),
                provider: Rc::new(self.clone()),
            })
        }
    }
    struct Plugin(Source);
    impl Module for Plugin {
        fn manifest(&self) -> ModuleManifest {
            ModuleManifest::bundled("test.editing", "Editing", semver::Version::new(0, 1, 0))
        }
        fn activate(&self, cx: &mut ActivationContext<'_>) -> Result<(), ActivationError> {
            cx.document_factory(
                DocumentKind::literal("test.source"),
                Rc::new(self.0.clone()),
            )?;
            cx.command(CommandId::literal("test.open-source"), "Source", |cx| {
                cx.shell
                    .open_document(DocumentKind::literal("test.source"), "source");
                Ok(())
            })?;
            cx.key_binding("ctrl-o", CommandId::literal("test.open-source"), None)?;
            cx.command(
                CommandId::literal("test.open-other"),
                "Other source",
                |cx| {
                    cx.shell
                        .open_document(DocumentKind::literal("test.source"), "other");
                    Ok(())
                },
            )?;
            cx.key_binding("ctrl-p", CommandId::literal("test.open-other"), None)
        }
    }
    for original in ["original", ""] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source.txt");
        std::fs::write(&path, original).unwrap();
        let file = DocumentFile::open(path, 128).unwrap();
        let model = SourceModel {
            text: file.text(),
            editing: Some(DocumentSession::new(file)),
            reveal_line: Some(1),
            message: String::new(),
        };
        let mut driver = Headless::launch(
            cx,
            Platform::builder().module(Plugin(Source(model))).build(),
        );
        let mut ui = Ui::new(&mut driver);
        ui.press("ctrl-o").unwrap();
        ui.click("doc.source").unwrap();
        ui.press("ctrl-a").unwrap();
        ui.type_text("x").unwrap();
        ui.expect_text("doc.source", "x").unwrap();
        ui.expect_text("doc.save-state", "Modified").unwrap();
        ui.press("ctrl-w").unwrap();
        ui.expect_present("doc.source").unwrap();
        ui.expect_text_contains("status.message", "Unsaved changes")
            .unwrap();
        ui.expect_present("dialog.discard.cancel").unwrap();
        ui.click("doc.source").unwrap();
        ui.type_text("blocked").unwrap();
        ui.expect_text("doc.source", "x").unwrap();
        ui.press("escape").unwrap();
        ui.expect_absent("dialog.discard.cancel").unwrap();
        ui.press("ctrl-w").unwrap();
        ui.expect_present("dialog.discard.cancel").unwrap();
        ui.press("enter").unwrap();
        ui.expect_absent("dialog.discard.cancel").unwrap();
        ui.expect_text("doc.source", "x").unwrap();
        ui.press("ctrl-q").unwrap();
        ui.expect_present("doc.source").unwrap();
        ui.click("dialog.discard.cancel").unwrap();
        ui.expect_absent("dialog.discard.cancel").unwrap();
        ui.click("doc.source").unwrap();
        ui.press("ctrl-z").unwrap();
        ui.expect_text("doc.source", original).unwrap();
        ui.expect_text("doc.save-state", "Saved").unwrap();
        ui.press("ctrl-y").unwrap();
        ui.expect_text("doc.source", "x").unwrap();
        ui.press("ctrl-z").unwrap();
        ui.press("ctrl-w").unwrap();
        ui.expect_absent("doc.source").unwrap();
        ui.press("ctrl-o").unwrap();
        ui.click("doc.source").unwrap();
        ui.type_text("discard").unwrap();
        ui.press("ctrl-w").unwrap();
        ui.click("dialog.discard.confirm").unwrap();
        ui.expect_absent("doc.source").unwrap();
        ui.press("ctrl-o").unwrap();
        ui.press("ctrl-p").unwrap();
        ui.press("ctrl-q").unwrap();
        ui.click("dialog.discard.confirm").unwrap();
        ui.expect_absent("doc.tab.test.source:source").unwrap();
        ui.expect_present("doc.tab.test.source:other").unwrap();
        ui.click("dialog.discard.cancel").unwrap();
        ui.expect_absent("dialog.discard.cancel").unwrap();
        ui.expect_present("doc.tab.test.source:other").unwrap();
        assert_eq!(
            std::fs::read_to_string(directory.path().join("source.txt")).unwrap(),
            original
        );
    }
}

#[gpui::test]
fn save_picker_cancellation_and_response_keep_the_original_document_identity(
    cx: &mut TestAppContext,
) {
    use retrowork_platform::{
        DocumentFactory, DocumentKind, OpenedDocument, SavePathRequest, TextModel,
    };
    struct Provider;
    impl ViewProvider for Provider {
        fn content(&self, _: &Shell) -> ViewContent {
            ViewContent::Text(TextModel::default())
        }
    }
    impl DocumentFactory for Provider {
        fn open(
            &self,
            resource: &str,
            _: &mut retrowork_platform::CommandContext<'_>,
        ) -> Result<OpenedDocument, retrowork_platform::CommandError> {
            Ok(OpenedDocument {
                title: resource.into(),
                provider: Rc::new(Provider),
            })
        }
    }
    struct Plugin;
    impl Module for Plugin {
        fn manifest(&self) -> ModuleManifest {
            ModuleManifest::bundled("test.picker", "Picker", semver::Version::new(0, 1, 0))
        }
        fn activate(&self, cx: &mut ActivationContext<'_>) -> Result<(), ActivationError> {
            cx.document_factory(DocumentKind::literal("test.file"), Rc::new(Provider))?;
            cx.command(CommandId::literal("test.open"), "Open", |cx| {
                cx.shell
                    .open_document(DocumentKind::literal("test.file"), "original");
                Ok(())
            })?;
            cx.key_binding("ctrl-o", CommandId::literal("test.open"), None)?;
            cx.command(CommandId::literal("test.save-as"), "Save As", |cx| {
                if let Some(args) = cx.args {
                    let id = serde_json::from_value(args["document"].clone()).unwrap();
                    let resource = cx.shell.document(id).unwrap().resource.clone();
                    cx.shell.set_status(format!(
                        "Chosen for {resource}: {}",
                        args["path"].as_str().unwrap()
                    ));
                } else {
                    cx.shell.choose_save_path(SavePathRequest {
                        document: cx.shell.active_document().unwrap().id,
                        directory: "/tmp".into(),
                        name: "copy.rs".into(),
                        command: CommandId::literal("test.save-as"),
                    });
                }
                Ok(())
            })?;
            cx.key_binding("ctrl-shift-s", CommandId::literal("test.save-as"), None)
        }
    }
    let platform = Platform::builder().module(Plugin).build();
    let inbox = platform.services().inbox().clone();
    let mut driver = Headless::launch(cx, platform);
    Ui::new(&mut driver).press("ctrl-o").unwrap();
    Ui::new(&mut driver).press("ctrl-shift-s").unwrap();
    assert!(driver.cx.did_prompt_for_new_path());
    driver.cx.simulate_new_path_selection(|_| None);
    driver.frame();
    assert!(
        !driver
            .snapshot()
            .unwrap()
            .text("status.message")
            .unwrap_or_default()
            .starts_with("Chosen")
    );
    Ui::new(&mut driver).press("ctrl-shift-s").unwrap();
    inbox
        .try_post(Box::new(|cx| {
            cx.shell
                .open_document(DocumentKind::literal("test.file"), "other")
        }))
        .unwrap();
    driver.frame();
    driver.cx.simulate_new_path_selection(|directory| {
        assert_eq!(directory, Path::new("/tmp"));
        Some(directory.join("copy.rs"))
    });
    Ui::new(&mut driver)
        .expect_text("status.message", "Chosen for original: /tmp/copy.rs")
        .unwrap();
}

#[gpui::test]
fn save_all_operates_background_source_tabs_without_switching_them(cx: &mut TestAppContext) {
    cx.background_executor.allow_parking();
    let directory = tempfile::tempdir().unwrap();
    let first = directory.path().join("first.rs");
    let second = directory.path().join("second.rs");
    fs::write(&first, "first").unwrap();
    fs::write(&second, "second").unwrap();
    let mut platform = Platform::builder()
        .module(retrowork_projects::ProjectsModule)
        .build();
    for path in [&first, &second] {
        platform.shell_mut().open_document(
            retrowork_platform::DocumentKind::literal(retrowork_projects::DOCUMENT_SOURCE),
            path.to_str().unwrap(),
        );
        platform
            .execute(&CommandId::literal(COMMAND_CLEAR_OUTPUT), None)
            .unwrap();
    }
    let first_tab = format!("doc.tab.projects.source:{}", first.display());
    let second_tab = format!("doc.tab.projects.source:{}", second.display());
    let mut driver = Headless::launch(cx, platform);
    let mut ui = Ui::new(&mut driver);
    ui.expect_text("doc.source", "second").unwrap();
    ui.click("doc.source").unwrap();
    ui.press("ctrl-a").unwrap();
    ui.type_text("second edited").unwrap();
    ui.click(&first_tab).unwrap();
    ui.expect_text("doc.source", "first").unwrap();
    ui.click("doc.source").unwrap();
    ui.press("ctrl-a").unwrap();
    ui.type_text("first edited").unwrap();
    ui.press("ctrl-alt-s").unwrap();
    ui.expect_text("doc.save-state", "Saved").unwrap();
    ui.expect_selected(&first_tab, true).unwrap();
    ui.click(&second_tab).unwrap();
    ui.expect_text("doc.save-state", "Saved").unwrap();
    assert_eq!(fs::read_to_string(&first).unwrap(), "first edited");
    assert_eq!(fs::read_to_string(&second).unwrap(), "second edited");
    ui.click("doc.source").unwrap();
    ui.press("ctrl-z").unwrap();
    ui.expect_text("doc.save-state", "Modified").unwrap();
    ui.click("menu.file").unwrap();
    ui.click("menu.file.save-all").unwrap();
    ui.expect_text("doc.save-state", "Saved").unwrap();
    assert_ne!(fs::read_to_string(&second).unwrap(), "second edited");
}

#[gpui::test]
fn reload_uses_discard_controls_and_resets_the_editor_only_after_success(cx: &mut TestAppContext) {
    cx.background_executor.allow_parking();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("source.rs");
    fs::write(&path, "original").unwrap();
    let mut platform = Platform::builder()
        .module(retrowork_projects::ProjectsModule)
        .build();
    platform.shell_mut().open_document(
        retrowork_platform::DocumentKind::literal(retrowork_projects::DOCUMENT_SOURCE),
        path.to_str().unwrap(),
    );
    platform
        .execute(&CommandId::literal(COMMAND_CLEAR_OUTPUT), None)
        .unwrap();
    let tab = format!("doc.tab.projects.source:{}", path.display());
    let mut driver = Headless::launch(cx, platform);
    let mut ui = Ui::new(&mut driver);
    ui.expect_text("doc.source", "original").unwrap();
    ui.click("doc.source").unwrap();
    ui.press("ctrl-a").unwrap();
    ui.type_text("mine").unwrap();
    fs::write(&path, "external").unwrap();
    ui.click("menu.file").unwrap();
    ui.click("menu.file.reload").unwrap();
    ui.click("dialog.discard.cancel").unwrap();
    ui.expect_text("doc.source", "mine").unwrap();
    ui.click("menu.file").unwrap();
    ui.click("menu.file.reload").unwrap();
    ui.click("dialog.discard.confirm").unwrap();
    ui.expect_text("doc.source", "external").unwrap();
    ui.expect_text("doc.save-state", "Saved").unwrap();
    ui.expect_text_contains("status.message", "Reloaded")
        .unwrap();
    ui.expect_selected(&tab, true).unwrap();
    ui.click("doc.source").unwrap();
    ui.press("ctrl-z").unwrap();
    ui.expect_text("doc.source", "external").unwrap();
    fs::remove_file(&path).unwrap();
    ui.click("menu.file").unwrap();
    ui.click("menu.file.reload").unwrap();
    ui.expect_text_contains("doc.save-state", "Cannot reload")
        .unwrap();
    ui.expect_text("doc.source", "external").unwrap();
    fs::write(&path, "recovered").unwrap();
    ui.click("menu.file").unwrap();
    ui.click("menu.file.reload").unwrap();
    ui.expect_text("doc.source", "recovered").unwrap();
    ui.expect_text("doc.save-state", "Saved").unwrap();
}
