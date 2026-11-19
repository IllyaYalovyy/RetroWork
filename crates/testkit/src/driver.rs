//! What a UI test can do, independent of how input reaches the application.
//!
//! A [`UiDriver`] delivers raw input (a pointer click at a position, a key
//! chord, typed text) and reads back what the window shows. It has no way to
//! call a command, a handler or a model: a scenario can only succeed if the
//! input really travels through hit testing, focus and key dispatch.
//!
//! [`Ui`] adds the vocabulary scenarios are written in: find an element by its
//! semantic id, click it where it is actually drawn, wait for an observable
//! condition.

use std::time::{Duration, Instant};

use retrowork_platform::TraceEvent;
use retrowork_workbench::semantics::{UiNode, UiSnapshot};

/// Success, or a message that says what was expected and what was seen.
pub type Outcome<T = ()> = Result<T, String>;

/// Delivers raw input to an application and observes its window.
pub trait UiDriver {
    /// How input is delivered, for reports: e.g. `xtest` or `gpui-test-platform`.
    fn input_route(&self) -> &'static str;

    /// The elements of the most recent frame.
    fn snapshot(&mut self) -> Outcome<UiSnapshot>;

    /// Trace events newer than sequence number `after`.
    fn trace_since(&mut self, after: u64) -> Outcome<Vec<TraceEvent>>;

    /// Presses and releases the left button `count` times at a position in
    /// window-relative logical pixels.
    fn pointer_click(&mut self, x: f32, y: f32, count: usize) -> Outcome;

    /// Presses a chord such as `ctrl-shift-s`, `f5` or `escape`.
    fn key(&mut self, chord: &str) -> Outcome;

    /// Types printable ASCII text into whatever has keyboard focus.
    fn text(&mut self, text: &str) -> Outcome;

    /// Selects or cancels a system folder dialog through the driver's native input route.
    fn choose_folder(&mut self, path: Option<&str>) -> Outcome {
        let _ = path;
        Err("system folder input is unavailable in this driver".into())
    }

    /// Selects or cancels the system Save As dialog through native input.
    fn choose_save_path(&mut self, path: Option<&str>) -> Outcome {
        let _ = path;
        Err("system save input is unavailable in this driver".into())
    }

    /// Lets the application make progress before the next observation.
    fn pause(&mut self);

    /// How long [`Ui::wait_for`] keeps observing before it gives up.
    fn patience(&self) -> Duration;

    /// Records evidence of the current state under `name`, if the driver can.
    fn checkpoint(&mut self, name: &str) -> Outcome {
        let _ = name;
        Ok(())
    }
}

/// Scenario vocabulary over a [`UiDriver`].
pub struct Ui<'a> {
    driver: &'a mut dyn UiDriver,
    /// Replaces the driver's patience inside [`Self::allowing`].
    patience: Option<Duration>,
}

impl<'a> Ui<'a> {
    /// Scenario operations over `driver`.
    pub fn new(driver: &'a mut dyn UiDriver) -> Self {
        Self {
            driver,
            patience: None,
        }
    }

    /// Completes the system folder dialog without calling an application command.
    pub fn choose_folder(&mut self, path: Option<&str>) -> Outcome {
        self.driver.choose_folder(path)
    }

    /// Completes the system Save As dialog without calling an application command.
    pub fn choose_save_path(&mut self, path: Option<&str>) -> Outcome {
        self.driver.choose_save_path(path)
    }

    /// Runs `steps` with waits that may last up to `limit`, for work that
    /// takes real time such as compiling. Never shortens the driver's own
    /// patience.
    pub fn allowing<T>(
        &mut self,
        limit: Duration,
        steps: impl FnOnce(&mut Self) -> Outcome<T>,
    ) -> Outcome<T> {
        let previous = self.patience.replace(limit.max(self.driver.patience()));
        let result = steps(self);
        self.patience = previous;
        result
    }

    fn patience(&self) -> Duration {
        self.patience.unwrap_or_else(|| self.driver.patience())
    }

