//! Test support for RetroWork.
//!
//! - [`driver`]: what a UI test may do (deliver raw input, observe the
//!   window) and the vocabulary scenarios are written in.
//! - [`scenarios`]: the user workflows, written once and run by every driver.
//! - [`sabotage`]: broken input for negative controls.
//! - [`e2e`]: driving the real executable in a real window with X11 input,
//!   with screenshots and reports.
//! - [`perf`]: measurements, budgets and the verdict of the performance gate.
//!
//! The headless driver over GPUI's test platform lives in `tests/`, because
//! it needs GPUI's `test-support` feature, which the application itself is
//! built without.

pub mod driver;
pub mod e2e;
pub mod perf;
pub mod sabotage;
pub mod scenarios;
