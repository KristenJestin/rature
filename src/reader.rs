//! The file being read: what it shows, where the reading stands, what changed since it was last
//! read, the search inside it, and the block being edited.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use futures::StreamExt as _;
use gpui_kit::base::input::{
    Editor, EditorState, Input, InputEditorStyle, InputEvent, InputHighlighter, InputState,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::cache::{Cache, Slot};
use crate::config::{Config, Side};
use crate::diff::{self, Changes, Mark};
use crate::document::{self, Block, Document, Links, Picture};
use crate::formats::{self, Content};
use crate::highlight::Highlighter;
use crate::highlight::{self, Lines};
use crate::list::{self, ListEdit};
use crate::live::{Caret, Edge, LiveBlock, LiveEvent};
use crate::render::{self, Ctx};
use crate::start::key_hint;
use crate::state::{self, State};
use crate::table::TableEdit;
use crate::ui::field::{field_style, focus_frame, follow_theme};
use crate::ui::motion::MODAL;
use crate::ui::toast::Toast;
use crate::ui::{Button, Icon, Palette, Type, enter, icon, palette, space, toast};
use crate::workspace::ToggleTree;
use crate::{diagram, format, watch};

/// What `j` and `k` move.
const LINE: f32 = 64.;
/// How long typing rests before the block being edited is written.
const AUTOSAVE: Duration = Duration::from_millis(800);

actions!(
    reader,
    [
        LineDown,
        LineUp,
        HalfPageDown,
        HalfPageUp,
        PageDown,
        PageUp,
        Top,
        Bottom,
        OpenInNvim,
        Quit,
        Search,
        NextHit,
        PreviousHit,
        ClearSearch,
        NextChange,
        PreviousChange,
        MarkRead,
        SwitchBaseline,
        Back,
        Forward,
        EditBlock,
        StopEditing,
        Save,
        FoldAll,
        UnfoldAll,
        ZoomIn,
        ZoomOut,
        ZoomReset,
    ]
);

pub fn bind_keys(cx: &mut App) {
    let at = Some("Reader");
    cx.bind_keys([
        KeyBinding::new("j", LineDown, at),
        KeyBinding::new("down", LineDown, at),
        KeyBinding::new("k", LineUp, at),
        KeyBinding::new("up", LineUp, at),
        KeyBinding::new("ctrl-d", HalfPageDown, at),
        KeyBinding::new("ctrl-u", HalfPageUp, at),
        KeyBinding::new("space", PageDown, at),
        KeyBinding::new("pagedown", PageDown, at),
        KeyBinding::new("shift-space", PageUp, at),
        KeyBinding::new("ctrl-b", PageUp, at),
        KeyBinding::new("pageup", PageUp, at),
        KeyBinding::new("g g", Top, at),
        KeyBinding::new("home", Top, at),
        KeyBinding::new("shift-g", Bottom, at),
        KeyBinding::new("end", Bottom, at),
        KeyBinding::new("e", OpenInNvim, at),
        KeyBinding::new("q", Quit, at),
        KeyBinding::new("ctrl-f", Search, at),
        KeyBinding::new("n", NextHit, at),
        KeyBinding::new("shift-n", PreviousHit, at),
        KeyBinding::new("escape", ClearSearch, at),
        KeyBinding::new("] c", NextChange, at),
        KeyBinding::new("[ c", PreviousChange, at),
        KeyBinding::new("m", MarkRead, at),
        KeyBinding::new("shift-d", SwitchBaseline, at),
        KeyBinding::new("ctrl-o", Back, at),
        KeyBinding::new("alt-left", Back, at),
        KeyBinding::new("ctrl-i", Forward, at),
        KeyBinding::new("alt-right", Forward, at),
        KeyBinding::new("i", EditBlock, at),
        KeyBinding::new("ctrl-s", Save, at),
        KeyBinding::new("z shift-m", FoldAll, at),
        KeyBinding::new("z shift-r", UnfoldAll, at),
        KeyBinding::new("ctrl-=", ZoomIn, at),
        KeyBinding::new("ctrl-+", ZoomIn, at),
        KeyBinding::new("ctrl--", ZoomOut, at),
        KeyBinding::new("ctrl-0", ZoomReset, at),
        KeyBinding::new("escape", StopEditing, Some("Editing")),
        KeyBinding::new("ctrl-enter", StopEditing, Some("Editing")),
        KeyBinding::new("ctrl-s", Save, Some("Editing")),
        KeyBinding::new("escape", ClearSearch, Some("SearchBar")),
    ]);
}

pub enum ReaderEvent {
    Opened(PathBuf),
    MarkedRead(PathBuf),
}

impl EventEmitter<ReaderEvent> for Reader {}

/// Where to put the reading position once a file is shown.
#[derive(Clone)]
pub enum Jump {
    Top,
    /// A top-level block, and how far into it.
    Block(usize, f32),
    /// Where the file was left last time, else its top.
    Restore,
    Anchor(String),
    Line(usize),
}

/// What "what changed" compares the file to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Baseline {
    LastRead,
    Head,
}

/// A file of code, shown in an editor: it is changed where it is read.
struct CodeFile {
    field: Entity<EditorState>,
    /// The text as the file holds it: as read, or as last written.
    on_disk: String,
    autosave: Option<Task<()>>,
    _events: Subscription,
}

/// What a block is edited in: its text, or a table's cells.
#[derive(Clone)]
enum Field {
    Text(Entity<LiveBlock>),
    Table(Entity<TableEdit>),
    List(Entity<ListEdit>),
}

impl Field {
    fn value(&self, cx: &App) -> String {
        match self {
            Field::Text(field) => field.read(cx).value().to_string(),
            Field::Table(table) => table.read(cx).value(cx),
            Field::List(list) => list.read(cx).value(cx),
        }
    }

    fn set_value(&self, text: String, cx: &mut App) {
        match self {
            Field::Text(field) => field.update(cx, |field, cx| field.set_value(text, cx)),
            Field::Table(table) => table.update(cx, |table, _| table.set_value(text)),
            Field::List(list) => list.update(cx, |list, _| list.set_value(text)),
        }
    }

    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match self {
            Field::Text(field) => field.read(cx).focus_handle().clone(),
            Field::Table(table) => table.read(cx).focus_handle(cx),
            Field::List(list) => list.read(cx).focus_handle(cx),
        }
    }

    fn close(&self, cx: &mut App) {
        match self {
            Field::Text(field) => field.update(cx, |field, cx| field.close(cx)),
            Field::Table(table) => table.update(cx, |table, cx| table.close(cx)),
            Field::List(list) => list.update(cx, |list, cx| list.close(cx)),
        }
    }

    fn element(&self) -> AnyElement {
        match self {
            Field::Text(field) => field.clone().into_any_element(),
            Field::Table(table) => table.clone().into_any_element(),
            Field::List(list) => list.clone().into_any_element(),
        }
    }
}