    /// Observes until `condition` holds and returns that snapshot.
    ///
    /// This is the only way scenarios wait: on something the window shows,
    /// never for a fixed time.
    pub fn wait_for(
        &mut self,
        what: &str,
        condition: impl Fn(&UiSnapshot) -> bool,
    ) -> Outcome<UiSnapshot> {
        let deadline = Instant::now() + self.patience();
        loop {
            let snapshot = self.driver.snapshot()?;
            if condition(&snapshot) {
                return Ok(snapshot);
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "timed out waiting for {what}\n  status: {:?}\n  elements: {}",
                    snapshot.text("status.message").unwrap_or("<none>"),
                    describe(&snapshot)
                ));
            }
            self.driver.pause();
        }
    }

    /// Waits until `id` is drawn and not clipped, and returns it.
    pub fn visible(&mut self, id: &str) -> Outcome<UiNode> {
        let snapshot = self.wait_for(&format!("`{id}` to be visible"), |snapshot| {
            snapshot.find(id).is_some_and(|node| node.visible.is_some())
        })?;
        Ok(snapshot.find(id).expect("condition checked it").clone())
    }

    fn click_times(&mut self, id: &str, count: usize) -> Outcome {
        // Aim at where the element is once layout has settled: two
        // consecutive observations must agree.
        let mut target = self.visible(id)?;
        loop {
            self.driver.pause();
            let again = self.visible(id)?;
            if again.visible == target.visible {
                break;
            }
            target = again;
        }
        let (x, y) = target.visible.expect("`visible` returned it").center();
        self.driver.pointer_click(x, y, count)
    }

    /// Clicks the centre of the visible part of element `id`.
    pub fn click(&mut self, id: &str) -> Outcome {
        self.click_times(id, 1)
    }

    /// Double-clicks the centre of the visible part of element `id`.
    pub fn double_click(&mut self, id: &str) -> Outcome {
        self.click_times(id, 2)
    }

    /// Clicks a window position directly, for negative controls.
    pub fn click_at(&mut self, x: f32, y: f32) -> Outcome {
        self.driver.pointer_click(x, y, 1)
    }

    /// Presses a key chord such as `ctrl-f`.
    pub fn press(&mut self, chord: &str) -> Outcome {
        self.driver.key(chord)
    }

    /// Types text into the element with keyboard focus.
    pub fn type_text(&mut self, text: &str) -> Outcome {
        self.driver.text(text)
    }

    /// Waits until element `id` shows exactly `expected`.
    pub fn expect_text(&mut self, id: &str, expected: &str) -> Outcome {
        self.wait_for(&format!("`{id}` to read {expected:?}"), |snapshot| {
            snapshot.text(id) == Some(expected)
        })
        .map(drop)
    }

    /// Waits until element `id` shows text containing `part`.
    pub fn expect_text_contains(&mut self, id: &str, part: &str) -> Outcome {
        self.wait_for(&format!("`{id}` to contain {part:?}"), |snapshot| {
            snapshot.text(id).is_some_and(|text| text.contains(part))
        })
        .map(drop)
    }

    /// Waits until element `id` is drawn with the given selection state.
    pub fn expect_selected(&mut self, id: &str, selected: bool) -> Outcome {
        self.wait_for(&format!("`{id}` selected = {selected}"), |snapshot| {
            snapshot
                .find(id)
                .is_some_and(|node| node.selected == selected)
        })
        .map(drop)
    }

    /// Waits until element `id` has keyboard focus.
    pub fn expect_focused(&mut self, id: &str) -> Outcome {
        self.wait_for(&format!("`{id}` to have keyboard focus"), |snapshot| {
            snapshot.find(id).is_some_and(|node| node.focused)
        })
        .map(drop)
    }

    /// Waits until element `id` is drawn.
    pub fn expect_present(&mut self, id: &str) -> Outcome {
        self.wait_for(&format!("`{id}` to be present"), |snapshot| {
            snapshot.find(id).is_some()
        })
        .map(drop)
    }

    /// Waits until element `id` is no longer drawn.
    pub fn expect_absent(&mut self, id: &str) -> Outcome {
        self.wait_for(&format!("`{id}` to be absent"), |snapshot| {
            snapshot.find(id).is_none()
        })
        .map(drop)
    }

    /// Waits until exactly `count` elements have an id starting with `prefix`.
    pub fn expect_count(&mut self, prefix: &str, count: usize) -> Outcome<UiSnapshot> {
        self.wait_for(&format!("{count} elements `{prefix}*`"), |snapshot| {
            snapshot.with_prefix(prefix).len() == count
        })
    }

    /// Waits until some line of the Output view contains `part`.
    pub fn expect_output_contains(&mut self, part: &str) -> Outcome {
        self.wait_for(&format!("an Output line containing {part:?}"), |snapshot| {
            snapshot
                .with_prefix("view.output.line.")
                .iter()
                .any(|line| line.label.contains(part))
        })
        .map(drop)
    }

    /// Sequence number of the newest trace event, to pass to
    /// [`Self::expect_trace`] later.
    pub fn trace_mark(&mut self) -> Outcome<u64> {
        Ok(self.driver.snapshot()?.trace_seq)
    }

    /// Checks that the trace after `mark` contains events matching `expected`
    /// in this order (other events may lie between). Each expectation is
    /// `(kind, detail-substring)`.
    pub fn expect_trace(&mut self, mark: u64, expected: &[(&str, &str)]) -> Outcome {
        let deadline = Instant::now() + self.patience();
        loop {
            let events = self.driver.trace_since(mark)?;
            let mut remaining = expected.iter();
            let mut next = remaining.next();
            for event in &events {
                if let Some((kind, detail)) = next
                    && event.kind == *kind
                    && event.detail.contains(detail)
                {
                    next = remaining.next();
                }
            }
            match next {
                None => return Ok(()),
                Some(missing) if Instant::now() >= deadline => {
                    let seen: Vec<String> = events
                        .iter()
                        .map(|event| format!("{} {}", event.kind, event.detail))
                        .collect();
                    return Err(format!(
                        "trace has no `{} {}` in the expected order {expected:?}; trace: {seen:?}",
                        missing.0, missing.1
                    ));
                }
                Some(_) => self.driver.pause(),
            }
        }
    }

    /// Checks that nothing of kind `kind` was traced after `mark`.
    pub fn expect_no_trace(&mut self, mark: u64, kind: &str) -> Outcome {
        // Give a wrongly triggered event the chance to show up.
        self.driver.pause();
        self.driver.pause();
        match self
            .driver
            .trace_since(mark)?
            .into_iter()
            .find(|event| event.kind == kind)
        {
            Some(event) => Err(format!(
                "unexpected trace event `{} {}`",
                event.kind, event.detail
            )),
            None => Ok(()),
        }
    }

    /// Asks the driver to keep evidence, such as a screenshot, under `name`.
    pub fn checkpoint(&mut self, name: &str) -> Outcome {
        self.driver.checkpoint(name)
    }
}

/// A compact listing of what a snapshot contains, for failure messages.
pub fn describe(snapshot: &UiSnapshot) -> String {
    snapshot
        .nodes
        .iter()
        .map(|node| {
            let mut text = node.id.clone();
            if node.selected {
                text.push_str("[selected]");
            }
            if node.focused {
                text.push_str("[focused]");
            }
            if let Some(value) = &node.value {
                text.push_str(&format!("={value:?}"));
            }
            text
        })
        .collect::<Vec<_>>()
        .join(", ")
}
