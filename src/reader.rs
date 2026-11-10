//! The file being read: what it shows, where the reading stands, what changed since it was last
//! read, the search inside it, and the block being edited.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt as _;
use gpui_kit::base::input::{
    Editor, EditorState, Input, InputEditorStyle, InputEvent, InputHighlighter, InputState,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::cache::{Cache, Slot};
use crate::config::{Config, Side};
use crate::diff::{Changes, Mark};
use crate::document::{self, Block, Document, Links, Picture};
use crate::formats::{self, Content, Loaded};
use crate::fsio::{self, Update};
use crate::highlight::Highlighter;
use crate::highlight::{self, Lines};
use crate::icons;
use crate::kit::focus_frame;
use crate::list::{self, ListEdit};
use crate::live::{Caret, Edge, LiveBlock, LiveEvent};
use crate::load::{self, Against, Baseline, Done, Entrances, Generation, Old, fingerprint};
use crate::render::{self, Ctx, DataRow};
use crate::start::key_hint;
use crate::state::{self, State};
use crate::table::TableEdit;
use crate::workspace::ToggleTree;
use crate::{diagram, format, watch};
use gpui_design_system::field::{field_style, follow_theme};
use gpui_design_system::motion::{ENTER, MODAL};
use gpui_design_system::toast::Toast;
use gpui_design_system::{Button, Icon, Palette, Type, enter, icon, palette, space, toast};

/// What `j` and `k` move.
const LINE: f32 = 64.;
/// The room left above a block scrolled to.
const ROOM: f32 = 24.;
/// How far beyond the window the page lays out its blocks, so that a scroll finds them ready.
const OVERDRAW: f32 = 800.;
/// How long typing rests before the block being edited is written.
const AUTOSAVE: Duration = Duration::from_millis(800);
/// Why a file that is not UTF-8 is not edited.
const NOT_UTF8: &str = "Not UTF-8 text: shown, never edited, so its bytes stay as they are";

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

/// When Rature started, set when `RATURE_TIMING` asks for the timings.
pub struct Started(pub Instant);

impl Global for Started {}

/// A file of code, shown in an editor: it is changed where it is read.
struct CodeFile {
    field: Entity<EditorState>,
    /// The text as the file holds it: as read, or as last written.
    on_disk: String,
    /// The editor's text when it was given `on_disk`: it differs only once something is typed.
    shown: String,
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
    /// A file opened and not read yet: nothing is shown of the one before.
    loading: bool,
    /// The file is UTF-8 text, which may be edited and written back.
    editable: bool,
    baseline: Baseline,
    /// The file as the baseline has it, and how the file now differs from it.
    before: Option<(Arc<Document>, Changes)>,
    /// The version compared to, parsed: the next change of the file does not parse it again.
    old: Option<Old>,
    /// The reads of the file asked for, and the comparisons: only the latest result is shown.
    loads: Generation,
    compares: Generation,
    /// `D` asked for git's version: said once its comparison is known.
    switching: bool,
    /// Where to go once the file being read is shown.
    pending_jump: Option<Jump>,
    /// A block to edit again once the file being read is shown: a paragraph become a list.
    pending_edit: Option<usize>,
    /// The window, which the work done off the main thread comes back to.
    window: AnyWindowHandle,
    /// The fingerprint of each top-level block, and which of them rise into place.
    prints: Vec<u64>,
    entrances: Entrances,
    drawings: Cache<String, Picture>,
    /// The link under the last click, followed if Ctrl was held.
    pub clicked_link: Option<String>,
    highlights: Cache<(String, String), Option<Lines>>,
    folded: HashSet<Vec<usize>>,
    /// The lines a JSON or YAML tree shows, its folds left out.
    data_lines: Rc<Vec<DataRow>>,
    /// The folder's tree, if there is one: the side it stands on and whether it is shown.
    tree: Option<(Side, bool)>,
    /// The page's blocks, of which only those in sight are built.
    list: ListState,
    /// The rows of a CSV file or of a JSON or YAML tree, all as high.
    rows: UniformListScrollHandle,
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
    /// The zoom the blocks were last laid out at.
    zoomed: f32,
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
            cx.observe_global::<gpui_design_system::theme::Theme>(|this, cx| {
                if this.ink != render::ink(cx) {
                    this.restyle(cx)
                }
            }),
        ];
        let now = cx.background_executor().now();
        Self {
            path: None,
            ink: render::ink(cx),
            content: Content::Empty,
            loading: false,
            editable: false,
            baseline: Baseline::LastRead,
            before: None,
            old: None,
            loads: Generation::default(),
            compares: Generation::default(),
            switching: false,
            pending_jump: None,
            pending_edit: None,
            window: window.window_handle(),
            prints: Vec::new(),
            entrances: Entrances::new(now, ENTER),
            drawings: Cache::new(),
            clicked_link: None,
            highlights: Cache::new(),
            folded: HashSet::new(),
            data_lines: Rc::default(),
            list: ListState::new(0, ListAlignment::Top, px(OVERDRAW)),
            rows: UniformListScrollHandle::new(),
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
            zoomed: 1.,
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
        self.old = None;
        self.loading = false;
        // What was being read is not shown when it comes.
        self.loads.next();
        self.compares.next();
        self.pending_jump = None;
        self.set_content(Content::Empty, Vec::new(), cx);
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
        // Nothing of the file before is shown while this one is read.
        self.loading = true;
        self.editable = false;
        self.before = None;
        self.old = None;
        self.prints.clear();
        self.set_content(Content::Empty, Vec::new(), cx);
        self.list.reset(0);
        self.rows = UniformListScrollHandle::new();
        self.entrances.opened(cx.background_executor().now());
        self.reload(cx);
        // Kept at once: a window killed rather than closed still lists it.
        cx.global_mut::<State>().opened(&path);
        cx.global::<State>().save();
        cx.emit(ReaderEvent::Opened(path));
        self.jump(jump, window, cx);
    }

    /// Reads the file again, off the main thread: read, parsed, its formulas drawn, its links
    /// checked and compared to its baseline there, put in place here once done, unless the file
    /// was asked for again meanwhile. The reading position is kept: what is above it rarely moves
    /// while a document grows below it.
    fn reload(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else {
            return;
        };
        let (load, compare) = (self.loads.next(), self.compares.next());
        let (ink, baseline, cached) = (render::ink(cx), self.baseline, self.old.clone());
        let work = cx
            .background_executor()
            .spawn(async move { load::load(path, ink, baseline, cached) });
        let window = self.window;
        cx.spawn(async move |this, cx| {
            let done = work.await;
            cx.update_window(window, |_, window, cx| {
                this.update(cx, |this, cx| this.loaded(load, compare, done, window, cx))
            })
            .ok();
        })
        .detach();
    }

    /// A file read off the main thread: shown, if it is still the one asked for.
    fn loaded(
        &mut self,
        load: u64,
        compare: u64,
        done: Done,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.loads.is_current(load) || self.path.as_deref() != Some(done.path.as_path()) {
            return;
        }
        let started = Instant::now();
        let (content, editable) = match done.loaded {
            Ok(Loaded { content, editable }) => (content, editable),
            // Between the removal and the rename of an atomic write: the next event brings it.
            // A file just opened is said not to be there.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && !self.loading => return,
            Err(error) => (
                Content::Message(format!("It cannot be read: {error}.")),
                false,
            ),
        };
        self.keep_typed_code(&content, cx);
        self.editable = editable;
        let opened = std::mem::take(&mut self.loading);
        if !opened {
            let now = cx.background_executor().now();
            self.entrances.changed(&self.prints, &done.prints, now);
        }
        self.set_content(content, done.prints, cx);
        if self.compares.is_current(compare) {
            self.compared(done.against, cx);
        } else {
            // A comparison asked for meanwhile compared the file before this one.
            self.before = None;
            self.compare(cx);
        }
        if let Some(jump) = self.pending_jump.take() {
            self.jump(jump, window, cx);
        }
        if let Some(block) = self.pending_edit.take() {
            self.edit_block(block, Caret::Start, window, cx);
        }
        if cx.has_global::<Started>() {
            eprintln!(
                "rature: {} read in {:?} off the main thread, put in place in {:?} on it, shown at {:?}",
                done.path.display(),
                done.took,
                started.elapsed(),
                cx.global::<Started>().0.elapsed(),
            );
        }
    }

    /// The file changed on disk while code typed into it was not yet written: the editor takes
    /// the file's new text, and what was typed goes to the clipboard rather than over it.
    fn keep_typed_code(&mut self, content: &Content, cx: &mut Context<Self>) {
        let Some(code) = &mut self.code else {
            return;
        };
        let typed = code.field.read(cx).value().to_string();
        let same_file = matches!(content, Content::Code { text, .. } if *text == code.on_disk);
        if typed == code.shown || same_file {
            return;
        }
        // Not written later, nor offered again by the next reload.
        code.autosave = None;
        code.shown = typed.clone();
        self.rescue(
            typed,
            "The file changed on disk: what you typed is in the clipboard",
            cx,
        );
    }

    /// Text that could not be written: it goes to the clipboard, and the reason stays shown.
    fn rescue(&mut self, text: String, why: impl Into<SharedString>, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.fail(why, cx);
    }

    /// The colours changed: what was drawn in the old ones is drawn again.
    fn restyle(&mut self, cx: &mut Context<Self>) {
        self.ink = render::ink(cx);
        // Coloured again in the new theme's colours, what was typed written first.
        self.write_code(cx);
        self.code = None;
        self.drawings.clear();
        self.highlights.clear();
        // Only a file read is shown: read again, its formulas in the new ink.
        self.reload(cx);
    }

    fn set_content(&mut self, content: Content, prints: Vec<u64>, cx: &mut Context<Self>) {
        let dark = render::is_dark(cx);
        let colors = diagram::Colors {
            font: cx.global::<Config>().font.clone(),
            ..diagram::Colors::of(&gpui_design_system::theme::target_palette(cx))
        };
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
        self.data_lines = match &content {
            Content::Data(nodes) => Rc::new(render::data_lines(nodes, &self.folded)),
            _ => Rc::default(),
        };
        self.content = content;
        self.prints = prints;
        self.relist(cx);
        cx.notify();
    }

    /// The page's blocks changed in number or in height: laid out again, the reading position
    /// kept, the block being edited kept built.
    fn relist(&mut self, cx: &App) {
        let top = self.list.logical_scroll_top();
        let count = self.items_count();
        self.list.reset(count);
        self.list.scroll_to(top);
        if let Some(editing) = &self.editing {
            let (block, focus) = (editing.block, editing.field.focus_handle(cx));
            self.keep_built(block, Some(focus));
        }
    }

    /// How many items the page's list has: a Markdown file's blocks and, after them, the blocks
    /// removed from its end; one for anything else.
    fn items_count(&self) -> usize {
        match (&self.content, self.loading) {
            (_, true) => 0,
            (Content::Markdown(doc), _) => {
                let end = doc.blocks.len();
                let trailing = self
                    .changes()
                    .is_some_and(|c| c.removed.iter().any(|(before, _)| *before == end));
                end + usize::from(trailing)
            }
            _ => 1,
        }
    }

    /// What changed since the baseline, worked out off the main thread.
    fn compare(&mut self, cx: &mut Context<Self>) {
        let generation = self.compares.next();
        let (Some(path), Content::Markdown(doc)) = (self.path.clone(), &self.content) else {
            return;
        };
        let doc = doc.clone();
        let (ink, baseline, cached) = (render::ink(cx), self.baseline, self.old.clone());
        let work = cx.background_executor().spawn({
            let doc = doc.clone();
            async move { load::compare(&path, &doc, &ink, baseline, cached) }
        });
        cx.spawn(async move |this, cx| {
            let against = work.await;
            this.update(cx, |this, cx| {
                let same =
                    matches!(&this.content, Content::Markdown(now) if Arc::ptr_eq(now, &doc));
                if this.compares.is_current(generation) && same {
                    this.compared(against, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// How the file shown differs from its baseline, now known. The first time a file is read,
    /// it is read: nothing changed yet.
    fn compared(&mut self, against: Against, cx: &mut Context<Self>) {
        self.before = None;
        let switching = std::mem::take(&mut self.switching);
        match against {
            Against::Nothing => {}
            Against::FirstRead => {
                if let (Some(path), Content::Markdown(doc)) = (&self.path, &self.content) {
                    state::mark_read(path, &doc.source);
                }
            }
            Against::NoHead => {
                if self.baseline == Baseline::Head {
                    self.baseline = Baseline::LastRead;
                    if switching {
                        self.say("Not in a git repository, or not committed", cx);
                    }
                    // Compared to what was read instead.
                    self.compare(cx);
                }
            }
            Against::Old(old, changes) => {
                if switching {
                    self.say("Changes since the last commit", cx);
                }
                self.before = Some((old.doc.clone(), changes));
                self.old = Some(old);
            }
        }
        self.relist(cx);
        cx.notify();
    }

    fn changes(&self) -> Option<&Changes> {
        self.before.as_ref().map(|(_, c)| c)
    }

    /// What is kept of the file the reader leaves, for the next time it opens: what was typed
    /// and not yet written, where it was read up to, and that its changes were seen.
    pub fn put_down(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else {
            return;
        };
        self.write_edit(cx);
        self.write_code(cx);
        // Not read yet: where it was read up to stays as it was.
        if !self.loading {
            let position = self.position();
            cx.global_mut::<State>().set_position(&path, position);
        }
        if let (Content::Markdown(doc), Some(_)) = (&self.content, &self.before) {
            state::mark_read(&path, &doc.source);
            cx.emit(ReaderEvent::MarkedRead(path));
        }
    }

    pub fn jump(&mut self, jump: Jump, window: &mut Window, cx: &mut Context<Self>) {
        // Once the file is read: its blocks, and its anchors, are not known before.
        if self.loading {
            self.pending_jump = Some(jump);
            return;
        }
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
        let Some((block, within)) = target else {
            return;
        };
        if self.in_rows() {
            // One item, scrolled by its offset once it is laid out.
            let rows = self.rows.0.borrow().base_handle.clone();
            cx.on_next_frame(window, move |_, _, cx| {
                let y = px(-within).min(px(0.)).max(-rows.max_offset().y);
                rows.set_offset(point(rows.offset().x, y));
                cx.notify();
            });
            return;
        }
        // The first block shows with the page's own margin above it; another, with a little
        // room, which needs the block above it laid out: done in the next frames.
        let block = block.min(self.list.item_count().saturating_sub(1));
        if block == 0 {
            self.list.scroll_to(ListOffset {
                item_ix: 0,
                offset_in_item: px(within.max(0.)),
            });
        } else {
            self.list.scroll_to(ListOffset {
                item_ix: block,
                offset_in_item: px(0.),
            });
            let shift = px(within - ROOM);
            self.after_layout(block, shift, 3, window, cx);
        }
        cx.notify();
    }

    /// Scrolls by `shift` from the block `block`, once it and the one above it are laid out.
    fn after_layout(
        &mut self,
        block: usize,
        shift: Pixels,
        tries: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.on_next_frame(window, move |this, window, cx| {
            let top = this.list.logical_scroll_top();
            if top.item_ix != block || top.offset_in_item != px(0.) {
                // Scrolled meanwhile, or put back by a reload: left where it is.
                return;
            }
            if this.list.bounds_for_item(block).is_some() {
                this.list.scroll_by(shift);
                cx.notify();
            } else if tries > 0 {
                this.after_layout(block, shift, tries - 1, window, cx);
            }
        });
    }

    /// Whether the page shows rows all as high (a CSV file, a JSON or YAML tree), not blocks.
    fn in_rows(&self) -> bool {
        matches!(self.content, Content::Data(_) | Content::Table(_)) && !self.loading
    }

    /// Where the page is read up to: the top-level block at its top, and how far into it.
    fn position(&self) -> (usize, f32) {
        if self.in_rows() {
            let offset = self.rows.0.borrow().base_handle.offset();
            return (0, -f32::from(offset.y));
        }
        let top = self.list.logical_scroll_top();
        (top.item_ix, f32::from(top.offset_in_item))
    }

    /// Follows a link: a page on the web, an anchor of this file, or another file (and its
    /// anchor), which the back key returns from.
    pub fn follow(&mut self, link: &str, window: &mut Window, cx: &mut Context<Self>) {
        match document::target(link) {
            document::Target::Web => return cx.open_url(link.trim()),
            document::Target::Foreign => {
                return self.fail(
                    format!("Not opened: {link}. Only http, https and mailto links open from here"),
                    cx,
                );
            }
            document::Target::Local => {}
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
        let (block, within) = self.position();
        self.path.clone().map(|p| (p, block, within))
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
        self.refold(cx);
    }

    /// The folds changed: the lines the tree shows are found again.
    fn refold(&mut self, cx: &mut Context<Self>) {
        if let Content::Data(nodes) = &self.content {
            self.data_lines = Rc::new(render::data_lines(nodes, &self.folded));
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
            self.refold(cx);
        }
    }

    fn unfold_all(&mut self, _: &UnfoldAll, _: &mut Window, cx: &mut Context<Self>) {
        self.folded.clear();
        self.refold(cx);
    }

    fn scroll_by(&mut self, delta: Pixels, cx: &mut Context<Self>) {
        if self.in_rows() {
            let rows = self.rows.0.borrow().base_handle.clone();
            let offset = rows.offset();
            let y = (offset.y - delta).min(px(0.)).max(-rows.max_offset().y);
            rows.set_offset(point(offset.x, y));
        } else {
            self.list.scroll_by(delta);
        }
        cx.notify();
    }

    fn page(&self) -> Pixels {
        if self.in_rows() {
            return self.rows.0.borrow().base_handle.bounds().size.height;
        }
        self.list.viewport_bounds().size.height
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
        if self.in_rows() {
            let rows = self.rows.0.borrow().base_handle.clone();
            rows.set_offset(point(rows.offset().x, px(0.)));
        } else {
            self.list.scroll_to(ListOffset {
                item_ix: 0,
                offset_in_item: px(0.),
            });
        }
        cx.notify();
    }
    fn bottom(&mut self, _: &Bottom, _: &mut Window, cx: &mut Context<Self>) {
        if self.in_rows() {
            let rows = self.rows.0.borrow().base_handle.clone();
            rows.set_offset(point(rows.offset().x, -rows.max_offset().y));
        } else {
            self.list.scroll_to_end();
        }
        cx.notify();
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
        let (block, _) = self.position();
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
        let (top, within) = self.position();
        let next = if forward {
            stops
                .iter()
                .find(|&&s| s > top || (s == top && within > 4.))
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
        // Nothing changed since: the file now is what it is compared to, parsed already.
        let (source, doc) = (doc.source.clone(), doc.clone());
        self.baseline = Baseline::LastRead;
        self.switching = false;
        self.compares.next();
        self.old = Some(Old {
            source,
            ink: self.ink.clone(),
            doc,
        });
        self.before = None;
        self.relist(cx);
        self.say("Marked read", cx);
        cx.emit(ReaderEvent::MarkedRead(path));
    }

    /// Compares to git's version, or back to what was read. Git is asked off the main thread:
    /// the answer is said once it is known.
    fn switch_baseline(&mut self, _: &SwitchBaseline, _: &mut Window, cx: &mut Context<Self>) {
        self.baseline = match self.baseline {
            Baseline::LastRead => Baseline::Head,
            Baseline::Head => Baseline::LastRead,
        };
        self.switching = self.baseline == Baseline::Head;
        self.compare(cx);
        if self.baseline == Baseline::LastRead {
            self.say("Changes since last read", cx);
        }
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
        let (top, within) = self.position();
        // A block more than half gone above is not the one being read.
        let block = if within > 40. { top + 1 } else { top };
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
        if !self.editable && matches!(self.content, Content::Markdown(_) | Content::Code { .. }) {
            return self.say(NOT_UTF8, cx);
        }
        let (Some(path), Content::Markdown(doc)) = (&self.path, &self.content) else {
            return self.say("Only Markdown is edited here: `e` opens nvim", cx);
        };
        if block >= doc.blocks.len() {
            return;
        }
        let original = doc.source_of(block).to_string();
        let links = Rc::new(Links::new(
            path.parent().unwrap_or(Path::new(".")).to_path_buf(),
            doc.anchors.keys().cloned().collect(),
        ));
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
        self.keep_built(block, Some(focus));
        cx.notify();
    }

    /// Keeps the block `block` built while it is scrolled out of sight (`focus`, the field it
    /// is edited in, holds the keys), or lets it go again (`None`). The reading position stays.
    fn keep_built(&mut self, block: usize, focus: Option<FocusHandle>) {
        if block >= self.list.item_count() {
            return;
        }
        let top = self.list.logical_scroll_top();
        self.list.splice_focusable(block..block + 1, [focus]);
        self.list.scroll_to(top);
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
            self.finish_editing(window, cx);
            // Edited again once the file is read again, as the list it is now.
            self.pending_edit = Some(block);
            self.reload(cx);
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
    /// The file is read again just before it is written, and replaced whole: what another
    /// program wrote elsewhere in it is kept.
    fn write_edit(&mut self, cx: &mut Context<Self>) {
        let (Some(path), Some(editing)) = (self.path.clone(), &mut self.editing) else {
            return;
        };
        let text = editing.field.value(cx);
        if text == editing.original {
            return;
        }
        let original = editing.original.clone();
        let written = fsio::update(&path, |now| {
            (now.matches(original.as_str()).count() == 1)
                .then(|| now.replacen(original.as_str(), &text, 1))
        });
        match written {
            Ok(Update::Written { .. }) => {
                // Your own change is read already: what is shown as new stays what others wrote.
                if let Some(read) = state::last_read(&path)
                    && read.matches(original.as_str()).count() == 1
                {
                    state::mark_read(&path, &read.replacen(original.as_str(), &text, 1));
                    cx.emit(ReaderEvent::MarkedRead(path));
                }
                // Read again as written, it is back in place: it does not rise into it.
                self.settled.insert(fingerprint(&text));
                editing.original = text;
                // Shown at once as written, rather than when the watch tells of it: the page
                // left now is marked read with the change in it.
                self.reload(cx);
            }
            Ok(Update::Left(_)) => self.rescue(
                text,
                "The block changed on disk: your text is in the clipboard",
                cx,
            ),
            Ok(Update::Busy) => self.rescue(
                text,
                "The file kept changing: not written, your text is in the clipboard",
                cx,
            ),
            Err(error) => self.rescue(
                text,
                format!("Cannot write: {error}. Your text is in the clipboard"),
                cx,
            ),
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
        self.keep_built(block, None);
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
        // Shown as text that is read, not in an editor that would write it back.
        if !self.editable {
            self.code = None;
            return;
        }
        if let Some(code) = &mut self.code {
            if code.on_disk != *text {
                code.on_disk = text.clone();
                code.autosave = None;
                let text = text.clone();
                code.field
                    .update(cx, |field, cx| field.set_value(text, window, cx));
                code.shown = code.field.read(cx).value().to_string();
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
        // What the editor holds before anything is typed: opening it writes nothing.
        let shown = field.read(cx).value().to_string();
        self.code = Some(CodeFile {
            field,
            on_disk: text,
            shown,
            autosave: None,
            _events: events,
        });
    }

    /// Writes the code editor's text into the file, once something was typed, and only over
    /// the text it was given: a file another program changed meanwhile is not written over.
    fn write_code(&mut self, cx: &mut Context<Self>) {
        let (Some(path), Some(code)) = (self.path.clone(), &mut self.code) else {
            return;
        };
        let text = code.field.read(cx).value().to_string();
        if text == code.shown || !self.editable {
            return;
        }
        code.autosave = None;
        let expected = code.on_disk.clone();
        match fsio::update(&path, |now| (now == expected).then(|| text.clone())) {
            Ok(Update::Written { .. }) => {
                code.on_disk = text.clone();
                code.shown = text;
            }
            Ok(Update::Left(_)) => {
                // Taken as typed: the reload gives the editor the file's text.
                code.shown = text.clone();
                self.rescue(
                    text,
                    "The file changed on disk: what you typed is in the clipboard",
                    cx,
                );
                self.reload(cx);
            }
            Ok(Update::Busy) => self.fail("The file kept changing: not written yet", cx),
            Err(error) => self.fail(format!("Cannot write: {error}"), cx),
        }
    }

    /// Writes the edit, then formats the whole file as its kind says.
    fn save(&mut self, _: &Save, _: &mut Window, cx: &mut Context<Self>) {
        match self.content {
            Content::Markdown(_) | Content::Code { .. } | Content::Data(_) | Content::Table(_) => {}
            Content::Image(_) | Content::Empty | Content::Message(_) => return,
        }
        if !self.editable {
            return self.say(NOT_UTF8, cx);
        }
        self.write_edit(cx);
        self.write_code(cx);
        let Some(path) = self.path.clone() else {
            return;
        };
        let result = fsio::update(&path, |now| {
            let formatted = format::format(&path, now);
            (formatted != now).then_some(formatted)
        });
        match result {
            Ok(Update::Written { before, after }) => {
                if state::last_read(&path).as_deref() == Some(before.as_str()) {
                    state::mark_read(&path, &after);
                }
                if let Some(editing) = &mut self.editing {
                    // The block moved under the formatting: its text is the formatted one now.
                    if let Some(span) = document::block_spans(&after).get(editing.block) {
                        editing.original = after[span.clone()].to_string();
                    }
                }
                self.say("Formatted and saved", cx)
            }
            Ok(Update::Left(_)) => self.say("Saved", cx),
            Ok(Update::Busy) => self.fail("The file kept changing: not formatted", cx),
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
                // The file read is a file: its icon by its name, no disk looked at per frame.
                .child(icon(formats::file_icon(path)).text_color(p.mute))
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
                (Side::Left, true) => icons::PANEL_LEFT_CLOSE,
                (Side::Left, false) => icons::PANEL_LEFT,
                (Side::Right, true) => icons::PANEL_RIGHT_CLOSE,
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

    /// The item `ix` of the page's list, built only while it is in sight: a block of a Markdown
    /// file with the blocks removed before it, the blocks removed from its end, or what else the
    /// file shows.
    fn item(&mut self, ix: usize, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = palette(cx);
        let config = cx.global::<Config>().clone();
        let query = (!self.query.is_empty()).then(|| self.query.clone());
        let now = cx.background_executor().now();
        let rises = match (&self.content, self.prints.get(ix)) {
            (Content::Markdown(_), Some(&print)) => {
                !self.settled.contains(&print) && self.entrances.rises(print, now)
            }
            _ => false,
        };
        // Not the list's own count: it is being laid out, its state borrowed.
        let count = self.items_count();
        let element = {
            let ctx = Ctx::new(
                &t,
                &config,
                self.zoom,
                &self.drawings,
                &self.highlights,
                query.as_deref(),
                cx,
            );
            self.item_element(&ctx, ix, rises, cx)
        };
        // The page's own margins: above its first item, below its last.
        div()
            .w_full()
            .flex()
            .flex_col()
            .items_center()
            .when(ix == 0, |d| d.pt(space::XXXL))
            .when(ix + 1 == count, |d| d.pb(px(160.)))
            .child(element)
            .into_any_element()
    }

    fn item_element(&self, ctx: &Ctx, ix: usize, rises: bool, cx: &Context<Self>) -> AnyElement {
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
                let Some(block) = doc.blocks.get(ix) else {
                    // After the last block: those removed from the end.
                    let trailing: Vec<AnyElement> = changes
                        .into_iter()
                        .flat_map(|(old, changes)| {
                            changes
                                .removed
                                .iter()
                                .filter(move |(before, _)| *before == ix)
                                .filter_map(|(_, gone)| old.blocks.get(*gone))
                        })
                        .map(|gone| render::ghost(ctx, gone))
                        .collect();
                    return wrap(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .children(trailing)
                            .into_any_element(),
                    );
                };
                {
                    let mut item = div().flex().flex_col().gap_2();
                    if let Some((old, changes)) = changes {
                        for (_, gone) in changes.removed.iter().filter(|(before, _)| *before == ix)
                        {
                            if let Some(gone) = old.blocks.get(*gone) {
                                item = item.child(render::ghost(ctx, gone));
                            }
                        }
                    }
                    // A comparison with another version of the file has as many marks as it
                    // has blocks; one not yet replaced has none for this block.
                    let mark = changes.and_then(|(_, c)| c.marks.get(ix));
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
                        None => self.block_element(ctx, doc, ix, block, mark, rises, cx),
                    };
                    item = item.child(shown);
                    wrap(item.into_any_element())
                }
            }
            Content::Code { lang, text } => wrap(render::code(ctx, lang, text).into_any_element()),
            // Shown as rows, by `rows_view`.
            Content::Data(_) | Content::Table(_) => div().into_any_element(),
            Content::Image(path) => wrap(render::image_file(path)),
            Content::Empty => notice(
                "no-file",
                Icon::Text,
                "No file open",
                "Pick one in the tree.",
                &[("f", "find a file"), ("s", "search the folder")],
                &t,
            ),
            Content::Message(message) => notice(
                "not-shown",
                Icon::Warning,
                "This file is not shown",
                message.clone(),
                &[],
                &t,
            ),
        }
    }

    /// A CSV file or a JSON or YAML tree: rows all as high, only those in sight built, scrolled
    /// sideways when wider than the window.
    fn rows_view(&self, config: &Config, cx: &mut Context<Self>) -> AnyElement {
        let page = |list: UniformList| {
            list.size_full()
                .pt(space::XXXL)
                .pb(px(160.))
                .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
                .track_scroll(&self.rows)
        };
        match &self.content {
            Content::Table(rows) => {
                let count = rows.len();
                let view = cx.processor(|this: &mut Self, range: std::ops::Range<usize>, _, cx| {
                    this.table_rows(range, cx)
                });
                page(uniform_list("csv", count, view))
                    .px_8()
                    .into_any_element()
            }
            Content::Data(_) => {
                let count = self.data_lines.len();
                let view = cx.processor(|this: &mut Self, range: std::ops::Range<usize>, _, cx| {
                    this.data_rows(range, cx)
                });
                // In the reading column, as the text is.
                div()
                    .size_full()
                    .flex()
                    .justify_center()
                    .child(
                        div()
                            .h_full()
                            .w_full()
                            .max_w(px(config.column))
                            .font_family(config.code_font.clone())
                            .text_size(px(config.font_size * 0.875 * self.zoom))
                            .line_height(rems(1.5))
                            .child(page(uniform_list("data", count, view)).px_8()),
                    )
                    .into_any_element()
            }
            _ => div().into_any_element(),
        }
    }

    fn table_rows(
        &mut self,
        range: std::ops::Range<usize>,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Content::Table(rows) = &self.content else {
            return Vec::new();
        };
        let (t, config) = (palette(cx), cx.global::<Config>().clone());
        let query = (!self.query.is_empty()).then(|| self.query.clone());
        let ctx = Ctx::new(
            &t,
            &config,
            self.zoom,
            &self.drawings,
            &self.highlights,
            query.as_deref(),
            cx,
        );
        let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
        range
            .filter_map(|ix| {
                let row = rows.get(ix)?;
                Some(render::table_row(&ctx, row, ix, rows.len(), columns))
            })
            .collect()
    }

    fn data_rows(
        &mut self,
        range: std::ops::Range<usize>,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Content::Data(nodes) = &self.content else {
            return Vec::new();
        };
        let (t, config) = (palette(cx), cx.global::<Config>().clone());
        let query = (!self.query.is_empty()).then(|| self.query.clone());
        let ctx = Ctx::new(
            &t,
            &config,
            self.zoom,
            &self.drawings,
            &self.highlights,
            query.as_deref(),
            cx,
        );
        range
            .filter_map(|ix| {
                let row = self.data_lines.get(ix)?;
                Some(render::data_row(&ctx, nodes, row, &self.folded))
            })
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    fn block_element(
        &self,
        ctx: &Ctx,
        doc: &Document,
        ix: usize,
        block: &Block,
        mark: Option<&Mark>,
        rises: bool,
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
        let arrived = if rises {
            let print = self
                .prints
                .get(ix)
                .copied()
                .unwrap_or_else(|| fingerprint(doc.source_of(ix)));
            enter(ElementId::Name(format!("block-{print}").into()), 0, inner).into_any_element()
        } else {
            inner
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
        self.sync_code(window, cx);
        let code = self.code.as_ref().map(|code| code.field.clone());
        let editing = self.editing.is_some()
            || code
                .as_ref()
                .is_some_and(|field| field.read(cx).focus_handle(cx).contains_focused(window, cx));
        // A zoom changes every block's height.
        if self.zoomed != self.zoom {
            self.zoomed = self.zoom;
            self.list.remeasure();
        }
        let page: AnyElement = if self.in_rows() {
            self.rows_view(&config, cx)
        } else {
            list(
                self.list.clone(),
                cx.processor(|this, ix, window, cx| this.item(ix, window, cx)),
            )
            .size_full()
            .into_any_element()
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
                        // The page scrolls its list, or its rows, itself.
                        None => d.child(page),
                    }),
            )
    }
}
