//! Project action planning, task ownership and output.

use super::*;

impl ProjectWorkspace {
    pub(super) fn describe(project: &Project, scope: &Scope) -> String {
        match scope {
            Scope::Project => project.name.clone(),
            Scope::Unit(unit) => project
                .unit(unit)
                .map_or_else(|| unit.clone(), |unit| unit.name.clone()),
            Scope::Target { unit, target } => project
                .unit(unit)
                .and_then(|unit| unit.targets.iter().find(|item| &item.id == target))
                .map_or_else(
                    || target.clone(),
                    |target| format!("{} {}", target.kind.label(), target.name),
                ),
        }
    }

    /// Explains why an action cannot run against the current selection.
    pub(super) fn action_available(&self, action: &ActionId) -> Result<(), String> {
        if self.is_running() {
            return Err("a task is already running; stop it first".into());
        }
        let state = self.state.borrow();
        if state.pending.is_some() {
            return Err("the project is still being opened".into());
        }
        match &state.active.current {
            Some(ProjectSession { project, provider }) => {
                let provider = provider
                    .as_ref()
                    .ok_or("this project has no build-tool provider")?;
                if !provider
                    .actions()
                    .iter()
                    .any(|definition| definition.id == action.as_str())
                {
                    return Err("the current provider does not support this action".into());
                }
                provider.availability(project, action, &state.active.scope)
            }
            _ => Err("no project is open; use File > Open Project".into()),
        }
    }

    /// Plans `action` for the selected scope and starts it.
    pub(super) fn start(
        &self,
        action: &ActionId,
        cx: &mut CommandContext<'_>,
    ) -> Result<(), CommandError> {
        self.action_available(action)
            .map_err(CommandError::Failed)?;
        let (project, provider) = {
            let state = self.state.borrow();
            let session = state
                .active
                .current
                .as_ref()
                .ok_or_else(|| CommandError::failed("no project is open"))?;
            (
                Arc::clone(&session.project),
                Arc::clone(session.provider.as_ref().ok_or_else(|| {
                    CommandError::failed("this project has no build-tool provider")
                })?),
            )
        };
        let definition = provider
            .actions()
            .iter()
            .find(|definition| definition.id == action.as_str())
            .ok_or_else(|| CommandError::failed("provider does not declare this action"))?;
        let scope = self.state.borrow().active.scope.clone();
        let spec = provider
            .plan(
                &project,
                action,
                &scope,
                cx.args.unwrap_or(&serde_json::Value::Null),
            )
            .map_err(CommandError::Failed)?;
        let target = Self::describe(&project, &scope);

        let generation = {
            let mut state = self.state.borrow_mut();
            state.next_generation += 1;
            state.active.generation = state.next_generation;
            state.active.output.clear();
            state
                .active
                .output
                .push(format!("> {}", spec.command_line()), Tone::Muted);
            state.active.problems.clear();
            state.active.selected_problem = None;
            state.active.generation
        };
        let inbox = cx.services.inbox();
        let handle = task::spawn(&spec, move |event| {
            // Interpreting happens here, off the UI thread.
            let update = match event {
                TaskEvent::Output(batch) => {
                    let items = batch
                        .lines
                        .iter()
                        .flat_map(|line| provider.interpret(&project, line))
                        .collect::<Vec<_>>();
                    Update::Output { items, batch }
                }
                TaskEvent::Finished { outcome, duration } => Update::Finished { outcome, duration },
            };
            let _ = inbox.post_blocking(Box::new(move |cx| {
                if let Some(workspace) = cx.services.get::<ProjectWorkspace>() {
                    workspace.apply(generation, update, cx);
                }
            }));
        })
        .map_err(CommandError::Failed)?;

        let message = format!("{} {target}: running…", definition.title);
        cx.shell.trace_mut().record(
            "task.started",
            format!("{} {}", definition.title, spec.command_line()),
        );
        cx.shell.set_status(message);
        cx.shell.show_view(ViewId::literal(VIEW_OUTPUT));
        self.state.borrow_mut().active.task = Some(RunningTask {
            handle,
            action: definition,
            target,
        });
        Ok(())
    }

