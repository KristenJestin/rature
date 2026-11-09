//! A folder's tree, beside the reader: what `.gitignore` hides is not there, it follows the
//! folder as files come and go, and a dot marks a Markdown file changed since it was last read.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use futures::StreamExt as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::formats::is_markdown;
use crate::theme::{theme, tint};
use crate::{state, watch};

actions!(tree, [Down, Up, Open, Close, First, Last]);

pub fn bind_keys(cx: &mut App) {
    let at = Some("Tree");
    cx.bind_keys([
        KeyBinding::new("j", Down, at),
        KeyBinding::new("down", Down, at),
        KeyBinding::new("k", Up, at),
        KeyBinding::new("up", Up, at),
        KeyBinding::new("l", Open, at),
        KeyBinding::new("enter", Open, at),
        KeyBinding::new("right", Open, at),
        KeyBinding::new("h", Close, at),
        KeyBinding::new("left", Close, at),
        KeyBinding::new("g g", First, at),
        KeyBinding::new("shift-g", Last, at),
    ]);
}

pub enum TreeEvent {
    Open(PathBuf),
}

impl EventEmitter<TreeEvent> for Tree {}

#[derive(Clone)]
struct Entry {
    path: PathBuf,
    dir: bool,
}

pub struct Tree {
    root: PathBuf,
    children: HashMap<PathBuf, Vec<Entry>>,
    open: HashSet<PathBuf>,
    selected: Option<PathBuf>,
    current: Option<PathBuf>,
    changed: HashSet<PathBuf>,
    focus: FocusHandle,
    scroll: ScrollHandle,
    _watch: Option<(notify::RecommendedWatcher, Task<()>)>,
}

