//! The workbench window: menu bar, toolbar, left dock, document tabs, bottom
//! dock and status bar, all drawn from what the platform's modules contributed.
//!
//! The view holds no feature logic. Pointer and key input arrives through
//! GPUI's hit testing and key dispatch, is turned into a platform command or a
//! [`ViewEvent`], and the next frame draws whatever the platform then reports.

mod discard;
mod paths;
mod render;
mod sources;

use std::{
    collections::{BTreeMap, HashMap},
    ops::Range,
    rc::Rc,
};

use futures::StreamExt as _;
use gpui::{
    Action, AnyElement, AnyWindowHandle, App, AppContext as _, ClickEvent, Context, DispatchPhase,
    Div, ElementId, Entity, FocusHandle, Focusable, InteractiveElement as _, IntoElement,
    KeyBinding, KeyBindingContextPredicate, KeyDownEvent, Keystroke, MouseButton, MouseDownEvent,
    ParentElement as _, Render, ScrollStrategy, SharedString, Stateful,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, UniformListScrollHandle,
    Window, WindowOptions, canvas,
    component::{
        Sizable as _, Theme, ThemeMode,
        input::{Input, InputEvent, InputState},
        tooltip::Tooltip,
    },
    div,
    prelude::FluentBuilder as _,
    px, rgb, uniform_list,
};
use retrowork_platform::{
    CommandId, FocusTarget, InputModel, MenuId, Platform, PropertyModel, TableModel, TextLine,
    TextModel, Tone, TreeModel, ViewContent, ViewEvent, ViewId, ViewLocation, ViewTarget,
};
use serde_json::Value;

use crate::{
    semantics::{Probe, Role, SharedFrame, UiSnapshot},
    theme::{self, *},
};

/// Key context of the workbench root. Module key bindings are bound in it.
pub const KEY_CONTEXT: &str = "Workbench";

/// GPUI action carrying a platform command. Key bindings contributed by
/// modules dispatch this through GPUI's keymap.
#[derive(Clone, Debug, PartialEq, Action)]
#[action(namespace = retrowork, no_json)]
pub struct RunCommand {
    pub command: CommandId,
    pub args: Option<Value>,
}

/// Initialises GPUI Kit and the workbench theme. Call once per application.
pub fn init(cx: &mut App) {
    gpui::init(cx);
    Theme::change(ThemeMode::Light, None, cx);
}

/// Opens a window showing `platform`.
pub fn open_workbench(
    platform: Platform,
    options: WindowOptions,
    cx: &mut App,
) -> gpui::Result<(AnyWindowHandle, Entity<Workbench>)> {
    gpui::open_window(options, cx, |window, cx| {
        cx.new(|cx| Workbench::new(platform, window, cx))
    })
}

/// Scroll position and what the last frame showed of one virtualized list.
#[derive(Default)]
struct ListState {
    scroll: UniformListScrollHandle,
    seen_len: usize,
    seen_selected: Option<usize>,
}

/// Creates the input fields of docked views whose content declares one, and
/// reports Enter in each to its view.
fn create_view_inputs(
    platform: &Platform,
    subscriptions: &mut Vec<Subscription>,
    window: &mut Window,
    cx: &mut Context<Workbench>,
) -> BTreeMap<ViewId, Entity<InputState>> {
    let mut inputs = BTreeMap::new();
    for location in [ViewLocation::Left, ViewLocation::Bottom] {
        for spec in platform.contributions().views(location) {
            let Some(ViewContent::Tree(TreeModel {
                input: Some(model), ..
            })) = platform.view_content(&spec.id)
            else {
                continue;
            };
            let input = cx.new(|cx| {
                let mut state = InputState::new(window, cx);
                state.set_value(model.initial_text.clone(), window, cx);
                state
            });
            let view = spec.id.clone();
            subscriptions.push(cx.subscribe_in(
                &input,
                window,
                move |this, input, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        let text = input.read(cx).value().to_string();
                        this.view_event(
                            &ViewTarget::View(view.clone()),
                            ViewEvent::InputSubmitted(text),
                            window,
                            cx,
                        );
                    }
                },
            ));
            inputs.insert(spec.id.clone(), input);
        }
    }
    inputs
}

fn eid(id: impl Into<String>) -> ElementId {
    ElementId::Name(SharedString::from(id.into()))
}

