//! What the window shows with nothing open: the files and folders read last, and a way to open
//! another through the desktop's file chooser.

use std::path::PathBuf;

use gpui_kit::base::Button;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::picker::{name, shown};
use crate::theme::theme;

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

pub enum StartEvent {
    Open(PathBuf),
}

impl EventEmitter<StartEvent> for Start {}

pub struct Start {
    recent: Vec<PathBuf>,
    selected: usize,
    focus: FocusHandle,
}

impl Start {
    pub fn new(recent: Vec<PathBuf>, cx: &mut Context<Self>) -> Self {
        Self {
            recent: recent.into_iter().filter(|p| p.exists()).collect(),
            selected: 0,
            focus: cx.focus_handle(),
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
        if let Some(path) = self.recent.get(self.selected) {
            cx.emit(StartEvent::Open(path.clone()));
        }
    }

    /// The desktop's file chooser (the XDG portal), for a file or for a folder.
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
}

impl Render for Start {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let this = cx.entity().downgrade();
        let button = |id: &'static str, label: &'static str, directories: bool| {
            let this = this.clone();
            Button::new(id)
                .px_3()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(t.border)
                .bg(t.surface)
                .text_color(t.text)
                .cursor_pointer()
                .hover(|s| s.border_color(t.accent))
                .child(label)
                .on_click(move |_, _, cx| {
                    this.update(cx, |this, cx| this.choose(directories, cx))
                        .ok();
                })
        };
        div()
            .key_context("Start")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::next))
            .on_action(cx.listener(Self::previous))
            .on_action(cx.listener(Self::open))
            .size_full()
            .flex()
            .justify_center()
            .pt(px(96.))
            .bg(t.background)
            .text_size(px(14.))
            .child(
                div()
                    .w(px(560.))
                    .max_w_full()
                    .px_4()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .when(!self.recent.is_empty(), |d| {
                        d.child(div().text_color(t.muted).text_size(px(12.)).child("Recent"))
                            .child(div().flex().flex_col().children(
                                self.recent.iter().enumerate().map(|(ix, path)| {
                                    let path = path.clone();
                                    div()
                                        .id(("recent", ix))
                                        .flex()
                                        .gap_3()
                                        .px_2()
                                        .py(px(5.))
                                        .rounded_md()
                                        .cursor_pointer()
                                        .when(ix == self.selected, |d| d.bg(t.selection))
                                        .hover(|s| s.bg(t.surface))
                                        .child(
                                            div().flex_none().text_color(t.text).child(name(&path)),
                                        )
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .overflow_hidden()
                                                .whitespace_nowrap()
                                                .text_ellipsis()
                                                .text_color(t.muted)
                                                .text_size(px(12.5))
                                                .child(
                                                    path.parent().map(shown).unwrap_or_default(),
                                                ),
                                        )
                                        .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                                            cx.emit(StartEvent::Open(path.clone()))
                                        }))
                                }),
                            ))
                    })
                    .child(
                        div()
                            .pt_4()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(button("open-file", "Open a file…", false))
                            .child(button("open-folder", "Open a folder…", true))
                            .child(
                                div()
                                    .pl_2()
                                    .text_color(t.muted)
                                    .text_size(px(12.5))
                                    .child("or f to type a path"),
                            ),
                    ),
            )
    }
}
