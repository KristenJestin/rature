//! The icons Rature adds to the kit's: Lucide icons by their file names, embedded here, the kit's
//! own served after them.

use std::borrow::Cow;

use gpui_design_system::{Icon, Icons};
use gpui_kit::{AssetSource, Result, SharedString};

pub const PANEL_LEFT: Icon = Icon::Lucide("panel-left-open");
pub const PANEL_LEFT_CLOSE: Icon = Icon::Lucide("panel-left-close");
pub const PANEL_RIGHT_CLOSE: Icon = Icon::Lucide("panel-right-close");
pub const FOLDER_OPEN: Icon = Icon::Lucide("folder-open");
pub const IMAGE: Icon = Icon::Lucide("image");
pub const TABLE: Icon = Icon::Lucide("sheet");
pub const DATA: Icon = Icon::Lucide("braces");
pub const CODE: Icon = Icon::Lucide("file-code");
pub const OUTLINE: Icon = Icon::Lucide("list-tree");
pub const ROW_ABOVE: Icon = Icon::Lucide("between-horizontal-start");
pub const ROW_BELOW: Icon = Icon::Lucide("between-horizontal-end");
pub const COLUMN_LEFT: Icon = Icon::Lucide("between-vertical-start");
pub const COLUMN_RIGHT: Icon = Icon::Lucide("between-vertical-end");

gpui_kit::assets::icon_assets!(
    Lucide,
    [
        PanelLeftOpen,
        PanelLeftClose,
        PanelRightClose,
        FolderOpen,
        Image,
        Sheet,
        Braces,
        FileCode,
        ListTree,
        BetweenHorizontalStart,
        BetweenHorizontalEnd,
        BetweenVerticalStart,
        BetweenVerticalEnd
    ]
);

/// Rature's icons, then the kit's.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        match Lucide.load(path)? {
            Some(found) => Ok(Some(found)),
            None => Icons.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = Icons.list(path)?;
        paths.extend(Lucide.list(path)?);
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_of_rature_is_served() {
        for icon in [
            PANEL_LEFT,
            PANEL_LEFT_CLOSE,
            PANEL_RIGHT_CLOSE,
            FOLDER_OPEN,
            IMAGE,
            TABLE,
            DATA,
            CODE,
            OUTLINE,
            ROW_ABOVE,
            ROW_BELOW,
            COLUMN_LEFT,
            COLUMN_RIGHT,
        ] {
            assert!(Assets.load(&icon.path()).unwrap().is_some(), "{icon:?}");
        }
        assert!(Assets.load(&Icon::Search.path()).unwrap().is_some());
    }
}
