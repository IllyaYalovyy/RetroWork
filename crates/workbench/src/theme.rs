//! Colours and metrics of the workbench: a light, dense desktop look in the
//! spirit of NetBeans' default theme.

use gpui::{Rgba, rgb};
use retrowork_platform::Tone;

pub(crate) const MENU_BAR_HEIGHT: f32 = 24.;
pub(crate) const TOOLBAR_HEIGHT: f32 = 32.;
pub(crate) const TAB_STRIP_HEIGHT: f32 = 24.;
pub(crate) const STATUS_BAR_HEIGHT: f32 = 22.;
pub(crate) const ROW_HEIGHT: f32 = 20.;
pub(crate) const LEFT_DOCK_WIDTH: f32 = 230.;
pub(crate) const BOTTOM_DOCK_HEIGHT: f32 = 190.;
pub(crate) const FONT_SIZE: f32 = 13.;

pub(crate) fn chrome() -> Rgba {
    rgb(0xf0f0f0)
}
pub(crate) fn chrome_dark() -> Rgba {
    rgb(0xe2e2e2)
}
pub(crate) fn surface() -> Rgba {
    rgb(0xffffff)
}
pub(crate) fn border() -> Rgba {
    rgb(0xb4b4b4)
}
pub(crate) fn border_light() -> Rgba {
    rgb(0xd9d9d9)
}
pub(crate) fn text() -> Rgba {
    rgb(0x1c1c1c)
}
pub(crate) fn text_muted() -> Rgba {
    rgb(0x6b6b6b)
}
pub(crate) fn hover() -> Rgba {
    rgb(0xdce7f7)
}
pub(crate) fn selection() -> Rgba {
    rgb(0x3169c6)
}
pub(crate) fn selection_text() -> Rgba {
    rgb(0xffffff)
}
pub(crate) fn accent() -> Rgba {
    rgb(0x3169c6)
}

pub(crate) fn tone(tone: Tone) -> Rgba {
    match tone {
        Tone::Normal => text(),
        Tone::Muted => text_muted(),
        Tone::Info => rgb(0x1a56b0),
        Tone::Warning => rgb(0x8a5a00),
        Tone::Error => rgb(0xb3261e),
    }
}