struct Editing {
    block: usize,
    /// The block's source as last written: what is looked for in the file to replace it.
    original: String,
    field: Field,
    autosave: Option<Task<()>>,
    _events: Subscription,
}

/// What a button of the bar does to the reader.
type Run = fn(&mut Reader, &mut Window, &mut Context<Reader>);

pub struct Reader {
    path: Option<PathBuf>,
    /// The ink formulas were drawn in: a theme with another draws them again.
    ink: String,
    content: Content,
    baseline: Baseline,
    /// The file as the baseline has it, and how the file now differs from it.
    before: Option<(Document, Changes)>,
    drawings: Cache<String, Picture>,
    /// The link under the last click, followed if Ctrl was held.
    pub clicked_link: Option<String>,
    highlights: Cache<(String, String), Option<Lines>>,
    folded: HashSet<Vec<usize>>,
    /// The folder's tree, if there is one: the side it stands on and whether it is shown.
    tree: Option<(Side, bool)>,
    scroll: ScrollHandle,
    focus: FocusHandle,
    watch: Option<(notify::RecommendedWatcher, Task<()>)>,
    back: Vec<(PathBuf, usize, f32)>,
    forward: Vec<(PathBuf, usize, f32)>,
    search_field: Entity<InputState>,
    searching: bool,
    query: String,
    editing: Option<Editing>,
    code: Option<CodeFile>,
    /// A block just edited, its marks fading out before it is read again.
    closing: Option<(usize, Field, Task<()>)>,
    /// Blocks read again after an edit: they are back in place, they do not rise into it.
    settled: HashSet<u64>,
    zoom: f32,
    _subscriptions: Vec<Subscription>,
}

impl Reader {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search_field = cx.new(|cx| {
            let mut field = InputState::new(window, cx).placeholder("Search this file");
            follow_theme(&mut field, cx);
            field
        });
        let subscriptions = vec![
            cx.subscribe_in(
                &search_field,
                window,
                |this, field, event: &InputEvent, window, cx| match event {
                    InputEvent::Change => {
                        this.query = field.read(cx).value().to_lowercase();
                        cx.notify();
                    }
                    InputEvent::PressEnter { shift, .. } => {
                        this.searching = false;
                        this.next_hit(!shift, window, cx);
                        window.focus(&this.focus, cx);
                    }
                    _ => {}
                },
            ),
            // Each frame of a theme's cross-fade touches it: drawn again only for a new ink.
            cx.observe_global::<crate::ui::theme::Theme>(|this, cx| {
                if this.ink != render::ink(cx) {
                    this.restyle(cx)
                }
            }),
        ];
        Self {
            path: None,
            ink: render::ink(cx),
            content: Content::Empty,
            baseline: Baseline::LastRead,
            before: None,
            drawings: Cache::new(),
            clicked_link: None,
            highlights: Cache::new(),
            folded: HashSet::new(),
            scroll: ScrollHandle::new(),
            focus: cx.focus_handle(),
            watch: None,
            back: Vec::new(),
            forward: Vec::new(),
            search_field,
            tree: None,
            searching: false,
            query: String::new(),
            editing: None,
            code: None,
            closing: None,
            settled: HashSet::new(),
            zoom: 1.,
            _subscriptions: subscriptions,
        }
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    pub fn set_tree(&mut self, tree: Option<(Side, bool)>, cx: &mut Context<Self>) {
        self.tree = tree;
        cx.notify();
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn document(&self) -> Option<&Document> {
        match &self.content {
            Content::Markdown(doc) => Some(doc),
            _ => None,
        }
    }

    /// Shows no file: a folder opened without a page to show first.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.put_down(cx);
        self.path = None;
        self.watch = None;
        self.before = None;
        self.set_content(Content::Empty, cx);
    }

    /// Shows the file at `path`, follows its changes, and puts the reading position where
    /// `jump` says.
    pub fn open(&mut self, path: PathBuf, jump: Jump, window: &mut Window, cx: &mut Context<Self>) {
        if self.path.as_deref() == Some(&path) {
            self.jump(jump, window, cx);
            return;
        }
        self.finish_editing(window, cx);
        self.put_down(cx);
        self.watch = match watch::file(&path) {
            Ok((watcher, mut changes)) => {
                let task = cx.spawn(async move |this, cx| {
                    while changes.next().await.is_some() {
                        // A burst of writes is read once.
                        while changes.try_recv().is_ok() {}
                        if this.update(cx, |this, cx| this.reload(cx)).is_err() {
                            break;
                        }
                    }
                });
                Some((watcher, task))
            }
            Err(error) => {
                eprintln!("rature: {}: not followed: {error}", path.display());
                None
            }
        };
        self.path = Some(path.clone());
        self.code = None;
        self.folded.clear();
        self.scroll.set_offset(point(px(0.), px(0.)));
        self.reload(cx);
        // Kept at once: a window killed rather than closed still lists it.
        cx.global_mut::<State>().opened(&path);
        cx.global::<State>().save();
        cx.emit(ReaderEvent::Opened(path));
        self.jump(jump, window, cx);
    }

    /// Reads the file again. The scroll offset is kept: what is above the reading position
    /// rarely moves while a document grows below it.
    fn reload(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else {
            return;
        };
        let ink = render::ink(cx);
        let content = match formats::load(&path, &ink) {
            Ok(content) => content,
            // Between the removal and the rename of an atomic write: the next event brings it.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) => Content::Message(format!("It cannot be read: {error}.")),
        };
        self.set_content(content, cx);
        self.compare(cx);
    }

    /// The colours changed: what was drawn in the old ones is drawn again.
    fn restyle(&mut self, cx: &mut Context<Self>) {
        self.ink = render::ink(cx);
        // Coloured again in the new theme's colours.
        self.code = None;
        self.drawings.clear();
        self.highlights.clear();
        match self.path {
            Some(_) => self.reload(cx),
            None => {
                if let Content::Markdown(doc) = &self.content {
                    let source = doc.source.clone();
                    let ink = render::ink(cx);
                    self.set_content(Content::Markdown(document::parse(&source, &ink, None)), cx);
                }
            }
        }
    }

