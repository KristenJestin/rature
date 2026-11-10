//! What Rature keeps between runs, in `$XDG_STATE_HOME/rature`, never beside the files read:
//! where each file was read up to, the files opened last, and each file as it was last marked
//! read (`read/`), which "what changed" compares to. It is the user's alone (the folder 0700,
//! its files 0600: a copy of a private note stays private), and it does not grow for ever: what
//! was not read for long is let go.
//!
//! It is written by a thread of its own, in the order the writes were asked for: each write waits
//! for the disk to keep it (tens of milliseconds on some disks), which the window does not.

use std::collections::HashMap;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::config::home;
use crate::fsio;

/// How many files the recent list keeps.
const RECENT: usize = 50;
/// How many files' reading positions are kept, the latest left.
const POSITIONS: usize = 1000;
/// How many files are kept as they were last read, the latest marked.
const READ_FILES: usize = 500;
/// How long a file is kept as it was last read, and its position, unless it is read again.
const KEPT: Duration = Duration::from_secs(90 * 24 * 60 * 60);

#[derive(Default, Serialize, Deserialize)]
pub struct State {
    /// Per file, the top-level block at the top of the window and how far into it.
    pub positions: HashMap<PathBuf, (usize, f32)>,
    /// When each position was last kept, in seconds since the epoch: the oldest go first.
    #[serde(default)]
    pub positioned_at: HashMap<PathBuf, u64>,
    /// The files and folders opened last, the latest first.
    pub recent: Vec<PathBuf>,
    /// When each of them was last opened, in seconds since the epoch.
    #[serde(default)]
    pub opened_at: HashMap<PathBuf, u64>,
}

impl gpui_kit::Global for State {}

impl State {
    /// The state kept, and what was kept too long let go.
    pub fn load() -> Self {
        crate::timing::time("  state: read copies pruned", || {
            prune_read(&folder().join("read"), READ_FILES, KEPT, SystemTime::now())
        });
        let mut state: Self = crate::timing::time("  state: state.json read", || {
            std::fs::read_to_string(folder().join("state.json"))
                .ok()
                .and_then(|text| serde_json::from_str(&text).ok())
                .unwrap_or_default()
        });
        state.prune_positions(now());
        state
    }

    /// Kept as it is now, written off the main thread.
    pub fn save(&self) {
        let folder = folder();
        let bytes = serde_json::to_vec(self).expect("the state serializes");
        later(move || {
            let written = private_folder(&folder)
                .and_then(|()| fsio::write_private(&folder.join("state.json"), &bytes));
            if let Err(error) = written {
                eprintln!("rature: cannot keep the state: {error}");
            }
        });
    }

    pub fn opened(&mut self, path: &Path) {
        self.recent.retain(|p| p != path);
        self.recent.insert(0, path.to_path_buf());
        self.recent.truncate(RECENT);
        self.opened_at.insert(path.to_path_buf(), now());
        let recent = &self.recent;
        self.opened_at.retain(|p, _| recent.contains(p));
    }

    /// Keeps where `path` was read up to.
    pub fn set_position(&mut self, path: &Path, position: (usize, f32)) {
        self.positions.insert(path.to_path_buf(), position);
        self.positioned_at.insert(path.to_path_buf(), now());
        self.prune_positions(now());
    }

    /// No more than `POSITIONS` positions, none older than `KEPT`: those of files not read for
    /// long go, the oldest first. A position kept before they were dated counts as the oldest.
    fn prune_positions(&mut self, now: u64) {
        let oldest = now.saturating_sub(KEPT.as_secs());
        let at = |p: &PathBuf, dates: &HashMap<PathBuf, u64>| dates.get(p).copied().unwrap_or(0);
        let dates = &self.positioned_at;
        self.positions.retain(|p, _| at(p, dates) >= oldest);
        if self.positions.len() > POSITIONS {
            let mut by_age: Vec<(u64, PathBuf)> = self
                .positions
                .keys()
                .map(|p| (at(p, dates), p.clone()))
                .collect();
            by_age.sort();
            for (_, path) in by_age.iter().take(self.positions.len() - POSITIONS) {
                self.positions.remove(path);
            }
        }
        let positions = &self.positions;
        self.positioned_at.retain(|p, _| positions.contains_key(p));
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn folder() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/state"))
        .join("rature")
}

/// Makes `folder` if it is not there, and leaves it to its owner alone, made by an earlier
/// version or not.
fn private_folder(folder: &Path) -> std::io::Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(folder)?;
    let mode = std::fs::metadata(folder)?.permissions().mode();
    if mode & 0o077 != 0 {
        std::fs::set_permissions(folder, std::fs::Permissions::from_mode(mode & 0o700))?;
    }
    Ok(())
}

