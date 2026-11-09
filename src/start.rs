//! What the window shows with nothing open: the files and folders read last, and a way to open
//! another through the desktop's file chooser.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use gpui_kit::*;

use crate::formats;
use crate::picker::shown;
use crate::render::hex;
use crate::state::State;
use crate::ui::color::mix;
use crate::ui::theme::target_palette;
use crate::ui::{Button, Icon, Palette, Type, enter, icon, palette, selectable, space};

actions!(start, [Next, Previous, Open]);

pub fn bind_keys(cx: &mut App) {
    let at = Some("Start");
    cx.bind_keys([
        KeyBinding::new("j", Next, at),
        KeyBinding::new("down", Next, at),
        KeyBinding::new("k", Previous, at),
        KeyBinding::new("up", Previous, at),
        KeyBinding::new("enter", Open, at),
        KeyBinding::new("l", Open, at),
    ]);
}

/// The most recent ones listed: the page stays a glance.
const SHOWN: usize = 8;

pub enum StartEvent {
    Open(PathBuf),
}

impl EventEmitter<StartEvent> for Start {}

pub struct Start {
    recent: Vec<(PathBuf, Option<u64>)>,
    selected: usize,
    focus: FocusHandle,
    /// Rature's mark in the colours of the theme it was drawn for.
    logo: Option<(Palette, Arc<Image>)>,
}

impl Start {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let state = cx.global::<State>();
        let recent = state
            .recent
            .iter()
            .filter(|p| p.exists())
            .take(SHOWN)
            .map(|p| (p.clone(), state.opened_at.get(p).copied()))
            .collect();
        Self {
            recent,
            selected: 0,
            focus: cx.focus_handle(),
            logo: None,
        }
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    fn next(&mut self, _: &Next, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected + 1 < self.recent.len() {
            self.selected += 1;
            cx.notify();
        }
    }

    fn previous(&mut self, _: &Previous, _: &mut Window, cx: &mut Context<Self>) {
        self.selected = self.selected.saturating_sub(1);
        cx.notify();
    }

    fn open(&mut self, _: &Open, _: &mut Window, cx: &mut Context<Self>) {
        if let Some((path, _)) = self.recent.get(self.selected) {
            cx.emit(StartEvent::Open(path.clone()));
        }
    }

    /// The desktop's file chooser (the XDG portal), for a file or for a folder: it cannot offer
    /// both at once.
    fn choose(&mut self, directories: bool, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: !directories,
            directories,
            multiple: false,
            prompt: None,
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = chosen.await
                && let Some(path) = paths.into_iter().next()
            {
                this.update(cx, |_, cx| cx.emit(StartEvent::Open(path)))
                    .ok();
            }
        })
        .detach();
    }

    fn logo(&mut self, p: Palette) -> Arc<Image> {
        match &self.logo {
            Some((drawn, image)) if *drawn == p => image.clone(),
            _ => {
                let svg = logo_svg(&p).into_bytes();
                let image = Arc::new(Image::from_bytes(ImageFormat::Svg, svg));
                self.logo = Some((p, image.clone()));
                image
            }
        }
    }

    fn row(&self, ix: usize, path: &Path, at: Option<u64>, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let chosen = ix == self.selected;
        let target = path.to_path_buf();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let place = path.parent().map(shown).unwrap_or_default();
        let element = div()
            .id(("recent", ix))
            .flex()
            .items_center()
            .gap(space::MD)
            .h(px(40.))
            .px(space::MD)
            .rounded(px(8.))
            .cursor_pointer()
            .child(
                icon(formats::icon(path))
                    .size(px(16.))
                    .text_color(if chosen { p.text } else { p.mute }),
            )
            .child(
                div()
                    .flex_none()
                    .ui_text()
                    .medium()
                    .text_color(p.text)
                    .child(name),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .mono(11.5)
                    .text_color(p.faint)
                    .child(place),
            )
            .child(
                div()
                    .flex_none()
                    .mono(11.5)
                    .text_color(p.faint)
                    .child(at.map(ago).unwrap_or_default()),
            )
            .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                cx.emit(StartEvent::Open(target.clone()))
            }));
        enter(
            ("recent-enter", ix),
            ix + 1,
            selectable(element, chosen, move |e, hover, chosen| {
                e.bg(mix(p.hover.opacity(hover), p.sel, chosen))
            }),
        )
        .into_any_element()
    }
}

