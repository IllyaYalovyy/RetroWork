//! Contribution types: what modules add to the workbench.

use std::{collections::BTreeMap, fmt, rc::Rc};

use serde_json::Value;

use crate::{
    activation::ExtensionPointHandler,
    command::{CommandContext, CommandError, CommandSpec, RegisteredCommand},
    id::{CommandId, DocumentKind, MenuId, ModuleId, ViewId},
    model::ViewProvider,
};

/// A top-level menu. Items are added separately, by any module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuSpec {
    pub id: MenuId,
    pub title: String,
    pub order: i32,
    pub owner: ModuleId,
}

/// An item of a top-level menu.
#[derive(Clone, Debug, PartialEq)]
pub struct MenuItem {
    pub menu: MenuId,
    /// Unique within its menu; part of the automation id `menu.<menu>.<id>`.
    pub id: String,
    pub title: String,
    pub command: CommandId,
    pub args: Option<Value>,
    pub order: i32,
    pub owner: ModuleId,
}

/// A button on the toolbar.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolbarItem {
    /// Unique within the toolbar; part of the automation id `toolbar.<id>`.
    pub id: String,
    pub title: String,
    pub command: CommandId,
    pub args: Option<Value>,
    pub order: i32,
    pub owner: ModuleId,
}

/// A keyboard shortcut that runs a command.
#[derive(Clone, Debug, PartialEq)]
pub struct KeyBinding {
    pub chord: KeyChord,
    pub command: CommandId,
    pub args: Option<Value>,
    pub owner: ModuleId,
}

/// Where a docked view is placed in the workbench.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ViewLocation {
    Left,
    Bottom,
}

impl ViewLocation {
    /// The name used in persisted state and element ids.
    pub fn as_str(self) -> &'static str {
        match self {
            ViewLocation::Left => "left",
            ViewLocation::Bottom => "bottom",
        }
    }

    /// The location named `value`, as written by [`Self::as_str`].
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "left" => Some(ViewLocation::Left),
            "bottom" => Some(ViewLocation::Bottom),
            _ => None,
        }
    }
}

/// A registered docked view, without its provider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewSpec {
    pub id: ViewId,
    pub title: String,
    pub location: ViewLocation,
    pub order: i32,
    pub owner: ModuleId,
}

/// A document created by a [`DocumentFactory`].
pub struct OpenedDocument {
    pub title: String,
    pub provider: Rc<dyn ViewProvider>,
}

/// Opens documents of one [`DocumentKind`] from a resource string. The same
/// resource string is what gets persisted, so it must be stable across runs.
pub trait DocumentFactory {
    /// Opens the document named by `resource`. An error is shown to the
    /// user, and while restoring a session it skips only this document.
    fn open(
        &self,
        resource: &str,
        cx: &mut CommandContext<'_>,
    ) -> Result<OpenedDocument, CommandError>;
}

/// A validated key chord in canonical form, e.g. `ctrl-shift-s` or `f5`.
///
/// The text form is the one GPUI's keymap parses, restricted to what is safe
/// as an application-wide shortcut: a chord must not swallow ordinary typing,
/// so a bare character, Enter, Tab, Space, Backspace or Escape is rejected.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeyChord(String);

/// Why a text is not an acceptable key chord.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum KeyChordError {
    #[error("key chord is empty")]
    Empty,
    #[error("unknown modifier `{0}` (allowed: ctrl, alt, shift)")]
    UnknownModifier(String),
    #[error("modifier `{0}` is repeated")]
    RepeatedModifier(String),
    #[error("unknown key `{0}`")]
    UnknownKey(String),
    #[error("`{0}` would intercept ordinary typing; add ctrl or alt, or use a function key")]
    WouldInterceptTyping(String),
}

const NAMED_KEYS: &[&str] = &[
    "enter",
    "escape",
    "tab",
    "space",
    "backspace",
    "delete",
    "up",
    "down",
    "left",
    "right",
    "home",
    "end",
    "pageup",
    "pagedown",
];

