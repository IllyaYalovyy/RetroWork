//! Deliberately broken input, for negative controls.
//!
//! A UI test suite is only worth something if it fails when input does not
//! reach the application. [`Sabotage`] wraps a working driver and breaks one
//! thing; the scenario run through it must then fail.

use std::time::Duration;

use retrowork_platform::TraceEvent;
use retrowork_workbench::semantics::UiSnapshot;

use crate::driver::{Outcome, UiDriver};

/// The one thing a [`Sabotage`] driver breaks.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Fault {
    /// Pointer clicks are not delivered at all.
    DropPointer,
    /// Pointer clicks land this many pixels away from where they were aimed.
    MisaimPointer { dx: f32, dy: f32 },
    /// Key chords and typed text are not delivered.
    DropKeyboard,
}

/// A driver that delivers input like `inner`, except for one [`Fault`].
pub struct Sabotage<'a> {
    inner: &'a mut dyn UiDriver,
    fault: Fault,
    /// Negative controls are expected to time out; do not wait long for it.
    patience: Duration,
}

impl<'a> Sabotage<'a> {
    /// Wraps `inner`. `patience` caps how long scenarios wait for a condition,
    /// because a negative control is expected to time out.
    pub fn new(inner: &'a mut dyn UiDriver, fault: Fault, patience: Duration) -> Self {
        Self {
            inner,
            fault,
            patience,
        }
    }
}

impl UiDriver for Sabotage<'_> {
    fn input_route(&self) -> &'static str {
        self.inner.input_route()
    }

    fn snapshot(&mut self) -> Outcome<UiSnapshot> {
        self.inner.snapshot()
    }

    fn trace_since(&mut self, after: u64) -> Outcome<Vec<TraceEvent>> {
        self.inner.trace_since(after)
    }

    fn pointer_click(&mut self, x: f32, y: f32, count: usize) -> Outcome {
        match self.fault {
            Fault::DropPointer => Ok(()),
            Fault::MisaimPointer { dx, dy } => self.inner.pointer_click(x + dx, y + dy, count),
            Fault::DropKeyboard => self.inner.pointer_click(x, y, count),
        }
    }

    fn key(&mut self, chord: &str) -> Outcome {
        match self.fault {
            Fault::DropKeyboard => Ok(()),
            _ => self.inner.key(chord),
        }
    }

    fn text(&mut self, text: &str) -> Outcome {
        match self.fault {
            Fault::DropKeyboard => Ok(()),
            _ => self.inner.text(text),
        }
    }

    fn choose_folder(&mut self, path: Option<&str>) -> Outcome {
        match self.fault {
            Fault::DropKeyboard => Ok(()),
            _ => self.inner.choose_folder(path),
        }
    }

    fn choose_save_path(&mut self, path: Option<&str>) -> Outcome {
        match self.fault {
            Fault::DropKeyboard => Ok(()),
            _ => self.inner.choose_save_path(path),
        }
    }

    fn pause(&mut self) {
        self.inner.pause();
    }

    fn patience(&self) -> Duration {
        self.patience.min(self.inner.patience())
    }

    fn checkpoint(&mut self, name: &str) -> Outcome {
        self.inner.checkpoint(name)
    }
}
