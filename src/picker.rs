//! The box that finds something to go to: a file by a few letters of its path, a line of the
//! folder by ripgrep, a heading of the file being read, or any file or folder to open.

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui_kit::base::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config as MatcherConfig, Matcher};

use crate::config::home;
use crate::formats;
use crate::icons;
use crate::kit::focus_frame;
use gpui_design_system::field::follow_theme;
use gpui_design_system::listing::{Item, Listing};
use gpui_design_system::modal::Modal;
use gpui_design_system::page::EmptyLine;
use gpui_design_system::rows::ListRow;
use gpui_design_system::{Icon, Type, icon, palette, presence, space};

/// How many results the box keeps.
const SHOWN: usize = 200;

actions!(picker, [Next, Previous, Pick, Dismiss, Complete]);

pub fn bind_keys(cx: &mut App) {
    let at = Some("Picker");
    cx.bind_keys([
        KeyBinding::new("down", Next, at),
        KeyBinding::new("ctrl-n", Next, at),
        KeyBinding::new("ctrl-j", Next, at),
        KeyBinding::new("up", Previous, at),
        KeyBinding::new("ctrl-p", Previous, at),
        KeyBinding::new("ctrl-k", Previous, at),
        KeyBinding::new("escape", Dismiss, at),
        // Over the field's own tab, which indents.
        KeyBinding::new("tab", Complete, Some("Picker > Input")),
    ]);
}

#[derive(Clone)]
pub enum Target {
    File(PathBuf),
    Line(PathBuf, usize),
    Block(usize),
    /// A file or a folder, opened as `rature` opens it.
    Open(PathBuf),
}

#[derive(Clone)]
pub struct Choice {
    pub label: String,
    pub detail: String,
    pub indent: usize,
    pub target: Target,
}

pub enum PickerEvent {
    Picked(Target),
    Dismissed,
}

impl EventEmitter<PickerEvent> for Picker {}

enum Source {
    /// Matched by name against all of them.
    Fixed(Vec<Choice>),
    /// ripgrep in this folder, run again as the query changes.
    Grep(PathBuf),
    /// These by name, or, for a query that is a path, what its folder holds.
    Paths(Vec<Choice>),
}

pub struct Picker {
    title: &'static str,
    /// What the keys do, said under the results.
    keys: &'static str,
    /// False once something is chosen or it is dismissed: it fades out, then the owner drops it.
    open: bool,
    list: Listing<Choice>,
    source: Source,
    field: Entity<InputState>,
    shown: Vec<Choice>,
    selected: usize,
    scroll: ScrollHandle,
    running: Option<Task<()>>,
    /// The ripgrep run of the last query: replacing or dropping it stops it.
    search: Option<Stopper>,
    /// Said instead of the results when the search cannot run.
    notice: Option<String>,
    _events: Subscription,
}

impl Picker {
    fn new(
        title: &'static str,
        placeholder: &'static str,
        keys: &'static str,
        source: Source,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let field = cx.new(|cx| {
            let mut field = InputState::new(window, cx).placeholder(placeholder);
            follow_theme(&mut field, cx);
            field
        });
        let events =
            cx.subscribe_in(
                &field,
                window,
                |this, _, event: &InputEvent, _, cx| match event {
                    InputEvent::Change => this.refresh(cx),
                    InputEvent::PressEnter { .. } => this.pick(cx),
                    _ => {}
                },
            );
        let focus = field.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
        let mut picker = Self {
            title,
            keys,
            open: true,
            list: Listing::default(),
            source,
            field,
            shown: Vec::new(),
            selected: 0,
            scroll: ScrollHandle::new(),
            running: None,
            search: None,
            notice: None,
            _events: events,
        };
        picker.refresh(cx);
        picker
    }

