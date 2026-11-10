//! The window: the reader, a folder's tree beside it, and the picker over them.

use std::path::{Path, PathBuf};

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::config::{Config, Side};
use crate::formats::is_markdown;
use crate::picker::{Choice, Picker, PickerEvent, Target};
use crate::reader::{Jump, Reader, ReaderEvent};
use crate::start::{Start, StartEvent};
use crate::state::State;
use crate::tree::{Tree, TreeEvent};
use gpui_design_system::ToastStack;
use gpui_design_system::motion::PANEL;
use gpui_design_system::{fade, palette};

actions!(
    workspace,
    [
        FindFile,
        OpenPath,
        SearchFolder,
        Outline,
        ToggleTree,
        FocusTree,
        FocusReader
    ]
);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("f", FindFile, Some("Start")),
        KeyBinding::new("shift-o", OpenPath, Some("Start")),
    ]);
    for at in ["Reader", "Tree"] {
        cx.bind_keys([
            KeyBinding::new("f", FindFile, Some(at)),
            KeyBinding::new("s", SearchFolder, Some(at)),
            KeyBinding::new("t", ToggleTree, Some(at)),
            KeyBinding::new("shift-o", OpenPath, Some(at)),
        ]);
    }
    cx.bind_keys([
        KeyBinding::new("o", Outline, Some("Reader")),
        KeyBinding::new("ctrl-p", FindFile, Some("Workspace")),
        KeyBinding::new("ctrl-shift-f", SearchFolder, Some("Workspace")),
        KeyBinding::new("ctrl-h", FocusTree, Some("Workspace")),
        KeyBinding::new("ctrl-l", FocusReader, Some("Workspace")),
    ]);
}

