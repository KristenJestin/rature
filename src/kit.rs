//! What Rature adds to the design system and the kit does not have yet: the colour of links and
//! the focus ring of a text field. To fold back into the kit.

use gpui_design_system::color::{TEXT_CONTRAST, mix, readable};
use gpui_design_system::motion::HOVER;
use gpui_design_system::{Palette, fade, palette};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{App, BoxShadow, ElementId, Hsla, Styled, Window, point, px};

/// The colour of links: the theme's primary, or its blue when the primary is grey or near the
/// text (it would not read as a link), made to read on the page, the cards and inline code.
pub fn link(p: &Palette) -> Hsla {
    let ink = if p.acc.s < 0.2 || distance(p.acc, p.text) < 0.2 {
        p.run
    } else {
        p.acc
    };
    [p.bg, p.card, p.card2]
        .into_iter()
        .fold(ink, |ink, surface| readable(ink, surface, TEXT_CONTRAST))
}

/// How far apart two colours are, in RGB: 0 the same, about 1.7 black and white.
fn distance(a: Hsla, b: Hsla) -> f32 {
    let (a, b) = (a.to_rgb(), b.to_rgb());
    ((a.r - b.r).powi(2) + (a.g - b.g).powi(2) + (a.b - b.b).powi(2)).sqrt()
}

/// The frame of a text field, its line and its focus ring: the line darkens to the focus line and
/// the ring spreads around it as the field takes the focus, and fade back as it leaves.
pub fn focus_frame<E: Styled + FluentBuilder>(
    id: impl Into<ElementId>,
    frame: E,
    focused: bool,
    window: &mut Window,
    cx: &mut App,
) -> E {
    let at = fade(id, focused, HOVER, window, cx);
    let p = palette(cx);
    frame
        .border_1()
        .border_color(mix(p.field_line, p.focus_line, at))
        .when(at > 0.0, |d| {
            d.shadow(vec![BoxShadow {
                color: p.focus_ring.opacity(at),
                offset: point(px(0.), px(0.)),
                blur_radius: px(0.),
                spread_radius: px(3.),
                inset: false,
            }])
        })
}

#[cfg(test)]
mod tests {
    use gpui_design_system::color::{contrast, hex};
    use gpui_design_system::omarchy::palette_from_toml;
    use gpui_design_system::theme::Shade;

    use super::*;

    fn theme(background: &str, accent: &str, selection: &str) -> Palette {
        palette_from_toml(&format!(
            "background = \"{background}\"\nforeground = \"#d8dee9\"\naccent = \"{accent}\"\n\
             blue = \"#4ea7fc\"\nselection_background = \"{selection}\"\n"
        ))
        .unwrap()
    }

    #[test]
    fn links_take_the_primary_unless_it_is_grey_or_the_text() {
        assert_eq!(link(&theme("#08090a", "#ff8226", "#2b2118")), hex(0xff8226));
        assert_eq!(
            link(&theme("#08090a", "#8d8d8d", "#2b2118")),
            hex(0x4ea7fc),
            "grey"
        );
        assert_eq!(
            link(&theme("#08090a", "#d8dee9", "#2b2118")),
            hex(0x4ea7fc),
            "the text"
        );
        let own = Palette::own(Shade::Dark);
        assert_eq!(link(&own), own.run, "the kit's own primary is grey");
    }

    #[test]
    fn links_read_on_the_page_the_cards_and_inline_code() {
        for p in [
            theme("#2e3440", "#81a1c1", "#d8dee9"),
            Palette::own(Shade::Dark),
            Palette::own(Shade::Light),
        ] {
            for surface in [p.bg, p.card, p.card2] {
                assert!(contrast(link(&p), surface) >= TEXT_CONTRAST);
            }
        }
    }
}