    /// The files under `root`, the ones read last first while the query is empty.
    pub fn files(
        root: PathBuf,
        recent: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let choices = recent
            .iter()
            .filter(|p| p.starts_with(&root) && p.is_file())
            .map(|p| file_choice(&root, p))
            .collect();
        let mut picker = Self::new(
            "Files",
            "Find a file",
            "↑↓ move · Enter open · Esc close",
            Source::Fixed(choices),
            window,
            cx,
        );
        let listed = cx.background_executor().spawn({
            let root = root.clone();
            async move { list_files(&root) }
        });
        picker.running = Some(cx.spawn(async move |this, cx| {
            let files = listed.await;
            this.update(cx, |this, cx| {
                let Source::Fixed(choices) = &mut this.source else {
                    return;
                };
                let known: std::collections::HashSet<PathBuf> = choices
                    .iter()
                    .filter_map(|c| match &c.target {
                        Target::File(p) => Some(p.clone()),
                        _ => None,
                    })
                    .collect();
                choices.extend(
                    files
                        .iter()
                        .filter(|p| !known.contains(*p))
                        .map(|p| file_choice(&root, p)),
                );
                this.refresh(cx);
            })
            .ok();
        }));
        picker
    }

    pub fn grep(root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::new(
            "Search the folder",
            "Search the folder",
            "ripgrep · ↑↓ move · Enter open at the line · Esc close",
            Source::Grep(root),
            window,
            cx,
        )
    }

    /// The files and folders read last, and any other by its path.
    pub fn open(recent: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let choices = recent
            .iter()
            .filter(|p| p.exists())
            .map(|p| Choice {
                label: name(p),
                detail: p.parent().map(shown).unwrap_or_default(),
                indent: 0,
                target: Target::Open(p.clone()),
            })
            .collect();
        Self::new(
            "Open",
            "A recent file or folder, or a path: ~/notes/",
            "↑↓ move · Tab completes a path · Enter open · Esc close",
            Source::Paths(choices),
            window,
            cx,
        )
    }

    pub fn outline(choices: Vec<Choice>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::new(
            "Outline",
            "Go to a heading",
            "↑↓ move · Enter go · Esc close",
            Source::Fixed(choices),
            window,
            cx,
        )
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let query = self.field.read(cx).value().to_string();
        self.selected = 0;
        match &self.source {
            Source::Paths(_) if query.starts_with(['/', '~']) => {
                self.shown = children(&query);
                cx.notify();
            }
            Source::Fixed(choices) | Source::Paths(choices) => {
                self.shown = if query.trim().is_empty() {
                    choices.iter().take(SHOWN).cloned().collect()
                } else {
                    let pattern = Pattern::parse(&query, CaseMatching::Smart, Normalization::Smart);
                    let mut matcher = Matcher::new(MatcherConfig::DEFAULT.match_paths());
                    let labels: Vec<(usize, String)> = choices
                        .iter()
                        .enumerate()
                        .map(|(ix, c)| (ix, format!("{} {}", c.label, c.detail)))
                        .collect();
                    let mut scored: Vec<(usize, u32)> = labels
                        .iter()
                        .filter_map(|(ix, label)| {
                            pattern
                                .match_list([label.as_str()], &mut matcher)
                                .first()
                                .map(|(_, score)| (*ix, *score))
                        })
                        .collect();
                    scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
                    scored
                        .into_iter()
                        .take(SHOWN)
                        .map(|(ix, _)| choices[ix].clone())
                        .collect()
                };
                cx.notify();
            }
            Source::Grep(root) => {
                if query.trim().is_empty() {
                    self.shown.clear();
                    self.notice = None;
                    self.running = None;
                    self.search = None;
                    return cx.notify();
                }
                let root = root.clone();
                // Typing replaces the search: only the last query runs, once typing rests, and
                // the ripgrep of the one before is stopped.
                let search = Search::default();
                self.search = Some(Stopper(search.clone()));
                self.running = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .timer(Duration::from_millis(150))
                        .await;
                    let found = cx
                        .background_executor()
                        .spawn(async move { grep(&root, &query, &search) })
                        .await;
                    this.update(cx, |this, cx| {
                        match found {
                            Ok(found) => {
                                this.shown = found;
                                this.notice = None;
                            }
                            Err(notice) => {
                                this.shown.clear();
                                this.notice = Some(notice);
                            }
                        }
                        this.selected = 0;
                        cx.notify();
                    })
                    .ok();
                }));
            }
        }
    }

    fn pick(&mut self, cx: &mut Context<Self>) {
        let target = self.shown.get(self.selected).map(|c| c.target.clone());
        if target.is_some() {
            self.close(target, cx);
        }
    }

    fn pick_key(&mut self, key: &str, cx: &mut Context<Self>) {
        if let Some(at) = self.shown.iter().position(|c| c.key() == key) {
            self.selected = at;
            self.pick(cx);
        }
    }

    fn mark_key(&mut self, key: &str, cx: &mut Context<Self>) {
        if let Some(at) = self.shown.iter().position(|c| c.key() == key) {
            self.selected = at;
            cx.notify();
        }
    }

    fn step(&mut self, by: isize, cx: &mut Context<Self>) {
        if self.shown.is_empty() {
            return;
        }
        let last = self.shown.len() as isize - 1;
        self.selected = (self.selected as isize + by).clamp(0, last) as usize;
        self.scroll.scroll_to_item(self.selected);
        cx.notify();
    }

    /// Closes the box, with what was chosen if anything: it fades out meanwhile.
    fn close(&mut self, target: Option<Target>, cx: &mut Context<Self>) {
        if !self.open {
            return;
        }
        self.open = false;
        cx.emit(match target {
            Some(target) => PickerEvent::Picked(target),
            None => PickerEvent::Dismissed,
        });
        cx.notify();
    }

    fn next(&mut self, _: &Next, _: &mut Window, cx: &mut Context<Self>) {
        self.step(1, cx);
    }

    fn previous(&mut self, _: &Previous, _: &mut Window, cx: &mut Context<Self>) {
        self.step(-1, cx);
    }

    fn dismiss(&mut self, _: &Dismiss, _: &mut Window, cx: &mut Context<Self>) {
        self.close(None, cx);
    }

    /// Puts the chosen path in the field: a folder, to go on into it.
    fn complete(&mut self, _: &Complete, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Choice {
            target: Target::Open(path),
            ..
        }) = self.shown.get(self.selected)
        else {
            return;
        };
        let mut text = shown(path);
        if path.is_dir() {
            text.push('/');
        }
        self.field
            .update(cx, |field, cx| field.set_value(text, window, cx));
        self.refresh(cx);
    }
}

