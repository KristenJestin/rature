//! A canvas as the window edits it: what the file held when it was last read or written, with
//! the operations not yet written replayed on it; undo and redo; and the writes, each one the
//! disk's text read again, the operations replayed on it ([`crate::canvas_ops`]) and the result written
//! whole, under a lock between Rature's processes.
//!
//! When to write is the window's to decide: once a card is let go after a move or a resize,
//! 800 ms after the last key in a card's text. Changes to the same fields of a node one after
//! the other, before they are written (a drag, keys typed), are one operation, undone at once.
//!
//! The lock is a file of Rature's own, `$XDG_RUNTIME_DIR/rature/canvas-<hash of the path>.lock`
//! (`/run/user/<uid>` without the variable), never one beside the canvas: it keeps the window and `rature mcp` from writing the same
//! canvas at once. Against another program (Obsidian, an editor), `fsio::update`'s look just
//! before its rename stays the only guard.

use std::ffi::OsStr;
use std::fs::{self, File};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::canvas::{Canvas, Problem};
use crate::canvas_ops::{Op, Report};
use crate::fsio::{self, Lock, Update};

/// How long a write waits for another process writing the same canvas.
const PATIENCE: Duration = Duration::from_secs(2);

/// A canvas being edited.
#[derive(Debug)]
pub struct Board {
    /// The canvas's file, links resolved: what the lock is named after.
    path: PathBuf,
    /// The folder of the locks.
    locks: PathBuf,
    /// What the window shows.
    canvas: Canvas,
    /// The operations not yet written, in the order they were made.
    pending: Vec<Op>,
    /// What undo undoes, an action (one or more operations) at a time; redo the same.
    undo: Vec<Vec<Op>>,
    redo: Vec<Vec<Op>>,
    /// The last action is an edit the next one may join.
    joinable: bool,
    /// How many of the pending operations, the first ones, a write off the window's thread took
    /// and has not given back.
    in_flight: usize,
}

/// What [`Board::save`] did.
#[derive(Debug, PartialEq)]
pub enum Saved {
    /// The operations were replayed on the disk's canvas and are no more pending; `written`
    /// unless that left the file as it was. The report says what did not go as Rature did it.
    Done { written: bool, report: Report },
    /// The file is not a valid canvas now (another program broke it): nothing was written, the
    /// operations wait for it to be mended.
    Invalid(Vec<Problem>),
    /// Another process held the lock past the patience, or the file kept changing: nothing was
    /// written, the operations wait.
    Busy,
}

/// A write of the operations pending when it was asked for ([`Board::writing`]): run off the
/// window's thread, since it may wait for the lock, then given back to [`Board::written`].
#[derive(Debug)]
pub struct Writing {
    path: PathBuf,
    locks: PathBuf,
    ops: Vec<Op>,
    /// How many of `ops`, the first ones, another write runs too: replayed, their report left
    /// to it.
    quiet: usize,
}

/// What a [`Writing`] did.
#[derive(Debug)]
pub struct Written {
    /// How many of the operations pending, the first ones, are written.
    count: usize,
    /// The file's text once they were replayed on it, when they were.
    text: Option<String>,
    saved: Saved,
}

impl Board {
    /// Opens the canvas at `path`, its locks in the user's runtime folder.
    #[cfg(test)]
    pub fn open(path: &Path) -> io::Result<Self> {
        Self::open_with_locks(path, runtime_locks())
    }

    /// Opens the canvas at `path`, its locks in `locks`.
    #[cfg(test)]
    pub fn open_with_locks(path: &Path, locks: PathBuf) -> io::Result<Self> {
        let text = fs::read_to_string(fs::canonicalize(path)?)?;
        Self::with_canvas(path, locks, Canvas::read(&text))
    }

    /// The canvas at `path` as `canvas` has it, read from the file already (by the window, off
    /// its thread); its locks in `locks`.
    pub fn with_canvas(path: &Path, locks: PathBuf, canvas: Canvas) -> io::Result<Self> {
        let path = fs::canonicalize(path)?;
        fsio::regular(&path)?;
        Ok(Self {
            path,
            locks,
            canvas,
            pending: Vec::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            joinable: false,
            in_flight: 0,
        })
    }

    #[cfg(test)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The canvas as the window shows it, the operations not yet written included.
    pub fn canvas(&self) -> &Canvas {
        &self.canvas
    }

    pub fn pending(&self) -> &[Op] {
        &self.pending
    }