/// How long ago, in the fewest characters: `now`, `5m`, `3h`, `2d`, `6w`.
fn ago(at: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let seconds = now.saturating_sub(at);
    match seconds {
        0..60 => "now".into(),
        60..3_600 => format!("{}m", seconds / 60),
        3_600..86_400 => format!("{}h", seconds / 3_600),
        86_400..1_209_600 => format!("{}d", seconds / 86_400),
        _ => format!("{}w", seconds / 604_800),
    }
}

/// The mark: a page with its lines, one struck through, in the theme's own colours.
fn logo_svg(p: &Palette) -> String {
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 256 256">
  <rect x="8" y="8" width="240" height="240" rx="56" fill="{card}" stroke="{line}" stroke-width="4"/>
  <path d="M84 52h70l38 38v110a14 14 0 0 1-14 14H84a14 14 0 0 1-14-14V66a14 14 0 0 1 14-14z" fill="{text}"/>
  <path d="M154 52v26a12 12 0 0 0 12 12h26z" fill="{mute}"/>
  <g stroke-linecap="round" stroke-width="10" stroke="{card}">
    <path d="M94 120h74"/><path d="M94 146h74"/><path d="M94 172h48"/>
  </g>
  <path d="M74 166l108-40" stroke="{fail}" stroke-width="14" stroke-linecap="round"/>
</svg>"##,
        card = hex(p.card),
        line = hex(p.line2),
        text = hex(p.text),
        mute = hex(p.mute),
        fail = hex(p.fail),
    )
}

impl Render for Start {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let logo = self.logo(target_palette(cx));
        let this = cx.entity().downgrade();
        let chooser = move |id: &'static str, label: &'static str, glyph, directories: bool| {
            let this = this.clone();
            Button::secondary(id, label)
                .leading(glyph)
                .on_click(move |_, _, cx| {
                    this.update(cx, |this, cx| this.choose(directories, cx))
                        .ok();
                })
        };
        let recent = self.recent.clone();
        let rows: Vec<AnyElement> = recent
            .iter()
            .enumerate()
            .map(|(ix, (path, at))| self.row(ix, path, *at, cx))
            .collect();
        let empty = rows.is_empty();
        let tall = window.viewport_size().height > px(640.);
        div()
            .key_context("Start")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::next))
            .on_action(cx.listener(Self::previous))
            .on_action(cx.listener(Self::open))
            .size_full()
            .flex()
            .justify_center()
            .pt(if tall { px(120.) } else { space::XXL })
            .bg(p.bg)
            .child(
                div()
                    .w(px(600.))
                    .max_w_full()
                    .px(space::LG)
                    .flex()
                    .flex_col()
                    .child(enter(
                        "brand",
                        0,
                        div()
                            .flex()
                            .items_center()
                            .gap(space::LG)
                            .px(space::MD)
                            .pb(space::XXL)
                            .child(img(logo).size(px(52.)).flex_none())
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(2.))
                                    .child(
                                        div()
                                            .ui_text()
                                            .semibold()
                                            .text_size(px(26.))
                                            .line_height(px(32.))
                                            .text_color(p.text)
                                            .child("Rature"),
                                    )
                                    .child(
                                        div().ui_text().text_color(p.mute).child(
                                            "Read what changed, and fix it where you read it.",
                                        ),
                                    ),
                            ),
                    ))
                    .child(
                        div()
                            .px(space::MD)
                            .pb(space::SM)
                            .mono(11.)
                            .medium()
                            .text_color(p.faint)
                            .child(if empty { "NOTHING READ YET" } else { "RECENT" }),
                    )
                    .children(rows)
                    .child(enter(
                        "choosers",
                        recent.len() + 1,
                        div()
                            .flex()
                            .items_center()
                            .gap(space::SM)
                            .pt(space::XL)
                            .px(space::MD)
                            .child(chooser("open-file", "Open file", Icon::File, false))
                            .child(chooser("open-folder", "Open folder", Icon::Folder, true))
                            .child(div().flex_1())
                            .child(key_hint("f", "type a path", &p)),
                    )),
            )
    }
}

/// A key in a small frame and what it does: `[f] type a path`.
pub fn key_hint(key: &'static str, what: &'static str, p: &Palette) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap(px(6.))
        .child(
            div()
                .px(px(6.))
                .h(px(20.))
                .flex()
                .items_center()
                .rounded(px(5.))
                .border_1()
                .border_color(p.line2)
                .bg(p.card)
                .mono(11.)
                .text_color(p.mute)
                .child(key),
        )
        .child(
            div()
                .ui_text()
                .text_size(px(12.5))
                .text_color(p.faint)
                .child(what),
        )
}