/// `path` with the home folder as `~`.
pub fn shown(path: &Path) -> String {
    match path.strip_prefix(home()) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

/// A file's name, a folder's with a `/`.
pub fn name(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    if path.is_dir() {
        format!("{name}/")
    } else {
        name
    }
}

/// For a query such as `~/notes/sp`: the folder typed first, if it is one, then what that folder
/// holds whose name starts with `sp` (case aside), folders first. Hidden ones only asked by a dot.
fn children(query: &str) -> Vec<Choice> {
    let path = match query.strip_prefix('~') {
        Some(rest) => home().join(rest.trim_start_matches('/')),
        None => PathBuf::from(query),
    };
    let (folder, start) = if query.ends_with('/') || query == "~" {
        (path.clone(), String::new())
    } else {
        let start = path
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        (
            path.parent().map(Path::to_path_buf).unwrap_or_default(),
            start,
        )
    };
    let choice = |path: PathBuf| Choice {
        label: name(&path),
        detail: String::new(),
        indent: 0,
        target: Target::Open(path),
    };
    let mut found: Vec<PathBuf> = std::fs::read_dir(&folder)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| {
                    let name = p
                        .file_name()
                        .map(|n| n.to_string_lossy().to_lowercase())
                        .unwrap_or_default();
                    name.starts_with(&start) && (start.starts_with('.') || !name.starts_with('.'))
                })
                .collect()
        })
        .unwrap_or_default();
    found.sort_by_key(|p| (!p.is_dir(), p.file_name().map(|n| n.to_ascii_lowercase())));
    let mut choices: Vec<Choice> = Vec::new();
    if start.is_empty() && folder.is_dir() {
        choices.push(Choice {
            label: format!("{} (open this folder)", shown(&folder)),
            ..choice(folder)
        });
    }
    choices.extend(found.into_iter().take(SHOWN).map(choice));
    choices
}

