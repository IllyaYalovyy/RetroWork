//! GPUI presentation of the RetroWork platform.
//!
//! - [`Workbench`] draws what modules contributed to a
//!   [`retrowork_platform::Platform`] and routes real pointer and key input
//!   back to it. It contains no feature logic.
//! - [`semantics`] records every drawn element under a stable id with its
//!   laid-out bounds; tests and automation read that instead of guessing
//!   coordinates.
//! - [`automation`] is the opt-in channel a test driver uses to observe a
//!   running application.
//!
//! Bundled Rust modules do not depend on this crate: they describe content
//! with the platform's view models.

pub mod automation;
pub mod semantics;
mod theme;
mod workbench;

pub use workbench::{KEY_CONTEXT, RunCommand, Workbench, init, open_workbench};

/// Native plugin view registration and rendering.
pub mod views;