/// FNV-1a: a name for a path that stays the same from one build to the next.
fn name(path: &Path) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in path.as_os_str().as_encoded_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

/// The file as it was last marked read, if it ever was: as it is being written, if it is.
pub fn last_read(path: &Path) -> Option<String> {
    last_read_in(&folder(), path)
}

fn last_read_in(folder: &Path, path: &Path) -> Option<String> {
    let file = folder.join("read").join(name(path));
    if let Some(source) = unwritten().get(&file) {
        return Some(source.to_string());
    }
    std::fs::read_to_string(file).ok()
}

/// Keeps `source` as the file at `path` was read, written off the main thread; `last_read` has
/// it at once.
pub fn mark_read(path: &Path, source: &str) {
    mark_read_in(folder(), path, source);
}

fn mark_read_in(folder: PathBuf, path: &Path, source: &str) {
    let read = folder.join("read");
    let file = read.join(name(path));
    let source: Arc<str> = source.into();
    unwritten().insert(file.clone(), source.clone());
    later(move || {
        let new = !file.exists();
        let written = private_folder(&folder)
            .and_then(|()| private_folder(&read))
            .and_then(|()| fsio::write_private(&file, source.as_bytes()));
        if let Err(error) = written {
            eprintln!("rature: cannot keep what was read: {error}");
        }
        // Written, or not writable: the disk has the last word, unless it was marked again.
        let mut unwritten = unwritten();
        if unwritten
            .get(&file)
            .is_some_and(|s| Arc::ptr_eq(s, &source))
        {
            unwritten.remove(&file);
        }
        drop(unwritten);
        // A file more: the oldest may go.
        if new {
            prune_read(&read, READ_FILES, KEPT, SystemTime::now());
        }
    });
}

/// Waits until what was asked to be kept is written (a few seconds at most): before quitting.
pub fn flush() {
    let (done, written) = mpsc::channel();
    later(move || {
        let _ = done.send(());
    });
    let _ = written.recv_timeout(Duration::from_secs(5));
}

type Job = Box<dyn FnOnce() + Send>;

/// Runs `job` on the thread that writes the state, after the jobs asked for before it; here, if
/// that thread cannot be had.
fn later(job: impl FnOnce() + Send + 'static) {
    static WRITER: OnceLock<Option<Sender<Job>>> = OnceLock::new();
    let writer = WRITER.get_or_init(|| {
        let (jobs, queue) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("rature-state".into())
            .spawn(move || {
                for job in queue {
                    job();
                }
            })
            .ok()
            .map(|_| jobs)
    });
    let job: Job = Box::new(job);
    let left = match writer {
        Some(jobs) => jobs.send(job).err().map(|failed| failed.0),
        None => Some(job),
    };
    if let Some(job) = left {
        job();
    }
}

