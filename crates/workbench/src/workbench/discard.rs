//! Discard confirmation using the component library's modal and button controls.

use super::*;
use gpui::component::{WindowExt as _, button::Button, dialog::DialogFooter};
use retrowork_platform::shell::DiscardRequest;

impl Workbench {
    /// Presents consent for one document revision. Enter and Escape both keep the edits.
    pub(super) fn confirm_discard(
        &mut self,
        request: DiscardRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(document) = self.platform.shell().document(request.document()) else {
            return;
        };
        let title = format!("Discard changes to {}?", document.title);
        self.discard_prompt = true;
        self.open_menu = None;
        let owner = cx.weak_entity();
        let frame = self.frame.clone();
        let automation = self.automation_enabled;
        self.platform
            .shell_mut()
            .trace_mut()
            .record("dialog.opened", "discard");
        window.open_dialog(cx, move |dialog, _, _| {
            let cancel = owner.clone();
            let enter = owner.clone();
            let footer = DialogFooter::new().children([(false, "Cancel"), (true, "Discard")].map(
                |(discard, label)| {
                    let owner = owner.clone();
                    let id = if discard {
                        "dialog.discard.confirm"
                    } else {
                        "dialog.discard.cancel"
                    };
                    div()
                        .relative()
                        .child(Button::new(id).label(label).on_click(move |_, window, cx| {
                            window.close_dialog(cx);
                            let _ = owner.update(cx, |this, cx| {
                                this.finish_discard(request, discard, window, cx)
                            });
                        }))
                        .child(Probe::new(id, Role::Button, label).element(&frame))
                },
            ));
            dialog
                .title(title.clone())
                .child("Unsaved edits will be lost. Cancel to keep editing or save the file.")
                .close_button(false)
                .overlay_closable(false)
                .footer(footer)
                .when(automation, |dialog| {
                    dialog.child(crate::semantics::frame_marker(&frame))
                })
                .on_cancel(move |_, window, cx| {
                    let _ = cancel.update(cx, |this, cx| {
                        this.finish_discard(request, false, window, cx)
                    });
                    true
                })
                .on_ok(move |_, window, cx| {
                    let _ = enter.update(cx, |this, cx| {
                        this.finish_discard(request, false, window, cx)
                    });
                    true
                })
        });
    }

    /// Lets the library finish dismissal before another document can ask for consent.
    fn finish_discard(
        &mut self,
        request: DiscardRequest,
        discard: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.discard_prompt {
            return;
        }
        self.platform.shell_mut().trace_mut().record(
            "dialog.closed",
            if discard {
                "discard.confirm"
            } else {
                "discard.cancel"
            },
        );
        self.platform.answer_discard(request, discard);
        let owner = cx.weak_entity();
        window.on_next_frame(move |window, cx| {
            let _ = owner.update(cx, |this, cx| {
                this.discard_prompt = false;
                this.after_change(window, cx);
            });
        });
        cx.notify();
    }
}