/// A keystroke in the notation key bindings use, e.g. `ctrl-shift-s`.
fn chord_text(keystroke: &Keystroke) -> String {
    let modifiers = &keystroke.modifiers;
    let mut text = String::new();
    for (held, name) in [
        (modifiers.control, "ctrl-"),
        (modifiers.alt, "alt-"),
        (modifiers.shift, "shift-"),
        (modifiers.platform, "super-"),
    ] {
        if held {
            text.push_str(name);
        }
    }
    text.push_str(&keystroke.key);
    text
}

/// The workbench window's root view. It draws what the platform's modules
/// contributed and turns pointer and key input into commands and view
/// events; it holds no feature logic of its own.
pub struct Workbench {
    pub(crate) platform: Platform,
    focus: FocusHandle,
    active_document: Option<retrowork_platform::DocumentId>,
    /// Protects newly requested focus until layout has registered its control.
    focus_requested_frame: Option<u64>,
    path_prompt: Option<Task<()>>,
    discard_prompt: bool,
    open_menu: Option<MenuId>,
    filter_input: Entity<InputState>,
    pub(crate) frame: SharedFrame,
    /// Scroll state of each virtualized list, by element id prefix, so that
    /// lists in different views and documents scroll independently.
    lists: HashMap<String, ListState>,
    /// Input fields of docked views that declare one, created once.
    view_inputs: BTreeMap<ViewId, Entity<InputState>>,
    _subscriptions: Vec<Subscription>,
    _pump: Task<()>,
    shutdown: Option<Task<()>>,
    pub(crate) automation_enabled: bool,
    source_editors: HashMap<retrowork_platform::DocumentId, sources::SourceEditor>,
    native_focus: HashMap<String, FocusHandle>,
}