    fn set_content(&mut self, content: Content, cx: &mut Context<Self>) {
        let dark = render::is_dark(cx);
        let colors = diagram::Colors::of(&crate::ui::theme::target_palette(cx));
        // Diagrams: each new one is drawn in the background, the one that stood at its place
        // shown meanwhile so the page does not jump.
        let (mut sources, mut before) = (Vec::new(), Vec::new());
        if let Content::Markdown(doc) = &content {
            document::diagrams(&doc.blocks, &mut sources);
        }
        if let Content::Markdown(doc) = &self.content {
            document::diagrams(&doc.blocks, &mut before);
        }
        let shown: Vec<Option<Picture>> = before
            .iter()
            .map(|s| self.drawings.get(s).and_then(|slot| slot.shown().cloned()))
            .collect();
        self.drawings.retain(|source, _| sources.contains(source));
        for (ix, source) in sources.into_iter().enumerate() {
            if self.drawings.contains_key(&source) {
                continue;
            }
            let stand_in = shown.get(ix).cloned().flatten();
            self.drawings
                .insert(source.clone(), Slot::Pending(stand_in));
            let drawn = cx.background_executor().spawn({
                let (source, colors) = (source.clone(), colors.clone());
                async move { diagram::render(&source, &colors) }
            });
            cx.spawn(async move |this, cx| {
                let slot = match drawn.await {
                    Ok(picture) => Slot::Done(picture),
                    Err(error) => Slot::Failed(error),
                };
                this.update(cx, |this, cx| {
                    if let Some(place) = this.drawings.get_mut(&source) {
                        *place = slot;
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }
        // Code: coloured in the background, shown plain meanwhile.
        let mut code = Vec::new();
        match &content {
            Content::Markdown(doc) => document::code_blocks(&doc.blocks, &mut code),
            Content::Code { lang, text } => code.push((lang.clone(), text.clone())),
            _ => {}
        }
        self.highlights.retain(|key, _| code.contains(key));
        for key in code {
            if self.highlights.contains_key(&key) {
                continue;
            }
            self.highlights.insert(key.clone(), Slot::Pending(None));
            let coloured = cx.background_executor().spawn({
                let (lang, text) = key.clone();
                async move { highlight::highlight(&lang, &text, dark) }
            });
            cx.spawn(async move |this, cx| {
                let lines = coloured.await;
                this.update(cx, |this, cx| {
                    if let Some(place) = this.highlights.get_mut(&key) {
                        *place = Slot::Done(lines);
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }
        self.content = content;
        cx.notify();
    }

    /// What changed since the baseline. The first time a file is read, it is read: nothing
    /// changed yet.
    fn compare(&mut self, cx: &mut Context<Self>) {
        self.before = None;
        let (Some(path), Content::Markdown(doc)) = (&self.path, &self.content) else {
            return;
        };
        let source = match self.baseline {
            Baseline::LastRead => match state::last_read(path) {
                Some(source) => source,
                None => {
                    state::mark_read(path, &doc.source);
                    return;
                }
            },
            Baseline::Head => match git_head(path) {
                Some(source) => source,
                None => return,
            },
        };
        let ink = render::ink(cx);
        let old = document::parse(&source, &ink, None);
        let changes = diff::compare(&old, doc);
        self.before = Some((old, changes));
    }

    fn changes(&self) -> Option<&Changes> {
        self.before.as_ref().map(|(_, c)| c)
    }

    /// What is kept of the file the reader leaves, for the next time it opens: where it was
    /// read up to, and that its changes were seen.
    pub fn put_down(&self, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else {
            return;
        };
        let (block, within) = self.scroll.logical_scroll_top();
        cx.global_mut::<State>()
            .positions
            .insert(path.clone(), (block, -f32::from(within)));
        if let (Content::Markdown(doc), Some(_)) = (&self.content, &self.before) {
            state::mark_read(&path, &doc.source);
            cx.emit(ReaderEvent::MarkedRead(path));
        }
    }

    pub fn jump(&mut self, jump: Jump, window: &mut Window, cx: &mut Context<Self>) {
        let target = match jump {
            Jump::Top => Some((0, 0.)),
            Jump::Block(block, within) => Some((block, within)),
            Jump::Restore => Some(
                self.path
                    .as_ref()
                    .and_then(|p| cx.global::<State>().positions.get(p).copied())
                    .unwrap_or((0, 0.)),
            ),
            Jump::Anchor(anchor) => {
                let found = self
                    .document()
                    .and_then(|d| d.anchors.get(&anchor).copied());
                if found.is_none() {
                    self.say(format!("No #{anchor} in this file"), cx);
                }
                found.map(|b| (b, 0.))
            }
            Jump::Line(line) => Some((self.document().map_or(0, |d| d.block_at_line(line)), 0.)),
        };
        if let Some((block, within)) = target {
            // The blocks are laid out in the next frame: their places are known after it.
            cx.on_next_frame(window, move |this, window, cx| {
                this.scroll_to_block(block, within, 3, window, cx)
            });
        }
    }

    fn scroll_to_block(
        &mut self,
        block: usize,
        within: f32,
        tries: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.scroll.bounds_for_item(block) {
            Some(bounds) => {
                let offset = self.scroll.offset();
                let top = bounds.top() - self.scroll.bounds().top() - offset.y;
                // The first block shows with the page's own margin above it; another, with a
                // little room.
                let room = if block == 0 { top } else { px(24.) };
                let y = (-(top + px(within) - room))
                    .min(px(0.))
                    .max(-self.scroll.max_offset().y);
                self.scroll.set_offset(point(offset.x, y));
                cx.notify();
            }
            None if tries > 0 => cx.on_next_frame(window, move |this, window, cx| {
                this.scroll_to_block(block, within, tries - 1, window, cx)
            }),
            None => {}
        }
    }

    /// Follows a link: a page on the web, an anchor of this file, or another file (and its
    /// anchor), which the back key returns from.
    pub fn follow(&mut self, link: &str, window: &mut Window, cx: &mut Context<Self>) {
        if link.contains("://") || link.starts_with("mailto:") {
            cx.open_url(link);
            return;
        }
        let (file, anchor) = link.split_once('#').unwrap_or((link, ""));
        let jump = if anchor.is_empty() {
            Jump::Top
        } else {
            Jump::Anchor(anchor.to_string())
        };
        let target = if file.is_empty() {
            self.path.clone()
        } else {
            let folder = self
                .path
                .as_deref()
                .and_then(Path::parent)
                .unwrap_or(Path::new("."));
            Some(folder.join(document::decode(file)))
        };
        let Some(target) = target else {
            return self.jump(jump, window, cx);
        };
        let Ok(target) = std::fs::canonicalize(&target) else {
            return self.fail(format!("{} is not there", target.display()), cx);
        };
        if target.is_dir() {
            return self.fail(format!("{} is a folder", target.display()), cx);
        }
        self.remember(cx);
        self.forward.clear();
        if self.path.as_deref() == Some(&target) {
            self.jump(jump, window, cx);
        } else {
            self.open(target, jump, window, cx);
        }
    }

    fn here(&self) -> Option<(PathBuf, usize, f32)> {
        let (block, within) = self.scroll.logical_scroll_top();
        self.path.clone().map(|p| (p, block, -f32::from(within)))
    }

    fn remember(&mut self, _: &mut Context<Self>) {
        if let Some(here) = self.here() {
            self.back.push(here);
        }
    }

    fn go_back(&mut self, _: &Back, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((path, block, within)) = self.back.pop() {
            if let Some(here) = self.here() {
                self.forward.push(here);
            }
            self.open(path, Jump::Block(block, within), window, cx);
        }
    }

    fn go_forward(&mut self, _: &Forward, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((path, block, within)) = self.forward.pop() {
            if let Some(here) = self.here() {
                self.back.push(here);
            }
            self.open(path, Jump::Block(block, within), window, cx);
        }
    }

    pub fn copy(&mut self, text: String, what: &str, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.say(what.to_string(), cx);
    }

    /// The result of what was asked, in a toast that leaves by itself.
    pub fn say(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        toast(Toast::success(message), cx);
    }

    /// What could not be done, in a toast that stays until it is dismissed.
    fn fail(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        toast(Toast::failure(message), cx);
    }

    pub fn toggle_fold(&mut self, path: Vec<usize>, cx: &mut Context<Self>) {
        if !self.folded.remove(&path) {
            self.folded.insert(path);
        }
        cx.notify();
    }

    fn fold_all(&mut self, _: &FoldAll, _: &mut Window, cx: &mut Context<Self>) {
        fn walk(nodes: &[formats::Node], path: Vec<usize>, out: &mut HashSet<Vec<usize>>) {
            for (ix, node) in nodes.iter().enumerate() {
                let mut inner = path.clone();
                inner.push(ix);
                if let formats::Value::List(children) | formats::Value::Map(children) = &node.value
                {
                    if !inner.is_empty() && inner.len() > 1 {
                        out.insert(inner.clone());
                    }
                    walk(children, inner, out);
                }
            }
        }
        if let Content::Data(nodes) = &self.content {
            let mut folded = HashSet::new();
            walk(nodes, Vec::new(), &mut folded);
            self.folded = folded;
            cx.notify();
        }
    }

    fn unfold_all(&mut self, _: &UnfoldAll, _: &mut Window, cx: &mut Context<Self>) {
        self.folded.clear();
        cx.notify();
    }

    fn scroll_by(&mut self, delta: Pixels, cx: &mut Context<Self>) {
        let offset = self.scroll.offset();
        let y = (offset.y - delta)
            .min(px(0.))
            .max(-self.scroll.max_offset().y);
        self.scroll.set_offset(point(offset.x, y));
        cx.notify();
    }

    fn page(&self) -> Pixels {
        self.scroll.bounds().size.height
    }

    fn line_down(&mut self, _: &LineDown, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(px(LINE), cx);
    }
    fn line_up(&mut self, _: &LineUp, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(px(-LINE), cx);
    }
    fn half_page_down(&mut self, _: &HalfPageDown, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(self.page() / 2., cx);
    }
    fn half_page_up(&mut self, _: &HalfPageUp, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(-self.page() / 2., cx);
    }
    fn page_down(&mut self, _: &PageDown, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(self.page() - px(LINE), cx);
    }
    fn page_up(&mut self, _: &PageUp, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(-(self.page() - px(LINE)), cx);
    }
    fn top(&mut self, _: &Top, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(self.scroll.offset().y, cx);
    }
    fn bottom(&mut self, _: &Bottom, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(self.scroll.max_offset().y + self.scroll.offset().y, cx);
    }
    fn quit(&mut self, _: &Quit, window: &mut Window, cx: &mut Context<Self>) {
        self.finish_editing(window, cx);
        self.put_down(cx);
        cx.global::<State>().save();
        cx.quit();
    }

    /// Opens the file in nvim, in the desktop's terminal.
    fn open_in_nvim(&mut self, _: &OpenInNvim, _: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = &self.path else { return };
        let (block, _) = self.scroll.logical_scroll_top();
        let line = self
            .document()
            .and_then(|d| d.lines.get(block).copied())
            .unwrap_or(1);
        let spawned = std::process::Command::new("xdg-terminal-exec")
            .arg("nvim")
            .arg(format!("+{line}"))
            .arg(path)
            .stdin(std::process::Stdio::null())
            .spawn();
        if let Err(error) = spawned {
            self.fail(format!("Cannot open nvim: {error}"), cx);
        }
    }

    fn search(&mut self, _: &Search, window: &mut Window, cx: &mut Context<Self>) {
        self.searching = true;
        let focus = self.search_field.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    fn clear_search(&mut self, _: &ClearSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.searching = false;
        self.query.clear();
        self.search_field
            .update(cx, |field, cx| field.set_value("", window, cx));
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// The top-level blocks the search finds.
    fn hits(&self) -> Vec<usize> {
        match (&self.content, self.query.is_empty()) {
            (Content::Markdown(doc), false) => doc
                .texts
                .iter()
                .enumerate()
                .filter(|(_, text)| text.contains(&self.query))
                .map(|(ix, _)| ix)
                .collect(),
            _ => Vec::new(),
        }
    }

    fn next_hit(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let hits = self.hits();
        self.go_through(&hits, forward, "No match", window, cx);
    }

    /// Moves to the next (or previous) of `stops` after the top of the window, round the end.
    fn go_through(
        &mut self,
        stops: &[usize],
        forward: bool,
        none: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if stops.is_empty() {
            return self.say(none.to_string(), cx);
        }
        let (top, within) = self.scroll.logical_scroll_top();
        let next = if forward {
            stops
                .iter()
                .find(|&&s| s > top || (s == top && within < px(-4.)))
                .or(stops.first())
        } else {
            stops.iter().rev().find(|&&s| s < top).or(stops.last())
        };
        if let Some(&block) = next {
            self.jump(Jump::Block(block, -16.), window, cx);
        }
    }

    fn next_change(&mut self, _: &NextChange, window: &mut Window, cx: &mut Context<Self>) {
        let stops = self.changes().map(Changes::stops).unwrap_or_default();
        self.go_through(&stops, true, "Nothing changed", window, cx);
    }

    fn previous_change(&mut self, _: &PreviousChange, window: &mut Window, cx: &mut Context<Self>) {
        let stops = self.changes().map(Changes::stops).unwrap_or_default();
        self.go_through(&stops, false, "Nothing changed", window, cx);
    }

    fn mark_read(&mut self, _: &MarkRead, _: &mut Window, cx: &mut Context<Self>) {
        let (Some(path), Content::Markdown(doc)) = (&self.path, &self.content) else {
            return;
        };
        state::mark_read(path, &doc.source);
        let path = path.clone();
        self.baseline = Baseline::LastRead;
        self.compare(cx);
        self.say("Marked read", cx);
        cx.emit(ReaderEvent::MarkedRead(path));
    }

    fn switch_baseline(&mut self, _: &SwitchBaseline, _: &mut Window, cx: &mut Context<Self>) {
        self.baseline = match self.baseline {
            Baseline::LastRead => Baseline::Head,
            Baseline::Head => Baseline::LastRead,
        };
        if self.baseline == Baseline::Head && self.path.as_deref().and_then(git_head).is_none() {
            self.baseline = Baseline::LastRead;
            return self.say("Not in a git repository, or not committed", cx);
        }
        self.compare(cx);
        self.say(
            match self.baseline {
                Baseline::LastRead => "Changes since last read",
                Baseline::Head => "Changes since the last commit",
            },
            cx,
        );
    }

    fn zoom_by(&mut self, factor: Option<f32>, cx: &mut Context<Self>) {
        self.zoom = factor.map_or(1., |f| (self.zoom * f).clamp(0.6, 2.5));
        cx.notify();
    }
    fn zoom_in(&mut self, _: &ZoomIn, _: &mut Window, cx: &mut Context<Self>) {
        self.zoom_by(Some(1.1), cx);
    }
    fn zoom_out(&mut self, _: &ZoomOut, _: &mut Window, cx: &mut Context<Self>) {
        self.zoom_by(Some(1. / 1.1), cx);
    }
    fn zoom_reset(&mut self, _: &ZoomReset, _: &mut Window, cx: &mut Context<Self>) {
        self.zoom_by(None, cx);
    }

    /// Edits the first block of the window, in place.
    fn edit_first_block(&mut self, _: &EditBlock, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(code) = &self.code {
            let focus = code.field.read(cx).focus_handle(cx);
            return window.focus(&focus, cx);
        }
        let (top, within) = self.scroll.logical_scroll_top();
        // A block more than half gone above is not the one being read.
        let block = if within < px(-40.) { top + 1 } else { top };
        self.edit_block(block, Caret::Start, window, cx);
    }

    /// Edits the block `block` where it is read, the caret where `caret` says.
    pub fn edit_block(
        &mut self,
        block: usize,
        caret: Caret,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.finish_editing(window, cx);
        let (Some(path), Content::Markdown(doc)) = (&self.path, &self.content) else {
            return self.say("Only Markdown is edited here: `e` opens nvim", cx);
        };
        if block >= doc.blocks.len() {
            return;
        }
        let original = doc.source_of(block).to_string();
        let links = Rc::new(Links {
            folder: path.parent().unwrap_or(Path::new(".")).to_path_buf(),
            anchors: doc.anchors.keys().cloned().collect(),
        });
        let on = move |this: &mut Self,
                       event: &LiveEvent,
                       window: &mut Window,
                       cx: &mut Context<Self>| {
            match event {
                LiveEvent::Change => this.changed(block, window, cx),
                LiveEvent::Blur => this.finish_editing(window, cx),
                LiveEvent::Leave(edge) => this.leave(block, *edge, window, cx),
                // A paragraph breaks its own lines; tables and lists take these themselves.
                LiveEvent::Break(_) | LiveEvent::Join => {}
                LiveEvent::Follow(link) => this.follow(link, window, cx),
            }
        };
        let (field, events) = if matches!(doc.blocks[block], Block::Table(_)) {
            let table = cx.new(|cx| TableEdit::new(original.clone(), caret, links, window, cx));
            let events = cx.subscribe_in(&table, window, move |this, _, event, window, cx| {
                on(this, event, window, cx)
            });
            (Field::Table(table), events)
        } else if matches!(doc.blocks[block], Block::List { .. }) {
            let list = cx.new(|cx| ListEdit::new(original.clone(), caret, links, window, cx));
            let events = cx.subscribe_in(&list, window, move |this, _, event, window, cx| {
                on(this, event, window, cx)
            });
            (Field::List(list), events)
        } else {
            let text = cx.new(|cx| LiveBlock::new(original.clone(), caret, links, window, cx));
            let events = cx.subscribe_in(&text, window, move |this, _, event, window, cx| {
                on(this, event, window, cx)
            });
            (Field::Text(text), events)
        };
        let focus = field.focus_handle(cx);
        window.focus(&focus, cx);
        self.editing = Some(Editing {
            block,
            original,
            field,
            autosave: None,
            _events: events,
        });
        cx.notify();
    }

    /// The block being edited changed. A paragraph given a bullet is a list: written, it is
    /// edited again as one.
    fn changed(&mut self, block: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.autosave_later(cx);
        let Some(Editing {
            field: Field::Text(text),
            ..
        }) = &self.editing
        else {
            return;
        };
        let paragraph = self
            .document()
            .is_some_and(|doc| matches!(doc.blocks.get(block), Some(Block::Paragraph(_))));
        if paragraph && list::is_item(text.read(cx).value()) {
            self.write_edit(cx);
            self.reload(cx);
            self.edit_block(block, Caret::Start, window, cx);
        }
    }

    fn autosave_later(&mut self, cx: &mut Context<Self>) {
        let Some(editing) = &mut self.editing else {
            return;
        };
        editing.autosave = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(AUTOSAVE).await;
            this.update(cx, |this, cx| this.write_edit(cx)).ok();
        }));
    }

    /// Writes the block being edited into the file, in place of what it was. When the file
    /// changed around it so much that it is not found once, nothing is written and the text goes
    /// to the clipboard.
    fn write_edit(&mut self, cx: &mut Context<Self>) {
        let (Some(path), Some(editing)) = (self.path.clone(), &mut self.editing) else {
            return;
        };
        let text = editing.field.value(cx);
        if text == editing.original {
            return;
        }
        let written = std::fs::read_to_string(&path).and_then(|now| {
            match now.matches(editing.original.as_str()).count() {
                1 => {
                    std::fs::write(&path, now.replacen(&editing.original, &text, 1)).map(|()| true)
                }
                _ => Ok(false),
            }
        });
        match written {
            Ok(true) => {
                // Your own change is read already: what is shown as new stays what others wrote.
                if let Some(read) = state::last_read(&path)
                    && read.matches(editing.original.as_str()).count() == 1
                {
                    state::mark_read(&path, &read.replacen(&editing.original, &text, 1));
                    cx.emit(ReaderEvent::MarkedRead(path));
                }
                editing.original = text;
            }
            Ok(false) => self.copy(
                text,
                "The block changed on disk: your text is in the clipboard",
                cx,
            ),
            Err(error) => self.fail(format!("Cannot write: {error}"), cx),
        }
    }

    fn finish_editing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing.is_none() {
            return;
        }
        self.write_edit(cx);
        let Some(editing) = self.editing.take() else {
            return;
        };
        let (block, field) = (editing.block, editing.field);
        field.close(cx);
        let done = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(MODAL).await;
            this.update(cx, |this, cx| {
                if let Some(doc) = this.document()
                    && block < doc.blocks.len()
                {
                    this.settled.insert(fingerprint(doc.source_of(block)));
                }
                this.closing = None;
                cx.notify();
            })
            .ok();
        });
        self.closing = Some((block, field, done));
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// The caret was moved out of the block `block`: the one beyond is edited, the caret on
    /// the side it came from.
    fn leave(&mut self, block: usize, edge: Edge, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.document().map_or(0, |doc| doc.blocks.len());
        let (next, caret) = match edge {
            Edge::Above | Edge::Before if block > 0 => (block - 1, Caret::End),
            Edge::Below | Edge::After if block + 1 < count => (block + 1, Caret::Start),
            _ => return,
        };
        self.edit_block(next, caret, window, cx);
    }

    fn stop_editing(&mut self, _: &StopEditing, window: &mut Window, cx: &mut Context<Self>) {
        if self.code.is_some() {
            self.write_code(cx);
            return window.focus(&self.focus, cx);
        }
        self.finish_editing(window, cx);
    }

    /// The code editor of the file shown, made the first time it is drawn and given the file's
    /// text again when something else changes it on disk.
    fn sync_code(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Content::Code { lang, text } = &self.content else {
            self.code = None;
            return;
        };
        if let Some(code) = &mut self.code {
            if code.on_disk != *text {
                code.on_disk = text.clone();
                code.autosave = None;
                let text = text.clone();
                code.field
                    .update(cx, |field, cx| field.set_value(text, window, cx));
            }
            return;
        }
        let (lang, text) = (lang.clone(), text.clone());
        let dark = render::is_dark(cx);
        let style = InputEditorStyle {
            background: palette(cx).bg,
            ..field_style(cx)
        };
        let field = cx.new(|cx| {
            let mut field = EditorState::new(window, cx)
                .line_number(false)
                .indent_guides(false)
                .folding(false)
                .language(lang);
            field.set_highlighter_factory(
                Rc::new(move |lang: &str| {
                    Some(Box::new(Highlighter::new(lang, dark)) as Box<dyn InputHighlighter>)
                }),
                cx,
            );
            field.set_editor_style(style);
            field.set_value(text.clone(), window, cx);
            field
        });
        let events = cx.subscribe(&field, |this, _, event: &InputEvent, cx| {
            if let (InputEvent::Change, Some(code)) = (event, &mut this.code) {
                code.autosave = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(AUTOSAVE).await;
                    this.update(cx, |this, cx| this.write_code(cx)).ok();
                }));
            }
        });
        self.code = Some(CodeFile {
            field,
            on_disk: text,
            autosave: None,
            _events: events,
        });
    }

    /// Writes the code editor's text into the file, when it differs from it.
    fn write_code(&mut self, cx: &mut Context<Self>) {
        let (Some(path), Some(code)) = (self.path.clone(), &mut self.code) else {
            return;
        };
        let text = code.field.read(cx).value().to_string();
        if text == code.on_disk {
            return;
        }
        match std::fs::write(&path, &text) {
            Ok(()) => code.on_disk = text,
            Err(error) => self.fail(format!("Cannot write: {error}"), cx),
        }
    }

    /// Writes the edit, then formats the whole file as its kind says.
    fn save(&mut self, _: &Save, _: &mut Window, cx: &mut Context<Self>) {
        self.write_edit(cx);
        self.write_code(cx);
        let Some(path) = self.path.clone() else {
            return;
        };
        let result = std::fs::read_to_string(&path).and_then(|now| {
            let formatted = format::format(&path, &now);
            if formatted == now {
                return Ok(false);
            }
            std::fs::write(&path, &formatted)?;
            if state::last_read(&path).as_deref() == Some(now.as_str()) {
                state::mark_read(&path, &formatted);
            }
            if let Some(editing) = &mut self.editing {
                // The block moved under the formatting: its text is the formatted one now.
                let doc = document::parse(&formatted, "#000000", None);
                if let Some(span) = doc.spans.get(editing.block) {
                    editing.original = formatted[span.clone()].to_string();
                }
            }
            Ok(true)
        });
        match result {
            Ok(true) => self.say("Formatted and saved", cx),
            Ok(false) => self.say("Saved", cx),
            Err(error) => self.fail(format!("Cannot save: {error}"), cx),
        }
        if let Some(editing) = &self.editing {
            let original = editing.original.clone();
            editing.field.clone().set_value(original, cx);
        }
    }

    /// The bar over the page: where the file is, what changed, and the search.
    fn bar(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let changes = self.changes().map_or(0, Changes::count);
        let this = cx.entity().downgrade();
        let act = move |id: &'static str, glyph: Icon, tip: &'static str, run: Run| {
            let this = this.clone();
            Button::icon(id, glyph, tip).on_click(move |_, window, cx| {
                this.update(cx, |this, cx| run(this, window, cx)).ok();
            })
        };
        let crumbs = self.path.as_deref().map(|path| {
            let folder = path
                .parent()
                .and_then(Path::file_name)
                .map(|n| n.to_string_lossy().into_owned());
            let file = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            div()
                .flex()
                .items_center()
                .gap(space::SM)
                .min_w_0()
                .overflow_hidden()
                .child(icon(formats::icon(path)).text_color(p.mute))
                .children(folder.map(|folder| {
                    div()
                        .flex()
                        .gap(space::SM)
                        .text_color(p.faint)
                        .child(folder)
                        .child("/")
                }))
                .child(
                    div()
                        .text_color(p.text)
                        .medium()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .overflow_hidden()
                        .child(file),
                )
        });
        let hits = (!self.query.is_empty()).then(|| self.hits().len());
        let status: AnyElement = if self.searching || hits.is_some() {
            let focused = self
                .search_field
                .read(cx)
                .focus_handle(cx)
                .is_focused(window);
            let frame = div()
                .h(px(28.))
                .w(px(300.))
                .px(space::SM)
                .rounded(px(7.))
                .bg(p.field);
            focus_frame("search-field", frame, focused, window, cx)
                .flex()
                .items_center()
                .gap(space::SM)
                .child(icon(Icon::Search).text_color(p.faint))
                .child(
                    div()
                        .key_context("SearchBar")
                        .on_action(cx.listener(Self::clear_search))
                        .flex_1()
                        .min_w_0()
                        .text_color(p.text)
                        .child(Input::new(&self.search_field)),
                )
                .children(hits.map(|n| {
                    div()
                        .flex_none()
                        .mono(11.5)
                        .text_color(if n == 0 { p.fail } else { p.mute })
                        .child(format!("{n} block{}", if n == 1 { "" } else { "s" }))
                }))
                .into_any_element()
        } else if changes > 0 {
            let since = match self.baseline {
                Baseline::LastRead => "since read",
                Baseline::Head => "since commit",
            };
            div()
                .flex()
                .items_center()
                .gap(px(2.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .h(px(24.))
                        .px(space::SM)
                        .mr(space::XS)
                        .rounded_full()
                        .bg(p.wait_bg)
                        .text_color(p.wait)
                        .mono(11.5)
                        .medium()
                        .child(div().size(px(6.)).rounded_full().bg(p.wait))
                        .child(format!(
                            "{changes} change{} {since}",
                            if changes == 1 { "" } else { "s" }
                        )),
                )
                .child(act(
                    "previous-change",
                    Icon::ChevronUp,
                    "Previous change  [c",
                    |this, window, cx| this.previous_change(&PreviousChange, window, cx),
                ))
                .child(act(
                    "next-change",
                    Icon::Chevron,
                    "Next change  ]c",
                    |this, window, cx| this.next_change(&NextChange, window, cx),
                ))
                .into_any_element()
        } else {
            div().into_any_element()
        };
        let panel = self.tree.map(|(side, shown)| {
            let glyph = match (side, shown) {
                (Side::Left, true) => Icon::PanelLeftClose,
                (Side::Left, false) => Icon::PanelLeft,
                (Side::Right, true) => Icon::PanelRightClose,
                (Side::Right, false) => Icon::PanelRight,
            };
            let tip = if shown {
                "Hide the tree  t"
            } else {
                "Show the tree  t"
            };
            (
                side,
                act("toggle-tree", glyph, tip, |_, window, cx| {
                    window.dispatch_action(Box::new(ToggleTree), cx)
                }),
            )
        });
        let (left, right) = match panel {
            Some((Side::Left, button)) => (Some(button), None),
            Some((Side::Right, button)) => (None, Some(button)),
            None => (None, None),
        };
        div()
            .flex_none()
            .h(px(44.))
            .px(space::LG)
            .flex()
            .items_center()
            .gap(space::MD)
            .border_b_1()
            .border_color(p.line)
            .bg(p.bg)
            .ui_text()
            .children(left)
            .children(crumbs)
            .child(div().flex_1())
            .child(status)
            .when(!self.searching && hits.is_none(), |d| {
                d.child(act(
                    "search",
                    Icon::Search,
                    "Search  Ctrl-F",
                    |this, window, cx| this.search(&Search, window, cx),
                ))
            })
            .children(right)
    }

    fn items(&self, ctx: &Ctx, cx: &Context<Self>) -> Vec<AnyElement> {
        let t = palette(cx);
        let column = cx.global::<Config>().column;
        let wrap = |element: AnyElement| -> AnyElement {
            div()
                .w_full()
                .max_w(px(column))
                .px_8()
                .pb_4()
                .child(element)
                .into_any_element()
        };
        match &self.content {
            Content::Markdown(doc) => {
                let changes = self.before.as_ref();
                let mut items = Vec::with_capacity(doc.blocks.len() + 1);
                for (ix, block) in doc.blocks.iter().enumerate() {
                    let mut item = div().flex().flex_col().gap_2();
                    if let Some((old, changes)) = changes {
                        for (_, gone) in changes.removed.iter().filter(|(before, _)| *before == ix)
                        {
                            item = item.child(render::ghost(ctx, &old.blocks[*gone]));
                        }
                    }
                    let mark = changes.map(|(_, c)| &c.marks[ix]);
                    let live = self
                        .editing
                        .as_ref()
                        .filter(|e| e.block == ix)
                        .map(|e| e.field.clone())
                        .or_else(|| {
                            self.closing
                                .as_ref()
                                .filter(|(block, ..)| *block == ix)
                                .map(|(_, field, _)| field.clone())
                        });
                    let shown = match live {
                        Some(field) => {
                            // In the type the block is read in: only its marks show.
                            let frame = div()
                                .key_context("Editing")
                                .on_action(cx.listener(Self::stop_editing))
                                .on_action(cx.listener(Self::save));
                            let frame = match block {
                                Block::Heading(level, _) => render::heading(ctx, *level, frame),
                                Block::Code { .. }
                                | Block::Diagram(_)
                                | Block::Broken { .. }
                                | Block::FrontMatter(_) => frame
                                    .font_family(cx.global::<Config>().code_font.clone())
                                    .text_size(ctx.size(0.9)),
                                _ => frame,
                            };
                            frame.child(field.element()).into_any_element()
                        }
                        None => self.block_element(ctx, doc, ix, block, mark, cx),
                    };
                    item = item.child(shown);
                    items.push(wrap(item.into_any_element()));
                }
                if let Some((old, changes)) = changes {
                    let end = doc.blocks.len();
                    let trailing: Vec<AnyElement> = changes
                        .removed
                        .iter()
                        .filter(|(before, _)| *before == end)
                        .map(|(_, gone)| render::ghost(ctx, &old.blocks[*gone]))
                        .collect();
                    if !trailing.is_empty() {
                        items.push(wrap(
                            div()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .children(trailing)
                                .into_any_element(),
                        ));
                    }
                }
                items
            }
            Content::Code { lang, text } => {
                vec![wrap(render::code(ctx, lang, text).into_any_element())]
            }
            Content::Data(nodes) => vec![wrap(render::data(ctx, nodes, &self.folded))],
            Content::Table(rows) => vec![
                div()
                    .w_full()
                    .px_8()
                    .pb_4()
                    .child(render::table(ctx, rows))
                    .into_any_element(),
            ],
            Content::Image(path) => vec![wrap(render::image_file(path))],
            Content::Empty => vec![notice(
                "no-file",
                Icon::Text,
                "No file open",
                "Pick one in the tree.",
                &[("f", "find a file"), ("s", "search the folder")],
                &t,
            )],
            Content::Message(message) => vec![notice(
                "not-shown",
                Icon::Warning,
                "This file is not shown",
                message.clone(),
                &[],
                &t,
            )],
        }
    }

    fn block_element(
        &self,
        ctx: &Ctx,
        doc: &Document,
        ix: usize,
        block: &Block,
        mark: Option<&Mark>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let t = palette(cx);
        let inner = match (block, mark) {
            (Block::Paragraph(_), Some(Mark::Changed(Some(words)))) => {
                render::changed_words(ctx, words).into_any_element()
            }
            (Block::Heading(level, _), Some(Mark::Changed(Some(words)))) => {
                render::heading(ctx, *level, render::changed_words(ctx, words)).into_any_element()
            }
            _ => render::block(ctx, block),
        };
        let inner = match mark {
            Some(Mark::Added) => render::marked(ctx, t.ok, t.ok_bg, inner),
            Some(Mark::Changed(_)) => render::marked(ctx, t.wait, t.wait_bg, inner),
            _ => inner,
        };
        // A block that arrives while the page is read rises into place.
        let print = fingerprint(doc.source_of(ix));
        let arrived = if self.settled.contains(&print) {
            inner
        } else {
            enter(ElementId::Name(format!("block-{print}").into()), 0, inner).into_any_element()
        };
        div()
            .id(("block", ix))
            .child(arrived)
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                if let Some(link) = this.clicked_link.take()
                    && event.modifiers().secondary()
                {
                    return this.follow(&link, window, cx);
                }
                this.edit_block(ix, Caret::At(event.position()), window, cx);
            }))
            .into_any_element()
    }
}

/// A short name for a block's source: the same text keeps its entrance played.
fn fingerprint(source: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut hasher);
    hasher.finish()
}

/// The file as the last commit has it.
fn git_head(path: &Path) -> Option<String> {
    let folder = path.parent()?;
    let name = path.file_name()?.to_string_lossy();
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(folder)
        .arg("show")
        .arg(format!("HEAD:./{name}"))
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// What the page says when it has no file to show: a sign, a title, a sentence and the keys
/// that lead on, in the window's own type rather than as a page.
fn notice(
    id: &'static str,
    glyph: Icon,
    title: &'static str,
    sentence: impl Into<SharedString>,
    keys: &[(&'static str, &'static str)],
    p: &Palette,
) -> AnyElement {
    enter(
        id,
        0,
        div()
            .pt(px(96.))
            .px_8()
            .flex()
            .flex_col()
            .items_center()
            .gap(space::SM)
            .ui_text()
            .child(
                div()
                    .size(px(48.))
                    .mb(space::SM)
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(12.))
                    .border_1()
                    .border_color(p.line2)
                    .bg(p.card)
                    .child(icon(glyph).size(px(20.)).text_color(p.mute)),
            )
            .child(
                div()
                    .text_size(px(15.))
                    .medium()
                    .text_color(p.text)
                    .child(title),
            )
            .child(
                div()
                    .max_w(px(420.))
                    .text_center()
                    .text_color(p.mute)
                    .child(sentence.into()),
            )
            .when(!keys.is_empty(), |d| {
                d.child(
                    div()
                        .pt(space::MD)
                        .flex()
                        .gap(space::LG)
                        .children(keys.iter().map(|(key, what)| key_hint(key, what, p))),
                )
            }),
    )
    .into_any_element()
}

impl Render for Reader {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = palette(cx);
        let config = cx.global::<Config>().clone();
        let folder = self
            .path
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf);
        let query = (!self.query.is_empty()).then(|| self.query.clone());
        self.sync_code(window, cx);
        let code = self.code.as_ref().map(|code| code.field.clone());
        let editing = self.editing.is_some()
            || code
                .as_ref()
                .is_some_and(|field| field.read(cx).focus_handle(cx).contains_focused(window, cx));
        let items = {
            let ctx = Ctx::new(
                &t,
                &config,
                self.zoom,
                folder.as_deref(),
                &self.drawings,
                &self.highlights,
                query.as_deref(),
                cx,
            );
            self.items(&ctx, cx)
        };
        let bar = self.bar(window, cx);
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(t.bg)
            .child(bar)
            .child(
                div()
                    .id("page")
                    .when(!editing, |d| d.key_context("Reader"))
                    .track_focus(&self.focus)
                    .on_action(cx.listener(Self::line_down))
                    .on_action(cx.listener(Self::line_up))
                    .on_action(cx.listener(Self::half_page_down))
                    .on_action(cx.listener(Self::half_page_up))
                    .on_action(cx.listener(Self::page_down))
                    .on_action(cx.listener(Self::page_up))
                    .on_action(cx.listener(Self::top))
                    .on_action(cx.listener(Self::bottom))
                    .on_action(cx.listener(Self::open_in_nvim))
                    .on_action(cx.listener(Self::quit))
                    .on_action(cx.listener(Self::search))
                    .on_action(
                        cx.listener(|this, _: &NextHit, window, cx| {
                            this.next_hit(true, window, cx)
                        }),
                    )
                    .on_action(cx.listener(|this, _: &PreviousHit, window, cx| {
                        this.next_hit(false, window, cx)
                    }))
                    .on_action(cx.listener(Self::clear_search))
                    .on_action(cx.listener(Self::next_change))
                    .on_action(cx.listener(Self::previous_change))
                    .on_action(cx.listener(Self::mark_read))
                    .on_action(cx.listener(Self::switch_baseline))
                    .on_action(cx.listener(Self::go_back))
                    .on_action(cx.listener(Self::go_forward))
                    .on_action(cx.listener(Self::edit_first_block))
                    .on_action(cx.listener(Self::save))
                    .on_action(cx.listener(Self::fold_all))
                    .on_action(cx.listener(Self::unfold_all))
                    .on_action(cx.listener(Self::zoom_in))
                    .on_action(cx.listener(Self::zoom_out))
                    .on_action(cx.listener(Self::zoom_reset))
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .text_color(t.text)
                    .font_family(config.font.clone())
                    .text_size(px(config.font_size * self.zoom))
                    .line_height(rems(1.6))
                    .map(|d| match code {
                        // The editor scrolls itself.
                        Some(field) => d.pt(space::XL).px_8().child(
                            div()
                                .key_context("Editing")
                                .on_action(cx.listener(Self::stop_editing))
                                .on_action(cx.listener(Self::save))
                                .size_full()
                                .font_family(config.code_font.clone())
                                .text_size(px(config.font_size * 0.9 * self.zoom))
                                .line_height(rems(1.5))
                                .child(Editor::new(&field)),
                        ),
                        None => d
                            .track_scroll(&self.scroll)
                            .overflow_y_scroll()
                            .flex()
                            .flex_col()
                            .items_center()
                            .pt(space::XXXL)
                            .pb(px(160.))
                            .children(items),
                    }),
            )
    }
}
