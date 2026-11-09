//! The base theme. Following the Omarchy theme comes after the prototype.

use gpui_kit::{Rgba, rgb};

pub const TEXT_HEX: &str = "#c0caf5";

pub fn background() -> Rgba {
    rgb(0x1a1b26)
}
pub fn text() -> Rgba {
    rgb(0xc0caf5)
}
pub fn muted() -> Rgba {
    rgb(0x787c99)
}
pub fn accent() -> Rgba {
    rgb(0x7aa2f7)
}
pub fn surface() -> Rgba {
    rgb(0x24283b)
}
pub fn border() -> Rgba {
    rgb(0x3b4261)
}
