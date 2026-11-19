//! Typed native renderers with view-scoped events and observable controls.

use crate::{
    Workbench,
    semantics::{Control, SharedFrame},
};
use gpui::{AnyElement, App, Context, FocusHandle, IntoElement, WeakEntity, Window};
use retrowork_platform::{
    ActivationContext, ActivationError, Module, ModuleId, ModuleManifest, NativeModel, NativeValue,
    Registry, ViewEvent, ViewTarget,
};
use std::{collections::HashMap, marker::PhantomData, rc::Rc};

/// Module that owns the native renderer registry.
pub const MODULE_ID: &str = "retrowork.workbench";

/// An event destination bound to one provider and one observed control.
pub struct EventSink<E> {
    owner: WeakEntity<Workbench>,
    target: ViewTarget,
    control: String,
    focus: FocusHandle,
    event: PhantomData<E>,
}
impl<E: Clone + std::fmt::Debug + PartialEq + 'static> EventSink<E> {
    /// Sends an event through its owning provider. Returns false after the window is gone.
    pub fn send(&self, event: E, window: &mut Window, cx: &mut App) -> bool {
        self.focus.focus(window, cx);
        self.owner
            .update(cx, |workbench, cx| {
                workbench.activated(&self.control);
                workbench.view_event(
                    &self.target,
                    ViewEvent::Native(NativeValue::new(event)),
                    window,
                    cx,
                );
            })
            .is_ok()
    }
}

/// Native rendering access without exposing the concrete workbench context.
pub struct ViewContext<'a, E> {
    pub window: &'a mut Window,
    pub app: &'a mut App,
    raw: ScopedView,
    focus: &'a mut HashMap<String, FocusHandle>,
    event: PhantomData<E>,
}

/// The private destination and semantic namespace of one rendered view.
struct ScopedView {
    prefix: String,
    target: ViewTarget,
    owner: WeakEntity<Workbench>,
    frame: SharedFrame,
}
impl<E: Clone + std::fmt::Debug + PartialEq + 'static> ViewContext<'_, E> {
    /// Observes actual bounds and the declared control's state during layout.
    pub fn observe(&self, mut control: Control) -> impl IntoElement + use<E> {
        control.id = format!("{}.{}", self.raw.prefix, control.id);
        control.element(&self.raw.frame)
    }
    /// Retains keyboard focus identity across frames of this view.
    pub fn focus_handle(&mut self, id: &str) -> FocusHandle {
        self.focus
            .entry(format!("{}.{}", self.raw.prefix, id))
            .or_insert_with(|| self.app.focus_handle())
            .clone()
    }
    /// Creates a callback whose emitted events can reach only this view's provider.
    pub fn listener<I: 'static, F: Fn(&I) -> Option<E> + 'static>(
        &mut self,
        id: &str,
        map: F,
    ) -> impl Fn(&I, &mut Window, &mut App) + use<E, I, F> {
        let sink = EventSink {
            owner: self.raw.owner.clone(),
            target: self.raw.target.clone(),
            control: format!("{}.{}", self.raw.prefix, id),
            focus: self.focus_handle(id),
            event: PhantomData,
        };
        move |input, window, cx| {
            if let Some(event) = map(input) {
                cx.stop_propagation();
                sink.send(event, window, cx);
            }
        }
    }
}

/// A renderer accepts only its declared model and emits only that model's event type.
pub trait ViewRenderer: 'static {
    /// State accepted by this renderer and the events its controls may emit.
    type Model: NativeModel;
    /// Every control and value that tests must be able to observe in this view.
    fn controls(&self) -> &'static [&'static str];
    /// Draws native GPUI elements without changing the model or performing I/O.
    fn render(
        &self,
        model: &Self::Model,
        context: &mut ViewContext<'_, <Self::Model as NativeModel>::Event>,
    ) -> AnyElement;
}

/// Erases a renderer only after its model/event relationship has been checked by Rust.
trait ErasedRenderer {
    fn model_type(&self) -> std::any::TypeId;
    fn render(
        &self,
        model: &NativeValue,
        raw: ScopedView,
        focus: &mut HashMap<String, FocusHandle>,
        window: &mut Window,
        app: &mut App,
    ) -> AnyElement;
}
impl<T: ViewRenderer> ErasedRenderer for T {
    fn model_type(&self) -> std::any::TypeId {
        std::any::TypeId::of::<T::Model>()
    }
    fn render(
        &self,
        model: &NativeValue,
        raw: ScopedView,
        focus: &mut HashMap<String, FocusHandle>,
        window: &mut Window,
        app: &mut App,
    ) -> AnyElement {
        for id in self.controls() {
            raw.frame
                .borrow_mut()
                .expected
                .push(format!("{}.{}", raw.prefix, id));
        }
        let model = model
            .get::<T::Model>()
            .expect("registered model type matches its renderer");
        ViewRenderer::render(
            self,
            model,
            &mut ViewContext {
                window,
                app,
                raw,
                focus,
                event: PhantomData,
            },
        )
    }
}

/// Registry of native renderers owned by their contributing modules.
pub struct ViewRenderers {
    renderers: Registry<Rc<dyn ErasedRenderer>>,
}
impl Default for ViewRenderers {
    fn default() -> Self {
        Self {
            renderers: Registry::new(ModuleId::literal(MODULE_ID)),
        }
    }
}
impl ViewRenderers {
    /// Stages a typed renderer and removes it when its module shuts down.
    pub fn register<T: ViewRenderer>(
        &self,
        cx: &mut ActivationContext<'_>,
        renderer: T,
    ) -> Result<(), ActivationError> {
        let controls = renderer.controls();
        let mut unique = std::collections::BTreeSet::new();
        for id in controls {
            if id.is_empty() || !unique.insert(id) {
                return Err(ActivationError::new(
                    "native controls must have unique nonempty IDs",
                ));
            }
        }
        self.renderers
            .register(cx, std::any::type_name::<T::Model>(), Rc::new(renderer))
    }

    pub(crate) fn render(
        &self,
        model: &NativeValue,
        destination: (String, ViewTarget),
        frame: SharedFrame,
        focus: &mut HashMap<String, FocusHandle>,
        window: &mut Window,
        cx: &mut Context<Workbench>,
    ) -> Option<AnyElement> {
        let renderer = self.renderers.get(model.type_name())?;
        if renderer.model_type() != model.concrete_type() {
            return None;
        }
        let (prefix, target) = destination;
        let raw = ScopedView {
            prefix,
            target,
            owner: cx.weak_entity(),
            frame,
        };
        Some(renderer.render(model, raw, focus, window, cx))
    }
}

/// Provides native rendering before UI plugins activate.
pub struct WorkbenchModule;
impl Module for WorkbenchModule {
    fn manifest(&self) -> ModuleManifest {
        ModuleManifest::bundled(MODULE_ID, "Workbench", semver::Version::new(0, 1, 0))
    }
    fn activate(&self, cx: &mut ActivationContext<'_>) -> Result<(), ActivationError> {
        cx.provide(Rc::new(ViewRenderers::default()))
    }
}
