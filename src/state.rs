//! What Rature keeps between runs, in `$XDG_STATE_HOME/rature`, never beside the files read:
//! where each file was read up to, the files opened last, and each file as it was last marked
//! read (`read/`), which "what changed" compares to.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::home;

/// How many files the recent list keeps.
const RECENT: usize = 50;

#[derive(Default, Serialize, Deserialize)]
pub struct State {
    /// Per file, the top-level block at the top of the window and how far into it.
    pub positions: HashMap<PathBuf, (usize, f32)>,
    /// The files and folders opened last, the latest first.
    pub recent: Vec<PathBuf>,
    /// When each of them was last opened, in seconds since the epoch.
    #[serde(default)]
    pub opened_at: HashMap<PathBuf, u64>,
}

impl gpui_kit::Global for State {}

impl State {
    pub fn load() -> Self {
        std::fs::read_to_string(folder().join("state.json"))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let folder = folder();
        let written = std::fs::create_dir_all(&folder).and_then(|()| {
            let next = folder.join(".state.json.new");
            std::fs::write(
                &next,
                serde_json::to_vec(self).expect("the state serializes"),
            )?;
            std::fs::rename(next, folder.join("state.json"))
        });
        if let Err(error) = written {
            eprintln!("rature: cannot keep the state: {error}");
        }
    }

    pub fn opened(&mut self, path: &Path) {
        self.recent.retain(|p| p != path);
        self.recent.insert(0, path.to_path_buf());
        self.recent.truncate(RECENT);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        self.opened_at.insert(path.to_path_buf(), now);
        let recent = &self.recent;
        self.opened_at.retain(|p, _| recent.contains(p));
    }
}

fn folder() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/state"))
        .join("rature")
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

/// The file as it was last marked read, if it ever was.
pub fn last_read(path: &Path) -> Option<String> {
    std::fs::read_to_string(folder().join("read").join(name(path))).ok()
}

pub fn mark_read(path: &Path, source: &str) {
    let folder = folder().join("read");
    let written = std::fs::create_dir_all(&folder)
        .and_then(|()| std::fs::write(folder.join(name(path)), source));
    if let Err(error) = written {
        eprintln!("rature: cannot keep what was read: {error}");
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
}
