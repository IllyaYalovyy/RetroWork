//! Native and built-in view content.

use super::*;

impl Workbench {
    pub(super) fn render_content(
        &mut self,
        prefix: String,
        target: ViewTarget,
        content: ViewContent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match content {
            ViewContent::Native(model) => self
                .platform
                .services()
                .get::<crate::views::ViewRenderers>()
                .and_then(|registry| {
                    registry.render(
                        &model,
                        (prefix.clone(), target),
                        Rc::clone(&self.frame),
                        &mut self.native_focus,
                        window,
                        cx,
                    )
                })
                .unwrap_or_else(|| {
                    self.hint(&prefix, &format!("No renderer for {}", model.type_name()))
                }),
            ViewContent::Tree(model) => self.render_tree(prefix, target, model, window, cx),
            ViewContent::Table(model) => self.render_table(prefix, target, model, window, cx),
            ViewContent::Source(model) => self.render_source(target, model, window, cx),
            ViewContent::Text(model) => self.render_text(prefix, model, window, cx),
            ViewContent::Properties(model) => self.render_properties(prefix, model),
        }
    }

    /// The input field above a docked view's content, if it has one.
    fn render_view_input(
        &self,
        prefix: &str,
        target: &ViewTarget,
        model: Option<&InputModel>,
        window: &Window,
        cx: &App,
    ) -> Option<impl IntoElement + use<>> {
        let (ViewTarget::View(view), Some(model)) = (target, model) else {
            return None;
        };
        let input = self.view_inputs.get(view)?;
        let state = input.read(cx);
        let focused = state.focus_handle(cx).is_focused(window);
        let value = state.value().to_string();
        Some(
            div()
                .relative()
                .flex_none()
                .px_2()
                .py_1()
                .bg(chrome())
                .border_b_1()
                .border_color(border_light())
                .child(Input::new(input).small())
                .child(
                    Probe::new(
                        format!("{prefix}.input"),
                        Role::TextField,
                        model.placeholder.clone(),
                    )
                    .value(value)
                    .focused(focused)
                    .element(&self.frame),
                ),
        )
    }

