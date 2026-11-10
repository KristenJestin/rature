//! What reading a file takes, worked out off the main thread: reading it, parsing it, drawing its
//! formulas, checking its links, and comparing it to the version it was last read in (or to
//! git's). The main thread only puts the result in place, and only the result of the latest
//! request: one asked for meanwhile makes the earlier one stale.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::diff::{self, Changes};
use crate::document::{self, Document};
use crate::formats::{self, Content, Loaded};
use crate::state;

/// What "what changed" compares the file to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Baseline {
    LastRead,
    Head,
}

/// Counts the requests of one kind: a result is put in place only when no request came after
/// the one it answers.
#[derive(Default, Debug)]
pub struct Generation(u64);

impl Generation {
    /// A new request: the ones before it are stale from now on.
    pub fn next(&mut self) -> u64 {
        self.0 += 1;
        self.0
    }

    pub fn is_current(&self, generation: u64) -> bool {
        self.0 == generation
    }
}

/// The version a file is compared to, parsed: kept so that the next change of the file, compared
/// to the same version, does not parse it again.
#[derive(Clone)]
pub struct Old {
    pub source: String,
    pub ink: String,
    pub doc: Arc<Document>,
}

/// What a file is compared to, and how it differs.
pub enum Against {
    /// Not Markdown, or nothing to compare to.
    Nothing,
    /// Never read before: it is read now, nothing changed.
    FirstRead,
    /// Not in git, or not committed.
    NoHead,
    Old(Old, Changes),
}

/// A file read off the main thread.
pub struct Done {
    pub path: PathBuf,
    pub loaded: io::Result<Loaded>,
    pub against: Against,
    /// The fingerprint of each top-level block's source, for a Markdown file.
    pub prints: Vec<u64>,
    /// How long the work took, off the main thread.
    pub took: Duration,
}

/// Reads `path`, its formulas in `ink`, and compares it to `baseline`, reusing `cached` when it
/// is the version compared to.
pub fn load(path: PathBuf, ink: String, baseline: Baseline, cached: Option<Old>) -> Done {
    let started = Instant::now();
    let loaded = formats::load(&path, &ink);
    let (against, prints) = match &loaded {
        Ok(Loaded {
            content: Content::Markdown(doc),
            ..
        }) => (compare(&path, doc, &ink, baseline, cached), prints(doc)),
        _ => (Against::Nothing, Vec::new()),
    };
    Done {
        path,
        loaded,
        against,
        prints,
        took: started.elapsed(),
    }
}

/// How `doc`, the file at `path`, differs from its `baseline`.
pub fn compare(
    path: &Path,
    doc: &Document,
    ink: &str,
    baseline: Baseline,
    cached: Option<Old>,
) -> Against {
    let source = match baseline {
        Baseline::LastRead => match state::last_read(path) {
            Some(source) => source,
            None => return Against::FirstRead,
        },
        Baseline::Head => match git_head(path) {
            Some(source) => source,
            None => return Against::NoHead,
        },
    };
    let (old, changes) = against(source, doc, ink, cached);
    Against::Old(old, changes)
}

/// How `doc` differs from `source`, which is parsed unless `cached` is it, in the same ink.
pub fn against(source: String, doc: &Document, ink: &str, cached: Option<Old>) -> (Old, Changes) {
    let old = match cached {
        Some(old) if old.source == source && old.ink == ink => old,
        _ => Old {
            doc: Arc::new(document::parse(&source, ink, None)),
            source,
            ink: ink.to_string(),
        },
    };
    let changes = diff::compare(&old.doc, doc);
    (old, changes)
}

/// The file as the last commit has it.
pub fn git_head(path: &Path) -> Option<String> {
    let folder = path.parent()?;
    let name = path.file_name()?.to_string_lossy();
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(folder)
        .arg("show")
        .arg(format!("HEAD:./{name}"))
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// A short name for a block's source: the same text keeps its entrance played.
pub fn fingerprint(source: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut hasher);
    hasher.finish()
}

fn prints(doc: &Document) -> Vec<u64> {
    (0..doc.blocks.len())
        .map(|ix| fingerprint(doc.source_of(ix)))
        .collect()
}

/// Which blocks rise into place. Only the blocks drawn are built, so a block's entrance cannot
/// be left to its own state, which is let go once it scrolls out: a block rises when it is new
/// (the file just opened, or the block just written) and is drawn soon after, and then for as
/// long as its entrance lasts. Scrolled to later, it is simply there.
pub struct Entrances {
    /// When the file was opened: its blocks are new then.
    opened: Instant,
    /// Blocks that came with a change of the file, and when.
    arrived: HashMap<u64, Instant>,
    /// Blocks that rose, and when they started.
    drawn: HashMap<u64, Instant>,
    /// How long an entrance plays.
    play: Duration,
}