fn file_choice(root: &Path, path: &Path) -> Choice {
    let relative = path.strip_prefix(root).unwrap_or(path);
    Choice {
        label: relative
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        detail: relative
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
        indent: 0,
        target: Target::File(path.to_path_buf()),
    }
}

/// The files under `root` that `.gitignore` leaves, at most a few tens of thousands.
pub fn list_files(root: &Path) -> Vec<PathBuf> {
    ignore::WalkBuilder::new(root)
        .hidden(true)
        .require_git(false)
        .build()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .map(|e| e.into_path())
        .take(50_000)
        .collect()
}

/// A ripgrep run, shared by the search that reads it and the box that may stop it.
#[derive(Clone, Default)]
struct Search(Arc<Mutex<Run>>);

#[derive(Default)]
struct Run {
    child: Option<Child>,
    stopped: bool,
}

impl Search {
    fn run(&self) -> std::sync::MutexGuard<'_, Run> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Kills ripgrep if it runs, and keeps it from starting. The reader waits for it, so this
    /// never blocks.
    fn stop(&self) {
        let mut run = self.run();
        run.stopped = true;
        if let Some(child) = &mut run.child {
            let _ = child.kill();
        }
    }
}

/// Stops its search when dropped: when a new query replaces it, or the box goes.
struct Stopper(Search);

impl Drop for Stopper {
    fn drop(&mut self) {
        self.0.stop();
    }
}

/// The longest line of ripgrep's read: a longer one, a match in a huge line, is skipped.
const LONGEST: usize = 1024 * 1024;
/// The most of a matched line shown.
const LABEL: usize = 240;

/// The lines under `root` holding `query`, by ripgrep: as it finds them, with case only when the
/// query has some, the first `SHOWN` of them. Stopping `search` stops it early.
fn grep(root: &Path, query: &str, search: &Search) -> Result<Vec<Choice>, String> {
    let mut run = search.run();
    if run.stopped {
        return Ok(Vec::new());
    }
    let child = std::process::Command::new("rg")
        .args([
            "--json",
            "--smart-case",
            "--max-count=20",
            "--fixed-strings",
            "--",
            query,
        ])
        .arg(root)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(
                "Searching the folder needs ripgrep: install the `ripgrep` package (`rg`).".into(),
            );
        }
        Err(error) => return Err(format!("ripgrep (rg) cannot start: {error}")),
    };
    let Some(output) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Ok(Vec::new());
    };
    run.child = Some(child);
    drop(run);
    let found = matches(root, std::io::BufReader::new(output));
    // Enough found, or stopped: ripgrep need not go on.
    let child = search.run().child.take();
    if let Some(mut child) = child {
        let _ = child.kill();
        let _ = child.wait();
    }
    Ok(found)
}

/// The first `SHOWN` matches in what `rg --json` prints.
fn matches(root: &Path, mut output: impl BufRead) -> Vec<Choice> {
    let mut found = Vec::new();
    let mut line = Vec::new();
    while found.len() < SHOWN && read_line(&mut output, &mut line, LONGEST) {
        if line.len() >= LONGEST {
            continue;
        }
        if let Some((path, number, text)) = parse_match(&line) {
            let relative = path.strip_prefix(root).unwrap_or(&path).to_string_lossy();
            let detail = format!("{relative}:{number}");
            found.push(Choice {
                label: text.trim().chars().take(LABEL).collect(),
                detail,
                indent: 0,
                target: Target::Line(path, number),
            });
        }
    }
    found
}

/// Reads a line of `input` into `line`, keeping at most `most` bytes of it. False at the end.
fn read_line(input: &mut impl BufRead, line: &mut Vec<u8>, most: usize) -> bool {
    line.clear();
    let mut any = false;
    loop {
        let buffer = match input.fill_buf() {
            Ok(buffer) => buffer,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return any,
        };
        if buffer.is_empty() {
            return any;
        }
        any = true;
        let (used, whole) = match buffer.iter().position(|&b| b == b'\n') {
            Some(at) => (at + 1, true),
            None => (buffer.len(), false),
        };
        let room = most.saturating_sub(line.len());
        line.extend_from_slice(&buffer[..used.min(room)]);
        input.consume(used);
        if whole {
            return true;
        }
    }
}