pub struct Workspace {
    reader: Entity<Reader>,
    tree: Option<Entity<Tree>>,
    tree_shown: bool,
    tree_subscription: Option<Subscription>,
    picker: Option<(Entity<Picker>, Subscription)>,
    /// With nothing open: the recent files and the file chooser.
    start: Option<(Entity<Start>, Subscription)>,
    toasts: Entity<ToastStack>,
    _subscriptions: Vec<Subscription>,
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let reader = cx.new(|cx| Reader::new(window, cx));
        let subscription = cx.subscribe(&reader, |this, _, event: &ReaderEvent, cx| match event {
            ReaderEvent::Opened(path) => {
                if let Some(tree) = &this.tree {
                    // A file read in the folder keeps the folder among the recent ones, just
                    // after the file.
                    let root = tree.read(cx).root().to_path_buf();
                    let state = cx.global_mut::<State>();
                    state.opened(&root);
                    state.opened(path);
                    state.save();
                    tree.update(cx, |tree, cx| tree.reveal(path, cx));
                }
            }
            ReaderEvent::MarkedRead(path) => {
                if let Some(tree) = &this.tree {
                    tree.update(cx, |tree, cx| tree.recheck(path.clone(), cx));
                }
            }
        });
        Self {
            reader,
            tree: None,
            tree_shown: false,
            tree_subscription: None,
            picker: None,
            start: None,
            toasts: cx.new(ToastStack::new),
            _subscriptions: vec![subscription],
        }
    }

    pub fn reader(&self) -> &Entity<Reader> {
        &self.reader
    }

    /// Opens a file, or a folder with its tree and the README it may hold. Nothing: the start
    /// page.
    pub fn open(&mut self, path: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        self.start = None;
        match path {
            None => {
                let start = cx.new(Start::new);
                let subscription = cx.subscribe_in(
                    &start,
                    window,
                    |this, _, StartEvent::Open(path), window, cx| {
                        this.open(Some(path.clone()), window, cx)
                    },
                );
                let focus = start.read(cx).focus_handle().clone();
                window.focus(&focus, cx);
                self.start = Some((start, subscription));
            }
            Some(path) if path.is_dir() => {
                // Kept at once: a window killed rather than closed still lists it.
                cx.global_mut::<State>().opened(&path);
                cx.global::<State>().save();
                self.set_root(path.clone(), window, cx);
                self.tree_shown = true;
                match first_page(&path) {
                    Some(page) => self.open_file(page, Jump::Restore, window, cx),
                    None => {
                        self.reader.update(cx, |reader, cx| reader.close(cx));
                        if let Some(tree) = &self.tree {
                            let focus = tree.read(cx).focus_handle().clone();
                            window.focus(&focus, cx);
                        }
                    }
                }
            }
            Some(path) => {
                // A file from elsewhere leaves the folder.
                if self
                    .tree
                    .as_ref()
                    .is_some_and(|t| !path.starts_with(t.read(cx).root()))
                {
                    self.tree = None;
                    self.tree_subscription = None;
                    self.tree_shown = false;
                }
                self.open_file(path, Jump::Restore, window, cx)
            }
        }
        self.tree_changed(cx);
        cx.notify();
    }

    /// Tells the reader, whose bar holds the button that shows and hides the tree.
    fn tree_changed(&self, cx: &mut Context<Self>) {
        let tree = self
            .tree
            .is_some()
            .then(|| (cx.global::<Config>().tree, self.tree_shown));
        self.reader
            .update(cx, |reader, cx| reader.set_tree(tree, cx));
    }

    fn open_file(
        &mut self,
        path: PathBuf,
        jump: Jump,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.reader
            .update(cx, |reader, cx| reader.open(path, jump, window, cx));
        let focus = self.reader.read(cx).focus_handle().clone();
        window.focus(&focus, cx);
    }

    fn set_root(&mut self, root: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .tree
            .as_ref()
            .is_some_and(|t| t.read(cx).root() == root)
        {
            return;
        }
        let tree = cx.new(|cx| Tree::new(root, cx));
        let subscription =
            cx.subscribe_in(&tree, window, |this, _, event: &TreeEvent, window, cx| {
                this.tree_event(event, window, cx)
            });
        self.tree = Some(tree);
        self.tree_subscription = Some(subscription);
    }

    /// The folder searched and listed: the tree's, else the file's.
    fn root(&self, cx: &App) -> Option<PathBuf> {
        self.tree
            .as_ref()
            .map(|t| t.read(cx).root().to_path_buf())
            .or_else(|| {
                self.reader
                    .read(cx)
                    .path()
                    .and_then(Path::parent)
                    .map(Path::to_path_buf)
            })
    }

    fn show_picker(&mut self, picker: Entity<Picker>, window: &mut Window, cx: &mut Context<Self>) {
        let subscription = cx.subscribe_in(
            &picker,
            window,
            |this, _, event: &PickerEvent, window, cx| {
                let target = match event {
                    PickerEvent::Picked(target) => Some(target.clone()),
                    PickerEvent::Dismissed => None,
                };
                // Drawn a little longer, the time it fades out.
                let closing = this.picker.as_ref().map(|(p, _)| p.downgrade());
                cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .timer(gpui_design_system::motion::MODAL)
                        .await;
                    this.update(cx, |this, cx| {
                        if this.picker.as_ref().map(|(p, _)| p.downgrade()) == closing {
                            this.picker = None;
                            cx.notify();
                        }
                    })
                    .ok();
                })
                .detach();
                let focus = match &this.start {
                    Some((start, _)) => start.read(cx).focus_handle().clone(),
                    None => this.reader.read(cx).focus_handle().clone(),
                };
                window.focus(&focus, cx);
                match target {
                    Some(Target::File(path)) => this.open_file(path, Jump::Restore, window, cx),
                    Some(Target::Open(path)) => this.open(Some(path), window, cx),
                    Some(Target::Line(path, line)) => {
                        this.open_file(path, Jump::Line(line), window, cx)
                    }
                    Some(Target::Block(block)) => this.reader.update(cx, |reader, cx| {
                        reader.jump(Jump::Block(block, 0.), window, cx)
                    }),
                    None => {}
                }
                cx.notify();
            },
        );
        self.picker = Some((picker, subscription));
        cx.notify();
    }

    fn find_file(&mut self, _: &FindFile, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.root(cx) else {
            return self.open_path(&OpenPath, window, cx);
        };
        let recent = cx.global::<State>().recent.clone();
        let picker = cx.new(|cx| Picker::files(root, recent, window, cx));
        self.show_picker(picker, window, cx);
    }

    fn open_path(&mut self, _: &OpenPath, window: &mut Window, cx: &mut Context<Self>) {
        let recent = cx.global::<State>().recent.clone();
        let picker = cx.new(|cx| Picker::open(recent, window, cx));
        self.show_picker(picker, window, cx);
    }

    fn search_folder(&mut self, _: &SearchFolder, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.root(cx) else { return };
        let picker = cx.new(|cx| Picker::grep(root, window, cx));
        self.show_picker(picker, window, cx);
    }

    fn outline(&mut self, _: &Outline, window: &mut Window, cx: &mut Context<Self>) {
        let Some(doc) = self.reader.read(cx).document() else {
            return;
        };
        let top = doc.headings.iter().map(|h| h.level).min().unwrap_or(1);
        let choices: Vec<Choice> = doc
            .headings
            .iter()
            .map(|h| Choice {
                label: h.text.clone(),
                detail: String::new(),
                indent: usize::from(h.level - top),
                target: Target::Block(h.block),
            })
            .collect();
        let picker = cx.new(|cx| Picker::outline(choices, window, cx));
        self.show_picker(picker, window, cx);
    }

    fn toggle_tree(&mut self, _: &ToggleTree, window: &mut Window, cx: &mut Context<Self>) {
        if self.tree.is_none() {
            let Some(root) = self.root(cx) else { return };
            self.set_root(root, window, cx);
            if let Some(path) = self.reader.read(cx).path().map(Path::to_path_buf)
                && let Some(tree) = &self.tree
            {
                tree.update(cx, |tree, cx| tree.reveal(&path, cx));
            }
        }
        self.tree_shown = !self.tree_shown;
        if !self.tree_shown {
            let focus = self.reader.read(cx).focus_handle().clone();
            window.focus(&focus, cx);
        }
        self.tree_changed(cx);
        cx.notify();
    }

    fn focus_tree(&mut self, _: &FocusTree, window: &mut Window, cx: &mut Context<Self>) {
        if let (Some(tree), true) = (&self.tree, self.tree_shown) {
            let focus = tree.read(cx).focus_handle().clone();
            window.focus(&focus, cx);
        }
    }

    fn focus_reader(&mut self, _: &FocusReader, window: &mut Window, cx: &mut Context<Self>) {
        let focus = self.reader.read(cx).focus_handle().clone();
        window.focus(&focus, cx);
    }

    fn tree_event(&mut self, event: &TreeEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            TreeEvent::Open(path) => self.open_file(path.clone(), Jump::Restore, window, cx),
        }
    }
}

