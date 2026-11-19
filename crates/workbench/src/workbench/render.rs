//! Workbench layout and chrome.

use super::*;
mod content;

impl Workbench {
    fn render_menu_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let contributions = self.platform.contributions();
        let menus: Vec<(MenuId, String)> = contributions
            .menus()
            .into_iter()
            .filter(|menu| !contributions.menu_items(&menu.id).is_empty())
            .map(|menu| (menu.id.clone(), menu.title.clone()))
            .collect();
        div()
            .h(px(MENU_BAR_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .px_1()
            .border_b_1()
            .border_color(border_light())
            .children(menus.into_iter().map(|(id, title)| {
                let open = self.open_menu.as_ref() == Some(&id);
                let element_id = format!("menu.{id}");
                div()
                    .id(eid(element_id.clone()))
                    .relative()
                    .h_full()
                    .px_2()
                    .flex()
                    .items_center()
                    .map(|this| {
                        if open {
                            this.bg(selection()).text_color(selection_text())
                        } else {
                            this.hover(|style| style.bg(hover()))
                        }
                    })
                    .on_click(cx.listener({
                        let id = id.clone();
                        move |this, _: &ClickEvent, _, cx| {
                            this.open_menu =
                                (this.open_menu.as_ref() != Some(&id)).then(|| id.clone());
                            cx.notify();
                        }
                    }))
                    .on_hover(cx.listener({
                        let id = id.clone();
                        move |this, hovered: &bool, _, cx| {
                            // With a menu open, moving along the bar switches menus.
                            if *hovered && this.open_menu.as_ref().is_some_and(|open| open != &id) {
                                this.open_menu = Some(id.clone());
                                cx.notify();
                            }
                        }
                    }))
                    .child(title.clone())
                    .child(
                        Probe::new(element_id, Role::Menu, title)
                            .selected(open)
                            .element(&self.frame),
                    )
            }))
    }

    /// The open menu's popup over a backdrop that swallows outside clicks.
    fn render_open_menu(&self, left: f32, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let menu = self.open_menu.clone()?;
        let contributions = self.platform.contributions();
        let items = contributions.menu_items(&menu).into_iter().map(|item| {
            let element_id = format!("menu.{menu}.{}", item.id);
            let chord = contributions
                .chord_for(&item.command, item.args.as_ref())
                .map(|chord| chord.display())
                .unwrap_or_default();
            let (command, args) = (item.command.clone(), item.args.clone());
            let disabled = self
                .platform
                .command_disabled_reason(&command, args.as_ref());
            div()
                .id(eid(element_id.clone()))
                .relative()
                .h(px(24.))
                .px_3()
                .flex()
                .items_center()
                .justify_between()
                .gap_6()
                .hover(|style| style.bg(selection()).text_color(selection_text()))
                // The backdrop closes the menu on mouse down; an item must
                // survive until its click completes.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener({
                    let control = element_id.clone();
                    move |this, _: &ClickEvent, window, cx| {
                        this.activated(&control);
                        this.execute(&command, args.as_ref(), window, cx);
                    }
                }))
                .when_some(disabled.clone(), |style, reason| {
                    style
                        .text_color(text_muted())
                        .tooltip(move |window, cx| Tooltip::new(reason.clone()).build(window, cx))
                })
                .child(item.title.clone())
                .child(div().text_color(text_muted()).child(chord))
                .child(
                    Probe::new(element_id, Role::MenuItem, item.title.clone())
                        .disabled_reason(disabled)
                        .element(&self.frame),
                )
        });
        Some(
            div()
                .id("menu-backdrop")
                .absolute()
                .top(px(MENU_BAR_HEIGHT))
                .left_0()
                .right_0()
                .bottom_0()
                .occlude()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.open_menu = None;
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .left(px(left))
                        .min_w(px(230.))
                        .py_1()
                        .bg(surface())
                        .border_1()
                        .border_color(border())
                        .shadow_md()
                        .children(items),
                ),
        )
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let items = self
            .platform
            .contributions()
            .toolbar()
            .into_iter()
            .map(|item| {
                let element_id = format!("toolbar.{}", item.id);
                let (command, args) = (item.command.clone(), item.args.clone());
                let disabled = self
                    .platform
                    .command_disabled_reason(&command, args.as_ref());
                div()
                    .id(eid(element_id.clone()))
                    .relative()
                    .h(px(22.))
                    .px_2()
                    .flex()
                    .items_center()
                    .rounded(px(3.))
                    .border_1()
                    .border_color(border_light())
                    .bg(surface())
                    .hover(|style| style.bg(hover()).border_color(accent()))
                    .active(|style| style.bg(chrome_dark()))
                    .on_click(cx.listener({
                        let control = element_id.clone();
                        move |this, _: &ClickEvent, window, cx| {
                            this.activated(&control);
                            this.execute(&command, args.as_ref(), window, cx);
                        }
                    }))
                    .when_some(disabled.clone(), |style, reason| {
                        style.text_color(text_muted()).tooltip(move |window, cx| {
                            Tooltip::new(reason.clone()).build(window, cx)
                        })
                    })
                    .child(item.title.clone())
                    .child(
                        Probe::new(element_id, Role::Button, item.title.clone())
                            .disabled_reason(disabled)
                            .element(&self.frame),
                    )
            });
        div()
            .h(px(TOOLBAR_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .gap_1()
            .px_2()
            .border_b_1()
            .border_color(border())
            .children(items)
    }

    fn tab(&self, element_id: String, title: &str, selected: bool) -> Stateful<Div> {
        div()
            .id(eid(element_id.clone()))
            .relative()
            .h_full()
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .border_r_1()
            .border_color(border())
            .map(|this| {
                if selected {
                    this.bg(surface()).child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .right_0()
                            .h(px(2.))
                            .bg(accent()),
                    )
                } else {
                    this.bg(chrome_dark())
                        .text_color(text_muted())
                        .hover(|style| style.bg(hover()))
                }
            })
            .child(title.to_owned())
            .child(
                Probe::new(element_id, Role::Tab, title)
                    .selected(selected)
                    .element(&self.frame),
            )
    }

    fn tab_strip(&self) -> Div {
        div()
            .h(px(TAB_STRIP_HEIGHT))
            .flex_none()
            .flex()
            .bg(chrome())
            .border_b_1()
            .border_color(border())
    }

    fn render_dock(
        &mut self,
        location: ViewLocation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let views: Vec<(ViewId, String)> = self
            .platform
            .contributions()
            .views(location)
            .into_iter()
            .map(|view| (view.id.clone(), view.title.clone()))
            .collect();
        let active = self.platform.shell().active_view(location).cloned();
        let tabs = self
            .tab_strip()
            .children(views.into_iter().map(|(id, title)| {
                let selected = active.as_ref() == Some(&id);
                self.tab(
                    format!("dock.{}.tab.{id}", location.as_str()),
                    &title,
                    selected,
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.activated(&format!("dock.{}.tab.{id}", location.as_str()));
                    if this.shutdown.is_some() {
                        return;
                    }
                    this.platform.activate_view(&id);
                    this.after_change(window, cx);
                }))
            }));
        let content = active.and_then(|view| {
            let content = self.platform.view_content(&view)?;
            Some(self.render_content(
                format!("view.{view}"),
                ViewTarget::View(view),
                content,
                window,
                cx,
            ))
        });
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(surface())
            .child(tabs)
            .child(div().flex_1().min_h_0().children(content))
    }

    fn render_center(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let shell = self.platform.shell();
        let active = shell.active_document().map(|document| document.id);
        let documents: Vec<(retrowork_platform::DocumentId, String, String)> = shell
            .documents()
            .iter()
            .map(|document| {
                (
                    document.id,
                    format!("doc.tab.{}:{}", document.kind, document.resource),
                    if document.provider.can_close().is_err() {
                        format!("{} *", document.title)
                    } else {
                        document.title.clone()
                    },
                )
            })
            .collect();
        let tabs =
            self.tab_strip()
                .children(documents.into_iter().map(|(index, element_id, title)| {
                    let close_id = format!("{element_id}.close");
                    self.tab(element_id, &title, active == Some(index))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            if this.shutdown.is_some() {
                                return;
                            }
                            this.platform.activate_document(index);
                            this.after_change(window, cx);
                        }))
                        .child(
                            div()
                                .id(eid(close_id.clone()))
                                .relative()
                                .px_1()
                                .rounded(px(2.))
                                .text_color(text_muted())
                                .hover(|style| style.bg(chrome_dark()).text_color(text()))
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    // Do not let the tab under the button activate.
                                    cx.stop_propagation();
                                    if this.shutdown.is_some() {
                                        return;
                                    }
                                    this.platform.close_document(index);
                                    this.after_change(window, cx);
                                }))
                                .child("×")
                                .child(
                                    Probe::new(close_id, Role::Button, format!("Close {title}"))
                                        .element(&self.frame),
                                ),
                        )
                }));
        let content =
            match active.and_then(|index| Some((index, self.platform.document_content(index)?))) {
                Some((index, content)) => self.render_content(
                    "doc".to_owned(),
                    ViewTarget::Document(index),
                    content,
                    window,
                    cx,
                ),
                None => self.hint("doc", &self.no_document_hint()),
            };
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(surface())
            .child(tabs)
            .child(div().flex_1().min_h_0().child(content))
    }

    /// What the centre says when no document is open: where documents come
    /// from, named by the views the modules docked on the left.
    fn no_document_hint(&self) -> String {
        let titles: Vec<&str> = self
            .platform
            .contributions()
            .views(ViewLocation::Left)
            .into_iter()
            .map(|view| view.title.as_str())
            .collect();
        match titles.as_slice() {
            [] => "No document is open.".to_owned(),
            [only] => format!("No document is open. Open one from {only}."),
            [rest @ .., last] => format!(
                "No document is open. Open one from {} or {last}.",
                rest.join(", ")
            ),
        }
    }

    pub(super) fn hint(&self, prefix: &str, text: &str) -> AnyElement {
        div()
            .relative()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .text_color(text_muted())
            .child(text.to_owned())
            .child(Probe::new(format!("{prefix}.empty"), Role::Text, text).element(&self.frame))
            .into_any_element()
    }

    fn render_status_bar(&self) -> impl IntoElement {
        let status = self.platform.shell().status().to_owned();
        let modules = self.platform.modules();
        let active = modules.iter().filter(|module| module.is_active()).count();
        let modules = if active == modules.len() {
            format!("{active} modules")
        } else {
            format!("{active} modules, {} not loaded", modules.len() - active)
        };
        div()
            .h(px(STATUS_BAR_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .justify_between()
            .px_2()
            .border_t_1()
            .border_color(border())
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(status.clone())
                    .child(Probe::new("status.message", Role::Text, status).element(&self.frame)),
            )
            .child(
                div()
                    .relative()
                    .flex_none()
                    .pl_3()
                    .text_color(text_muted())
                    .child(modules.clone())
                    .child(Probe::new("status.modules", Role::Text, modules).element(&self.frame)),
            )
    }
}