    /// Does an operation, made against [`Board::canvas`]. One that sets the same fields of the
    /// same node or edge as the one before it, not yet written, joins it.
    pub fn edit(&mut self, op: Op) {
        op.apply(&mut self.canvas, &mut Report::default());
        self.redo.clear();
        if self.joinable
            && let Some(last) = self.pending.last_mut()
            && let Some([action]) = self.undo.last_mut().map(Vec::as_mut_slice)
            && last.joins(&op)
            && action.joins(&op)
        {
            last.join(&op);
            action.join(&op);
            return;
        }
        self.joinable = matches!(op, Op::Set { .. });
        self.pending.push(op.clone());
        self.undo.push(vec![op]);
    }

    /// Does operations as one action, undone at once (cards deleted together). Each is made
    /// against the canvas as the ones before it leave it.
    pub fn edit_all(&mut self, ops: Vec<Op>) {
        if ops.is_empty() {
            return;
        }
        self.do_all(&ops);
        self.redo.clear();
        self.undo.push(ops);
    }

    /// How many actions undo holds now: a mark for [`Board::join_since`].
    pub fn actions(&self) -> usize {
        self.undo.len()
    }

    /// The actions done since `mark`, which [`Board::actions`] gave, made one, undone at once: a
    /// card made and its text typed, written in several times.
    pub fn join_since(&mut self, mark: usize) {
        if self.undo.len() > mark + 1 {
            let joined: Vec<Op> = self.undo.drain(mark..).flatten().collect();
            self.undo.push(joined);
        }
    }

    /// Undoes the last action; false when there is none.
    pub fn undo(&mut self) -> bool {
        let Some(action) = self.undo.pop() else {
            return false;
        };
        let inverse: Vec<Op> = action.iter().rev().map(Op::inverse).collect();
        self.do_all(&inverse);
        self.redo.push(action);
        true
    }

    /// Does again the last action undone; false when there is none.
    pub fn redo(&mut self) -> bool {
        let Some(action) = self.redo.pop() else {
            return false;
        };
        self.do_all(&action);
        self.undo.push(action);
        true
    }

    fn do_all(&mut self, ops: &[Op]) {
        for op in ops {
            op.apply(&mut self.canvas, &mut Report::default());
        }
        self.pending.extend_from_slice(ops);
        self.joinable = false;
    }

    /// Takes the file's new text, read after another program wrote it: what the window shows is
    /// it, with the operations not yet written replayed on it.
    pub fn refresh(&mut self, text: &str) {
        self.refreshed(Canvas::read(text));
    }

    /// As [`Board::refresh`], the file's new text read already.
    pub fn refreshed(&mut self, canvas: Canvas) {
        self.canvas = canvas;
        for op in &self.pending {
            op.apply(&mut self.canvas, &mut Report::default());
        }
    }

    /// Writes the operations not yet written: replayed on the file's text as it is now, under
    /// the lock. Blocks while another process writes the canvas (at most [`PATIENCE`]), and for
    /// a write: not on the window's thread.
    ///
    /// What a write off the window's thread took is written again, since the process may end
    /// before that write does (the board left, the window closed): replayed on a file that has
    /// it already, it changes nothing, and what it could not do is that write's to say.
    pub fn save(&mut self) -> io::Result<Saved> {
        self.joinable = false;
        let writing = Writing {
            path: self.path.clone(),
            locks: self.locks.clone(),
            ops: self.pending.clone(),
            quiet: std::mem::take(&mut self.in_flight),
        };
        let written = writing.run()?;
        let count = written.count;
        Ok(self.take_written(written, count))
    }

    /// The write of the operations pending now, to [`Writing::run`] off the window's thread.
    /// Those made meanwhile wait for the next one: none joins those being written.
    pub fn writing(&mut self) -> Writing {
        self.joinable = false;
        self.in_flight = self.pending.len();
        Writing {
            path: self.path.clone(),
            locks: self.locks.clone(),
            ops: self.pending.clone(),
            quiet: 0,
        }
    }

    /// A write off the window's thread done: the operations it wrote are no more pending, and
    /// what the window shows is the file as written with those made since replayed on it. None
    /// is, when a [`Board::save`] meanwhile wrote them already.
    pub fn written(&mut self, written: Written) -> Saved {
        let taken = std::mem::take(&mut self.in_flight);
        self.take_written(written, taken)
    }

    /// A write done, the first `taken` of the pending operations its own.
    fn take_written(&mut self, written: Written, taken: usize) -> Saved {
        if taken > 0
            && let Some(text) = written.text
        {
            let count = written.count.min(taken).min(self.pending.len());
            self.pending.drain(..count);
            self.refresh(&text);
        }
        written.saved
    }
}

impl Written {
    /// Whether the operations were replayed on the file's text, and are on disk now.
    pub fn is_done(&self) -> bool {
        self.text.is_some()
    }
}

impl Writing {
    /// The operations it writes.
    pub fn ops(&self) -> &[Op] {
        &self.ops
    }

