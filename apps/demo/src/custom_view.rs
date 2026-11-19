//! Optional native view sample exercising typed controls and events.

use gpui::{
    AnyElement, ClickEvent, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, div,
};
use retrowork_platform::{
    ActivationContext, ActivationError, CommandContext, CommandError, Dependency, Module,
    ModuleManifest, NativeModel, NativeProvider, Shell, ViewId, ViewLocation,
};
use retrowork_workbench::{
    semantics::{Control, Role},
    views::{ViewContext, ViewRenderer, ViewRenderers},
};
use std::{cell::Cell, rc::Rc};

/// Registers a custom native view only in the sample application configuration.
pub struct CustomViewModule;
impl Module for CustomViewModule {
    fn manifest(&self) -> ModuleManifest {
        ModuleManifest::bundled(
            "sample.native-view",
            "Native View Sample",
            semver::Version::new(0, 1, 0),
        )
        .depends_on(Dependency::literal(
            retrowork_workbench::views::MODULE_ID,
            "^0.1",
        ))
    }
    fn activate(&self, cx: &mut ActivationContext<'_>) -> Result<(), ActivationError> {
        cx.require_service::<ViewRenderers>()?
            .register(cx, CounterRenderer)?;
        cx.view(
            ViewId::literal("sample.counter"),
            "Native Sample",
            ViewLocation::Bottom,
            60,
            Rc::new(Counter(Cell::new(0))),
        )
    }
}

/// Immutable state shown by the native counter.
#[derive(Clone, Debug, PartialEq)]
struct CounterModel(u64);
/// Interactions accepted by the counter provider.
#[derive(Clone, Debug, PartialEq)]
enum CounterEvent {
    Increment,
}
impl NativeModel for CounterModel {
    type Event = CounterEvent;
}

/// Sample state owned entirely by its plugin.
struct Counter(Cell<u64>);
impl NativeProvider for Counter {
    type Model = CounterModel;
    fn model(&self, _: &Shell) -> CounterModel {
        CounterModel(self.0.get())
    }
    fn handle_native(
        &self,
        _: &CounterEvent,
        _: &mut CommandContext<'_>,
    ) -> Result<(), CommandError> {
        self.0.set(self.0.get().saturating_add(1));
        Ok(())
    }
}

/// Draws a native control with pointer, keyboard and observation support.
struct CounterRenderer;
impl ViewRenderer for CounterRenderer {
    type Model = CounterModel;
    fn controls(&self) -> &'static [&'static str] {
        &["value", "increment"]
    }
    fn render(
        &self,
        model: &CounterModel,
        context: &mut ViewContext<'_, CounterEvent>,
    ) -> AnyElement {
        let label = format!("Count: {}", model.0);
        let focus = context.focus_handle("increment");
        let focused = focus.is_focused(context.window);
        div()
            .p_3()
            .flex()
            .gap_3()
            .child(
                div()
                    .relative()
                    .child(label.clone())
                    .child(context.observe(Control::new("value", Role::Text, label))),
            )
            .child(
                div()
                    .id("sample-increment")
                    .relative()
                    .px_2()
                    .border_1()
                    .track_focus(&focus)
                    .child("Increment")
                    .child(context.observe(
                        Control::new("increment", Role::Button, "Increment").focused(focused),
                    ))
                    .on_click(
                        context
                            .listener("increment", |_: &ClickEvent| Some(CounterEvent::Increment)),
                    ),
            )
            .into_any_element()
    }
}
