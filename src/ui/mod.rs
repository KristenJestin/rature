//! The window's components, copied from Flottille's `flottille-ui` so both apps share one look:
//! tokens, type, motion, buttons, toasts, the modal layer.

pub mod button;
pub mod color;
pub mod cut;
pub mod dropdown;
pub mod field;
pub mod fuzzy;
pub mod icons;
mod interact;
pub mod layer;
pub mod listing;
pub mod menu;
pub mod modal;
pub mod motion;
pub mod omarchy;
pub mod page;
pub mod reveal;
pub mod rows;
pub mod space;
pub mod theme;
pub mod toast;
pub mod typography;

use gpui_kit::App;

pub use button::Button;
pub use icons::{Icon, Icons, icon};
pub use interact::{fade, selectable};
pub use reveal::{enter, presence};
pub use theme::{Palette, palette};

pub use toast::{ToastStack, toast};
pub use typography::Type;

/// The bundled fonts and the desktop's theme. Register [`Icons`] as the application's assets.
pub fn init(cx: &mut App) {
    gpui_kit::init(cx);
    typography::register(cx);
    omarchy::install(omarchy::Environment::from_system(), cx);
}