impl KeyChord {
    /// Validates `text` and returns it in canonical modifier order
    /// (`ctrl`, `alt`, `shift`).
    pub fn parse(text: &str) -> Result<Self, KeyChordError> {
        if text.is_empty() {
            return Err(KeyChordError::Empty);
        }
        let mut parts: Vec<&str> = text.split('-').collect();
        let key = parts.pop().unwrap_or_default();
        let (mut ctrl, mut alt, mut shift) = (false, false, false);
        for modifier in parts {
            let flag = match modifier {
                "ctrl" => &mut ctrl,
                "alt" => &mut alt,
                "shift" => &mut shift,
                other => return Err(KeyChordError::UnknownModifier(other.to_owned())),
            };
            if std::mem::replace(flag, true) {
                return Err(KeyChordError::RepeatedModifier(modifier.to_owned()));
            }
        }
        let is_function_key = key
            .strip_prefix('f')
            .and_then(|number| number.parse::<u8>().ok())
            .is_some_and(|number| (1..=12).contains(&number));
        let is_character = key.len() == 1
            && key
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
        if !(is_function_key || is_character || NAMED_KEYS.contains(&key)) {
            return Err(KeyChordError::UnknownKey(key.to_owned()));
        }
        if !is_function_key && !ctrl && !alt {
            return Err(KeyChordError::WouldInterceptTyping(text.to_owned()));
        }
        let mut canonical = String::new();
        for (enabled, name) in [(ctrl, "ctrl-"), (alt, "alt-"), (shift, "shift-")] {
            if enabled {
                canonical.push_str(name);
            }
        }
        canonical.push_str(key);
        Ok(Self(canonical))
    }

    /// The canonical text, e.g. `ctrl-shift-s`.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Text for menus, e.g. `Ctrl+Shift+S`.
    pub fn display(&self) -> String {
        self.0
            .split('-')
            .map(|part| {
                let mut chars = part.chars();
                match chars.next() {
                    Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
                    None => String::new(),
                }
            })
            .collect::<Vec<_>>()
            .join("+")
    }
}

impl fmt::Display for KeyChord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Everything active modules have contributed.
#[derive(Default)]
pub struct Contributions {
    pub(crate) commands: BTreeMap<CommandId, RegisteredCommand>,
    pub(crate) menus: Vec<MenuSpec>,
    pub(crate) menu_items: Vec<MenuItem>,
    pub(crate) toolbar: Vec<ToolbarItem>,
    pub(crate) key_bindings: Vec<KeyBinding>,
    pub(crate) views: Vec<(ViewSpec, Rc<dyn ViewProvider>)>,
    pub(crate) document_factories: BTreeMap<DocumentKind, (ModuleId, Rc<dyn DocumentFactory>)>,
    pub(crate) extension_points: BTreeMap<String, (ModuleId, ExtensionPointHandler)>,
}

impl Contributions {
    /// The command registered as `id`, if any.
    pub fn command(&self, id: &CommandId) -> Option<&CommandSpec> {
        self.commands.get(id).map(|command| &command.spec)
    }

    /// All registered commands, ordered by id.
    pub fn commands(&self) -> impl Iterator<Item = &CommandSpec> {
        self.commands.values().map(|command| &command.spec)
    }

    /// Menus in display order.
    pub fn menus(&self) -> Vec<&MenuSpec> {
        let mut menus: Vec<&MenuSpec> = self.menus.iter().collect();
        menus.sort_by(|a, b| (a.order, &a.id).cmp(&(b.order, &b.id)));
        menus
    }

    /// Items of `menu` in display order.
    pub fn menu_items(&self, menu: &MenuId) -> Vec<&MenuItem> {
        let mut items: Vec<&MenuItem> = self
            .menu_items
            .iter()
            .filter(|item| &item.menu == menu)
            .collect();
        items.sort_by(|a, b| (a.order, &a.id).cmp(&(b.order, &b.id)));
        items
    }

    /// Toolbar items in display order.
    pub fn toolbar(&self) -> Vec<&ToolbarItem> {
        let mut items: Vec<&ToolbarItem> = self.toolbar.iter().collect();
        items.sort_by(|a, b| (a.order, &a.id).cmp(&(b.order, &b.id)));
        items
    }

