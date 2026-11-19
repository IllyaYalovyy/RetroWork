//! Semantic UI registry.
//!
//! Every interactive or informative element the workbench draws registers a
//! [`UiNode`] under a stable id together with the bounds it was actually laid
//! out at. The registry is rebuilt on every frame from the real element tree,
//! so it describes laid-out controls. Pixel capture verifies the painted frame
//! separately.
//!
//! The same snapshot type is what headless GPUI tests assert on and what the
//! automation channel returns from a running application.
//!
//! # Id scheme
//!
//! | Id | Element |
//! |---|---|
//! | `menu.<menu>` | menu title in the menu bar |
//! | `menu.<menu>.<item>` | item of the open menu |
//! | `toolbar.<item>` | toolbar button |
//! | `dock.<left\|bottom>.tab.<view>` | tab of a docked view |
//! | `view.<view>.node.<node>` | tree node |
//! | `view.<view>.line.<n>` | text line |
//! | `view.<view>.prop.<name>` | property row; value is the property value |
//! | `view.<view>.empty` | hint shown for empty content |
//! | `doc.tab.<kind>:<resource>` (`.close`) | document tab and its close button |
//! | `doc.filter` | filter field of the active document; value is its text |
//! | `doc.summary` | summary line of the active document |
//! | `doc.row.<n>` | table row of the active document; value is its cells joined by tabs |
//! | `doc.empty` | placeholder when no document is open |
//! | `status.message`, `status.modules` | status bar texts |

use std::{cell::RefCell, rc::Rc};

use gpui::{Bounds, IntoElement, Pixels, Styled, canvas};
use serde::{Deserialize, Serialize};

/// A rectangle in window-relative logical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    /// The centre point, where a pointer click aims.
    pub fn center(&self) -> (f32, f32) {
        (self.x + self.width / 2., self.y + self.height / 2.)
    }

    fn from_bounds(bounds: Bounds<Pixels>) -> Self {
        Self {
            x: bounds.origin.x.into(),
            y: bounds.origin.y.into(),
            width: bounds.size.width.into(),
            height: bounds.size.height.into(),
        }
    }
}

/// What kind of control an element is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    Menu,
    MenuItem,
    Button,
    Tab,
    TreeItem,
    Row,
    TextField,
    Text,
    Property,
}

/// One drawn element under its stable id.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UiNode {
    pub id: String,
    pub role: Role,
    /// The text the element shows as its name.
    pub label: String,
    /// Content beyond the label, such as field text or row cells.
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub selected: bool,
    /// Why this control cannot currently execute its command.
    #[serde(default)]
    pub disabled_reason: Option<String>,
    #[serde(default)]
    pub focused: bool,
    /// Where layout placed the element.
    pub bounds: Rect,
    /// The part of `bounds` inside every enclosing clip (scroll area).
    /// `None` when the element is scrolled out of view.
    pub visible: Option<Rect>,
}

/// The elements the workbench registered while preparing one frame.
///
/// Elements register during GPUI's prepaint, after layout and before the
/// frame is painted and handed to the GPU. A snapshot therefore shows the
/// state the window is about to present; it does not prove that the frame
/// reached the screen.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UiSnapshot {
    /// Increases with every prepared frame.
    pub frame: u64,
    pub width: f32,
    pub height: f32,
    /// Device pixels per logical pixel.
    pub scale: f32,
    /// Whether the window system reports the window as active.
    pub window_active: bool,
    /// Newest trace sequence when this frame began rendering.
    pub trace_seq: u64,
    pub nodes: Vec<UiNode>,
    /// Bounds of the automation-only marker painted with this frame.
    pub marker: Option<Rect>,
    /// Missing or duplicate observed controls make this frame invalid for testing.
    pub errors: Vec<String>,
}

impl UiSnapshot {
    /// Rejects incomplete or ambiguous control observations.
    pub fn validate(&self) -> Result<(), String> {
        if let Some(error) = self.errors.first() {
            return Err(error.clone());
        }
        let mut seen = std::collections::BTreeSet::new();
        for node in &self.nodes {
            if !seen.insert(&node.id) {
                return Err(format!("duplicate element id `{}`", node.id));
            }
        }
        Ok(())
    }

    /// The element with id `id`, if it was drawn.
    pub fn find(&self, id: &str) -> Option<&UiNode> {
        self.nodes.iter().find(|node| node.id == id)
    }