/// The page a folder opens on: its README or index, else its only Markdown file.
fn first_page(folder: &Path) -> Option<PathBuf> {
    for name in [
        "README.md",
        "readme.md",
        "Readme.md",
        "index.md",
        "INDEX.md",
    ] {
        let page = folder.join(name);
        if page.is_file() {
            return Some(page);
        }
    }
    let markdown: Vec<PathBuf> = std::fs::read_dir(folder)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_file() && is_markdown(p))
        .collect();
    (markdown.len() == 1).then(|| markdown[0].clone())
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = palette(cx);
        let config = cx.global::<Config>().clone();
        // The panel folds to nothing and back, its tree sliding under the reader's edge.
        let shown = fade("tree-panel", self.tree_shown, PANEL, window, cx);
        let panel = self.tree.as_ref().filter(|_| shown > 0.0).map(|tree| {
            div()
                .flex_none()
                .w(px(config.tree_width * shown))
                .h_full()
                .overflow_hidden()
                .flex()
                .when(config.tree == Side::Left, |d| d.justify_end().border_r_1())
                .when(config.tree == Side::Right, |d| d.border_l_1())
                .border_color(t.line)
                .child(
                    div()
                        .flex_none()
                        .w(px(config.tree_width))
                        .h_full()
                        .opacity(shown)
                        .child(tree.clone()),
                )
        });
        let reader = div().flex_1().h_full().min_w_0().child(self.reader.clone());
        div()
            .key_context("Workspace")
            .on_action(cx.listener(Self::find_file))
            .on_action(cx.listener(Self::open_path))
            .on_action(cx.listener(Self::search_folder))
            .on_action(cx.listener(Self::outline))
            .on_action(cx.listener(Self::toggle_tree))
            .on_action(cx.listener(Self::focus_tree))
            .on_action(cx.listener(Self::focus_reader))
            .size_full()
            .relative()
            .flex()
            .bg(t.bg)
            .text_color(t.text)
            .font_family(config.font.clone())
            .map(|d| match (&self.start, config.tree) {
                (Some((start, _)), _) => d.child(start.clone()),
                (None, Side::Left) => d.children(panel).child(reader),
                (None, Side::Right) => d.child(reader).children(panel),
            })
            .children(self.picker.as_ref().map(|(picker, _)| picker.clone()))
            .child(self.toasts.clone())
    }
}