/// The path, line number and line of a `match` message of `rg --json`.
fn parse_match(line: &[u8]) -> Option<(PathBuf, usize, String)> {
    use std::os::unix::ffi::OsStringExt as _;
    let message: serde_json::Value = serde_json::from_slice(line).ok()?;
    if message["type"] != "match" {
        return None;
    }
    let data = &message["data"];
    let path = PathBuf::from(std::ffi::OsString::from_vec(rg_bytes(&data["path"])?));
    let number = usize::try_from(data["line_number"].as_u64()?).ok()?;
    let text = String::from_utf8_lossy(&rg_bytes(&data["lines"])?).into_owned();
    Some((path, number, text))
}

/// The bytes of a piece of `rg --json`: `{"text": ...}` when they are UTF-8, else
/// `{"bytes": base64}`.
fn rg_bytes(piece: &serde_json::Value) -> Option<Vec<u8>> {
    if let Some(text) = piece["text"].as_str() {
        return Some(text.as_bytes().to_vec());
    }
    base64(piece["bytes"].as_str()?)
}

/// Standard base64, padded or not.
fn base64(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut bits = 0u32;
    let mut held = 0;
    for c in text.bytes().filter(|&c| c != b'=') {
        let value = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        bits = (bits << 6) | u32::from(value);
        held += 6;
        if held >= 8 {
            held -= 8;
            out.push((bits >> held) as u8);
            bits &= (1 << held) - 1;
        }
    }
    Some(out)
}

impl Choice {
    /// What keeps a result the same row while the list changes around it.
    fn key(&self) -> String {
        match &self.target {
            Target::File(path) | Target::Open(path) => path.display().to_string(),
            Target::Line(path, line) => format!("{}:{line}", path.display()),
            Target::Block(block) => format!("#{block}"),
        }
    }

    fn icon(&self) -> Icon {
        match &self.target {
            Target::File(path) | Target::Open(path) | Target::Line(path, _) => formats::icon(path),
            Target::Block(_) => icons::OUTLINE,
        }
    }
}

/// `label` in pieces, those `query` matched marked: the letters a fuzzy match took, or the
/// stretches a plain search found.
fn pieces(label: &str, query: &str, fuzzy: bool) -> Vec<(String, bool)> {
    let query = query.trim();
    if query.is_empty() || query.starts_with(['/', '~']) {
        return vec![(label.to_string(), false)];
    }
    let chars: Vec<char> = label.chars().collect();
    let mut hit = vec![false; chars.len()];
    if fuzzy {
        let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
        let mut matcher = Matcher::new(MatcherConfig::DEFAULT.match_paths());
        let mut buffer = Vec::new();
        let mut indices = Vec::new();
        pattern.indices(
            nucleo_matcher::Utf32Str::new(label, &mut buffer),
            &mut matcher,
            &mut indices,
        );
        for at in indices {
            if let Some(h) = hit.get_mut(at as usize) {
                *h = true;
            }
        }
    } else {
        let lower = label.to_lowercase();
        let needle = query.to_lowercase();
        if lower.chars().count() == chars.len() {
            let mut from = 0;
            while let Some(at) = lower[from..].find(&needle) {
                let start = lower[..from + at].chars().count();
                for h in &mut hit[start..start + needle.chars().count()] {
                    *h = true;
                }
                from += at + needle.len().max(1);
            }
        }
    }
    let mut out: Vec<(String, bool)> = Vec::new();
    for (c, h) in chars.into_iter().zip(hit) {
        match out.last_mut() {
            Some((text, last)) if *last == h => text.push(c),
            _ => out.push((c.to_string(), h)),
        }
    }
    out
}

/// The field a search modal opens with.
fn field_frame(field: &Entity<InputState>, window: &mut Window, cx: &mut App) -> impl IntoElement {
    let p = palette(cx);
    let focused = field.read(cx).focus_handle(cx).is_focused(window);
    let frame = div()
        .id("picker-input")
        .h(px(36.))
        .px(space::MD)
        .rounded(px(8.))
        .bg(p.field);
    focus_frame("picker-input", frame, focused, window, cx)
        .flex()
        .items_center()
        .gap(space::SM)
        .ui_text()
        .text_color(p.text)
        .child(icon(Icon::Search).text_color(p.faint))
        .child(div().flex_1().min_w_0().child(Input::new(field)))
}