    /// Replays the operations on the file's text as it is now, under the lock, and writes it.
    /// Blocks while another process writes the canvas (at most [`PATIENCE`]).
    pub fn run(self) -> io::Result<Written> {
        let nothing = |saved| Written {
            count: 0,
            text: None,
            saved,
        };
        if self.ops.is_empty() {
            return Ok(nothing(Saved::Done {
                written: false,
                report: Report::default(),
            }));
        }
        // What the last replay found: `update` may ask more than once.
        let mut found = Err(Vec::new());
        let (pending, quiet) = (&self.ops, self.quiet);
        let update = update(&self.path, &self.locks, |now| {
            let mut disk = Canvas::read(now);
            let problems = disk.problems();
            if !problems.is_empty() {
                found = Err(problems);
                return None;
            }
            let mut report = Report::default();
            for (i, op) in pending.iter().enumerate() {
                match i < quiet {
                    true => op.apply(&mut disk, &mut Report::default()),
                    false => op.apply(&mut disk, &mut report),
                }
            }
            match disk.write() {
                Ok(text) => {
                    found = Ok(report);
                    (text != now).then_some(text)
                }
                Err(problems) => {
                    found = Err(problems);
                    None
                }
            }
        })?;
        let (text, written) = match update {
            Update::Busy => return Ok(nothing(Saved::Busy)),
            Update::Written { after, .. } => (after, true),
            Update::Left(text) => (text, false),
        };
        Ok(match found {
            Ok(report) => Written {
                count: self.ops.len(),
                text: Some(text),
                saved: Saved::Done { written, report },
            },
            Err(problems) => nothing(Saved::Invalid(problems)),
        })
    }
}

/// [`fsio::update`] of the canvas at `path`, under the lock of that canvas in `locks`, so that
/// no other Rature process reads it between this one's read and its write: `Busy` when another
/// holds the lock past [`PATIENCE`]. For several operations in one write, all or none, replay
/// them on `Canvas::read` of the text given, and return `None` unless each report is clean.
pub fn update(
    path: &Path,
    locks: &Path,
    edit: impl FnMut(&str) -> Option<String>,
) -> io::Result<Update> {
    let path = fs::canonicalize(path)?;
    let Some(_lock) = Lock::take(&lock_path(locks, &path), PATIENCE)? else {
        return Ok(Update::Busy);
    };
    fsio::update(&path, edit)
}

/// [`update`] of the canvas `name` in the folder `folder` is open on ([`fsio::update_in`]): a
/// folder on its way swapped for a link since it was opened leads nowhere else. Its lock is
/// named after the path the folder has now.
pub fn update_in(
    folder: &File,
    name: &OsStr,
    locks: &Path,
    edit: impl FnMut(&str) -> Option<String>,
) -> io::Result<Update> {
    let path = fsio::folder_path(folder)?.join(name);
    let Some(_lock) = Lock::take(&lock_path(locks, &path), PATIENCE)? else {
        return Ok(Update::Busy);
    };
    fsio::update_in(folder, name, edit)
}

/// Writes a new canvas `name` in the folder `folder` is open on ([`fsio::create_in`]: never over
/// a file there, an `AlreadyExists` error then), under the lock its path has once made, so that
/// it is not made while another Rature process writes a canvas there. `Busy` when another holds
/// the lock past [`PATIENCE`]. No folder is made.
pub fn create_in(folder: &File, name: &OsStr, locks: &Path, text: &str) -> io::Result<Update> {
    // The path the canvas will have, links resolved, as `update` names its lock.
    let path = fsio::folder_path(folder)?.join(name);
    let Some(_lock) = Lock::take(&lock_path(locks, &path), PATIENCE)? else {
        return Ok(Update::Busy);
    };
    fsio::create_in(folder, name, text.as_bytes())?;
    Ok(Update::Written {
        before: String::new(),
        after: text.into(),
    })
}

/// The folder of the canvases' locks: `rature` in the user's runtime folder, beside the
/// socket of the window: the same folder whether `XDG_RUNTIME_DIR` is set or not, so that the
/// window and `rature mcp` take the same locks (`instance::runtime_folder`).
pub fn runtime_locks() -> PathBuf {
    let socket = crate::instance::socket();
    socket.parent().unwrap_or(Path::new("/")).join("rature")
}

/// The lock of the canvas at `canvas` (a path with its links resolved), in `locks`.
pub fn lock_path(locks: &Path, canvas: &Path) -> PathBuf {
    locks.join(format!(
        "canvas-{:016x}.lock",
        fnv(canvas.as_os_str().as_bytes())
    ))
}