impl Tree {
    pub fn new(root: PathBuf, cx: &mut Context<Self>) -> Self {
        let watch = match watch::folder(&root) {
            Ok((watcher, mut events)) => {
                let task = cx.spawn(async move |this, cx| {
                    while let Some(mut paths) = events.next().await {
                        while let Ok(more) = events.try_recv() {
                            paths.extend(more);
                        }
                        if this.update(cx, |this, cx| this.touched(paths, cx)).is_err() {
                            break;
                        }
                    }
                });
                Some((watcher, task))
            }
            Err(error) => {
                eprintln!("rature: {}: not followed: {error}", root.display());
                None
            }
        };
        let mut tree = Self {
            root: root.clone(),
            children: HashMap::new(),
            open: HashSet::from([root.clone()]),
            selected: None,
            current: None,
            changed: HashSet::new(),
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            _watch: watch,
        };
        tree.load(&root, cx);
        tree
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    fn load(&mut self, folder: &Path, cx: &mut Context<Self>) {
        let entries = list(folder);
        let markdown: Vec<PathBuf> = entries
            .iter()
            .filter(|e| !e.dir && is_markdown(&e.path))
            .map(|e| e.path.clone())
            .collect();
        self.children.insert(folder.to_path_buf(), entries);
        self.check(markdown, cx);
    }

    /// Finds, off the main thread, which of `files` changed since they were last read.
    fn check(&mut self, files: Vec<PathBuf>, cx: &mut Context<Self>) {
        if files.is_empty() {
            return;
        }
        let checked = cx.background_executor().spawn(async move {
            files
                .into_iter()
                .map(|path| {
                    let changed = state::last_read(&path).is_some_and(|old| {
                        std::fs::read_to_string(&path).is_ok_and(|now| now != old)
                    });
                    (path, changed)
                })
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let checked = checked.await;
            this.update(cx, |this, cx| {
                for (path, changed) in checked {
                    if changed {
                        this.changed.insert(path);
                    } else {
                        this.changed.remove(&path);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Something changed under the root: the folders that hold it are listed again.
    fn touched(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let mut folders = HashSet::new();
        let mut files = Vec::new();
        for path in paths {
            if let Some(parent) = path.parent()
                && self.children.contains_key(parent)
            {
                folders.insert(parent.to_path_buf());
            }
            if is_markdown(&path) {
                files.push(path);
            }
        }
        for folder in folders {
            self.load(&folder, cx);
        }
        self.check(files, cx);
        cx.notify();
    }

    /// The file was marked read, or written: its dot is checked again.
    pub fn recheck(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.check(vec![path], cx);
    }

    /// Shows `path` as the file being read: its folders open, and it selected.
    pub fn reveal(&mut self, path: &Path, cx: &mut Context<Self>) {
        if !path.starts_with(&self.root) {
            return;
        }
        let mut folder = path.parent();
        let mut folders = Vec::new();
        while let Some(f) = folder {
            folders.push(f.to_path_buf());
            if f == self.root {
                break;
            }
            folder = f.parent();
        }
        for f in folders.into_iter().rev() {
            if !self.children.contains_key(&f) {
                self.load(&f, cx);
            }
            self.open.insert(f);
        }
        self.current = Some(path.to_path_buf());
        self.selected = Some(path.to_path_buf());
        self.show_selected();
        cx.notify();
    }

    fn rows(&self) -> Vec<(Entry, usize)> {
        let mut rows = Vec::new();
        self.walk(&self.root, 0, &mut rows);
        rows
    }

    fn walk(&self, folder: &Path, depth: usize, rows: &mut Vec<(Entry, usize)>) {
        for entry in self.children.get(folder).into_iter().flatten() {
            rows.push((entry.clone(), depth));
            if entry.dir && self.open.contains(&entry.path) {
                self.walk(&entry.path, depth + 1, rows);
            }
        }
    }

    fn selected_row(&self, rows: &[(Entry, usize)]) -> Option<usize> {
        let selected = self.selected.as_ref()?;
        rows.iter().position(|(e, _)| &e.path == selected)
    }

    fn show_selected(&self) {
        if let Some(row) = self.selected_row(&self.rows()) {
            self.scroll.scroll_to_item(row);
        }
    }

    fn select(&mut self, row: usize, cx: &mut Context<Self>) {
        if let Some((entry, _)) = self.rows().get(row) {
            self.selected = Some(entry.path.clone());
            self.scroll.scroll_to_item(row);
            cx.notify();
        }
    }

    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        let rows = self.rows();
        let next = self
            .selected_row(&rows)
            .map_or(0, |r| (r + 1).min(rows.len().saturating_sub(1)));
        self.select(next, cx);
    }

    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        let rows = self.rows();
        let previous = self.selected_row(&rows).map_or(0, |r| r.saturating_sub(1));
        self.select(previous, cx);
    }

    fn first(&mut self, _: &First, _: &mut Window, cx: &mut Context<Self>) {
        self.select(0, cx);
    }

    fn last(&mut self, _: &Last, _: &mut Window, cx: &mut Context<Self>) {
        let last = self.rows().len().saturating_sub(1);
        self.select(last, cx);
    }

    fn open_selected(&mut self, _: &Open, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = self.selected.clone() {
            self.activate(path, cx);
        }
    }

    fn activate(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.selected = Some(path.clone());
        if path.is_dir() {
            if !self.open.remove(&path) {
                if !self.children.contains_key(&path) {
                    self.load(&path, cx);
                }
                self.open.insert(path);
            }
        } else {
            cx.emit(TreeEvent::Open(path));
        }
        cx.notify();
    }

    fn close(&mut self, _: &Close, _: &mut Window, cx: &mut Context<Self>) {
        let Some(selected) = self.selected.clone() else {
            return;
        };
        if selected.is_dir() && self.open.contains(&selected) {
            self.open.remove(&selected);
        } else if let Some(parent) = selected.parent()
            && parent != self.root
            && parent.starts_with(&self.root)
        {
            self.open.remove(parent);
            self.selected = Some(parent.to_path_buf());
            self.show_selected();
        }
        cx.notify();
    }
}

/// A folder's entries, folders first, as `.gitignore` and hidden files leave them.
fn list(folder: &Path) -> Vec<Entry> {
    let mut entries: Vec<Entry> = ignore::WalkBuilder::new(folder)
        .max_depth(Some(1))
        .hidden(true)
        .git_ignore(true)
        .require_git(false)
        .build()
        .filter_map(Result::ok)
        .filter(|e| e.depth() == 1)
        .map(|e| Entry {
            dir: e.file_type().is_some_and(|t| t.is_dir()),
            path: e.into_path(),
        })
        .collect();
    entries.sort_by_cached_key(|e| {
        (
            !e.dir,
            e.path
                .file_name()
                .map(|n| n.to_string_lossy().to_lowercase())
                .unwrap_or_default(),
        )
    });
    entries
}

impl Tree {
    fn row(
        &self,
        entry: Entry,
        depth: usize,
        focused: bool,
        t: &crate::theme::Theme,
        cx: &Context<Self>,
    ) -> AnyElement {
        let selected = self.selected.as_ref() == Some(&entry.path);
        let current = self.current.as_ref() == Some(&entry.path);
        let changed = self.changed.contains(&entry.path);
        let name = entry
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let icon = match (entry.dir, self.open.contains(&entry.path)) {
            (true, true) => "▾",
            (true, false) => "▸",
            (false, _) => " ",
        };
        let ink = if current {
            t.accent
        } else if entry.dir {
            t.text
        } else {
            t.muted
        };
        let background = match (selected, focused) {
            (true, true) => Some(Hsla::from(t.selection)),
            (true, false) => Some(tint(t.selection, 0.5)),
            _ => None,
        };
        let path = entry.path.clone();
        div()
            .id(ElementId::Path(entry.path.into()))
            .flex()
            .items_center()
            .gap_1()
            .pl(px(12. + 14. * depth as f32))
            .pr_2()
            .cursor_pointer()
            .when_some(background, |d, bg| d.bg(bg))
            .text_color(ink)
            .when(current, |d| d.font_weight(FontWeight::BOLD))
            .child(div().w(px(12.)).flex_none().text_color(t.muted).child(icon))
            .child(
                div()
                    .flex_1()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(name),
            )
            .when(changed, |d| {
                d.child(div().flex_none().text_color(t.green).child("●"))
            })
            .on_click(
                cx.listener(move |this, _: &ClickEvent, _, cx| this.activate(path.clone(), cx)),
            )
            .into_any_element()
    }
}

impl Render for Tree {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let focused = self.focus.contains_focused(window, cx);
        let name = self
            .root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.root.display().to_string());
        let rows = self.rows();
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(t.surface)
            .text_size(px(13.5))
            .child(
                div()
                    .flex_none()
                    .px_3()
                    .py_2()
                    .text_color(t.muted)
                    .font_weight(FontWeight::BOLD)
                    .child(name),
            )
            .child(
                div()
                    .id("tree")
                    .key_context("Tree")
                    .track_focus(&self.focus)
                    .on_action(cx.listener(Self::down))
                    .on_action(cx.listener(Self::up))
                    .on_action(cx.listener(Self::first))
                    .on_action(cx.listener(Self::last))
                    .on_action(cx.listener(Self::open_selected))
                    .on_action(cx.listener(Self::close))
                    .flex_1()
                    .track_scroll(&self.scroll)
                    .overflow_y_scroll()
                    .pb_4()
                    .children(
                        rows.into_iter()
                            .map(|(entry, depth)| self.row(entry, depth, focused, &t, cx)),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::list;

    #[test]
    fn a_folder_lists_its_folders_first_without_what_git_ignores() {
        let root = std::env::temp_dir().join(format!("rature-tree-{}", std::process::id()));
        std::fs::create_dir_all(root.join("specs")).unwrap();
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::write(root.join(".gitignore"), "target/\n").unwrap();
        std::fs::write(root.join("README.md"), "# R\n").unwrap();
        std::fs::write(root.join("a.md"), "# A\n").unwrap();
        let names: Vec<String> = list(&root)
            .iter()
            .map(|e| e.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["specs", "a.md", "README.md"]);
        std::fs::remove_dir_all(root).unwrap();
    }
}
