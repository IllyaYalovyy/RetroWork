//! Source documents backed by the native editor's buffer, selection and undo history.

use super::*;
use gpui::component::input::{Editor, EditorState, InputEditsEvent, Position, RopeExt};
use retrowork_platform::SourceModel;
use std::sync::Arc;

/// Editor state retained only while its document remains open.
pub(super) struct SourceEditor {
    input: Entity<EditorState>,
    text: Arc<str>,
    reveal: Option<usize>,
    _observation: Subscription,
    _edits: Subscription,
}
impl Workbench {
    /// Keeps editor cursor, selection and scrolling independent of streaming output.
    pub(super) fn render_source(
        &mut self,
        target: ViewTarget,
        model: SourceModel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ViewTarget::Document(id) = target else {
            return self.hint("source", "Source content requires a document");
        };
        self.source_editors
            .retain(|id, _| self.platform.shell().document(*id).is_some());
        let editor = self.source_editors.entry(id).or_insert_with(|| {
            let input = cx.new(|cx| {
                EditorState::new(window, cx)
                    .line_number(true)
                    .soft_wrap(false)
            });
            let mut previous = None;
            let edits = cx.subscribe_in(
                &input,
                window,
                move |this, _, event: &InputEditsEvent, window, cx| {
                    if let Some((_, text)) = event.edits().last() {
                        this.view_event(
                            &ViewTarget::Document(id),
                            retrowork_platform::document_session::SourceEvent::Edited(text.clone())
                                .into(),
                            window,
                            cx,
                        );
                    }
                },
            );
            let observation = cx.observe_in(&input, window, move |_, input, window, cx| {
                let state = input.read(cx);
                let current = (
                    state.selected_range(),
                    state.is_selection_reversed(),
                    state.scroll_offset(),
                    state.focus_handle(cx).is_focused(window),
                );
                if previous.as_ref() != Some(&current) {
                    previous = Some(current);
                    cx.notify();
                }
            });
            SourceEditor {
                input,
                text: Arc::from(""),
                reveal: None,
                _observation: observation,
                _edits: edits,
            }
        });
        if !Arc::ptr_eq(&editor.text, &model.text) {
            editor.input.update(cx, |input, cx| {
                let text = model.editing.as_ref().map_or_else(
                    || model.text.to_string(),
                    |session| session.text().to_string(),
                );
                input.set_value(text, window, cx);
                input.set_readonly(model.editing.is_none(), cx);
            });
            editor.text = model.text.clone();
            editor.reveal = None;
        }
        if editor.reveal != model.reveal_line {
            if let Some(line) = model.reveal_line {
                editor.input.update(cx, |input, cx| {
                    input.set_cursor_position(
                        Position::new(line.saturating_sub(1) as u32, 0),
                        window,
                        cx,
                    )
                });
            }
            editor.reveal = model.reveal_line;
        }
        let state = editor.input.read(cx);
        let position = state.cursor_position();
        let selected = state.selected_text().chars().take(4096).collect::<String>();
        let current = state
            .text()
            .slice_line(position.line as usize)
            .chars()
            .take(4096)
            .collect::<String>();
        let save = model.editing.as_ref().map(|session| session.status());
        let save_label = save.as_ref().map_or("Read-only", |status| {
            if status.saving {
                "Saving…"
            } else if status.dirty {
                "Modified"
            } else {
                "Saved"
            }
        });
        let status = format!(
            "{save_label} · Ln {}, Col {}",
            position.line + 1,
            position.character + 1
        );
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        Editor::new(&editor.input)
                            .readonly(model.editing.is_none())
                            .appearance(false)
                            .bordered(false)
                            .h(gpui::relative(1.)),
                    )
                    .child(
                        Probe::new("doc.source", Role::TextField, "Source")
                            .value(current)
                            .focused(state.focus_handle(cx).is_focused(window))
                            .element(&self.frame),
                    ),
            )
            .child(
                div()
                    .relative()
                    .child(if !model.message.is_empty() {
                        model.message.clone()
                    } else if let Some(error) =
                        save.as_ref().and_then(|status| status.error.as_ref())
                    {
                        error.clone()
                    } else {
                        status.clone()
                    })
                    .child(Probe::new("doc.position", Role::Text, status).element(&self.frame))
                    .child(
                        Probe::new(
                            "doc.save-state",
                            Role::Text,
                            if model.message.is_empty() {
                                save.as_ref()
                                    .and_then(|status| status.error.as_deref())
                                    .unwrap_or(save_label)
                            } else {
                                &model.message
                            },
                        )
                        .element(&self.frame),
                    )
                    .child(
                        Probe::new("doc.selection", Role::Text, "Selected text")
                            .value(selected)
                            .element(&self.frame),
                    ),
            )
            .into_any_element()
    }
}