    pub(super) fn apply(&self, generation: u64, update: Update, cx: &mut CommandContext<'_>) {
        let mut state = self.state.borrow_mut();
        let is_active = state.active.generation == generation;
        let session = if is_active {
            &mut state.active
        } else {
            let Some(session) = state
                .others
                .values_mut()
                .find(|session| session.generation == generation)
            else {
                return;
            };
            session
        };
        match update {
            Update::Output { items, batch } => {
                for item in items {
                    match item {
                        Interpretation::Line { text, tone } => session.output.push(text, tone),
                        Interpretation::Diagnostic(diagnostic) => session.problems.push(diagnostic),
                    }
                }
                // Releases the runner to deliver the next batch.
                drop(batch);
            }
            Update::Finished { outcome, duration } => {
                let Some(running) = session.task.take() else {
                    return;
                };
                let result = TaskResult {
                    action: running.action,
                    target: running.target,
                    outcome,
                    duration,
                };
                let summary = summary(&result, &session.problems);
                cx.shell.trace_mut().record("task.finished", &summary);
                if is_active {
                    cx.shell.set_status(summary);
                }
                session.last = Some(result);
            }
        }
    }

    pub(super) fn stop(&self, cx: &mut CommandContext<'_>) -> Result<(), CommandError> {
        let state = self.state.borrow();
        if let Some(discovery) = &state.pending {
            if !discovery.cancel() {
                cx.shell.set_status("Finishing project import");
                return Ok(());
            }
            cx.shell.set_status("Stopping project discovery…");
            return Ok(());
        }
        let running = state
            .active
            .task
            .as_ref()
            .or_else(|| {
                state
                    .others
                    .values()
                    .find_map(|session| session.task.as_ref())
            })
            .ok_or_else(|| CommandError::failed("nothing is running"))?;
        running.handle.cancel();
        cx.shell
            .trace_mut()
            .record("task.cancel", running.action.title);
        cx.shell.set_status(format!(
            "Stopping {} {}…",
            running.action.title, running.target
        ));
        Ok(())
    }

    pub(super) fn header(&self) -> TextLine {
        let state = self.state.borrow();
        let mut line = if let Some(running) = &state.active.task {
            TextLine {
                text: format!("{} {}: running", running.action.title, running.target),
                tone: Tone::Info,
            }
        } else if let Some(last) = &state.active.last {
            let tone = match last.outcome {
                TaskOutcome::Exited(0) => Tone::Info,
                TaskOutcome::Cancelled => Tone::Warning,
                _ => Tone::Error,
            };
            TextLine {
                text: summary(last, &state.active.problems),
                tone,
            }
        } else {
            TextLine {
                text: "No task has run yet.".to_owned(),
                tone: Tone::Muted,
            }
        };
        if state.active.output.dropped > 0 {
            line.text.push_str(&format!(
                " ({} earlier lines not kept)",
                state.active.output.dropped
            ));
        }
        line
    }
}

/// One line describing how a task ended, e.g.
/// `Build app: failed (exit code 101) after 1.2 s; 2 errors, 1 warning`.
pub(super) fn summary(result: &TaskResult, problems: &[Diagnostic]) -> String {
    let state = match &result.outcome {
        TaskOutcome::Exited(0) => "succeeded".to_owned(),
        TaskOutcome::Exited(code) => format!("failed (exit code {code})"),
        TaskOutcome::Signalled(signal) => format!("failed (signal {signal})"),
        TaskOutcome::Cancelled => "cancelled".to_owned(),
    };
    let mut text = format!(
        "{} {}: {state} after {:.1} s",
        result.action.title,
        result.target,
        result.duration.as_secs_f64()
    );
    let count = |severity| {
        problems
            .iter()
            .filter(|problem| problem.severity == severity)
            .count()
    };
    let (errors, warnings) = (count(Severity::Error), count(Severity::Warning));
    let plural = |n: usize, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });
    if errors + warnings > 0 {
        text.push_str(&format!(
            "; {}, {}",
            plural(errors, "error"),
            plural(warnings, "warning")
        ));
    }
    text
}