impl Focusable for Workbench {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Workbench {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The popup is placed under its title, whose position the previous
        // frame recorded.
        let menu_left = {
            let mut frame = self.frame.borrow_mut();
            let left = self.open_menu.as_ref().and_then(|menu| {
                let id = format!("menu.{menu}");
                frame
                    .nodes
                    .iter()
                    .find(|node| node.id == id)
                    .map(|node| node.bounds.x)
            });
            frame.frame += 1;
            frame.nodes.clear();
            frame.trace_seq = self.platform.shell().trace().last_seq();
            frame.marker = None;
            frame.expected.clear();
            left.unwrap_or(0.)
        };

        let has_left_dock = !self
            .platform
            .contributions()
            .views(ViewLocation::Left)
            .is_empty();
        let left_dock = has_left_dock.then(|| {
            div()
                .w(px(LEFT_DOCK_WIDTH))
                .flex_none()
                .border_r_1()
                .border_color(border())
                .child(self.render_dock(ViewLocation::Left, window, cx))
        });
        let center = self.render_center(window, cx);
        let bottom_dock = self.render_dock(ViewLocation::Bottom, window, cx);

        div()
            .id("workbench")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::on_run_command))
            // Record every pointer press the window receives, before any
            // element handles it and regardless of what is under the pointer,
            // so the trace shows which input led to which command. (Key input
            // is recorded by the keystroke interceptor installed in `new`.)
            .child(
                canvas(|_, _, _| {}, {
                    let this = cx.weak_entity();
                    move |_, _, window, _| {
                        window.on_mouse_event(move |event: &MouseDownEvent, phase, _, cx| {
                            if phase == DispatchPhase::Capture {
                                let _ = this.update(cx, |this, _| {
                                    this.platform.shell_mut().trace_mut().record(
                                        "ui.mouse_down",
                                        format!(
                                            "{:.0},{:.0}",
                                            f32::from(event.position.x),
                                            f32::from(event.position.y)
                                        ),
                                    );
                                });
                            }
                        });
                    }
                })
                .absolute()
                .size_0(),
            )
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" && this.open_menu.take().is_some() {
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(chrome())
            .text_color(text())
            .text_size(px(FONT_SIZE))
            .child(self.render_menu_bar(cx))
            .child(self.render_toolbar(cx))
            .child(
                div().flex_1().min_h_0().flex().children(left_dock).child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().flex_1().min_h_0().child(center))
                        .child(
                            div()
                                .h(px(BOTTOM_DOCK_HEIGHT))
                                .flex_none()
                                .border_t_1()
                                .border_color(border())
                                .child(bottom_dock),
                        ),
                ),
            )
            .child(self.render_status_bar())
            .children(self.render_open_menu(menu_left, cx))
            .when(self.automation_enabled, |element| {
                element.child(crate::semantics::frame_marker(&self.frame))
            })
    }
}