    /// All key bindings, in registration order.
    pub fn key_bindings(&self) -> &[KeyBinding] {
        &self.key_bindings
    }

    /// The chord bound to `command` with exactly `args`, for menu hints.
    pub fn chord_for(&self, command: &CommandId, args: Option<&Value>) -> Option<&KeyChord> {
        self.key_bindings
            .iter()
            .find(|binding| &binding.command == command && binding.args.as_ref() == args)
            .map(|binding| &binding.chord)
    }

    /// Views docked at `location`, in display order.
    pub fn views(&self, location: ViewLocation) -> Vec<&ViewSpec> {
        let mut views: Vec<&ViewSpec> = self
            .views
            .iter()
            .map(|(spec, _)| spec)
            .filter(|spec| spec.location == location)
            .collect();
        views.sort_by(|a, b| (a.order, &a.id).cmp(&(b.order, &b.id)));
        views
    }

    /// The docked view registered as `id`, if any.
    pub fn view(&self, id: &ViewId) -> Option<&ViewSpec> {
        self.views
            .iter()
            .map(|(spec, _)| spec)
            .find(|spec| &spec.id == id)
    }

    /// Moves everything from `staged` into `self`. The caller has already
    /// checked for conflicts.
    pub(crate) fn merge(&mut self, staged: Contributions) {
        self.commands.extend(staged.commands);
        self.menus.extend(staged.menus);
        self.menu_items.extend(staged.menu_items);
        self.toolbar.extend(staged.toolbar);
        self.key_bindings.extend(staged.key_bindings);
        self.views.extend(staged.views);
        self.document_factories.extend(staged.document_factories);
        self.extension_points.extend(staged.extension_points);
    }

    pub(crate) fn view_provider(&self, id: &ViewId) -> Option<&Rc<dyn ViewProvider>> {
        self.views
            .iter()
            .find(|(spec, _)| &spec.id == id)
            .map(|(_, provider)| provider)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chords_are_canonicalised() {
        assert_eq!(
            KeyChord::parse("shift-ctrl-s").unwrap().as_str(),
            "ctrl-shift-s"
        );
        assert_eq!(
            KeyChord::parse("alt-ctrl-f4").unwrap().as_str(),
            "ctrl-alt-f4"
        );
        assert_eq!(KeyChord::parse("f5").unwrap().as_str(), "f5");
        assert_eq!(
            KeyChord::parse("ctrl-pagedown").unwrap().display(),
            "Ctrl+Pagedown"
        );
        assert_eq!(
            KeyChord::parse("ctrl-shift-s").unwrap().display(),
            "Ctrl+Shift+S"
        );
    }

    #[test]
    fn chords_that_would_swallow_typing_are_rejected() {
        for chord in ["a", "shift-a", "enter", "escape", "shift-tab", "space", "7"] {
            assert_eq!(
                KeyChord::parse(chord),
                Err(KeyChordError::WouldInterceptTyping(chord.to_owned())),
                "{chord}"
            );
        }
    }

    #[test]
    fn malformed_chords_are_rejected_with_a_reason() {
        assert_eq!(KeyChord::parse(""), Err(KeyChordError::Empty));
        assert_eq!(
            KeyChord::parse("cmd-s"),
            Err(KeyChordError::UnknownModifier("cmd".into()))
        );
        assert_eq!(
            KeyChord::parse("ctrl-ctrl-s"),
            Err(KeyChordError::RepeatedModifier("ctrl".into()))
        );
        assert_eq!(
            KeyChord::parse("ctrl-S"),
            Err(KeyChordError::UnknownKey("S".into()))
        );
        assert_eq!(
            KeyChord::parse("ctrl-f13"),
            Err(KeyChordError::UnknownKey("f13".into()))
        );
        assert_eq!(
            KeyChord::parse("ctrl-"),
            Err(KeyChordError::UnknownKey(String::new()))
        );
    }
}