/// The copies marked read and not yet written, by the file they go to.
fn unwritten() -> std::sync::MutexGuard<'static, HashMap<PathBuf, Arc<str>>> {
    static UNWRITTEN: OnceLock<Mutex<HashMap<PathBuf, Arc<str>>>> = OnceLock::new();
    UNWRITTEN
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// Lets go of the files of `read` marked longer than `kept` before `now`, then of the oldest
/// past the `most` latest; those kept are made their owner's alone, as an earlier version may
/// have left them readable by others.
fn prune_read(read: &Path, most: usize, kept: Duration, now: SystemTime) {
    let Ok(entries) = std::fs::read_dir(read) else {
        return;
    };
    let mut files: Vec<(SystemTime, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let meta = entry.metadata().ok()?;
            meta.is_file().then(|| {
                (
                    meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                    entry.path(),
                )
            })
        })
        .collect();
    // The latest first.
    files.sort_by_key(|(marked, _)| std::cmp::Reverse(*marked));
    let oldest = now.checked_sub(kept).unwrap_or(SystemTime::UNIX_EPOCH);
    for (ix, (marked, path)) in files.iter().enumerate() {
        if ix >= most || *marked < oldest {
            let _ = std::fs::remove_file(path);
        } else if let Ok(meta) = std::fs::metadata(path)
            && meta.permissions().mode() & 0o077 != 0
        {
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_has_one_name_and_two_paths_two() {
        assert_eq!(name(Path::new("/a/b.md")), name(Path::new("/a/b.md")));
        assert_ne!(name(Path::new("/a/b.md")), name(Path::new("/a/c.md")));
    }

    #[test]
    fn a_file_opened_again_moves_to_the_top_once() {
        let mut state = State::default();
        state.opened(Path::new("/a.md"));
        state.opened(Path::new("/b.md"));
        state.opened(Path::new("/a.md"));
        assert_eq!(
            state.recent,
            [PathBuf::from("/a.md"), PathBuf::from("/b.md")]
        );
    }

    #[test]
    fn positions_are_bounded_and_the_oldest_go() {
        let mut state = State::default();
        let day = 24 * 60 * 60;
        let now = 1000 * day;
        for ix in 0..POSITIONS + 10 {
            let path = PathBuf::from(format!("/{ix}.md"));
            state.positions.insert(path.clone(), (ix, 0.));
            state.positioned_at.insert(path, now - ix as u64);
        }
        // Undated, from an earlier version: the oldest of all.
        state.positions.insert("/old.md".into(), (0, 0.));
        state.prune_positions(now);
        assert_eq!(state.positions.len(), POSITIONS);
        assert!(state.positions.contains_key(Path::new("/0.md")));
        assert!(!state.positions.contains_key(Path::new("/old.md")));
        assert!(
            !state
                .positions
                .contains_key(&PathBuf::from(format!("/{}.md", POSITIONS + 9)))
        );
        assert_eq!(state.positioned_at.len(), POSITIONS);

        // Not read for longer than kept: gone, however few they are.
        let mut state = State::default();
        state.positions.insert("/stale.md".into(), (1, 0.));
        state
            .positioned_at
            .insert("/stale.md".into(), now - 100 * day);
        state.positions.insert("/fresh.md".into(), (1, 0.));
        state.positioned_at.insert("/fresh.md".into(), now - day);
        state.prune_positions(now);
        assert_eq!(state.positions.len(), 1);
        assert!(state.positions.contains_key(Path::new("/fresh.md")));
    }

    #[test]
    fn a_copy_marked_read_is_had_at_once_and_written_in_the_order_asked() {
        let folder =
            std::env::temp_dir().join(format!("rature-state-marked-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        let path = Path::new("/notes/a.md");
        mark_read_in(folder.clone(), path, "one");
        mark_read_in(folder.clone(), path, "two");
        // At once, written or not: the latest.
        assert_eq!(last_read_in(&folder, path).as_deref(), Some("two"));
        flush();
        // Written in the order asked: the latest is the one kept, and nothing waits any more.
        let file = folder.join("read").join(name(path));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "two");
        assert!(!unwritten().contains_key(&file));
        assert_eq!(last_read_in(&folder, path).as_deref(), Some("two"));
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn copies_read_long_ago_or_past_the_most_go_and_the_rest_are_private() {
        let read = std::env::temp_dir().join(format!("rature-state-read-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&read);
        private_folder(&read).unwrap();
        assert_eq!(
            std::fs::metadata(&read).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let now = SystemTime::now();
        let day = Duration::from_secs(24 * 60 * 60);
        for (name, age) in [("a", 1), ("b", 2), ("c", 3), ("d", 200)] {
            let path = read.join(name);
            std::fs::write(&path, name).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(now - day * age)
                .unwrap();
        }
        prune_read(&read, 2, KEPT, now);
        let mut left: Vec<String> = std::fs::read_dir(&read)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, ["a", "b"]);
        for name in left {
            let mode = std::fs::metadata(read.join(name))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        std::fs::remove_dir_all(read).unwrap();
    }
}