    /// Nodes whose id starts with `prefix`, in the order they were drawn.
    pub fn with_prefix(&self, prefix: &str) -> Vec<&UiNode> {
        self.nodes
            .iter()
            .filter(|node| node.id.starts_with(prefix))
            .collect()
    }

    /// The value of node `id`, or its label when it has no value.
    pub fn text(&self, id: &str) -> Option<&str> {
        self.find(id)
            .map(|node| node.value.as_deref().unwrap_or(&node.label))
    }
}

/// Elements registered while one frame was prepared. The workbench clears
/// `nodes` and increments `frame` at the start of each render; probes append
/// during prepaint, so the list is complete once a frame has been prepared.
#[derive(Default)]
pub(crate) struct SemanticFrame {
    pub(crate) frame: u64,
    pub(crate) nodes: Vec<UiNode>,
    pub(crate) trace_seq: u64,
    pub(crate) marker: Option<Rect>,
    pub(crate) expected: Vec<String>,
}

/// Shared between the workbench, which reads and resets it, and the probes
/// of the elements it draws, which write to it.
pub(crate) type SharedFrame = Rc<RefCell<SemanticFrame>>;

/// Description of an element, before layout.
pub struct Control {
    pub(crate) id: String,
    pub(crate) role: Role,
    pub(crate) label: String,
    pub(crate) value: Option<String>,
    pub(crate) selected: bool,
    pub(crate) disabled_reason: Option<String>,
    pub(crate) focused: bool,
}

impl Control {
    /// Describes one observed control before layout assigns its bounds.
    pub fn new(id: impl Into<String>, role: Role, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            role,
            label: label.into(),
            value: None,
            selected: false,
            disabled_reason: None,
            focused: false,
        }
    }

    /// Adds a meaningful value beyond the control label.
    pub fn value(mut self, value: impl Into<String>) -> Self {
        self.value = Some(value.into());
        self
    }

    /// Explains why the control cannot currently be used.
    pub fn disabled_reason(mut self, reason: Option<String>) -> Self {
        self.disabled_reason = reason;
        self
    }

    /// Records selection independently of keyboard focus.
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Records whether this control owns keyboard focus.
    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    /// An invisible child that fills its parent and records the parent's
    /// laid-out bounds, clipped by the active content mask, into `frame`.
    pub(crate) fn element(self, frame: &SharedFrame) -> impl IntoElement + use<> {
        let frame = Rc::clone(frame);
        canvas(
            move |bounds, window, _| {
                let clipped = bounds.intersect(&window.content_mask().bounds);
                let visible = (clipped.size.width > Pixels::ZERO
                    && clipped.size.height > Pixels::ZERO)
                    .then(|| Rect::from_bounds(clipped));
                frame.borrow_mut().nodes.push(UiNode {
                    id: self.id,
                    role: self.role,
                    label: self.label,
                    value: self.value,
                    selected: self.selected,
                    disabled_reason: self.disabled_reason,
                    focused: self.focused,
                    bounds: Rect::from_bounds(bounds),
                    visible,
                });
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }
}

/// Encodes a frame number in two complementary rows of black and white cells.
/// The leading cells distinguish the marker from ordinary window content.
pub fn frame_marker_bit(frame: u64, cell: usize, row: usize) -> bool {
    let bit = match cell {
        0..=3 => cell.is_multiple_of(2),
        4..68 => frame & (1 << (cell - 4)) != 0,
        _ => false,
    };
    bit ^ (row == 1)
}

/// Draws the marker in the same scene as the observed controls.
pub(crate) fn frame_marker(shared: &SharedFrame) -> impl IntoElement + use<> {
    use gpui::{ParentElement as _, point, px, size};
    let shared = Rc::clone(shared);
    gpui::div()
        .absolute()
        .top_0()
        .right_0()
        .w(px(136.))
        .h(px(4.))
        .child(
            canvas(
                move |bounds, _, _| {
                    let mut shared = shared.borrow_mut();
                    shared.marker = Some(Rect::from_bounds(bounds));
                    shared.frame
                },
                |bounds, frame, window, _| {
                    for row in 0..2 {
                        for cell in 0..68 {
                            let origin =
                                bounds.origin + point(px(cell as f32 * 2.), px(row as f32 * 2.));
                            let colour = if frame_marker_bit(frame, cell, row) {
                                0xffffff
                            } else {
                                0
                            };
                            window.paint_quad(gpui::fill(
                                Bounds::new(origin, size(px(2.), px(2.))),
                                gpui::rgb(colour),
                            ));
                        }
                    }
                },
            )
            .size_full(),
        )
}

pub(crate) type Probe = Control;