impl Entrances {
    /// How long after it arrived a block drawn for the first time still rises.
    const FRESH: Duration = Duration::from_millis(500);

    pub fn new(now: Instant, play: Duration) -> Self {
        Self {
            opened: now,
            arrived: HashMap::new(),
            drawn: HashMap::new(),
            play,
        }
    }

    /// A file opened: all its blocks are new.
    pub fn opened(&mut self, now: Instant) {
        self.opened = now;
        self.arrived.clear();
        self.drawn.clear();
    }

    /// The file changed from `before` to `after`: the blocks it did not have are new.
    pub fn changed(&mut self, before: &[u64], after: &[u64], now: Instant) {
        let before: std::collections::HashSet<&u64> = before.iter().collect();
        for print in after {
            if !before.contains(print) {
                self.arrived.insert(*print, now);
            }
        }
        let keep = Self::FRESH + self.play;
        self.arrived.retain(|_, at| now.duration_since(*at) < keep);
        self.drawn.retain(|_, at| now.duration_since(*at) < keep);
    }

    /// Whether the block `print`, drawn now, rises.
    pub fn rises(&mut self, print: u64, now: Instant) -> bool {
        if let Some(started) = self.drawn.get(&print) {
            return now.duration_since(*started) < self.play;
        }
        let since = self.arrived.get(&print).copied().unwrap_or(self.opened);
        let fresh = now.duration_since(since) < Self::FRESH;
        if fresh {
            self.drawn.insert(print, now);
        }
        fresh
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_latest_request_is_current() {
        let mut loads = Generation::default();
        let first = loads.next();
        assert!(loads.is_current(first));
        let second = loads.next();
        // The first answer, come late, is dropped; the second is put in place.
        assert!(!loads.is_current(first));
        assert!(loads.is_current(second));
    }

    fn folder(name: &str) -> PathBuf {
        let folder =
            std::env::temp_dir().join(format!("rature-load-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        folder
    }

    #[test]
    fn a_file_is_read_and_parsed_off_the_main_thread_in_one_go() {
        let files = folder("files");
        let path = files.join("doc.md");
        std::fs::write(&path, "# A\n\nOne.\n\nTwo.\n").unwrap();
        // Not in git: compared to its last commit, it says so.
        let done = load(path, "#000000".into(), Baseline::Head, None);
        assert!(matches!(
            done.loaded,
            Ok(Loaded {
                content: Content::Markdown(_),
                ..
            })
        ));
        assert!(matches!(done.against, Against::NoHead));
        assert_eq!(done.prints.len(), 3);
        assert_eq!(done.prints[1], fingerprint("One."));
        std::fs::remove_dir_all(files).unwrap();
    }

    #[test]
    fn the_version_compared_to_is_parsed_once() {
        let ink = "#000000";
        let read = "# A\n\nOne.\n".to_string();
        let now = document::parse("# A\n\nOne.\n\nTwo.\n", ink, None);
        let (old, changes) = against(read.clone(), &now, ink, None);
        assert_eq!(changes.stops(), [2]);
        // The file changed again, compared to the same version: not parsed again.
        let later = document::parse("# A\n\nOne.\n\nThree.\n", ink, None);
        let (again, _) = against(read.clone(), &later, ink, Some(old.clone()));
        assert!(Arc::ptr_eq(&old.doc, &again.doc), "parsed again");
        // In other colours, or against another version, it is.
        let (redrawn, _) = against(read, &later, "#ffffff", Some(old.clone()));
        assert!(!Arc::ptr_eq(&old.doc, &redrawn.doc));
        let (other, _) = against("# B\n".into(), &later, ink, Some(old.clone()));
        assert!(!Arc::ptr_eq(&old.doc, &other.doc));
    }

    #[test]
    fn a_block_rises_when_new_and_drawn_soon_not_when_scrolled_to() {
        let play = Duration::from_millis(240);
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut entrances = Entrances::new(t0, play);
        // Opened: what shows at once rises, and goes on rising until its entrance ends.
        assert!(entrances.rises(1, at(10)));
        assert!(entrances.rises(1, at(200)));
        assert!(!entrances.rises(1, at(300)));
        // Scrolled to long after: simply there.
        assert!(!entrances.rises(2, at(2000)));
        // A block the change brought rises; those it kept do not.
        entrances.changed(&[1, 2], &[1, 2, 3], at(3000));
        assert!(entrances.rises(3, at(3010)));
        assert!(!entrances.rises(2, at(3010)));
        // Opened again: its blocks are new again.
        entrances.opened(at(5000));
        assert!(entrances.rises(2, at(5010)));
    }
}