impl Render for Picker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let now = cx.background_executor().now();
        let reduce = cx.reduce_motion();
        // Closed, it keeps the results it showed while it fades out.
        if self.open {
            let items = self
                .shown
                .iter()
                .map(|choice| (choice.key(), choice.clone()));
            self.list.sync(items, now, reduce);
        }
        let query = self.field.read(cx).value().to_string();
        let fuzzy = !matches!(self.source, Source::Grep(_));
        let marked = self.shown.get(self.selected).map(Choice::key);
        let this = cx.entity().downgrade();
        let rows: Vec<AnyElement> = self
            .list
            .rows()
            .map(|row| {
                let choice = row.value;
                let key = row.key.to_string();
                let on_click = this.clone();
                let on_enter = this.clone();
                let enter_key = key.clone();
                let leading = div()
                    .flex()
                    .items_center()
                    .pl(px(14. * choice.indent as f32))
                    .child(icon(choice.icon()).size(px(14.)).text_color(p.mute));
                let row_content = ListRow::new(
                    ElementId::Name(format!("picker-row-{key}").into()),
                    choice.label.clone(),
                )
                .pieces(pieces(&choice.label, &query, fuzzy))
                .leading(leading)
                .when(!choice.detail.is_empty(), |row| {
                    row.line(choice.detail.clone()).mono_line()
                })
                .marked(marked.as_deref() == Some(key.as_str()))
                .on_click(move |_, _, cx| {
                    on_click.update(cx, |this, cx| this.pick_key(&key, cx)).ok();
                })
                .on_enter(move |cx| {
                    on_enter
                        .update(cx, |this, cx| this.mark_key(&enter_key, cx))
                        .ok();
                });
                Item::new(
                    ElementId::Name(format!("picker-item-{}", row.key).into()),
                    row.placement,
                    row_content,
                )
                .into_any_element()
            })
            .collect();
        let nothing = self.open && self.shown.is_empty() && !query.is_empty();
        let close_with = this.clone();
        let key_with = this.clone();
        Modal::new("picker", self.title)
            .search()
            .open(self.open)
            .on_close(move |_, cx| {
                close_with.update(cx, |this, cx| this.close(None, cx)).ok();
            })
            .on_key(move |event: &KeyDownEvent, _, cx| {
                let by = match event.keystroke.key.as_str() {
                    "down" => 1,
                    "up" => -1,
                    _ => return,
                };
                cx.stop_propagation();
                key_with.update(cx, |this, cx| this.step(by, cx)).ok();
            })
            .scroll(&self.scroll)
            .above(
                div()
                    .key_context("Picker")
                    .on_action(cx.listener(Self::next))
                    .on_action(cx.listener(Self::previous))
                    .on_action(cx.listener(Self::dismiss))
                    .on_action(cx.listener(Self::complete))
                    .child(field_frame(&self.field, window, cx)),
            )
            .children(rows)
            .child(presence(
                "picker-nothing",
                nothing,
                EmptyLine::new(
                    "picker-none",
                    Icon::Search,
                    self.notice
                        .clone()
                        .unwrap_or_else(|| "Nothing matches what is typed.".into()),
                ),
            ))
            .kept(
                div()
                    .id("picker-keys")
                    .flex_none()
                    .px(space::MD)
                    .py(space::SM)
                    .border_t_1()
                    .border_color(p.line)
                    .mono(11.)
                    .text_color(p.faint)
                    .child(self.keys),
            )
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: it brings gpui's own `test` attribute.
    use super::{Choice, LONGEST, SHOWN, Search, Stopper, Target, base64, grep, matches};
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt as _;
    use std::path::{Path, PathBuf};

    fn found(choices: &[Choice]) -> Vec<(PathBuf, usize, String, String)> {
        choices
            .iter()
            .map(|c| match &c.target {
                Target::Line(path, line) => {
                    (path.clone(), *line, c.label.clone(), c.detail.clone())
                }
                _ => panic!("a search finds lines"),
            })
            .collect()
    }

    #[test]
    fn ripgrep_json_is_read_whatever_the_path_holds() {
        let output = [
            r#"{"type":"begin","data":{"path":{"text":"/w/a:b.md"}}}"#,
            r#"{"type":"match","data":{"path":{"text":"/w/a:b.md"},"lines":{"text":"  x: 1:2  \n"},"line_number":7,"absolute_offset":0,"submatches":[]}}"#,
            r#"{"type":"match","data":{"path":{"bytes":"L24vY2Fm6S5tZA=="},"lines":{"bytes":"Y2Fm6Q=="},"line_number":3,"absolute_offset":0,"submatches":[]}}"#,
            "not json",
            r#"{"type":"summary","data":{}}"#,
            "",
        ]
        .join("\n");
        let choices = matches(Path::new("/w"), output.as_bytes());
        assert_eq!(
            found(&choices),
            vec![
                (
                    PathBuf::from("/w/a:b.md"),
                    7,
                    "x: 1:2".into(),
                    "a:b.md:7".into()
                ),
                (
                    PathBuf::from(OsStr::from_bytes(b"/n/caf\xe9.md")),
                    3,
                    "caf\u{fffd}".into(),
                    "/n/caf\u{fffd}.md:3".into()
                ),
            ]
        );
    }

    #[test]
    fn ripgrep_output_stops_at_the_cap() {
        let line = r#"{"type":"match","data":{"path":{"text":"/w/a.md"},"lines":{"text":"hit"},"line_number":1}}"#;
        let output = format!("{line}\n").repeat(SHOWN + 50);
        assert_eq!(matches(Path::new("/w"), output.as_bytes()).len(), SHOWN);
    }

    #[test]
    fn an_overlong_line_is_skipped_not_kept() {
        let long = format!(
            r#"{{"type":"match","data":{{"path":{{"text":"/w/a.md"}},"lines":{{"text":"{}"}},"line_number":1}}}}"#,
            "x".repeat(LONGEST)
        );
        let short = r#"{"type":"match","data":{"path":{"text":"/w/b.md"},"lines":{"text":"y"},"line_number":2}}"#;
        let output = format!("{long}\n{short}\n");
        let choices = matches(Path::new("/w"), output.as_bytes());
        assert_eq!(found(&choices).len(), 1);
        assert_eq!(choices[0].detail, "b.md:2");
    }

    #[test]
    fn base64_reads_what_ripgrep_writes() {
        assert_eq!(base64("L24vY2Fm6S5tZA==").unwrap(), b"/n/caf\xe9.md");
        assert_eq!(base64("YWJj").unwrap(), b"abc");
        assert_eq!(base64("YQ").unwrap(), b"a");
        assert_eq!(base64("").unwrap(), b"");
        assert!(base64("a*b").is_none());
    }

    /// With ripgrep there, a real search, in names that broke the old parsing.
    #[test]
    fn ripgrep_finds_lines_in_odd_names() {
        let root = std::env::temp_dir().join(format!("rature-grep-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let colon = root.join("a:12:b.md");
        let odd = root.join(OsStr::from_bytes(b"caf\xe9.md"));
        std::fs::write(&colon, "one\nneedle here\n").unwrap();
        std::fs::write(&odd, "needle\n").unwrap();
        let result = grep(&root, "needle", &Search::default());
        std::fs::remove_dir_all(&root).unwrap();
        let Ok(choices) = result else {
            eprintln!("ripgrep is not installed: skipped");
            return;
        };
        let mut found = found(&choices);
        found.sort();
        let mut expected = vec![
            (
                colon,
                2,
                "needle here".to_string(),
                "a:12:b.md:2".to_string(),
            ),
            (odd, 1, "needle".into(), "caf\u{fffd}.md:1".into()),
        ];
        expected.sort();
        assert_eq!(found, expected);
    }

    #[test]
    fn a_stopped_search_does_not_start() {
        let search = Search::default();
        drop(Stopper(search.clone()));
        assert!(grep(Path::new("/"), "x", &search).unwrap().is_empty());
    }
}