impl Workbench {
    /// A workbench for `platform`: binds the modules' key bindings, records
    /// key input in the trace, and runs background completions when the
    /// platform's inbox wakes it.
    pub fn new(platform: Platform, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let context = Rc::new(
            KeyBindingContextPredicate::parse(KEY_CONTEXT)
                .expect("the key context is a valid name"),
        );
        let mut bindings = Vec::new();
        for binding in platform.contributions().key_bindings() {
            let action = RunCommand {
                command: binding.command.clone(),
                args: binding.args.clone(),
            };
            match KeyBinding::load(
                binding.chord.as_str(),
                Box::new(action),
                Some(Rc::clone(&context)),
                false,
                None,
                cx,
            ) {
                Ok(loaded) => bindings.push(loaded),
                // The platform validated the chord; say so if GPUI disagrees
                // rather than dropping the shortcut silently.
                Err(error) => platform.shell().output().append(
                    format!(
                        "Shortcut `{}` of `{}` is not usable: {error}",
                        binding.chord, binding.owner
                    ),
                    Tone::Warning,
                ),
            }
        }
        cx.bind_keys(bindings);

        let filter_input = cx.new(|cx| InputState::new(window, cx));
        let filter_edits = cx.subscribe_in(
            &filter_input,
            window,
            |this, input, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = input.read(cx).value().to_string();
                    this.on_filter_edited(text, window, cx);
                } else if matches!(event, InputEvent::PressEnter { .. })
                    && let Some(document) = this.platform.shell().active_document()
                {
                    this.view_event(
                        &ViewTarget::Document(document.id),
                        ViewEvent::InputSubmitted(input.read(cx).value().to_string()),
                        window,
                        cx,
                    );
                }
            },
        );

        // Background work posts to the platform inbox from other threads; the
        // waker turns each post into a pump on the UI thread.
        let (wake, mut woken) = futures::channel::mpsc::channel::<()>(1);
        let wake = std::sync::Mutex::new(wake);
        platform.set_waker(move || {
            if let Ok(mut wake) = wake.lock() {
                let _ = wake.try_send(());
            }
        });
        let pump = cx.spawn_in(window, async move |this, cx| {
            while woken.next().await.is_some() {
                let alive = this.update_in(cx, |this, window, cx| {
                    if this.shutdown.is_some() {
                        return;
                    }
                    this.platform.pump();
                    this.after_change(window, cx);
                });
                if alive.is_err() {
                    break;
                }
                let (ready, next_frame) = futures::channel::oneshot::channel();
                if this
                    .update_in(cx, |_, window, _| {
                        window.on_next_frame(move |_, _| {
                            let _ = ready.send(());
                        });
                    })
                    .is_err()
                {
                    break;
                }
                if next_frame.await.is_err() {
                    break;
                }
            }
        });

        // Runs before key bindings are resolved, so the trace has the key
        // first and the command it triggered second.
        let own_window = window.window_handle();
        let this = cx.weak_entity();
        let keystrokes = cx.intercept_keystrokes(move |event, window, cx| {
            if window.window_handle() == own_window {
                let _ = this.update(cx, |this, _| {
                    this.platform
                        .shell_mut()
                        .trace_mut()
                        .record("ui.key_down", chord_text(&event.keystroke));
                });
            }
        });

        let shutdown = cx.on_app_quit(|this, cx| {
            let persistence = this.platform.begin_shutdown();
            cx.background_executor().spawn(async move {
                if let Some(writer) = persistence
                    && let Err(error) = writer.shutdown(std::time::Duration::from_millis(100))
                {
                    eprintln!("State flush failed: {error}");
                }
            })
        });
        let owner = cx.weak_entity();
        window.on_window_should_close(cx, move |window, cx| {
            owner
                .update(cx, |this, cx| {
                    if !this.discard_prompt && this.path_prompt.is_none() {
                        this.platform.shell_mut().request_quit();
                        this.after_change(window, cx);
                    }
                })
                .is_err()
        });
        let mut subscriptions = vec![filter_edits, keystrokes, shutdown];
        let view_inputs = create_view_inputs(&platform, &mut subscriptions, window, cx);

        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let mut workbench = Self {
            active_document: platform
                .shell()
                .active_document()
                .map(|document| document.id),
            platform,
            focus,
            focus_requested_frame: None,
            path_prompt: None,
            discard_prompt: false,
            open_menu: None,
            filter_input,
            frame: SharedFrame::default(),
            lists: HashMap::new(),
            view_inputs,
            _subscriptions: subscriptions,
            _pump: pump,
            shutdown: None,
            automation_enabled: false,
            source_editors: HashMap::new(),
            native_focus: HashMap::new(),
        };
        workbench.sync_filter_input(window, cx);
        workbench
    }

    /// The platform this window shows.
    pub fn platform(&self) -> &Platform {
        &self.platform
    }

    /// The elements drawn in the most recent frame of `window`.
    pub fn snapshot(&self, window: &Window) -> UiSnapshot {
        let frame = self.frame.borrow();
        let size = window.viewport_size();
        UiSnapshot {
            frame: frame.frame,
            width: size.width.into(),
            height: size.height.into(),
            scale: window.scale_factor(),
            window_active: window.is_window_active(),
            trace_seq: frame.trace_seq,
            marker: frame.marker,
            errors: frame
                .expected
                .iter()
                .filter(|id| !frame.nodes.iter().any(|node| &node.id == *id))
                .map(|id| format!("missing observed control `{id}`"))
                .collect(),
            nodes: frame.nodes.clone(),
        }
    }

    fn execute(
        &mut self,
        command: &CommandId,
        args: Option<&Value>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.shutdown.is_some() {
            return;
        }
        self.open_menu = None;
        // A failure is already reported in Output, status and trace.
        let _ = self.platform.execute(command, args);
        self.after_change(window, cx);
    }

    /// Routes native plugin control input to its provider and refreshes the window.
    pub fn view_event(
        &mut self,
        target: &ViewTarget,
        event: ViewEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.shutdown.is_some() {
            return;
        }
        let _ = self.platform.view_event(target, event);
        self.after_change(window, cx);
    }

    fn on_run_command(&mut self, action: &RunCommand, window: &mut Window, cx: &mut Context<Self>) {
        if self.path_prompt.is_some() || self.discard_prompt {
            return;
        }
        let shortcut = self
            .platform
            .contributions()
            .chord_for(&action.command, action.args.as_ref())
            .map(|chord| format!("key.{chord}"));
        if let Some(shortcut) = shortcut {
            self.activated(&shortcut);
        }
        self.execute(&action.command, action.args.as_ref(), window, cx);
    }

    /// Records that the user operated a contributed control. The UI coverage
    /// gate compares these records with [`Self::inventory`].
    pub(crate) fn activated(&mut self, control: &str) {
        self.platform
            .shell_mut()
            .trace_mut()
            .record("ui.activate", control);
    }

    /// Ids of every control modules contributed to this window: menu items,
    /// toolbar buttons, dock tabs and shortcuts (`key.<chord>`).
    pub fn inventory(&self) -> Vec<String> {
        let contributions = self.platform.contributions();
        let mut controls = Vec::new();
        for menu in contributions.menus() {
            controls.extend(
                contributions
                    .menu_items(&menu.id)
                    .into_iter()
                    .map(|item| format!("menu.{}.{}", menu.id, item.id)),
            );
        }
        controls.extend(
            contributions
                .toolbar()
                .into_iter()
                .map(|item| format!("toolbar.{}", item.id)),
        );
        for location in [ViewLocation::Left, ViewLocation::Bottom] {
            controls.extend(
                contributions
                    .views(location)
                    .into_iter()
                    .map(|view| format!("dock.{}.tab.{}", location.as_str(), view.id)),
            );
        }
        controls.extend(
            contributions
                .key_bindings()
                .iter()
                .map(|binding| format!("key.{}", binding.chord)),
        );
        controls
    }

    fn on_filter_edited(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = self
            .platform
            .shell()
            .active_document()
            .map(|document| document.id)
        {
            self.view_event(
                &ViewTarget::Document(index),
                ViewEvent::FilterChanged(text),
                window,
                cx,
            );
        }
    }

    /// Stops accepting input and waits for persistence before asking GPUI to quit.
    fn begin_quit(&mut self, cx: &mut Context<Self>) {
        if self.shutdown.is_some() {
            return;
        }
        if let Some(reason) = self.platform.shell().close_blocker() {
            self.platform.shell_mut().set_status(reason);
            cx.notify();
            return;
        }
        let persistence = self.platform.begin_shutdown();
        self.shutdown = Some(cx.spawn(async move |_, cx| {
            cx.background_executor()
                .spawn(async move {
                    if let Some(writer) = persistence
                        && let Err(error) = writer.shutdown(std::time::Duration::from_secs(2))
                    {
                        eprintln!("State flush failed: {error}");
                    }
                })
                .await;
            cx.update(|cx| cx.quit());
        }));
    }

    /// Applies what the platform asked of the front end after any change.
    pub(crate) fn after_change(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.discard_prompt {
            cx.notify();
            return;
        }
        if self.platform.shell().quit_requested() {
            self.begin_quit(cx);
            return;
        }
        if let Some(request) = self.platform.shell_mut().take_path_request() {
            self.choose_path(request, window, cx);
        }
        let active = self
            .platform
            .shell()
            .active_document()
            .map(|document| document.id);
        if self.active_document != active {
            self.active_document = active;
            if self.path_prompt.is_none() {
                // The previous document's focused control may disappear in the next layout.
                self.focus.focus(window, cx);
            }
        }
        self.source_editors
            .retain(|id, _| self.platform.shell().document(*id).is_some());
        self.sync_filter_input(window, cx);
        let focus_request = self.platform.shell_mut().take_focus_request();
        if focus_request.is_some() {
            self.focus_requested_frame = Some(self.frame.borrow().frame);
        }
        match focus_request {
            Some(FocusTarget::DocumentFilter) => {
                self.filter_input
                    .update(cx, |input, cx| input.focus(window, cx));
            }
            Some(FocusTarget::ViewInput(view)) => {
                if let Some(input) = self.view_inputs.get(&view) {
                    input.update(cx, |input, cx| input.focus(window, cx));
                }
            }
            None if self.path_prompt.is_none()
                && self.focus_requested_frame != Some(self.frame.borrow().frame) =>
            {
                self.focus_requested_frame = None;
                // A closed document can leave focus outside the rendered tree.
                if !self.focus.contains_focused(window, cx) {
                    self.focus.focus(window, cx);
                }
            }
            None => {}
        }
        if let Some(request) = self.platform.shell_mut().take_discard_request() {
            self.confirm_discard(request, window, cx);
        }
        cx.notify();
    }

    fn active_filter_text(&self) -> Option<String> {
        let index = self
            .platform
            .shell()
            .active_document()
            .map(|document| document.id)?;
        match self.platform.document_content(index)? {
            ViewContent::Table(TableModel {
                filter: Some(filter),
                ..
            }) => Some(filter.text),
            _ => None,
        }
    }

    /// Makes the filter field show the active document's filter, e.g. after
    /// switching tabs or after a command changed the filter.
    fn sync_filter_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.active_filter_text()
            && self.filter_input.read(cx).value().as_ref() != text
        {
            self.filter_input
                .update(cx, |input, cx| input.set_value(text, window, cx));
        }
    }
}
