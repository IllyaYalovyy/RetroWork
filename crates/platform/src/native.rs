//! Typed content and interactions for statically linked native views.

use crate::{CommandContext, CommandError, Shell, ViewContent, ViewEvent, ViewProvider};
use std::{any::Any, fmt::Debug, rc::Rc};

/// A native view model and the events understood by its provider.
pub trait NativeModel: Clone + Debug + PartialEq + 'static {
    /// Interactions understood by the provider of this model.
    type Event: Clone + Debug + PartialEq + 'static;
}

/// Supplies typed state and handles only the matching event type.
pub trait NativeProvider {
    /// Rejects closure while owned work would be lost or a write is still running.
    fn can_close(&self) -> Result<(), crate::CloseError> {
        Ok(())
    }
    /// Immutable state and event contract shared with the native renderer.
    type Model: NativeModel;
    /// Returns the current immutable model without performing I/O.
    fn model(&self, shell: &Shell) -> Self::Model;
    /// Applies an event from this provider's native controls.
    fn handle_native(
        &self,
        event: &<Self::Model as NativeModel>::Event,
        cx: &mut CommandContext<'_>,
    ) -> Result<(), CommandError>;
}

impl<T: NativeProvider> ViewProvider for T {
    fn can_close(&self) -> Result<(), crate::CloseError> {
        NativeProvider::can_close(self)
    }
    fn content(&self, shell: &Shell) -> ViewContent {
        ViewContent::Native(NativeValue::new(self.model(shell)))
    }
    fn handle(&self, event: ViewEvent, cx: &mut CommandContext<'_>) -> Result<(), CommandError> {
        match event {
            ViewEvent::Native(value) => {
                let event = value
                    .get::<<T::Model as NativeModel>::Event>()
                    .ok_or_else(|| {
                        CommandError::failed("native view received the wrong event type")
                    })?;
                self.handle_native(event, cx)
            }
            _ => Err(CommandError::failed(
                "native view requires its typed events",
            )),
        }
    }
}

/// Type erasure confined to the platform/frontend boundary; native data is not serialized.
#[derive(Clone, Debug)]
pub struct NativeValue(Rc<dyn Erased>);
impl NativeValue {
    /// Wraps native data while retaining its Rust type and equality behavior.
    pub fn new<T: Clone + Debug + PartialEq + 'static>(value: T) -> Self {
        Self(Rc::new(value))
    }
    /// Reads a value only when its concrete type matches `T`.
    pub fn get<T: 'static>(&self) -> Option<&T> {
        self.0.as_any().downcast_ref()
    }
    /// The concrete Rust identity used to validate native renderer boundaries.
    pub fn concrete_type(&self) -> std::any::TypeId {
        self.0.as_any().type_id()
    }
    /// Names the concrete model type for the renderer registry.
    pub fn type_name(&self) -> &'static str {
        self.0.type_name()
    }
}
impl PartialEq for NativeValue {
    fn eq(&self, other: &Self) -> bool {
        self.0.equals(other.0.as_ref())
    }
}

/// Operations needed after the concrete type crosses the frontend boundary.
trait Erased: Debug {
    fn as_any(&self) -> &dyn Any;
    fn type_name(&self) -> &'static str;
    fn equals(&self, other: &dyn Erased) -> bool;
}
impl<T: Debug + PartialEq + 'static> Erased for T {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn type_name(&self) -> &'static str {
        std::any::type_name::<T>()
    }
    fn equals(&self, other: &dyn Erased) -> bool {
        other
            .as_any()
            .downcast_ref::<T>()
            .is_some_and(|other| self == other)
    }
}