    fn render_tree(
        &self,
        prefix: String,
        target: ViewTarget,
        model: TreeModel,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let input = self.render_view_input(&prefix, &target, model.input.as_ref(), window, cx);
        let nodes = model.visible();
        if nodes.is_empty() {
            return div()
                .size_full()
                .flex()
                .flex_col()
                .children(input)
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .child(self.hint(&prefix, &model.empty_hint)),
                )
                .into_any_element();
        }
        let rows = nodes.into_iter().map(|(depth, node)| {
            let selected = model.selected.as_deref() == Some(node.id.as_str());
            let element_id = format!("{prefix}.node.{}", node.id);
            let (node_id, is_folder) = (node.id.clone(), node.is_folder);
            let target = target.clone();
            let marker = match (node.is_folder, node.expanded) {
                (true, true) => "▾",
                (true, false) => "▸",
                (false, _) => "",
            };
            div()
                .id(eid(element_id.clone()))
                .relative()
                .h(px(ROW_HEIGHT))
                .flex()
                .items_center()
                .gap_1()
                .pl(px(6. + 16. * depth as f32))
                .pr_2()
                .map(|this| {
                    if selected {
                        this.bg(selection()).text_color(selection_text())
                    } else {
                        this.hover(|style| style.bg(hover()))
                    }
                })
                .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                    let event = if is_folder {
                        ViewEvent::TreeToggle(node_id.clone())
                    } else if event.click_count() > 1 {
                        ViewEvent::TreeActivate(node_id.clone())
                    } else {
                        ViewEvent::TreeSelect(node_id.clone())
                    };
                    this.view_event(&target, event, window, cx);
                }))
                .child(div().w(px(12.)).flex_none().child(marker))
                .tooltip({
                    let label = node.label.clone();
                    move |window, cx| Tooltip::new(label.clone()).build(window, cx)
                })
                .child(node.label.clone())
                .child(
                    Probe::new(element_id, Role::TreeItem, node.label.clone())
                        .selected(selected)
                        .value(match (node.is_folder, node.expanded) {
                            (true, true) => "expanded",
                            (true, false) => "collapsed",
                            (false, _) => "leaf",
                        })
                        .element(&self.frame),
                )
        });
        div()
            .size_full()
            .flex()
            .flex_col()
            .children(input)
            .child(
                div()
                    .id(eid(format!("{prefix}.tree")))
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .py_1()
                    .children(rows),
            )
            .into_any_element()
    }

    fn render_table(
        &mut self,
        prefix: String,
        target: ViewTarget,
        model: TableModel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let TableModel {
            columns,
            rows,
            selected,
            filter,
            summary,
        } = model;
        let cell = |width: Option<f32>| {
            let cell = div().px_2().truncate();
            match width {
                Some(width) => cell.w(px(width)).flex_none(),
                None => cell.flex_1().min_w_0(),
            }
        };

        // The filter field belongs to the active document; docked views with
        // a filter are not supported yet.
        let filter_bar = filter
            .filter(|_| matches!(target, ViewTarget::Document(_)))
            .map(|filter| {
                let input = self.filter_input.read(cx);
                let focused = input.focus_handle(cx).is_focused(window);
                let shown = input.value().to_string();
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_2()
                    .py_1()
                    .bg(chrome())
                    .border_b_1()
                    .border_color(border_light())
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .child(Input::new(&self.filter_input).small())
                            .child(
                                Probe::new(
                                    format!("{prefix}.filter"),
                                    Role::TextField,
                                    filter.placeholder,
                                )
                                .value(shown)
                                .focused(focused)
                                .element(&self.frame),
                            ),
                    )
                    .child(
                        div()
                            .relative()
                            .flex_none()
                            .text_color(text_muted())
                            .child(summary.clone())
                            .child(
                                Probe::new(
                                    format!("{prefix}.summary"),
                                    Role::Text,
                                    summary.clone(),
                                )
                                .element(&self.frame),
                            ),
                    )
            });

        let header = div()
            .h(px(ROW_HEIGHT + 2.))
            .flex_none()
            .flex()
            .items_center()
            .bg(chrome())
            .border_b_1()
            .border_color(border())
            .text_color(text_muted())
            .children(
                columns
                    .iter()
                    .map(|column| cell(column.width).child(column.title.clone())),
            );

        let body = if rows.is_empty() {
            self.hint(&prefix, "No records match.")
        } else {
            let widths: Rc<Vec<Option<f32>>> =
                Rc::new(columns.iter().map(|column| column.width).collect());
            let row_prefix = prefix.clone();
            let scroll = self.list(&prefix).scroll.clone();
            uniform_list(
                eid(format!("{prefix}.rows")),
                rows.len(),
                cx.processor(move |this: &mut Self, range: Range<usize>, _, cx| {
                    range
                        .map(|index| {
                            let row = &rows[index];
                            let is_selected = selected == Some(index);
                            let element_id = format!("{row_prefix}.row.{index}");
                            let target = target.clone();
                            div()
                                .id(eid(element_id.clone()))
                                .relative()
                                .h(px(ROW_HEIGHT))
                                .w_full()
                                .flex()
                                .items_center()
                                .map(|this| {
                                    if is_selected {
                                        this.bg(selection()).text_color(selection_text())
                                    } else {
                                        this.text_color(theme::tone(row.tone))
                                            .when(index % 2 == 1, |this| this.bg(rgb(0xf6f8fb)))
                                            .hover(|style| style.bg(hover()))
                                    }
                                })
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    this.view_event(
                                        &target,
                                        ViewEvent::RowSelect(index),
                                        window,
                                        cx,
                                    );
                                }))
                                .children(
                                    row.cells
                                        .iter()
                                        .zip(widths.iter())
                                        .map(|(text, width)| cell(*width).child(text.clone())),
                                )
                                .child(
                                    Probe::new(
                                        element_id,
                                        Role::Row,
                                        row.cells.last().cloned().unwrap_or_default(),
                                    )
                                    .value(row.cells.join("\t"))
                                    .selected(is_selected)
                                    .element(&this.frame),
                                )
                        })
                        .collect::<Vec<_>>()
                }),
            )
            .track_scroll(&scroll)
            .size_full()
            .into_any_element()
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .children(filter_bar)
            .child(header)
            .child(div().flex_1().min_h_0().child(body))
            .into_any_element()
    }

    fn list(&mut self, prefix: &str) -> &mut ListState {
        self.lists.entry(prefix.to_owned()).or_default()
    }

    fn render_text(
        &mut self,
        prefix: String,
        model: TextModel,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let TextModel {
            lines,
            empty_hint,
            header,
            selected,
        } = model;
        let header = header.map(|line| {
            div()
                .relative()
                .flex_none()
                .h(px(ROW_HEIGHT + 4.))
                .px_2()
                .flex()
                .items_center()
                .bg(chrome())
                .border_b_1()
                .border_color(border_light())
                .text_color(theme::tone(line.tone))
                .child(div().truncate().child(line.text.clone()))
                .child(
                    Probe::new(format!("{prefix}.header"), Role::Text, line.text)
                        .element(&self.frame),
                )
        });
        let list = self.list(&prefix);
        let body = if lines.is_empty() {
            list.seen_len = 0;
            list.seen_selected = None;
            self.hint(&prefix, &empty_hint)
        } else {
            match selected.filter(|&line| line < lines.len()) {
                // Bring a newly selected line into view.
                Some(line) if list.seen_selected != Some(line) => {
                    list.scroll.scroll_to_item(line, ScrollStrategy::Center);
                }
                Some(_) => {}
                // Follow new output.
                None if lines.len() != list.seen_len => {
                    list.scroll
                        .scroll_to_item(lines.len() - 1, ScrollStrategy::Bottom);
                }
                None => {}
            }
            list.seen_len = lines.len();
            list.seen_selected = selected;
            let scroll = list.scroll.clone();
            self.text_lines(&prefix, lines, selected, &scroll, cx)
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .children(header)
            .child(div().flex_1().min_h_0().child(body))
            .into_any_element()
    }

    fn text_lines(
        &self,
        prefix: &str,
        lines: Rc<Vec<TextLine>>,
        selected: Option<usize>,
        scroll: &UniformListScrollHandle,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let prefix = prefix.to_owned();
        uniform_list(
            eid(format!("{prefix}.lines")),
            lines.len(),
            cx.processor(move |this: &mut Self, range: Range<usize>, _, _| {
                range
                    .map(|index| {
                        let line = &lines[index];
                        let is_selected = selected == Some(index);
                        div()
                            .relative()
                            .h(px(ROW_HEIGHT))
                            .w_full()
                            .px_2()
                            .flex()
                            .items_center()
                            .map(|this| {
                                if is_selected {
                                    this.bg(selection()).text_color(selection_text())
                                } else {
                                    this.text_color(theme::tone(line.tone))
                                }
                            })
                            .child(div().truncate().child(line.text.clone()))
                            .child(
                                Probe::new(
                                    format!("{prefix}.line.{index}"),
                                    Role::Text,
                                    line.text.clone(),
                                )
                                .selected(is_selected)
                                .element(&this.frame),
                            )
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(scroll)
        .size_full()
        .into_any_element()
    }

    fn render_properties(&self, prefix: String, model: PropertyModel) -> AnyElement {
        if model.rows.is_empty() {
            return self.hint(&prefix, &model.empty_hint);
        }
        let rows = model.rows.into_iter().map(|(name, value)| {
            let slug: String = name
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() {
                        c.to_ascii_lowercase()
                    } else {
                        '-'
                    }
                })
                .collect();
            div()
                .relative()
                .min_h(px(ROW_HEIGHT))
                .flex()
                .items_center()
                .border_b_1()
                .border_color(border_light())
                .child(
                    div()
                        .w(px(110.))
                        .flex_none()
                        .px_2()
                        .text_color(text_muted())
                        .child(name.clone()),
                )
                .child(div().flex_1().min_w_0().px_2().child(value.clone()))
                .child(
                    Probe::new(format!("{prefix}.prop.{slug}"), Role::Property, name)
                        .value(value)
                        .element(&self.frame),
                )
        });
        div()
            .id(eid(format!("{prefix}.props")))
            .size_full()
            .overflow_y_scroll()
            .children(rows)
            .into_any_element()
    }
}