/// FNV-1a, 64 bits: the same from one build of Rature to the next, unlike `DefaultHasher`.
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::{Edge, Node, Rect};
    use crate::canvas_ops::{Conflict, Target};
    use std::time::Instant;

    /// Three cards, `a` `b` `c`, and two edges, `ab` and `bc`: as Rature writes it.
    const START: &str = concat!(
        "{\n",
        "\t\"nodes\":[\n",
        "\t\t{\"id\":\"a\",\"type\":\"text\",\"text\":\"A\",\"x\":0,\"y\":0,\"width\":100,\"height\":50},\n",
        "\t\t{\"id\":\"b\",\"type\":\"text\",\"text\":\"B\",\"x\":200,\"y\":0,\"width\":100,\"height\":50,\"color\":\"2\"},\n",
        "\t\t{\"id\":\"c\",\"type\":\"text\",\"text\":\"C\",\"x\":400,\"y\":0,\"width\":100,\"height\":50}\n",
        "\t],\n",
        "\t\"edges\":[\n",
        "\t\t{\"id\":\"ab\",\"fromNode\":\"a\",\"toNode\":\"b\"},\n",
        "\t\t{\"id\":\"bc\",\"fromNode\":\"b\",\"toNode\":\"c\"}\n",
        "\t]\n",
        "}"
    );

    /// A folder of its own for each test, removed at its end: the canvas, and the locks in a
    /// folder of their own (never the user's runtime folder).
    struct Folder(PathBuf);

    impl Folder {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("rature-board-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(path.join("notes")).unwrap();
            fs::write(path.join("notes/a.canvas"), START).unwrap();
            Self(path)
        }

        fn canvas(&self) -> PathBuf {
            self.0.join("notes/a.canvas")
        }

        fn locks(&self) -> PathBuf {
            self.0.join("run/rature")
        }

        fn board(&self) -> Board {
            Board::open_with_locks(&self.canvas(), self.locks()).unwrap()
        }

        fn text(&self) -> String {
            fs::read_to_string(self.canvas()).unwrap()
        }

        fn disk(&self) -> Canvas {
            Canvas::read(&self.text())
        }

        /// Another program changes the file.
        fn theirs(&self, change: impl FnOnce(&Canvas) -> Op) {
            let mut canvas = self.disk();
            change(&canvas).apply(&mut canvas, &mut Report::default());
            fs::write(self.canvas(), canvas.write().unwrap()).unwrap();
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn rect(node: &Node) -> (f64, f64, f64, f64) {
        let r = node.rect().unwrap();
        (r.x, r.y, r.width, r.height)
    }

    fn done(saved: Saved) -> Report {
        match saved {
            Saved::Done {
                written: true,
                report,
            } => report,
            other => panic!("not written: {other:?}"),
        }
    }

    #[test]
    fn the_start_is_written_as_rature_writes() {
        assert_eq!(Canvas::read(START).write().unwrap(), START);
    }

    /// Row 1: the disk did not change the field: Rature's value is written, and what the disk
    /// changed elsewhere is kept.
    #[test]
    fn a_field_the_disk_left_takes_raturess_value() {
        let folder = Folder::new("row1");
        let mut board = folder.board();
        let op = Op::move_node(board.canvas(), "a", 10., 20.).unwrap();
        board.edit(op);
        folder.theirs(|c| Op::set_text(c, "b", "theirs").unwrap());
        assert!(done(board.save().unwrap()).is_clean());
        let disk = folder.disk();
        assert_eq!(rect(disk.node("a").unwrap()), (10., 20., 100., 50.));
        assert_eq!(disk.node("b").unwrap().text(), Some("theirs"));
        assert_eq!(board.canvas(), &disk, "the window shows the disk's canvas");
        assert!(board.pending().is_empty());
    }

    /// A write runs off the window's thread: what is done meanwhile is neither written nor
    /// lost, nor joined to what is being written.
    #[test]
    fn an_edit_made_while_writing_waits_for_the_next_write() {
        let folder = Folder::new("writing");
        let mut board = folder.board();
        let op = Op::move_node(board.canvas(), "a", 10., 0.).unwrap();
        board.edit(op);
        let writing = board.writing();
        let op = Op::move_node(board.canvas(), "a", 20., 0.).unwrap();
        board.edit(op);
        let written = writing.run().unwrap();
        assert!(done(board.written(written)).is_clean());
        assert_eq!(rect(folder.disk().node("a").unwrap()).0, 10.);
        assert_eq!(rect(board.canvas().node("a").unwrap()).0, 20.);
        assert_eq!(board.pending().len(), 1);
        assert!(done(board.save().unwrap()).is_clean());
        assert_eq!(rect(folder.disk().node("a").unwrap()).0, 20.);
        // Two moves, two writes: undone one at a time.
        assert!(board.undo());
        assert_eq!(rect(board.canvas().node("a").unwrap()).0, 10.);
    }

    /// The board left while a write runs off the window's thread: what that write took is
    /// written again, in case it never ends, but not said twice; its own result, back after,
    /// leaves nothing to undo.
    #[test]
    fn a_write_on_leaving_writes_what_a_running_write_took_without_saying_it_again() {
        let new = |id: &str| {
            let rect = Rect {
                x: 0.,
                y: 100.,
                width: 100.,
                height: 50.,
            };
            Op::add_node(Node::new_text(id, "N", rect))
        };
        let folder = Folder::new("leaving");
        let mut board = folder.board();
        board.edit(new("n"));
        let writing = board.writing();
        // Written, its result not back yet when the board is left.
        let written = writing.run().unwrap();
        let op = Op::move_node(board.canvas(), "a", 10., 0.).unwrap();
        board.edit(op);
        assert!(done(board.save().unwrap()).is_clean(), "n is not said taken");
        assert!(board.pending().is_empty());
        assert!(done(board.written(written)).is_clean());
        assert!(board.pending().is_empty());
        let disk = folder.disk();
        let ns = disk.nodes().iter().filter(|n| n.id() == Some("n")).count();
        assert_eq!(ns, 1);
        assert_eq!(rect(disk.node("a").unwrap()).0, 10.);
        // A write that never ends: what it took is written on leaving all the same.
        board.edit(new("m"));
        let _never = board.writing();
        assert!(done(board.save().unwrap()).is_clean());
        assert!(folder.disk().node("m").is_some());
    }

    /// Row 2: the disk changed a field Rature did not: the disk's is kept, nothing said.
    #[test]
    fn a_field_only_the_disk_changed_stays_the_disks() {
        let folder = Folder::new("row2");
        let mut board = folder.board();
        let op = Op::resize_node(board.canvas(), "a", 150., 60.).unwrap();
        board.edit(op);
        folder.theirs(|c| Op::move_node(c, "a", 5., 5.).unwrap());
        assert!(done(board.save().unwrap()).is_clean());
        assert_eq!(rect(folder.disk().node("a").unwrap()), (5., 5., 150., 60.));
    }

    /// Row 3: both moved or resized the card: the disk wins, and the report says so.
    #[test]
    fn both_moved_the_card_the_disk_wins_and_says_so() {
        let folder = Folder::new("row3");
        let mut board = folder.board();
        let op = Op::move_node(board.canvas(), "a", 10., 10.).unwrap();
        board.edit(op);
        let op = Op::resize_node(board.canvas(), "a", 120., 50.).unwrap();
        board.edit(op);
        folder.theirs(|c| Op::move_node(c, "a", 30., 30.).unwrap());
        folder.theirs(|c| Op::resize_node(c, "a", 90., 50.).unwrap());
        let report = done_or_left(board.save().unwrap());
        assert_eq!(
            report.conflicts,
            [
                Conflict::Kept {
                    id: "a".into(),
                    fields: vec!["x".into(), "y".into()]
                },
                Conflict::Kept {
                    id: "a".into(),
                    fields: vec!["width".into()]
                }
            ]
        );
        assert!(report.clipboard.is_empty());
        assert_eq!(rect(folder.disk().node("a").unwrap()), (30., 30., 90., 50.));
        assert_eq!(
            report.conflicts[0].to_string(),
            "a was changed in the file too: its x, y there kept"
        );
    }

    /// Row 4: both changed a card's text: the disk wins, what the user typed is to copy.
    #[test]
    fn both_changed_the_text_the_disk_wins_and_the_typed_text_is_kept_to_paste() {
        let folder = Folder::new("row4");
        let mut board = folder.board();
        for typed in ["m", "mi", "min", "mine"] {
            let op = Op::set_text(board.canvas(), "a", typed).unwrap();
            board.edit(op);
        }
        folder.theirs(|c| Op::set_text(c, "a", "theirs").unwrap());
        let report = done_or_left(board.save().unwrap());
        assert_eq!(
            report.conflicts,
            [Conflict::Kept {
                id: "a".into(),
                fields: vec!["text".into()]
            }]
        );
        assert_eq!(report.clipboard, ["mine"]);
        assert_eq!(folder.disk().node("a").unwrap().text(), Some("theirs"));
        assert_eq!(board.canvas().node("a").unwrap().text(), Some("theirs"));
    }

    fn done_or_left(saved: Saved) -> Report {
        match saved {
            Saved::Done { report, .. } => report,
            other => panic!("not done: {other:?}"),
        }
    }

    /// Row 5: the card was deleted on disk: the operations on it are dropped and said, the text
    /// typed for it kept to paste.
    #[test]
    fn a_card_deleted_on_disk_drops_the_operations_on_it() {
        let folder = Folder::new("row5");
        let mut board = folder.board();
        let ops = [
            Op::set_text(board.canvas(), "a", "typed").unwrap(),
            Op::move_node(board.canvas(), "a", 1., 1.).unwrap(),
            Op::to_front(board.canvas(), "a").unwrap(),
        ];
        for op in ops {
            board.edit(op);
        }
        let op = Op::add_edge(Edge::new("ca", "c", "a"));
        board.edit(op);
        folder.theirs(|c| Op::delete_node(c, "a").unwrap());
        let report = done_or_left(board.save().unwrap());
        let gone = Conflict::Gone { id: "a".into() };
        assert_eq!(
            report.conflicts,
            [gone.clone(), gone.clone(), gone.clone(), gone]
        );
        assert_eq!(report.clipboard, ["typed"]);
        let disk = folder.disk();
        assert!(disk.node("a").is_none());
        assert_eq!(disk.problems(), []);
        assert_eq!(board.canvas(), &disk);
    }

    /// Row 6: Rature deletes a card the disk changed: the deletion goes through, with its edges,
    /// those the disk added included.
    #[test]
    fn a_card_rature_deletes_goes_whatever_the_disk_did_with_its_edges() {
        let folder = Folder::new("row6");
        let mut board = folder.board();
        let op = Op::delete_node(board.canvas(), "b").unwrap();
        board.edit(op);
        folder.theirs(|c| Op::set_text(c, "b", "theirs").unwrap());
        folder.theirs(|_| Op::add_edge(Edge::new("cb", "c", "b")));
        folder.theirs(|_| Op::add_edge(Edge::new("ca", "c", "a")));
        assert!(done(board.save().unwrap()).is_clean());
        let disk = folder.disk();
        assert!(disk.node("b").is_none());
        let edges: Vec<_> = disk.edges().iter().map(|e| e.id().unwrap()).collect();
        assert_eq!(edges, ["ca"]);
    }

    #[test]
    fn each_operation_is_undone_and_done_again_on_disk() {
        let new = Node::new_text(
            "n",
            "N",
            Rect {
                x: 0.,
                y: 100.,
                width: 100.,
                height: 50.,
            },
        );
        type Make = Box<dyn Fn(&Canvas) -> Op>;
        let cases: Vec<(&str, Make)> = vec![
            (
                "move",
                Box::new(|c| Op::move_node(c, "a", 10., 20.).unwrap()),
            ),
            (
                "resize",
                Box::new(|c| Op::resize_node(c, "a", 120., 80.).unwrap()),
            ),
            ("text", Box::new(|c| Op::set_text(c, "a", "new").unwrap())),
            (
                "colour",
                Box::new(|c| Op::set_color(c, Target::Node, "a", Some("#FF8000")).unwrap()),
            ),
            (
                "no colour",
                Box::new(|c| Op::set_color(c, Target::Node, "b", None).unwrap()),
            ),
            (
                "edge colour",
                Box::new(|c| Op::set_color(c, Target::Edge, "ab", Some("4")).unwrap()),
            ),
            ("add node", Box::new(move |_| Op::add_node(new.clone()))),
            (
                "delete node",
                Box::new(|c| Op::delete_node(c, "b").unwrap()),
            ),
            (
                "add edge",
                Box::new(|_| Op::add_edge(Edge::new("ca", "c", "a"))),
            ),
            (
                "delete edge",
                Box::new(|c| Op::delete_edge(c, "ab").unwrap()),
            ),
            ("to front", Box::new(|c| Op::to_front(c, "a").unwrap())),
            ("to back", Box::new(|c| Op::to_back(c, "c").unwrap())),
        ];
        for (name, make) in cases {
            let folder = Folder::new(&format!("undo-{}", name.replace(' ', "-")));
            let mut board = folder.board();
            let op = make(board.canvas());
            board.edit(op);
            done(board.save().unwrap());
            let after = folder.text();
            assert_ne!(after, START, "{name}");
            assert!(board.undo(), "{name}");
            assert_eq!(
                board.canvas().write().unwrap(),
                START,
                "{name}: undone in the window"
            );
            done(board.save().unwrap());
            assert_eq!(
                folder.text(),
                START,
                "{name}: undone on disk, byte for byte"
            );
            assert!(board.redo(), "{name}");
            done(board.save().unwrap());
            assert_eq!(folder.text(), after, "{name}: done again");
            assert!(!board.redo(), "{name}");
        }
    }

    #[test]
    fn keys_typed_one_after_the_other_are_one_operation_undone_at_once() {
        let folder = Folder::new("join");
        let mut board = folder.board();
        for typed in ["t", "ty", "typ"] {
            let op = Op::set_text(board.canvas(), "a", typed).unwrap();
            board.edit(op);
        }
        assert_eq!(board.pending().len(), 1);
        assert!(board.undo());
        assert_eq!(board.canvas().node("a").unwrap().text(), Some("A"));
        assert!(!board.undo(), "one action");
        assert_eq!(
            board.save().unwrap(),
            Saved::Done {
                written: false,
                report: Report::default()
            }
        );
        assert_eq!(folder.text(), START);
        // Once written, the next keys are another action.
        let op = Op::set_text(board.canvas(), "a", "x").unwrap();
        board.edit(op);
        done(board.save().unwrap());
        let op = Op::set_text(board.canvas(), "a", "xy").unwrap();
        board.edit(op);
        assert!(board.undo());
        assert_eq!(board.canvas().node("a").unwrap().text(), Some("x"));
    }

    #[test]
    fn a_file_made_invalid_meanwhile_is_never_written() {
        let folder = Folder::new("invalid");
        let mut board = folder.board();
        let op = Op::move_node(board.canvas(), "a", 10., 20.).unwrap();
        board.edit(op);
        let broken = r#"{"nodes":[{"id":"a","#;
        fs::write(folder.canvas(), broken).unwrap();
        let Saved::Invalid(problems) = board.save().unwrap() else {
            panic!("written over a broken file");
        };
        assert!(matches!(problems.as_slice(), [Problem::Json(_)]));
        assert_eq!(folder.text(), broken);
        assert_eq!(board.pending().len(), 1, "kept for when it is mended");
        // An edge to a node that is not there: valid JSON, an invalid canvas.
        let dangling = START.replace("\"toNode\":\"c\"", "\"toNode\":\"z\"");
        fs::write(folder.canvas(), &dangling).unwrap();
        assert!(matches!(board.save().unwrap(), Saved::Invalid(_)));
        assert_eq!(folder.text(), dangling);
        fs::write(folder.canvas(), START).unwrap();
        assert!(done(board.save().unwrap()).is_clean());
        assert_eq!(
            rect(folder.disk().node("a").unwrap()),
            (10., 20., 100., 50.)
        );
    }

    #[test]
    fn a_change_from_another_program_is_shown_under_the_pending_operations() {
        let folder = Folder::new("refresh");
        let mut board = folder.board();
        let op = Op::move_node(board.canvas(), "a", 10., 20.).unwrap();
        board.edit(op);
        folder.theirs(|c| Op::set_text(c, "a", "theirs").unwrap());
        board.refresh(&folder.text());
        let a = board.canvas().node("a").unwrap();
        assert_eq!((a.text(), rect(a).0), (Some("theirs"), 10.));
    }

    #[test]
    fn a_write_waits_for_the_lock_and_writes_nothing_beside_the_canvas() {
        let folder = Folder::new("lock");
        let mut board = folder.board();
        let lock = lock_path(&folder.locks(), board.path());
        assert_eq!(lock.parent().unwrap(), folder.locks());
        let name = lock.file_name().unwrap().to_str().unwrap();
        assert!(
            name.starts_with("canvas-") && name.ends_with(".lock"),
            "{name}"
        );
        let held = Lock::take(&lock, Duration::ZERO).unwrap().unwrap();
        let holder = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            drop(held);
        });
        let op = Op::move_node(board.canvas(), "a", 10., 20.).unwrap();
        board.edit(op);
        let started = Instant::now();
        done(board.save().unwrap());
        assert!(
            started.elapsed() >= Duration::from_millis(150),
            "waited for the holder"
        );
        holder.join().unwrap();
        let names: Vec<_> = fs::read_dir(folder.0.join("notes"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["a.canvas"]);
    }

    #[test]
    fn a_new_canvas_is_made_under_the_lock_and_never_over_a_file() {
        let folder = Folder::new("create");
        let new = folder.0.join("notes/new.canvas");
        let notes = File::open(folder.0.join("notes")).unwrap();
        let name = OsStr::new("new.canvas");
        let lock = lock_path(&folder.locks(), &new);
        let held = Lock::take(&lock, Duration::ZERO).unwrap().unwrap();
        assert_eq!(
            create_in(&notes, name, &folder.locks(), "{}").unwrap(),
            Update::Busy,
            "another process writes there"
        );
        assert!(!new.exists());
        drop(held);
        let text = Canvas::default().write().unwrap();
        assert!(matches!(
            create_in(&notes, name, &folder.locks(), &text).unwrap(),
            Update::Written { .. }
        ));
        assert_eq!(fs::read_to_string(&new).unwrap(), text);
        // Its lock is the one its writes take.
        assert_eq!(
            lock_path(&folder.locks(), &fs::canonicalize(&new).unwrap()),
            lock
        );
        let error = create_in(&notes, OsStr::new("a.canvas"), &folder.locks(), &text).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(folder.text(), START);
    }

    #[test]
    fn a_folder_swapped_for_a_link_once_opened_leads_nowhere_else() {
        let folder = Folder::new("swap");
        let notes = File::open(folder.0.join("notes")).unwrap();
        // Another program moves the folder away, and puts a link to another one in its place.
        let other = folder.0.join("other");
        fs::create_dir(&other).unwrap();
        fs::write(other.join("a.canvas"), START).unwrap();
        fs::rename(folder.0.join("notes"), folder.0.join("moved")).unwrap();
        std::os::unix::fs::symlink(&other, folder.0.join("notes")).unwrap();

        let mut seen = None;
        let done = update_in(&notes, OsStr::new("a.canvas"), &folder.locks(), |now| {
            seen = Some(now.to_owned());
            Some(now.replace("\"A\"", "\"mine\""))
        })
        .unwrap();
        assert!(matches!(done, Update::Written { .. }));
        assert_eq!(seen.as_deref(), Some(START));
        let moved = fs::read_to_string(folder.0.join("moved/a.canvas")).unwrap();
        assert!(moved.contains("\"mine\""), "{moved}");
        assert_eq!(fs::read_to_string(other.join("a.canvas")).unwrap(), START);
        assert_eq!(fs::read_dir(&other).unwrap().count(), 1);
        // Its lock is named after where the folder is now.
        let moved = fs::canonicalize(folder.0.join("moved/a.canvas")).unwrap();
        assert!(lock_path(&folder.locks(), &moved).exists());

        let text = Canvas::default().write().unwrap();
        create_in(&notes, OsStr::new("new.canvas"), &folder.locks(), &text).unwrap();
        assert!(folder.0.join("moved/new.canvas").exists());
        assert!(!other.join("new.canvas").exists());
    }

    /// Set in the processes the test below starts: the canvas they write, and their name.
    const WRITER: &str = "RATURE_TEST_CANVAS_WRITER";
    const CANVAS: &str = "RATURE_TEST_CANVAS";
    /// The cards each process adds, one write each: enough for cards to be lost without the
    /// lock (`fsio::update`'s own guard alone lost some at 300, tried while this was written).
    const CARDS: usize = 300;

    /// Two processes (this test binary, started again on this test alone) add cards to the same
    /// canvas at once, each card a write of its own: no card is lost. Their runtime folder is a temporary one: the default place of the locks is used.
    #[test]
    fn two_processes_writing_the_same_canvas_lose_nothing() {
        if let Some(writer) = std::env::var_os(WRITER) {
            let writer = writer.to_str().unwrap().to_owned();
            let mut board = Board::open(Path::new(&std::env::var_os(CANVAS).unwrap())).unwrap();
            for card in 0..CARDS {
                board.refresh(&fs::read_to_string(board.path()).unwrap());
                let id = format!("{writer}{card}");
                let rect = Rect {
                    x: card as f64 * 10.,
                    y: 100.,
                    width: 50.,
                    height: 50.,
                };
                board.edit(Op::add_node(Node::new_text(&id, &id, rect)));
                loop {
                    match board.save().unwrap() {
                        Saved::Done { written: true, .. } => break,
                        Saved::Busy => continue,
                        other => panic!("{other:?}"),
                    }
                }
            }
            return;
        }
        let folder = Folder::new("processes");
        let test = format!(
            "{}::two_processes_writing_the_same_canvas_lose_nothing",
            module_path!().split_once("::").unwrap().1
        );
        let writers: Vec<_> = ["p", "q"]
            .into_iter()
            .map(|name| {
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args([test.as_str(), "--exact", "--test-threads=1", "--nocapture"])
                    .env(WRITER, name)
                    .env(CANVAS, folder.canvas())
                    .env("XDG_RUNTIME_DIR", folder.0.join("run"))
                    .spawn()
                    .unwrap()
            })
            .collect();
        for mut writer in writers {
            assert!(writer.wait().unwrap().success());
        }
        let disk = folder.disk();
        assert_eq!(disk.problems(), []);
        for name in ["p", "q"] {
            for card in 0..CARDS {
                let id = format!("{name}{card}");
                assert!(disk.node(&id).is_some(), "{id} lost");
            }
        }
        assert_eq!(disk.nodes().len(), 3 + 2 * CARDS);
        // The lock was taken where the user's runtime folder says, never beside the canvas.
        let locks: Vec<_> = fs::read_dir(folder.0.join("run/rature"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(locks.len(), 1);
        assert_eq!(fs::read_dir(folder.0.join("notes")).unwrap().count(), 1);
    }
}
